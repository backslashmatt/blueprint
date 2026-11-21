//! The platform's text-size setting (#118, requirement §5.5).
//!
//! GPUI does not report it at the pinned revision, so it is read here:
//! GNOME's `text-scaling-factor` on Linux and the "Make text bigger"
//! setting (`TextScaleFactor`) on Windows. macOS has no system-wide text
//! size for desktop apps, so there the scale is 1. The workspace sets the
//! window's rem size from it, and every text size and row height is given
//! in rems.
//!
//! On X11, GNOME already folds the factor into `Xft.dpi`, which GPUI turns
//! into the window's scale factor, so it is not applied a second time
//! there. Outside GNOME the key is left over from another desktop and
//! nothing else follows it, so it is not read.

/// Overrides the platform setting, e.g. `BLUEPRINT_TEXT_SCALE=1.5`.
pub const ENV: &str = "BLUEPRINT_TEXT_SCALE";

/// GPUI's rem at a scale of 1, in pixels.
pub const BASE_REM: f32 = 16.0;

const MIN: f32 = 0.5;
const MAX: f32 = 3.0;

/// `s` kept within the sizes the layout is checked at; anything that is not
/// a number is 1.
pub fn clamp(s: f32) -> f32 {
    if s.is_finite() && s > 0.0 {
        s.clamp(MIN, MAX)
    } else {
        1.0
    }
}

/// The scale in `gsettings get org.gnome.desktop.interface
/// text-scaling-factor` output, such as `1.25`.
pub fn parse_gsettings(out: &str) -> Option<f32> {
    out.trim().parse().ok()
}

/// The scale in `reg query HKCU\Software\Microsoft\Accessibility /v
/// TextScaleFactor` output: a percentage between 100 and 225, as a hex
/// `REG_DWORD`.
pub fn parse_reg(out: &str) -> Option<f32> {
    let line = out.lines().find(|l| l.contains("TextScaleFactor"))?;
    let mut words = line.split_whitespace().skip_while(|w| *w != "REG_DWORD");
    let value = words.nth(1)?;
    let percent = u32::from_str_radix(value.strip_prefix("0x")?, 16).ok()?;
    Some(percent as f32 / 100.0)
}

static SCALE: std::sync::LazyLock<f32> = std::sync::LazyLock::new(system);

/// Starts reading the scale on another thread, so that [`get`] does not
/// wait for it. Call it first thing in `main`.
pub fn prefetch() {
    std::thread::spawn(|| *SCALE);
}

/// The text scale, read once per process: waits for [`prefetch`] if it
/// has not finished, or reads it now if it was never started.
pub fn get() -> f32 {
    *SCALE
}

/// Whether GNOME's factor should be applied by the app: on GNOME, and not
/// on X11, where it is already in the window's scale factor. GPUI uses
/// Wayland whenever `WAYLAND_DISPLAY` is set.
pub fn gnome_applies(desktop: Option<&str>, wayland_display: Option<&str>) -> bool {
    let gnome =
        desktop.is_some_and(|d| d.split(':').any(|part| part.eq_ignore_ascii_case("gnome")));
    gnome && wayland_display.is_some_and(|w| !w.is_empty())
}

/// The text scale: the `BLUEPRINT_TEXT_SCALE` override, else the platform's
/// setting, else 1. May run a short command; [`get`] caches it.
pub fn system() -> f32 {
    if let Some(s) = std::env::var(ENV).ok().and_then(|v| v.trim().parse().ok()) {
        return clamp(s);
    }
    clamp(platform().unwrap_or(1.0))
}

#[cfg(target_os = "linux")]
fn platform() -> Option<f32> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    if !gnome_applies(desktop.as_deref(), wayland.as_deref()) {
        return None;
    }
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "text-scaling-factor"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_gsettings(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

#[cfg(windows)]
fn platform() -> Option<f32> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: no console flashes up behind the app.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Accessibility",
            "/v",
            "TextScaleFactor",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    // The value is absent until the setting is first changed: that is 100%.
    out.status
        .success()
        .then(|| parse_reg(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform() -> Option<f32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_output_is_read() {
        assert_eq!(parse_gsettings("1.25\n"), Some(1.25));
        assert_eq!(parse_gsettings("1.0"), Some(1.0));
        assert_eq!(parse_gsettings("No such schema"), None);
    }

    #[test]
    fn windows_output_is_read() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Accessibility\r\n    \
                   TextScaleFactor    REG_DWORD    0x7d\r\n\r\n";
        assert_eq!(parse_reg(out), Some(1.25));
        assert_eq!(
            parse_reg("    TextScaleFactor    REG_DWORD    0xe1"),
            Some(2.25)
        );
        assert_eq!(parse_reg("ERROR: The system was unable to find"), None);
    }

    #[test]
    fn gnome_factor_applies_only_on_gnome_wayland() {
        assert!(gnome_applies(Some("GNOME"), Some("wayland-0")));
        assert!(gnome_applies(Some("ubuntu:GNOME"), Some("wayland-0")));
        // X11: already in Xft.dpi and so in the window's scale factor.
        assert!(!gnome_applies(Some("GNOME"), None));
        assert!(!gnome_applies(Some("GNOME"), Some("")));
        // Other desktops do not follow the key.
        assert!(!gnome_applies(Some("KDE"), Some("wayland-0")));
        assert!(!gnome_applies(None, Some("wayland-0")));
    }

    #[test]
    fn odd_scales_are_kept_in_range() {
        assert_eq!(clamp(1.5), 1.5);
        assert_eq!(clamp(10.0), 3.0);
        assert_eq!(clamp(0.1), 0.5);
        assert_eq!(clamp(f32::NAN), 1.0);
        assert_eq!(clamp(0.0), 1.0);
        assert_eq!(clamp(-2.0), 1.0);
    }
}
