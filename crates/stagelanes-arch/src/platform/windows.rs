//! Windows: `GetLogicalProcessorInformationEx`, `CallNtPowerInformation`,
//! `SetThreadGroupAffinity`, `GetCurrentProcessorNumberEx`.

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Kernel::PROCESSOR_NUMBER;
use windows_sys::Win32::System::Power::{CallNtPowerInformation, ProcessorInformation};
use windows_sys::Win32::System::SystemInformation::{
    GROUP_AFFINITY, GetLogicalProcessorInformationEx, RelationAll,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessorNumberEx, GetCurrentThread, SetThreadGroupAffinity,
};

use super::slpi;
use crate::assemble::RawTopology;
use crate::{Error, OsCpu};

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
