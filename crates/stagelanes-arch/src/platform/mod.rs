//! All OS- and architecture-specific code in this crate.
//!
//! Each OS module provides `SOURCE`, `detect`, `pin_current_thread`,
//! `current_cpu`, `set_current_thread_priority` and `DurableWriter`. The `sysfs` and `slpi` parsers are plain data parsing and
//! are compiled on every OS so their tests run everywhere.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod sysfs;

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod slpi;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{
    DurableWriter, SOURCE, current_cpu, detect, pin_current_thread, set_current_thread_priority,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{
    DurableWriter, SOURCE, current_cpu, detect, pin_current_thread, set_current_thread_priority,
};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{
    DurableWriter, SOURCE, current_cpu, detect, pin_current_thread, set_current_thread_priority,
};

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod other;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) use other::{
    DurableWriter, SOURCE, current_cpu, detect, pin_current_thread, set_current_thread_priority,
};

#[cfg(target_arch = "x86_64")]
#[path = "cpu_x86_64.rs"]
pub(crate) mod cpu;

#[cfg(target_arch = "aarch64")]
#[path = "cpu_aarch64.rs"]
pub(crate) mod cpu;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[path = "cpu_other.rs"]
pub(crate) mod cpu;
