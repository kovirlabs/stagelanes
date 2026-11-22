//! What every run file records about where and how it was produced.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Today's date in UTC as `YYYY-MM-DD`.
pub fn utc_date() -> String {
    let (y, m, d) = civil_from_days((now_secs() / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// UTC time of day as `HH:MM:SSZ`.
pub fn utc_time() -> String {
    let s = now_secs() % 86_400;
    format!("{:02}:{:02}:{:02}Z", s / 3600, (s / 60) % 60, s % 60)
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    (!s.is_empty() && out.status.success()).then(|| s.to_owned())
}

/// The OS build string: `ver` on Windows, `uname -srv` elsewhere.
pub fn os_build() -> String {
    run("cmd", &["/C", "ver"])
        .or_else(|| run("uname", &["-srv"]))
        .unwrap_or_else(|| "unknown".into())
}

/// The commit the benchmark was built from, and whether the tree had
/// uncommitted changes.
pub fn git() -> Value {
    let commit = run("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let branch = run("git", &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let dirty = run("git", &["status", "--porcelain", "--untracked-files=no"]).is_some();
    json!({ "stagelanes": commit, "branch": branch, "uncommitted_changes": dirty })
}

/// The exact command line.
pub fn command() -> String {
    std::env::args().collect::<Vec<_>>().join(" ")
}

/// `rustc -V` of the compiler that built this binary.
pub fn rustc() -> &'static str {
    env!("BENCH_RUSTC")
}
