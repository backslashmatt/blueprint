//! The app's update check (#179, DR 0019): at most once a day, on a
//! background thread, from this repository's GitHub Releases. Nothing is
//! checked without the compiled-in release key, for a build that was not
//! installed from a package, in scripted runs, or with
//! `BLUEPRINT_NO_UPDATE_CHECK` set.

use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use blueprint_ui::workspace::Workspace;
use blueprint_update::{Asset, Check, schedule};
use gpui::{App, WeakEntity};

/// Turns the update check off when set to anything.
pub const OFF_ENV: &str = "BLUEPRINT_NO_UPDATE_CHECK";

/// The release page, for the banner until the app installs updates itself.
const RELEASES_PAGE: &str = "https://github.com/backslashmatt/blueprint/releases/latest";

/// Which installer updates this app, judged from where the running
/// executable is (`exe`) and, on Linux, which package's files are under
/// `root` (`/` in use). `None` for anything else, such as a build from source
/// run from `target/`, which is never updated.
#[must_use]
pub fn installed_platform(exe: &Path, root: &Path) -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        nsis_installed(exe).then_some("windows-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        in_app_bundle(exe).then_some("macos-aarch64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        linux_package(exe, root)
    } else {
        None
    }
}

/// Whether `exe` was installed by the NSIS installer, which writes
/// `uninstall.exe` beside it.
fn nsis_installed(exe: &Path) -> bool {
    exe.parent()
        .is_some_and(|d| d.join("uninstall.exe").is_file())
}

/// Whether `exe` is the main executable of an `.app` bundle
/// (`….app/Contents/MacOS/<exe>`), as the `.dmg` installs it.
fn in_app_bundle(exe: &Path) -> bool {
    let mut up = exe.ancestors().skip(1);
    let (Some(macos), Some(contents), Some(app)) = (up.next(), up.next(), up.next()) else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && app.extension().is_some_and(|e| e == "app")
}

/// The Linux package `exe` came from: it must be `/usr/bin/blueprint`, and
/// the package is told by its files (the `.rpm` puts its licences in
/// `/usr/share/licenses/blueprint`, the `.deb` its files in
/// `/usr/lib/blueprint`).
fn linux_package(exe: &Path, root: &Path) -> Option<&'static str> {
    if exe != root.join("usr/bin/blueprint") {
        return None;
    }
    if root.join("usr/share/licenses/blueprint").is_dir() {
        Some("linux-x86_64-rpm")
    } else if root.join("usr/lib/blueprint").is_dir() {
        Some("linux-x86_64-deb")
    } else {
        None
    }
}

/// The banner for an available update.
#[must_use]
pub fn banner(version: &str, current: &str) -> String {
    format!(
        "Blueprint {version} is available (this is {current}). Ctrl+U, or Update in the command palette, downloads and opens its installer."
    )
}

/// The installer's file name: the last segment of its URL, which
/// `blueprint_update::verify` only accepts as letters, digits, `.`, `_`, `-`
/// and `+`.
#[must_use]
pub fn file_name(asset: &Asset) -> Option<&str> {
    asset.url.rsplit('/').next().filter(|n| !n.is_empty())
}

/// How the platform opens a downloaded installer: the package manager's
/// front end on Linux, Finder on macOS, the NSIS installer itself on
/// Windows. Nothing is installed without the user going through it.
#[must_use]
pub fn installer_command(platform: &str, installer: &Path) -> (OsString, Vec<OsString>) {
    match platform {
        "windows-x86_64" => (installer.as_os_str().to_owned(), Vec::new()),
        "macos-aarch64" => ("open".into(), vec![installer.as_os_str().to_owned()]),
        _ => ("xdg-open".into(), vec![installer.as_os_str().to_owned()]),
    }
}

/// Downloads `asset`, checked against the signed manifest, into
/// `<data>/updates/`, and opens it with the platform. Blocking: run on its
/// own thread.
fn fetch_and_open(
    asset: &Asset,
    platform: &str,
    dir: &Path,
    current: &str,
) -> Result<PathBuf, String> {
    let name = file_name(asset).ok_or("the installer's URL has no file name")?;
    let https = blueprint_update::net::Https::new(current);
    let bytes = blueprint_update::download(&https, asset).map_err(|e| e.to_string())?;
    let updates = dir.join("updates");
    std::fs::create_dir_all(&updates).map_err(|e| format!("{}: {e}", updates.display()))?;
    let path = updates.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let (program, args) = installer_command(platform, &path);
    std::process::Command::new(&program)
        .args(&args)
        .spawn()
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    Ok(path)
}

/// The update action: downloads the installer on its own thread and opens
/// it, saying how it went in the banner. Pressed again while it runs, it
/// only says so.
fn install_hook(
    version: String,
    asset: Asset,
    platform: &'static str,
    dir: PathBuf,
    current: &'static str,
) -> blueprint_ui::workspace::UpdateHook {
    let busy = Rc::new(Cell::new(false));
    Rc::new(move |w: &mut Workspace, cx| {
        if busy.get() {
            w.notice = Some(format!("Blueprint {version} is already downloading."));
            return;
        }
        busy.set(true);
        w.banner = Some(format!("Downloading Blueprint {version}…"));
        cx.notify();
        let (tx, rx) = futures::channel::oneshot::channel();
        let (asset, dir) = (asset.clone(), dir.clone());
        let spawned = std::thread::Builder::new()
            .name("update-download".into())
            .spawn(move || {
                let _ = tx.send(fetch_and_open(&asset, platform, &dir, current));
            });
        if let Err(e) = spawned {
            busy.set(false);
            w.banner = Some(format!("Updating failed: {e}. Get it from {RELEASES_PAGE}"));
            return;
        }
        let (busy, version) = (Rc::clone(&busy), version.clone());
        cx.spawn(async move |ws, cx| {
            let done = rx.await.unwrap_or_else(|_| Err("the download stopped".into()));
            let _ = ws.update(cx, |w, cx| {
                busy.set(false);
                w.banner = Some(match done {
                    Ok(path) => format!(
                        "The installer for Blueprint {version} is open ({}). Quit Blueprint to finish installing.",
                        path.display()
                    ),
                    Err(e) => format!("Updating failed: {e}. Get it from {RELEASES_PAGE}"),
                });
                cx.notify();
            });
        })
        .detach();
    })
}

/// Starts the check if one is due, and shows the banner if it finds an
/// update. Failures are quiet: the check is tried again at the next start.
pub fn start(ws: WeakEntity<Workspace>, cx: &mut App) {
    if std::env::var_os(OFF_ENV).is_some() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let (Some(key), Some(platform)) = (
        blueprint_update::release_key(),
        installed_platform(&exe, Path::new("/")),
    ) else {
        return;
    };
    let Ok(dir) = blueprint_engine::reviews::data_root() else {
        return;
    };
    if !schedule::due(schedule::last_checked(&dir), SystemTime::now()) {
        return;
    }
    let current = env!("CARGO_PKG_VERSION");
    let dir_for_install = dir.clone();
    // Its own thread: the requests block, for minutes on a bad network, and
    // GPUI's few background threads are needed for the map.
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let https = blueprint_update::net::Https::new(current);
            let found = blueprint_update::check(&https, current, platform, &key);
            // A found update is not marked, so the banner returns at every start
            // until it is installed; a failure is retried at the next start.
            if matches!(found, Ok(Check::UpToDate | Check::NoRelease)) {
                let _ = schedule::mark_checked(&dir, SystemTime::now());
            }
            let _ = tx.send(found);
        });
    if spawned.is_err() {
        return;
    }
    cx.spawn(async move |cx| match rx.await {
        Ok(Ok(Check::Available { manifest, asset })) => {
            let _ = ws.update(cx, |w, cx| {
                w.banner = Some(banner(&manifest.version, current));
                w.on_update = Some(install_hook(
                    manifest.version.clone(),
                    asset,
                    platform,
                    dir_for_install,
                    current,
                ));
                cx.notify();
            });
        }
        Ok(Ok(_)) | Err(_) => {}
        Ok(Err(e)) => eprintln!("blueprint: update check: {e}"),
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_installed_app_is_updated() {
        let root = std::env::temp_dir().join(format!("updater-platform-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("usr/bin")).expect("root");
        let usr_bin = root.join("usr/bin/blueprint");
        // Linux: /usr/bin/blueprint from a package, never a build from source.
        assert_eq!(linux_package(&usr_bin, &root), None, "no package files");
        std::fs::create_dir_all(root.join("usr/lib/blueprint")).expect("deb");
        assert_eq!(linux_package(&usr_bin, &root), Some("linux-x86_64-deb"));
        assert_eq!(
            linux_package(&root.join("src/blueprint/target/release/blueprint"), &root),
            None,
            "a source build beside an old install"
        );
        std::fs::create_dir_all(root.join("usr/share/licenses/blueprint")).expect("rpm");
        assert_eq!(linux_package(&usr_bin, &root), Some("linux-x86_64-rpm"));
        // macOS: inside an .app bundle only.
        assert!(in_app_bundle(Path::new(
            "/Applications/Blueprint.app/Contents/MacOS/blueprint"
        )));
        assert!(!in_app_bundle(Path::new(
            "/Users/me/blueprint/target/release/blueprint"
        )));
        assert!(!in_app_bundle(Path::new("blueprint")));
        // Windows: beside the NSIS uninstaller only.
        let nsis = root.join("Blueprint");
        std::fs::create_dir_all(&nsis).expect("nsis");
        assert!(!nsis_installed(&nsis.join("blueprint.exe")));
        std::fs::write(nsis.join("uninstall.exe"), b"").expect("uninstaller");
        assert!(nsis_installed(&nsis.join("blueprint.exe")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn each_platform_opens_its_installer_its_own_way() {
        let p = Path::new("/data/updates/x");
        let (prog, args) = installer_command("linux-x86_64-deb", p);
        assert_eq!((prog.to_str(), args.len()), (Some("xdg-open"), 1));
        let (prog, args) = installer_command("linux-x86_64-rpm", p);
        assert_eq!((prog.to_str(), args.len()), (Some("xdg-open"), 1));
        let (prog, args) = installer_command("macos-aarch64", p);
        assert_eq!(
            (prog.to_str(), args[0].to_str()),
            (Some("open"), Some("/data/updates/x"))
        );
        let (prog, args) = installer_command("windows-x86_64", p);
        assert_eq!((prog.to_str(), args.len()), (Some("/data/updates/x"), 0));
    }

    #[test]
    fn the_installer_keeps_its_released_file_name() {
        let asset = |url: &str| Asset {
            url: url.into(),
            sha256: String::new(),
            size: 0,
        };
        assert_eq!(
            file_name(&asset(
                "https://github.com/backslashmatt/blueprint/releases/download/v0.2.0/blueprint_0.2.0_amd64.deb"
            )),
            Some("blueprint_0.2.0_amd64.deb")
        );
        assert_eq!(file_name(&asset("https://example.com/")), None);
    }

    #[test]
    fn the_banner_names_both_versions_and_where_to_get_it() {
        let b = banner("0.2.0", "0.1.0");
        assert!(b.contains("0.2.0") && b.contains("0.1.0") && b.contains("Ctrl+U"));
    }
}
