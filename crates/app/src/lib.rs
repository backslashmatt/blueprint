//! The platform shell (REQUIREMENTS §5.3): windows, menus and packaging.

use std::time::Instant;

use gpui::{App, Window};

/// After the window's first frame is painted, writes the time since `start`
/// to `path` as `{"cold_start_ms": …}` and quits.
pub fn first_paint(window: &mut Window, cx: &mut App, start: Instant, path: String) {
    window.on_next_frame(move |_window, cx| {
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let json = format!("{{\"cold_start_ms\": {ms:.2}}}\n");
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("blueprint: cannot write {path}: {e}");
            std::process::exit(1);
        }
        cx.quit();
    });
    let _ = cx;
}
