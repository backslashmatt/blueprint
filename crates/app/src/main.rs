//! `blueprint`: the desktop app (REQUIREMENTS §5.3). Opens one window with
//! the shell view.
//!
//! `--cold-start-report <file>` writes the time from process start to the
//! first painted frame, in milliseconds, to `<file>` as JSON and quits: the
//! §4 cold-start budget as bp-bench measures it (#88).

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use blueprint_app::first_paint;
use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

static START: OnceLock<Instant> = OnceLock::new();

fn main() {
    let start = *START.get_or_init(Instant::now);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let report = match args.as_slice() {
        [] => None,
        [flag, path] if flag == "--cold-start-report" => Some(path.clone()),
        _ => {
            eprintln!("usage: blueprint [--cold-start-report <file>]");
            std::process::exit(2);
        }
    };
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            if let Some(path) = report.clone() {
                first_paint(window, cx, start, path);
            }
            cx.new(|_| blueprint_ui::Shell)
        });
        if let Err(e) = opened {
            eprintln!("blueprint: cannot open a window: {e}");
            std::process::exit(1);
        }
        cx.activate(true);
        if report.is_some() {
            // Never hang a benchmark.
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(60))
                    .await;
                eprintln!("blueprint: no frame was painted within 60 s");
                std::process::exit(3);
            })
            .detach();
        }
    });
}
