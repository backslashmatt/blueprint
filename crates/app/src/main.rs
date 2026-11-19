//! `blueprint`: the desktop app (REQUIREMENTS §5.3). Opens one window: the
//! review list of the repository given on the command line, or the empty
//! shell without one.
//!
//! ```text
//! blueprint [<repo>] [--cold-start-report <file>] [--script <keys>] [--review-report <file> [--stay-open]]
//! ```
//!
//! - `--cold-start-report <file>` writes the time from process start to the
//!   first painted frame, in milliseconds, to `<file>` as JSON and quits:
//!   the §4 cold-start budget as bp-bench measures it (#88).
//! - `--script <keys>` presses the comma-separated keys (GPUI key names) in
//!   the review list once it has loaded, and `--review-report <file>` writes
//!   the review reached as JSON and quits once its diff is complete and
//!   its map laid out: the app test of #90 and #92. With `--stay-open` it
//!   does not quit, so a screenshot can be taken (`.github/scripts`).

use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use blueprint_app::first_paint;
use blueprint_engine::service::Service;
use blueprint_protocol::Detail;
use blueprint_ui::workspace::{Screen, Workspace};
use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

static START: OnceLock<Instant> = OnceLock::new();

const USAGE: &str = "usage: blueprint [<repo>] [--cold-start-report <file>] [--script <keys>] [--review-report <file> [--stay-open]]";

#[derive(Default)]
struct Args {
    repo: Option<String>,
    cold_start_report: Option<String>,
    script: Vec<String>,
    review_report: Option<String>,
    stay_open: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--cold-start-report" => out.cold_start_report = Some(value()?),
            "--script" => {
                out.script = value()?
                    .split(',')
                    .filter(|k| !k.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "--review-report" => out.review_report = Some(value()?),
            "--stay-open" => out.stay_open = true,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            repo if out.repo.is_none() => out.repo = Some(repo.to_owned()),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(out)
}

fn main() {
    let start = *START.get_or_init(Instant::now);
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("blueprint: {e}\n{USAGE}");
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
        let report = args.cold_start_report.clone();
        let opened = match args.repo.clone() {
            None => cx
                .open_window(options, |window, cx| {
                    if let Some(path) = report.clone() {
                        first_paint(window, cx, start, path);
                    }
                    cx.new(|_| blueprint_ui::Shell)
                })
                .map(|_| ()),
            Some(repo) => {
                let (service, replies) = match Service::start() {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("blueprint: cannot start the engine: {e}");
                        std::process::exit(1);
                    }
                };
                let service = Arc::new(service);
                let script = args.script.clone();
                let review_report = args.review_report.clone();
                let stay_open = args.stay_open;
                let opened = cx.open_window(options, |window, cx| {
                    if let Some(path) = report.clone() {
                        first_paint(window, cx, start, path);
                    }
                    let to_engine = Arc::clone(&service);
                    let send = Rc::new(move |id, request| to_engine.send(id, request));
                    let ws = cx.new(|cx| Workspace::new(&repo, send, script, cx));
                    let focus = ws.read(cx).focus_handle().clone();
                    window.focus(&focus, cx);
                    pump(ws.clone(), replies, Arc::clone(&service), cx);
                    if let Some(path) = review_report {
                        report_review(&ws, path, stay_open, cx);
                    }
                    ws
                });
                opened.map(|_| ())
            }
        };
        if let Err(e) = opened {
            eprintln!("blueprint: cannot open a window: {e}");
            std::process::exit(1);
        }
        cx.activate(true);
        if report.is_some() || (args.review_report.is_some() && !args.stay_open) {
            // Never hang a benchmark or a test.
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(120))
                    .await;
                eprintln!("blueprint: gave up after 120 s");
                std::process::exit(3);
            })
            .detach();
        }
    });
}

/// Delivers engine replies to the workspace, waiting for each on a
/// background thread so the UI thread never blocks. Holds the service for
/// as long as the window lives.
fn pump(
    ws: gpui::Entity<Workspace>,
    replies: std::sync::mpsc::Receiver<blueprint_protocol::Reply>,
    service: Arc<Service>,
    cx: &mut App,
) {
    let replies = Arc::new(Mutex::new(replies));
    let ws = ws.downgrade();
    cx.spawn(async move |cx| {
        let _service = service;
        loop {
            let rx = Arc::clone(&replies);
            let reply = cx
                .background_executor()
                .spawn(async move { rx.lock().ok()?.recv().ok() })
                .await;
            let Some(reply) = reply else { break };
            if ws.update(cx, |w, cx| w.receive(reply, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
}

/// Once the review's diff is complete and its map laid out, writes it to
/// `path` as JSON and quits.
fn report_review(ws: &gpui::Entity<Workspace>, path: String, stay_open: bool, cx: &mut App) {
    let mut written = false;
    cx.observe(ws, move |ws, cx| {
        if written {
            return;
        }
        let w = ws.read(cx);
        let Screen::Review(r) = &w.screen else {
            return;
        };
        let (Some(map), Some(_)) = (&r.map, &r.layout) else {
            return;
        };
        if r.mapped != Some(Detail::Full) || w.script_pending() {
            return;
        }
        // A code view opened by the script must have its highlighted diff.
        if let Some(code) = &r.code
            && code.detail != Some(Detail::Full)
            && code.error.is_none()
        {
            return;
        }
        let code_rows = r.code.as_ref().map_or(0, |c| c.rows());
        let json = format!(
            "{{\"base\": \"{}\", \"head\": \"{}\", \"findings\": {}, \"changed\": {}, \"map_nodes\": {}, \"code_rows\": {code_rows}}}\n",
            r.base,
            r.head,
            r.findings,
            r.changed,
            map.view.nodes.len()
        );
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("blueprint: cannot write {path}: {e}");
            std::process::exit(1);
        }
        written = true;
        if !stay_open {
            cx.quit();
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_the_repository_and_options() {
        let a = parse(&args(&[
            "repo",
            "--script",
            "down,b,,enter",
            "--review-report",
            "r",
            "--stay-open",
        ]));
        let a = a.unwrap_or_default();
        assert_eq!(a.repo.as_deref(), Some("repo"));
        assert_eq!(a.script, vec!["down", "b", "enter"]);
        assert_eq!(a.review_report.as_deref(), Some("r"));
        assert!(a.stay_open);
        assert!(parse(&args(&["--bogus"])).is_err());
        assert!(parse(&args(&["a", "b"])).is_err());
        assert!(parse(&args(&["--script"])).is_err());
    }
}
