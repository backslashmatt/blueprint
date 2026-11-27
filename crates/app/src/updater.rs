//! The app's update check (#179, DR 0019): at most once a day, on a
//! background thread, from this repository's GitHub Releases. Nothing is
//! checked without the compiled-in release key, for a build that was not
//! installed from a package, in scripted runs, or with
//! `BLUEPRINT_NO_UPDATE_CHECK` set.

use std::path::Path;
use std::time::SystemTime;

use blueprint_ui::workspace::Workspace;
use blueprint_update::{Check, schedule};
use gpui::{App, WeakEntity};

/// Turns the update check off when set to anything.
pub const OFF_ENV: &str = "BLUEPRINT_NO_UPDATE_CHECK";

/// The release page, for the banner until the app installs updates itself.
const RELEASES_PAGE: &str = "https://github.com/backslashmatt/blueprint/releases/latest";

/// Which installer updates this app, from what is installed under `root`
/// (`/` in use): the `.rpm` puts its licences in `/usr/share/licenses`, the
/// `.deb` its files in `/usr/lib/blueprint`. `None` for a build from source,
/// which is not updated.
#[must_use]
pub fn platform_at(root: &Path) -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-aarch64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        if root.join("usr/share/licenses/blueprint").is_dir() {
            Some("linux-x86_64-rpm")
        } else if root.join("usr/lib/blueprint").is_dir() {
            Some("linux-x86_64-deb")
        } else {
            None
        }
    } else {
        None
    }
}

/// The banner for an available update.
#[must_use]
pub fn banner(version: &str, current: &str) -> String {
    format!("Blueprint {version} is available (this is {current}): {RELEASES_PAGE}")
}

/// Starts the check if one is due, and shows the banner if it finds an
/// update. Failures are quiet: the check is tried again at the next start.
pub fn start(ws: WeakEntity<Workspace>, cx: &mut App) {
    if std::env::var_os(OFF_ENV).is_some() {
        return;
    }
    let (Some(key), Some(platform)) =
        (blueprint_update::release_key(), platform_at(Path::new("/")))
    else {
        return;
    };
    let Ok(dir) = blueprint_engine::reviews::data_root() else {
        return;
    };
    if !schedule::due(schedule::last_checked(&dir), SystemTime::now()) {
        return;
    }
    let current = env!("CARGO_PKG_VERSION");
    let task = cx.background_executor().spawn(async move {
        let https = blueprint_update::net::Https::new(current);
        let found = blueprint_update::check(&https, current, platform, &key);
        if found.is_ok() {
            let _ = schedule::mark_checked(&dir, SystemTime::now());
        }
        found
    });
    cx.spawn(async move |cx| match task.await {
        Ok(Check::Available { manifest, .. }) => {
            let _ = ws.update(cx, |w, cx| {
                w.banner = Some(banner(&manifest.version, current));
                cx.notify();
            });
        }
        Ok(_) => {}
        Err(e) => eprintln!("blueprint: update check: {e}"),
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_package_decides_the_platform() {
        let root = std::env::temp_dir().join(format!("updater-platform-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(platform_at(&root), None, "built from source");
            std::fs::create_dir_all(root.join("usr/lib/blueprint")).expect("deb");
            assert_eq!(platform_at(&root), Some("linux-x86_64-deb"));
            std::fs::create_dir_all(root.join("usr/share/licenses/blueprint")).expect("rpm");
            assert_eq!(platform_at(&root), Some("linux-x86_64-rpm"));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_banner_names_both_versions_and_where_to_get_it() {
        let b = banner("0.2.0", "0.1.0");
        assert!(b.contains("0.2.0") && b.contains("0.1.0") && b.contains("releases/latest"));
    }
}
