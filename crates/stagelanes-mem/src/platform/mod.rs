//! All OS-specific code in this crate. Each OS module provides
//! `base_page_size`, `alloc` and `free`.

use std::ptr::NonNull;

use crate::PageKind;

/// A mapping returned by `alloc`.
pub(crate) struct Alloc {
    pub ptr: NonNull<u8>,
    /// Bytes mapped; at least the request, rounded to `page_size`.
    pub len: usize,
    pub page_size: usize,
    pub pages: PageKind,
    pub locked: bool,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{alloc, base_page_size, free};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{alloc, base_page_size, free};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{alloc, base_page_size, free};

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod other;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) use other::{alloc, base_page_size, free};
