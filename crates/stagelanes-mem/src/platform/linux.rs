//! Linux: `mmap` (with `MAP_HUGETLB` for large pages), `mbind`, `mlock`.
//!
//! Not run on this milestone's build machine (Windows): compile-checked
//! there, tested by CI on Linux and by agent A on the Dell.

use std::ptr::NonNull;
use std::sync::OnceLock;

use super::Alloc;
use crate::{Error, LargePages, PageKind};

/// `MPOL_BIND` from `<linux/mempolicy.h>`.
const MPOL_BIND: libc::c_long = 2;

fn errno() -> i64 {
    i64::from(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

pub(crate) fn base_page_size() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: sysconf has no preconditions.
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 { p as usize } else { 4096 }
    })
}

/// Default huge page size from `/proc/meminfo`, 2 MiB if not found.
fn huge_page_size() -> usize {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Hugepagesize:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<usize>().ok())
        })
        .map_or(2 << 20, |kb| kb * 1024)
}

fn map(len: usize, extra_flags: libc::c_int) -> Option<NonNull<u8>> {
    // SAFETY: a fresh anonymous private mapping at an address the kernel
    // picks; no existing memory is affected.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | extra_flags,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        None
    } else {
        NonNull::new(p.cast())
    }
}

/// Binds the not-yet-touched range to `node`.
fn bind(ptr: NonNull<u8>, len: usize, node: u32) -> Result<(), Error> {
    if !std::path::Path::new(&format!("/sys/devices/system/node/node{node}")).exists() {
        return Err(Error::NoSuchNode(node));
    }
    let bits = libc::c_ulong::BITS as usize;
    let mut mask: Vec<libc::c_ulong> = vec![0; node as usize / bits + 1];
    mask[node as usize / bits] |= 1 << (node as usize % bits);
    // The kernel reads `maxnode - 1` bits.
    let maxnode = (mask.len() * bits + 1) as libc::c_ulong;
    // SAFETY: `ptr..ptr+len` is a mapping we own; `mask` holds `maxnode - 1`
    // readable bits; flags 0.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_mbind,
            ptr.as_ptr(),
            len,
            MPOL_BIND,
            mask.as_ptr(),
            maxnode,
            0 as libc::c_uint,
        )
    };
    if rc != 0 {
        return Err(Error::Os {
            call: "mbind",
            code: errno(),
        });
    }
    Ok(())
}

pub(crate) fn alloc(
    len: usize,
    node: Option<u32>,
    large: LargePages,
    lock: bool,
) -> Result<Alloc, Error> {
    if large != LargePages::Off {
        let huge = huge_page_size();
        let rounded = len.next_multiple_of(huge);
        match map(rounded, libc::MAP_HUGETLB) {
            Some(ptr) => {
                if let Some(n) = node
                    && let Err(e) = bind(ptr, rounded, n)
                {
                    // SAFETY: just mapped, released once.
                    unsafe { free(ptr, rounded) };
                    return Err(e);
                }
                // hugetlb pages are never swapped out.
                return Ok(Alloc {
                    ptr,
                    len: rounded,
                    page_size: huge,
                    pages: PageKind::Large,
                    locked: true,
                });
            }
            None if large == LargePages::Require => {
                return Err(Error::LargePagesUnavailable(format!(
                    "mmap MAP_HUGETLB of {rounded} bytes failed (errno {}); reserve huge \
                     pages with /proc/sys/vm/nr_hugepages",
                    errno()
                )));
            }
            None => {}
        }
    }

    let ptr = map(len, 0).ok_or_else(|| Error::Os {
        call: "mmap",
        code: errno(),
    })?;
    let mut pages = PageKind::Normal;
    if large == LargePages::Prefer {
        // SAFETY: advice on a mapping we own; no effect on its contents.
        if unsafe { libc::madvise(ptr.as_ptr().cast(), len, libc::MADV_HUGEPAGE) } == 0 {
            pages = PageKind::TransparentHint;
        }
    }
    if let Some(n) = node
        && let Err(e) = bind(ptr, len, n)
    {
        // SAFETY: just mapped, released once.
        unsafe { free(ptr, len) };
        return Err(e);
    }
    if lock {
        // SAFETY: locks (and faults in) a range of a mapping we own.
        if unsafe { libc::mlock(ptr.as_ptr().cast(), len) } != 0 {
            let code = errno();
            // SAFETY: just mapped, released once.
            unsafe { free(ptr, len) };
            return Err(Error::LockFailed(format!(
                "mlock of {len} bytes failed (errno {code}); raise the locked-memory \
                 limit (ulimit -l)"
            )));
        }
    }
    Ok(Alloc {
        ptr,
        len,
        page_size: base_page_size(),
        pages,
        locked: lock,
    })
}

/// Unmaps a mapping from `alloc`, which also unlocks it.
///
/// # Safety
///
/// `ptr` and `len` must describe a mapping from `alloc` that is not freed
/// already, and nothing may use the memory afterwards.
pub(crate) unsafe fn free(ptr: NonNull<u8>, len: usize) {
    // SAFETY: per the caller's contract.
    unsafe { libc::munmap(ptr.as_ptr().cast(), len) };
}
