//! Windows: `VirtualAllocExNuma`, `MEM_LARGE_PAGES`, `VirtualLock`.

use std::ptr::NonNull;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_LOCK_MEMORY_NAME,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Memory::{
    GetLargePageMinimum, GetProcessWorkingSetSizeEx, MEM_COMMIT, MEM_LARGE_PAGES, MEM_RELEASE,
    MEM_RESERVE, PAGE_READWRITE, SetProcessWorkingSetSizeEx, VirtualAllocExNuma, VirtualFree,
    VirtualLock,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::SystemServices::NUMA_NO_PREFERRED_NODE;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetNumaHighestNodeNumber, OpenProcessToken,
};

use super::Alloc;
use crate::{Error, LargePages, PageKind};

fn last_error() -> u32 {
    // SAFETY: reads the thread's last-error value; no preconditions.
    unsafe { GetLastError() }
}

fn os_error(call: &'static str) -> Error {
    Error::Os {
        call,
        code: i64::from(last_error()),
    }
}

pub(crate) fn base_page_size() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: SYSTEM_INFO is plain data; all-zero is a valid value that
        // GetSystemInfo overwrites.
        let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, writable SYSTEM_INFO.
        unsafe { GetSystemInfo(&mut info) };
        (info.dwPageSize as usize).max(4096)
    })
}

/// Enables `SeLockMemoryPrivilege` in the process token, once. Large pages
/// need it. The account must hold the "Lock pages in memory" right
/// (secpol.msc, User Rights Assignment) for this to succeed; it is not
/// granted to anyone by default.
fn enable_lock_memory_privilege() -> Result<(), String> {
    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            let mut token: HANDLE = std::ptr::null_mut();
            // SAFETY: the pseudo-handle from GetCurrentProcess needs no
            // closing; `token` receives a handle we close below.
            let ok = unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                    &mut token,
                )
            };
            if ok == 0 {
                return Err(format!("OpenProcessToken failed ({})", last_error()));
            }
            let mut luid = LUID {
                LowPart: 0,
                HighPart: 0,
            };
            // SAFETY: SE_LOCK_MEMORY_NAME is a static NUL-terminated wide
            // string; `luid` is writable.
            let ok =
                unsafe { LookupPrivilegeValueW(std::ptr::null(), SE_LOCK_MEMORY_NAME, &mut luid) };
            let result = if ok == 0 {
                Err(format!("LookupPrivilegeValueW failed ({})", last_error()))
            } else {
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                // SAFETY: `token` is open with TOKEN_ADJUST_PRIVILEGES;
                // `privileges` is a valid one-entry TOKEN_PRIVILEGES; no
                // previous state is requested.
                let ok = unsafe {
                    AdjustTokenPrivileges(
                        token,
                        0,
                        &privileges,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                // AdjustTokenPrivileges succeeds even when the privilege is
                // not held; the last error says so.
                let err = last_error();
                if ok == 0 {
                    Err(format!("AdjustTokenPrivileges failed ({err})"))
                } else if err == ERROR_NOT_ALL_ASSIGNED {
                    Err("this account does not hold SeLockMemoryPrivilege \
                         (\"Lock pages in memory\" in secpol.msc)"
                        .to_owned())
                } else {
                    Ok(())
                }
            };
            // SAFETY: `token` was opened above and is closed once.
            unsafe { CloseHandle(token) };
            result
        })
        .clone()
}

fn check_node(node: Option<u32>) -> Result<u32, Error> {
    let Some(node) = node else {
        return Ok(NUMA_NO_PREFERRED_NODE);
    };
    let mut highest: u32 = 0;
    // SAFETY: `highest` is writable.
    if unsafe { GetNumaHighestNodeNumber(&mut highest) } == 0 {
        return Err(os_error("GetNumaHighestNodeNumber"));
    }
    if node > highest {
        return Err(Error::NoSuchNode(node));
    }
    Ok(node)
}

fn virtual_alloc(len: usize, kind: u32, node: u32) -> Option<NonNull<u8>> {
    // SAFETY: reserving and committing fresh memory anywhere in our own
    // address space; no existing memory is affected.
    let p = unsafe {
        VirtualAllocExNuma(
            GetCurrentProcess(),
            std::ptr::null(),
            len,
            kind,
            PAGE_READWRITE,
            node,
        )
    };
    NonNull::new(p.cast())
}

/// Grows the working-set limits by `len` so `VirtualLock` can lock that much
/// more.
fn grow_working_set(len: usize) -> Result<(), String> {
    let (mut min, mut max, mut flags) = (0usize, 0usize, 0u32);
    // SAFETY: the pseudo-handle needs no closing; the outputs are writable.
    let ok =
        unsafe { GetProcessWorkingSetSizeEx(GetCurrentProcess(), &mut min, &mut max, &mut flags) };
    if ok == 0 {
        return Err(format!(
            "GetProcessWorkingSetSizeEx failed ({})",
            last_error()
        ));
    }
    let (min, max) = (
        min.saturating_add(len),
        max.saturating_add(len).max(min.saturating_add(len)),
    );
    // SAFETY: as above. Flags 0 keeps the limits soft.
    let ok = unsafe { SetProcessWorkingSetSizeEx(GetCurrentProcess(), min, max, 0) };
    if ok == 0 {
        return Err(format!(
            "SetProcessWorkingSetSizeEx failed ({})",
            last_error()
        ));
    }
    Ok(())
}

pub(crate) fn alloc(
    len: usize,
    node: Option<u32>,
    large: LargePages,
    lock: bool,
) -> Result<Alloc, Error> {
    let node = check_node(node)?;

    if large != LargePages::Off {
        // SAFETY: no preconditions.
        let large_page = unsafe { GetLargePageMinimum() };
        let attempt = if large_page == 0 {
            Err("the processor does not support large pages".to_owned())
        } else {
            enable_lock_memory_privilege().and_then(|()| {
                let rounded = len.next_multiple_of(large_page);
                virtual_alloc(rounded, MEM_RESERVE | MEM_COMMIT | MEM_LARGE_PAGES, node)
                    .map(|ptr| (ptr, rounded))
                    .ok_or_else(|| {
                        format!(
                            "VirtualAllocExNuma with MEM_LARGE_PAGES failed ({}); physical \
                             memory may be too fragmented for {rounded} bytes of large pages",
                            last_error()
                        )
                    })
            })
        };
        match attempt {
            // Large pages on Windows are never paged out: they count as locked.
            Ok((ptr, rounded)) => {
                return Ok(Alloc {
                    ptr,
                    len: rounded,
                    page_size: large_page,
                    pages: PageKind::Large,
                    locked: true,
                });
            }
            Err(why) if large == LargePages::Require => {
                return Err(Error::LargePagesUnavailable(why));
            }
            Err(_) => {}
        }
    }

    let ptr = virtual_alloc(len, MEM_RESERVE | MEM_COMMIT, node)
        .ok_or_else(|| os_error("VirtualAllocExNuma"))?;
    if lock {
        // Growing the working set is read-modify-write on process-wide
        // limits; two threads doing it at once lose one increment and the
        // second VirtualLock fails with ERROR_WORKING_SET_QUOTA. Start-up
        // path only, so a lock here costs stage cores nothing.
        static WORKING_SET: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = WORKING_SET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let locked = grow_working_set(len).and_then(|()| {
            // SAFETY: `ptr..ptr+len` is the committed range just allocated.
            if unsafe { VirtualLock(ptr.as_ptr().cast(), len) } == 0 {
                Err(format!("VirtualLock failed ({})", last_error()))
            } else {
                Ok(())
            }
        });
        if let Err(why) = locked {
            // SAFETY: `ptr` came from VirtualAllocExNuma and is released once.
            unsafe { free(ptr, len) };
            return Err(Error::LockFailed(why));
        }
    }
    Ok(Alloc {
        ptr,
        len,
        page_size: base_page_size(),
        pages: PageKind::Normal,
        locked: lock,
    })
}

/// Releases a mapping from `alloc`. Locked pages are unlocked by the
/// release.
///
/// # Safety
///
/// `ptr` must come from `alloc` and not be freed already, and nothing may
/// use the memory afterwards.
pub(crate) unsafe fn free(ptr: NonNull<u8>, _len: usize) {
    // SAFETY: per the caller's contract, `ptr` is the base of a live
    // VirtualAlloc mapping; MEM_RELEASE with size 0 frees all of it.
    unsafe { VirtualFree(ptr.as_ptr().cast(), 0, MEM_RELEASE) };
}
