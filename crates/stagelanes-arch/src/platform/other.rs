//! Any other OS: nothing is supported.

use crate::assemble::RawTopology;
use crate::{Error, OsCpu, Priority};

pub(crate) const SOURCE: &str = "none";

pub(crate) fn detect() -> Result<RawTopology, Error> {
    Err(Error::Unsupported("topology detection"))
}

pub(crate) fn pin_current_thread(_cpu: OsCpu) -> Result<(), Error> {
    Err(Error::Unsupported("thread pinning"))
}

pub(crate) fn current_cpu() -> Option<OsCpu> {
    None
}

pub(crate) fn set_current_thread_priority(priority: Priority) -> Result<(), Error> {
    match priority {
        Priority::Normal => Ok(()),
        _ => Err(Error::Unsupported("raising thread priority")),
    }
}
