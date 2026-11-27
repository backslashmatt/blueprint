//! When the app checks for updates (#179): at most once a day, remembered
//! in a file in the data directory.

use std::time::{Duration, SystemTime};

use blueprint_update::schedule::{CHECK_FILE, due, mark_checked};

#[test]
fn a_check_is_due_once_a_day() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    assert!(due(None, now), "never checked");
    assert!(!due(Some(now - Duration::from_secs(3600)), now));
    assert!(due(Some(now - Duration::from_secs(24 * 3600)), now));
    // A clock that went backwards does not stop checks for ever.
    assert!(due(Some(now + Duration::from_secs(10 * 24 * 3600)), now));
}

#[test]
fn the_last_check_is_remembered_in_the_data_directory() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("update-schedule-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(blueprint_update::schedule::last_checked(&dir), None);
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_123);
    mark_checked(&dir, at).expect("written");
    assert_eq!(blueprint_update::schedule::last_checked(&dir), Some(at));
    // A damaged file is as if never checked.
    std::fs::write(dir.join(CHECK_FILE), "not a time").expect("write");
    assert_eq!(blueprint_update::schedule::last_checked(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_release_key_is_compiled_in_only_when_committed() {
    let committed = concat!(env!("CARGO_MANIFEST_DIR"), "/release-key.txt");
    let key = blueprint_update::release_key();
    match std::fs::read_to_string(committed) {
        Ok(text) => assert_eq!(key, blueprint_update::parse_public_key(&text).ok()),
        Err(_) => assert_eq!(key, None, "no key, no update check"),
    }
}
