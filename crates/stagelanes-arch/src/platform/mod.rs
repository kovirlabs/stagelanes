//! All OS- and architecture-specific code in this crate.
//!
//! Each OS module provides `SOURCE`, `detect`, `pin_current_thread` and
//! `current_cpu`. The `sysfs` and `slpi` parsers are plain data parsing and
//! are compiled on every OS so their tests run everywhere.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod sysfs;

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod slpi;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{SOURCE, current_cpu, detect, pin_current_thread};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{SOURCE, current_cpu, detect, pin_current_thread};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{SOURCE, current_cpu, detect, pin_current_thread};

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod other;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) use other::{SOURCE, current_cpu, detect, pin_current_thread};

#[cfg(target_arch = "x86_64")]
#[path = "cpu_x86_64.rs"]
pub(crate) mod cpu;

#[cfg(target_arch = "aarch64")]
#[path = "cpu_aarch64.rs"]
pub(crate) mod cpu;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[path = "cpu_other.rs"]
pub(crate) mod cpu;
