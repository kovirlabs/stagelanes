//! Windows: `GetLogicalProcessorInformationEx`, `CallNtPowerInformation`,
//! `SetThreadGroupAffinity`, `GetCurrentProcessorNumberEx`.

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Kernel::PROCESSOR_NUMBER;
use windows_sys::Win32::System::Power::{CallNtPowerInformation, ProcessorInformation};
use windows_sys::Win32::System::SystemInformation::{
    GROUP_AFFINITY, GetLogicalProcessorInformationEx, RelationAll,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessorNumberEx, GetCurrentThread, SetThreadGroupAffinity, SetThreadPriority,
    THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_NORMAL, THREAD_PRIORITY_TIME_CRITICAL,
};

use std::os::windows::fs::FileExt;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Storage::FileSystem::{
    BuildIoRingFlushFile, BuildIoRingWriteFile, CloseIoRing, CreateIoRing, FILE_FLUSH_DEFAULT,
    FILE_WRITE_FLAGS_NONE, HIORING, IORING_BUFFER_REF, IORING_BUFFER_REF_0, IORING_CQE,
    IORING_CREATE_ADVISORY_FLAGS_NONE, IORING_CREATE_FLAGS, IORING_CREATE_REQUIRED_FLAGS_NONE,
    IORING_HANDLE_REF, IORING_HANDLE_REF_0, IORING_REF_RAW, IORING_VERSION_3,
    IOSQE_FLAGS_DRAIN_PRECEDING_OPS, IOSQE_FLAGS_NONE, PopIoRingCompletion, SubmitIoRing,
};

use super::slpi;
use crate::assemble::RawTopology;
use crate::{Error, OsCpu, Priority};

pub(crate) const SOURCE: &str =
    "Windows GetLogicalProcessorInformationEx; frequency from CallNtPowerInformation";

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

fn last_error(call: &'static str) -> Error {
    // SAFETY: reads the calling thread's last-error value; no preconditions.
    let code = unsafe { GetLastError() };
    Error::Os {
        call,
        code: i64::from(code),
    }
}

fn processor_information() -> Result<Vec<u8>, Error> {
    let mut len: u32 = 0;
    // SAFETY: a null buffer with length 0 asks for the required size, which
    // the call writes to `len`.
    let ok =
        unsafe { GetLogicalProcessorInformationEx(RelationAll, core::ptr::null_mut(), &mut len) };
    if ok != 0 {
        return Ok(Vec::new());
    }
    // SAFETY: as above.
    let err = unsafe { GetLastError() };
    if err != ERROR_INSUFFICIENT_BUFFER {
        return Err(Error::Os {
            call: "GetLogicalProcessorInformationEx",
            code: i64::from(err),
        });
    }
    // u64 elements give the 8-byte alignment the records need.
    let mut buf: Vec<u64> = vec![0; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is writable for at least `len` bytes and 8-byte aligned;
    // the call writes at most `len` bytes and updates `len`.
    let ok =
        unsafe { GetLogicalProcessorInformationEx(RelationAll, buf.as_mut_ptr().cast(), &mut len) };
    if ok == 0 {
        return Err(last_error("GetLogicalProcessorInformationEx"));
    }
    Ok(buf
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .take(len as usize)
        .collect())
}

/// `PROCESSOR_POWER_INFORMATION`: Number, MaxMhz, CurrentMhz, MhzLimit,
/// MaxIdleState, CurrentIdleState, each a `u32`.
const POWER_INFO_WORDS: usize = 6;

/// Maximum MHz per processor, indexed by the processor's position among all
/// processors in (group, number) order.
fn max_mhz(count: usize) -> Vec<Option<u32>> {
    let mut words: Vec<u32> = vec![0; count * POWER_INFO_WORDS];
    let bytes = (words.len() * 4) as u32;
    // SAFETY: `words` is writable for `bytes` bytes, room for `count`
    // records; no input buffer.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            core::ptr::null(),
            0,
            words.as_mut_ptr().cast(),
            bytes,
        )
    };
    if status != 0 {
        return vec![None; count];
    }
    words
        .chunks_exact(POWER_INFO_WORDS)
        .map(|r| (r[1] > 0).then_some(r[1]))
        .collect()
}

pub(crate) fn detect() -> Result<RawTopology, Error> {
    let buf = processor_information()?;
    let records = slpi::records(&buf)?;
    let mut all: Vec<OsCpu> = records
        .iter()
        .filter_map(|r| match r {
            slpi::Record::Core { cpus, .. } => Some(cpus.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    all.sort_unstable();
    let mhz = max_mhz(all.len());
    slpi::to_raw(&records, |cpu| {
        all.binary_search(&cpu)
            .ok()
            .and_then(|i| mhz.get(i).copied().flatten())
    })
}

pub(crate) fn pin_current_thread(cpu: OsCpu) -> Result<(), Error> {
    if cpu.number >= usize::BITS {
        return Err(Error::Parse(format!(
            "CPU number {} out of range",
            cpu.number
        )));
    }
    let affinity = GROUP_AFFINITY {
        Mask: 1usize << cpu.number,
        Group: cpu.group,
        Reserved: [0; 3],
    };
    // SAFETY: `GetCurrentThread` returns a pseudo-handle that needs no
    // closing; `affinity` is a valid GROUP_AFFINITY; the previous-affinity
    // pointer may be null.
    let ok =
        unsafe { SetThreadGroupAffinity(GetCurrentThread(), &affinity, core::ptr::null_mut()) };
    if ok == 0 {
        return Err(last_error("SetThreadGroupAffinity"));
    }
    Ok(())
}

pub(crate) fn set_current_thread_priority(priority: Priority) -> Result<(), Error> {
    let level = match priority {
        Priority::Normal => THREAD_PRIORITY_NORMAL,
        Priority::High => THREAD_PRIORITY_HIGHEST,
        Priority::TimeCritical => THREAD_PRIORITY_TIME_CRITICAL,
    };
    // SAFETY: `GetCurrentThread` returns a pseudo-handle that needs no
    // closing; `level` is one of the documented THREAD_PRIORITY values.
    let ok = unsafe { SetThreadPriority(GetCurrentThread(), level) };
    if ok == 0 {
        return Err(last_error("SetThreadPriority"));
    }
    Ok(())
}

pub(crate) fn current_cpu() -> Option<OsCpu> {
    let mut n = PROCESSOR_NUMBER {
        Group: 0,
        Number: 0,
        Reserved: 0,
    };
    // SAFETY: writes the current processor into `n`; cannot fail.
    unsafe { GetCurrentProcessorNumberEx(&mut n) };
    Some(OsCpu {
        group: n.Group,
        number: u32::from(n.Number),
    })
}

/// Most operations one IoRing submission holds (writes plus the flush).
const RING_ENTRIES: u32 = 256;

pub(crate) struct DurableWriter {
    /// The IoRing, if the OS offers one with file write and flush.
    ring: Option<HIORING>,
}

// SAFETY: an IoRing handle is not tied to the thread that created it, and
// `write` takes `&mut self`, so only one thread uses the ring at a time.
unsafe impl Send for DurableWriter {}

impl DurableWriter {
    pub(crate) fn new() -> Result<Self, Error> {
        let mut ring: HIORING = core::ptr::null_mut();
        let flags = IORING_CREATE_FLAGS {
            Required: IORING_CREATE_REQUIRED_FLAGS_NONE,
            Advisory: IORING_CREATE_ADVISORY_FLAGS_NONE,
        };
        // SAFETY: `ring` is a valid out-pointer; the sizes are within the
        // documented limits. Version 3 is the first with write and flush.
        let hr = unsafe {
            CreateIoRing(
                IORING_VERSION_3,
                flags,
                RING_ENTRIES,
                RING_ENTRIES * 2,
                &mut ring,
            )
        };
        Ok(DurableWriter {
            ring: (hr >= 0 && !ring.is_null()).then_some(ring),
        })
    }

    pub(crate) fn method(&self) -> &'static str {
        if self.ring.is_some() {
            "ioring"
        } else {
            "write+sync"
        }
    }

    pub(crate) fn write(
        &mut self,
        file: &std::fs::File,
        offset: u64,
        chunks: &[&[u8]],
    ) -> Result<(), Error> {
        let Some(ring) = self.ring else {
            return write_then_flush(file, offset, chunks);
        };
        let handle = IORING_HANDLE_REF {
            Kind: IORING_REF_RAW,
            Handle: IORING_HANDLE_REF_0 {
                Handle: file.as_raw_handle(),
            },
        };
        let groups: Vec<&[&[u8]]> = chunks.chunks(RING_ENTRIES as usize - 1).collect();
        let mut at = offset;
        for (g, group) in groups.iter().enumerate() {
            let last = g + 1 == groups.len();
            for (i, chunk) in group.iter().enumerate() {
                let len = u32::try_from(chunk.len())
                    .map_err(|_| Error::Parse("chunk over 4 GiB".into()))?;
                let buffer = IORING_BUFFER_REF {
                    Kind: IORING_REF_RAW,
                    Buffer: IORING_BUFFER_REF_0 {
                        Address: chunk.as_ptr() as *mut core::ffi::c_void,
                    },
                };
                // SAFETY: `ring` is open; `handle` refers to an open file and
                // `buffer` to `len` readable bytes, both of which outlive the
                // operation because `submit` below waits for it to complete.
                let hr = unsafe {
                    BuildIoRingWriteFile(
                        ring,
                        handle,
                        buffer,
                        len,
                        at,
                        FILE_WRITE_FLAGS_NONE,
                        i,
                        IOSQE_FLAGS_NONE,
                    )
                };
                if hr < 0 {
                    return Err(Error::Os {
                        call: "BuildIoRingWriteFile",
                        code: i64::from(hr),
                    });
                }
                at += u64::from(len);
            }
            let mut ops = group.len() as u32;
            if last {
                // SAFETY: as above; the flush waits for the writes before it
                // (DRAIN_PRECEDING_OPS).
                let hr = unsafe {
                    BuildIoRingFlushFile(
                        ring,
                        handle,
                        FILE_FLUSH_DEFAULT,
                        usize::MAX,
                        IOSQE_FLAGS_DRAIN_PRECEDING_OPS,
                    )
                };
                if hr < 0 {
                    return Err(Error::Os {
                        call: "BuildIoRingFlushFile",
                        code: i64::from(hr),
                    });
                }
                ops += 1;
            }
            submit(ring, ops, group)?;
        }
        Ok(())
    }
}

/// Submits `ops` queued operations, waits for all of them, and checks every
/// completion (a write must have written its whole chunk).
fn submit(ring: HIORING, ops: u32, group: &[&[u8]]) -> Result<(), Error> {
    let mut submitted = 0u32;
    // SAFETY: `ring` is open; `submitted` is a valid out-pointer. Waiting
    // for `ops` completions with no time limit.
    let hr = unsafe { SubmitIoRing(ring, ops, u32::MAX, &mut submitted) };
    if hr < 0 {
        return Err(Error::Os {
            call: "SubmitIoRing",
            code: i64::from(hr),
        });
    }
    for _ in 0..ops {
        let mut cqe = IORING_CQE {
            UserData: 0,
            ResultCode: 0,
            Information: 0,
        };
        // SAFETY: `ring` is open; `cqe` is a valid out-pointer.
        let hr = unsafe { PopIoRingCompletion(ring, &mut cqe) };
        if hr != 0 {
            return Err(Error::Os {
                call: "PopIoRingCompletion",
                code: i64::from(hr),
            });
        }
        if cqe.ResultCode < 0 {
            return Err(Error::Os {
                call: "IoRing operation",
                code: i64::from(cqe.ResultCode),
            });
        }
        if let Some(chunk) = group.get(cqe.UserData) {
            if cqe.Information != chunk.len() {
                return Err(Error::Parse(format!(
                    "IoRing wrote {} of {} bytes",
                    cqe.Information,
                    chunk.len()
                )));
            }
        }
    }
    Ok(())
}

/// Positioned writes, then `FlushFileBuffers` (through `sync_data`).
fn write_then_flush(file: &std::fs::File, offset: u64, chunks: &[&[u8]]) -> Result<(), Error> {
    let io = |e: std::io::Error| Error::Os {
        call: "WriteFile/FlushFileBuffers",
        code: i64::from(e.raw_os_error().unwrap_or(0)),
    };
    let mut at = offset;
    for chunk in chunks {
        let mut done = 0;
        while done < chunk.len() {
            let n = file
                .seek_write(&chunk[done..], at + done as u64)
                .map_err(io)?;
            done += n;
        }
        at += chunk.len() as u64;
    }
    file.sync_data().map_err(io)
}

impl Drop for DurableWriter {
    fn drop(&mut self) {
        if let Some(ring) = self.ring.take() {
            // SAFETY: `ring` came from CreateIoRing and is closed once.
            unsafe { CloseIoRing(ring) };
        }
    }
}
