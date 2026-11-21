//! stagelanes-mem: pinned memory pools.
//!
//! - [`Region`]: one contiguous block of memory, committed and touched up
//!   front, optionally on large pages, placed on a NUMA node and locked so
//!   it is never paged out. Sizes are capped ([`RegionConfig::max_bytes`]).
//! - [`BlockPool`]: a region carved into fixed-size [`Block`]s, owned by one
//!   core. Taking and returning a block never allocates, locks or makes a
//!   syscall, so stage cores can use it after start-up.
//!
//! Per OS:
//!
//! | | Large pages | NUMA placement | Locking |
//! | --- | --- | --- | --- |
//! | Windows | `MEM_LARGE_PAGES`; needs the "Lock pages in memory" right (`SeLockMemoryPrivilege`) | `VirtualAllocExNuma` | `VirtualLock` after growing the working set; large pages are always locked |
//! | Linux | `MAP_HUGETLB` from the reserved huge page pool; with [`LargePages::Prefer`], falls back to `madvise(MADV_HUGEPAGE)` | `mbind(MPOL_BIND)` before the pages are touched | `mlock`, limited by `ulimit -l`; huge pages are never swapped |
//! | macOS | not available | not available (one node) | `mlock` |

use std::fmt;
use std::ptr::NonNull;

mod platform;

/// Default cap on a region's size: 1 GiB. Raise it per region with
/// [`RegionConfig::max_bytes`].
pub const DEFAULT_MAX_BYTES: usize = 1 << 30;

/// Whether to back a region with large pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LargePages {
    /// Use normal pages.
    Off,
    /// Use large pages if the OS grants them, otherwise normal pages.
    Prefer,
    /// Use large pages or fail.
    Require,
}

/// What backs a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    /// Normal pages.
    Normal,
    /// Large pages, granted by the OS.
    Large,
    /// Normal pages with a hint to the kernel to use transparent huge pages
    /// (Linux). Whether it did is not known here.
    TransparentHint,
}

/// How to allocate a [`Region`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionConfig {
    /// Bytes wanted. Rounded up to the page size.
    pub bytes: usize,
    /// NUMA node to place the memory on, as the OS numbers nodes. `None`
    /// leaves placement to the OS.
    pub numa_node: Option<u32>,
    /// Large page policy.
    pub large_pages: LargePages,
    /// Lock the pages in memory. Fails the allocation if locking fails.
    pub lock: bool,
    /// Refuse regions larger than this.
    pub max_bytes: usize,
}

impl RegionConfig {
    /// `bytes` of locked memory, large pages if available, no NUMA
    /// preference, default cap.
    pub fn new(bytes: usize) -> Self {
        Self {
            bytes,
            numa_node: None,
            large_pages: LargePages::Prefer,
            lock: true,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Errors from allocating pinned memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A zero-byte region or zero-byte block was asked for.
    ZeroSize,
    /// The request is larger than the configured cap.
    OverCap {
        /// Bytes asked for, after rounding to the page size.
        requested: usize,
        /// The cap.
        cap: usize,
    },
    /// [`LargePages::Require`] was set and large pages could not be had.
    LargePagesUnavailable(String),
    /// The NUMA node does not exist.
    NoSuchNode(u32),
    /// The OS does not offer this.
    Unsupported(&'static str),
    /// Locking the pages failed.
    LockFailed(String),
    /// An OS call failed.
    Os {
        /// Name of the call.
        call: &'static str,
        /// OS error code.
        code: i64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ZeroSize => f.write_str("zero-sized request"),
            Error::OverCap { requested, cap } => {
                write!(f, "{requested} bytes requested, cap is {cap}")
            }
            Error::LargePagesUnavailable(why) => write!(f, "large pages unavailable: {why}"),
            Error::NoSuchNode(n) => write!(f, "NUMA node {n} does not exist"),
            Error::Unsupported(what) => write!(f, "not supported on this OS: {what}"),
            Error::LockFailed(why) => write!(f, "could not lock pages: {why}"),
            Error::Os { call, code } => write!(f, "{call} failed with code {code}"),
        }
    }
}

impl std::error::Error for Error {}

/// A contiguous, page-aligned, zero-filled block of memory that stays
/// resident. Freed on drop.
pub struct Region {
    ptr: NonNull<u8>,
    len: usize,
    page_size: usize,
    pages: PageKind,
    locked: bool,
    numa_node: Option<u32>,
}

// SAFETY: a `Region` exclusively owns its mapping. Shared references only
// expose the address and metadata; writing needs `&mut self`.
unsafe impl Send for Region {}
// SAFETY: as above; `&Region` gives no access to the bytes.
unsafe impl Sync for Region {}

impl fmt::Debug for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Region")
            .field("ptr", &self.ptr)
            .field("len", &self.len)
            .field("page_size", &self.page_size)
            .field("pages", &self.pages)
            .field("locked", &self.locked)
            .field("numa_node", &self.numa_node)
            .finish()
    }
}

impl Region {
    /// Allocates, places, locks and touches a region. Call at start-up; this
    /// makes several syscalls.
    pub fn new(config: &RegionConfig) -> Result<Self, Error> {
        if config.bytes == 0 {
            return Err(Error::ZeroSize);
        }
        let base = platform::base_page_size();
        let rounded = config
            .bytes
            .checked_next_multiple_of(base)
            .ok_or(Error::OverCap {
                requested: usize::MAX,
                cap: config.max_bytes,
            })?;
        if rounded > config.max_bytes {
            return Err(Error::OverCap {
                requested: rounded,
                cap: config.max_bytes,
            });
        }
        let a = platform::alloc(rounded, config.numa_node, config.large_pages, config.lock)?;
        if a.len > config.max_bytes {
            // Large-page rounding pushed it over the cap.
            // SAFETY: `a` came from `platform::alloc` and is freed once here.
            unsafe { platform::free(a.ptr, a.len) };
            return Err(Error::OverCap {
                requested: a.len,
                cap: config.max_bytes,
            });
        }
        let region = Region {
            ptr: a.ptr,
            len: a.len,
            page_size: a.page_size,
            pages: a.pages,
            locked: a.locked,
            numa_node: config.numa_node,
        };
        region.touch();
        Ok(region)
    }

    /// Writes one byte per page so every page is resident before the hot
    /// path runs. The memory is already zero; this writes zero.
    fn touch(&self) {
        let step = self.page_size.min(platform::base_page_size()).max(1);
        let mut off = 0;
        while off < self.len {
            // SAFETY: `off < len`, so the address is inside the mapping,
            // which is readable and writable and owned by this region.
            unsafe { self.ptr.as_ptr().add(off).write_volatile(0) };
            off += step;
        }
    }

    /// Start address.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Length in bytes (the request rounded up to the page size).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false: regions are never empty.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Page size backing the region.
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// What kind of pages back the region.
    pub fn pages(&self) -> PageKind {
        self.pages
    }

    /// Whether the pages are locked in memory.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// The NUMA node asked for.
    pub fn numa_node(&self) -> Option<u32> {
        self.numa_node
    }

    /// The whole region as bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the mapping is `len` readable bytes, initialised (zeroed by
        // the OS), and lives as long as `self`. Writes need `&mut self`, so
        // none can happen while this borrow lives.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// The whole region as mutable bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` makes this the only access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` came from `platform::alloc`, and the region
        // is dropped once. Blocks handed out by a `BlockPool` keep the
        // region alive (see `BlockPool`'s `Drop`).
        unsafe { platform::free(self.ptr, self.len) };
    }
}

/// One fixed-size block from a [`BlockPool`]. Owns its bytes exclusively
/// until returned with [`BlockPool::put`]. Can be sent to another thread,
/// for example through a ring to the next stage.
#[derive(Debug)]
pub struct Block {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: a `Block` is the only handle to its bytes, which are plain memory.
unsafe impl Send for Block {}
// SAFETY: `&Block` gives read access only.
unsafe impl Sync for Block {}

impl Block {
    /// Start address.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Block size in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false: blocks are never empty.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The block's bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the block is `len` bytes inside a live region (a pool never
        // frees its region while blocks are out), initialised, and this
        // handle is its only owner.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// The block's bytes, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, with `&mut self` for exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

/// A [`Region`] split into equal blocks, for one owning core.
///
/// [`take`](BlockPool::take) and [`put`](BlockPool::put) only push and pop a
/// free list whose capacity is reserved up front: no allocation, no lock, no
/// syscall. The pool is not `Sync`; blocks travel between cores, but taking
/// and returning happen on the owner.
///
/// Dropping a pool with blocks still out leaks the region rather than
/// freeing memory those blocks point into.
pub struct BlockPool {
    region: Option<Region>,
    block_bytes: usize,
    count: usize,
    free: Vec<u32>,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl fmt::Debug for BlockPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockPool")
            .field("region", &self.region)
            .field("block_bytes", &self.block_bytes)
            .field("count", &self.count)
            .field("free", &self.free.len())
            .finish()
    }
}

impl BlockPool {
    /// Splits `region` into blocks of `block_bytes`, rounded up to a
    /// multiple of 64 so blocks never share a cache line. Leftover bytes at
    /// the end are unused.
    pub fn new(region: Region, block_bytes: usize) -> Result<Self, Error> {
        if block_bytes == 0 {
            return Err(Error::ZeroSize);
        }
        let block_bytes = block_bytes.next_multiple_of(64);
        let count = (region.len() / block_bytes).min(u32::MAX as usize);
        if count == 0 {
            return Err(Error::OverCap {
                requested: block_bytes,
                cap: region.len(),
            });
        }
        // Reverse order so blocks come out in address order.
        let free: Vec<u32> = (0..count as u32).rev().collect();
        Ok(Self {
            region: Some(region),
            block_bytes,
            count,
            free,
            _not_sync: std::marker::PhantomData,
        })
    }

    fn region(&self) -> &Region {
        // Only `Drop` takes the region out.
        self.region.as_ref().expect("region present until drop")
    }

    /// Takes a free block, or `None` if all are out.
    pub fn take(&mut self) -> Option<Block> {
        let i = self.free.pop()? as usize;
        // SAFETY: `i < count` and `count * block_bytes <= region.len()`, so
        // the offset is inside the region.
        let ptr = unsafe { self.region().as_ptr().add(i * self.block_bytes) };
        Some(Block {
            // SAFETY: an offset inside a non-null mapping is non-null.
            ptr: unsafe { NonNull::new_unchecked(ptr) },
            len: self.block_bytes,
        })
    }

    /// Returns a block. Gives it back if it did not come from this pool.
    pub fn put(&mut self, block: Block) -> Result<(), Block> {
        let base = self.region().as_ptr() as usize;
        let addr = block.ptr.as_ptr() as usize;
        let off = addr.wrapping_sub(base);
        if addr < base || off % self.block_bytes != 0 || off / self.block_bytes >= self.count {
            return Err(block);
        }
        // The free list never exceeds `count` entries: each block exists once
        // and `Block` is not `Clone`, so this push stays within capacity.
        debug_assert!(self.free.len() < self.count);
        self.free.push((off / self.block_bytes) as u32);
        Ok(())
    }

    /// Block size in bytes.
    pub fn block_bytes(&self) -> usize {
        self.block_bytes
    }

    /// Total blocks.
    pub fn capacity(&self) -> usize {
        self.count
    }

    /// Blocks currently free.
    pub fn available(&self) -> usize {
        self.free.len()
    }

    /// The region the blocks come from.
    pub fn region_info(&self) -> &Region {
        self.region()
    }
}

impl Drop for BlockPool {
    fn drop(&mut self) {
        if self.free.len() != self.count {
            // Blocks are still out and point into the region: keep it.
            std::mem::forget(self.region.take());
        }
    }
}

#[cfg(test)]
mod tests;
