//! macOS: `mmap` and `mlock`. No large pages for user code on Apple
//! silicon, and one memory node. Compile-checked only; agent C runs it.

use std::ptr::NonNull;
use std::sync::OnceLock;

use super::Alloc;
use crate::{Error, LargePages, PageKind};

fn errno() -> i64 {
    i64::from(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

pub(crate) fn base_page_size() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: sysconf has no preconditions.
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 { p as usize } else { 16384 }
    })
}

pub(crate) fn alloc(
    len: usize,
    node: Option<u32>,
    large: LargePages,
    lock: bool,
) -> Result<Alloc, Error> {
    match node {
        None | Some(0) => {}
        Some(n) => return Err(Error::NoSuchNode(n)),
    }
    if large == LargePages::Require {
        return Err(Error::LargePagesUnavailable(
            "macOS offers no large pages to user code".into(),
        ));
    }
    // SAFETY: a fresh anonymous private mapping; affects no existing memory.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(Error::Os {
            call: "mmap",
            code: errno(),
        });
    }
    let ptr = NonNull::new(p.cast::<u8>()).ok_or(Error::Os {
        call: "mmap",
        code: 0,
    })?;
    // SAFETY: locks a range of the mapping just made.
    if lock && unsafe { libc::mlock(ptr.as_ptr().cast(), len) } != 0 {
        let code = errno();
        // SAFETY: just mapped, released once.
        unsafe { free(ptr, len) };
        return Err(Error::LockFailed(format!("mlock failed (errno {code})")));
    }
    Ok(Alloc {
        ptr,
        len,
        page_size: base_page_size(),
        pages: PageKind::Normal,
        locked: lock,
    })
}

/// Unmaps a mapping from `alloc`.
///
/// # Safety
///
/// `ptr` and `len` must describe a mapping from `alloc` that is not freed
/// already, and nothing may use the memory afterwards.
pub(crate) unsafe fn free(ptr: NonNull<u8>, len: usize) {
    // SAFETY: per the caller's contract.
    unsafe { libc::munmap(ptr.as_ptr().cast(), len) };
}
