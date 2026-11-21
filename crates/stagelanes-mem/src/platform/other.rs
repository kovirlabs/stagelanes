//! Any other OS: nothing is supported.

use std::ptr::NonNull;

use super::Alloc;
use crate::{Error, LargePages};

pub(crate) fn base_page_size() -> usize {
    4096
}

pub(crate) fn alloc(
    _len: usize,
    _node: Option<u32>,
    _large: LargePages,
    _lock: bool,
) -> Result<Alloc, Error> {
    Err(Error::Unsupported("pinned memory"))
}

/// Never called: `alloc` never succeeds here.
///
/// # Safety
///
/// No requirements.
pub(crate) unsafe fn free(_ptr: NonNull<u8>, _len: usize) {}
