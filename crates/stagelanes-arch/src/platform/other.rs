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

pub(crate) struct DurableWriter;

impl DurableWriter {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(DurableWriter)
    }

    pub(crate) fn method(&self) -> &'static str {
        "write+sync"
    }

    /// Seek, write, then `sync_data`, through the portable std API.
    pub(crate) fn write(
        &mut self,
        file: &std::fs::File,
        offset: u64,
        chunks: &[&[u8]],
    ) -> Result<(), Error> {
        use std::io::{Seek, SeekFrom, Write};
        let io = |e: std::io::Error| Error::Os {
            call: "write/sync",
            code: i64::from(e.raw_os_error().unwrap_or(0)),
        };
        let mut f = file;
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        for chunk in chunks {
            f.write_all(chunk).map_err(io)?;
        }
        file.sync_data().map_err(io)
    }
}
