//! When the app checks for updates (#179, DR 0019): at most once a day. The
//! time of the last check is a file in the app's data directory.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// The file, in the data directory, holding the last check's time in
/// seconds since the Unix epoch.
pub const CHECK_FILE: &str = "update-check";

/// How often the app checks.
pub const INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Whether a check is due at `now`, given the last one. A last check in the
/// future (a clock set back) counts as due, so checks never stop for good.
#[must_use]
pub fn due(last: Option<SystemTime>, now: SystemTime) -> bool {
    match last {
        None => true,
        Some(last) => match now.duration_since(last) {
            Ok(since) => since >= INTERVAL,
            Err(_) => true,
        },
    }
}

/// The last check's time, from `dir`; `None` if there is none or the file
/// cannot be read.
#[must_use]
pub fn last_checked(dir: &Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(dir.join(CHECK_FILE)).ok()?;
    let secs: u64 = text.trim().parse().ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// Records a check at `at` in `dir`, creating it if needed.
///
/// # Errors
/// When the file cannot be written.
pub fn mark_checked(dir: &Path, at: SystemTime) -> std::io::Result<()> {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(CHECK_FILE), format!("{secs}\n"))
}
