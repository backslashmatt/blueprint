//! M0 UI toolkit spike (issue #13). Throwaway code: it answers one question,
//! whether GPUI can meet the REQUIREMENTS §4 rendering budgets, and is then
//! deleted or kept only as a measurement tool.
//!
//! Usage: `gpui-spike graph|text [--out metrics.json] [--hold-ms N]`
//!
//! Each scenario drives itself with no human in the loop, records timings,
//! writes JSON and quits. `--hold-ms` keeps the final frame on screen so CI can
//! take a screenshot.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::ops::Range;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, Context, FontWeight, HighlightStyle, Hsla, PathBuilder, Pixels,
    Render, ScrollStrategy, SharedString, StyledText, UniformListScrollHandle, Window,
    WindowBounds, WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
    uniform_list,
};

static START: OnceLock<Instant> = OnceLock::new();

const NODES: usize = 5_000;
const EDGES: usize = 10_000;
const LINES: usize = 50_000;
const ANIMATED_FRAMES: usize = 300;
const INPUTS: usize = 30;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Timings shared between the view and its paint callbacks.
#[derive(Default)]
struct Recorder {
    first_paint_ms: Option<f64>,
    open_started: Option<Instant>,
    open_ms: Option<f64>,
    frame_start: Option<Instant>,
    last_paint_end: Option<Instant>,
    /// CPU time from the start of `render` to the end of `paint`.
    work_ms: Vec<f64>,
    /// Time between the ends of consecutive paints while animating.
    interval_ms: Vec<f64>,
    input_at: Option<Instant>,
    input_ms: Vec<f64>,
    animating: bool,
}

impl Recorder {
    fn begin_frame(&mut self) {
        self.frame_start = Some(Instant::now());
    }

    fn end_paint(&mut self) {
        let now = Instant::now();
        if self.first_paint_ms.is_none() {
            self.first_paint_ms = START.get().map(|s| ms(now - *s));
        }
        if let Some(t) = self.open_started.take() {
            self.open_ms = Some(ms(now - t));
        }
        if let Some(t) = self.input_at.take() {
            self.input_ms.push(ms(now - t));
        }
        if self.animating {
            if let Some(s) = self.frame_start {
                self.work_ms.push(ms(now - s));
            }
            if let Some(p) = self.last_paint_end {
                self.interval_ms.push(ms(now - p));
            }
        }
        self.last_paint_end = Some(now);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Starting,
    Animating(usize),
    Input(usize),
    Done,
}

/// A tiny deterministic generator so every platform renders the same scene.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Spike {
    scenario: Scenario,
    phase: Phase,
    rec: Rc<RefCell<Recorder>>,
    out: Option<String>,
    hold_ms: u64,
    // Graph state.
    nodes: Rc<Vec<(f32, f32)>>,
    edges: Rc<Vec<(usize, usize)>>,
    offset: (f32, f32),
    scale: f32,
    // Text state.
    lines: Rc<Vec<(SharedString, Vec<(Range<usize>, HighlightStyle)>)>>,
    scroll: UniformListScrollHandle,
    top_line: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Graph,
    Text,
}

impl Spike {
    fn new(scenario: Scenario, out: Option<String>, hold_ms: u64) -> Self {
        let mut rng = Lcg(42);
        let cols = 100;
        let nodes: Vec<(f32, f32)> = (0..NODES)
            .map(|i| {
                let (c, r) = ((i % cols) as f32, (i / cols) as f32);
                (c * 40.0 + rng.below(20) as f32, r * 40.0 + rng.below(20) as f32)
            })
            .collect();
        let edges = (0..EDGES)
            .map(|i| {
                let a = i % NODES;
                let b = if i % 10 == 0 {
                    rng.below(NODES as u64) as usize
                } else {
                    (a + 1 + rng.below(150) as usize) % NODES
                };
                (a, b)
            })
            .collect();
        Spike {
            scenario,
            phase: Phase::Starting,
            rec: Rc::default(),
            out,
            hold_ms,
            nodes: Rc::new(nodes),
            edges: Rc::new(edges),
            offset: (0.0, 0.0),
            scale: 1.0,
            lines: Rc::new(Vec::new()),
            scroll: UniformListScrollHandle::new(),
            top_line: 0,
        }
    }

    /// Advances the scripted scenario by one frame.
    fn step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.phase {
            Phase::Starting => {
                if self.scenario == Scenario::Text {
                    // "Open" the file: the bytes are in memory; splitting and
                    // highlighting them is part of the measured open.
                    let source = generate_source();
                    self.rec.borrow_mut().open_started = Some(Instant::now());
                    self.lines = Rc::new(highlight(&source));
                }
                self.phase = Phase::Animating(0);
                self.rec.borrow_mut().animating = true;
                window.request_animation_frame();
            }
            Phase::Animating(k) if k < ANIMATED_FRAMES => {
                match self.scenario {
                    Scenario::Graph if k < ANIMATED_FRAMES / 2 => {
                        self.offset.0 -= 6.0;
                        self.offset.1 -= 3.0;
                    }
                    Scenario::Graph => self.scale *= if k < ANIMATED_FRAMES * 3 / 4 { 0.99 } else { 1.012 },
                    Scenario::Text => {
                        self.top_line += 3;
                        self.scroll.scroll_to_item(self.top_line, ScrollStrategy::Top);
                    }
                }
                self.phase = Phase::Animating(k + 1);
                window.request_animation_frame();
            }
            Phase::Animating(_) => {
                self.rec.borrow_mut().animating = false;
                self.phase = Phase::Input(0);
                self.schedule_input(cx);
            }
            Phase::Input(_) | Phase::Done => {}
        }
    }

    /// Simulates one input event arriving 30 ms from now: the state change an
    /// input handler would make, timed until the next paint completes.
    fn schedule_input(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            let _ = this.update(cx, |s, cx| {
                let Phase::Input(n) = s.phase else { return };
                if n == INPUTS {
                    s.phase = Phase::Done;
                    s.finish(cx);
                    return;
                }
                s.rec.borrow_mut().input_at = Some(Instant::now());
                match s.scenario {
                    Scenario::Graph => s.offset.0 += 1.0,
                    Scenario::Text => {
                        s.top_line = s.top_line.saturating_sub(1);
                        s.scroll.scroll_to_item(s.top_line, ScrollStrategy::Top);
                    }
                }
                s.phase = Phase::Input(n + 1);
                cx.notify();
                s.schedule_input(cx);
            });
        })
        .detach();
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let json = report(self.scenario, &self.rec.borrow());
        println!("{json}");
        if let Some(path) = &self.out {
            if let Err(e) = std::fs::write(path, &json) {
                eprintln!("gpui-spike: writing {path}: {e}");
            }
        }
        let hold = self.hold_ms;
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(hold)).await;
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    }

    fn graph(&self) -> impl IntoElement {
        let (nodes, edges, rec) = (self.nodes.clone(), self.edges.clone(), self.rec.clone());
        let (offset, scale) = (self.offset, self.scale);
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                let o = bounds.origin;
                let to_screen = |(x, y): (f32, f32)| {
                    point(o.x + px((x + offset.0) * scale), o.y + px((y + offset.1) * scale))
                };
                let visible = |p: gpui::Point<Pixels>| bounds.contains(&p);
                let mut path = PathBuilder::stroke(px(1.0));
                let mut drawn = 0;
                for &(a, b) in edges.iter() {
                    let (pa, pb) = (to_screen(nodes[a]), to_screen(nodes[b]));
                    if visible(pa) || visible(pb) {
                        path.move_to(pa);
                        path.line_to(pb);
                        drawn += 1;
                    }
                }
                if drawn > 0 {
                    if let Ok(p) = path.build() {
                        window.paint_path(p, rgb(0x8899aa));
                    }
                }
                let side = px(8.0 * scale.max(0.25));
                for &n in nodes.iter() {
                    let p = to_screen(n);
                    if visible(p) {
                        window.paint_quad(fill(Bounds::new(p, size(side, side)), rgb(0x3366cc)));
                    }
                }
                rec.borrow_mut().end_paint();
            },
        )
        .size_full()
    }

    fn text(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.lines.clone();
        let rec = self.rec.clone();
        div()
            .size_full()
            .child(
                uniform_list("lines", lines.len(), move |range, _, _| {
                    range
                        .map(|ix| {
                            let (text, hl) = &lines[ix];
                            div()
                                .h(px(18.0))
                                .font_family("monospace")
                                .text_size(px(13.0))
                                .child(StyledText::new(text.clone()).with_highlights(hl.clone()))
                        })
                        .collect()
                })
                .track_scroll(self.scroll.clone())
                .size_full(),
            )
            // An empty canvas painted last marks the end of the frame's paint.
            .child(
                canvas(|_, _, _| {}, move |_, _, _, _| rec.borrow_mut().end_paint())
                    .absolute()
                    .size_0(),
            )
            .text_color(rgb(0x202020))
            .bg(rgb(0xfafafa))
            .id("text-root")
            .on_click(cx.listener(|_, _, _, _| {}))
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rec.borrow_mut().begin_frame();
        self.step(window, cx);
        let body = match self.scenario {
            Scenario::Graph => self.graph().into_any_element(),
            Scenario::Text => self.text(cx).into_any_element(),
        };
        div().size_full().bg(rgb(0xffffff)).child(body)
    }
}

fn generate_source() -> String {
    let mut s = String::with_capacity(LINES * 48);
    for i in 0..LINES {
        let _ = match i % 6 {
            0 => writeln!(s, "/// Documentation for item number {i}."),
            1 => writeln!(s, "pub fn function_{i}(input: &str, count: usize) -> Result<u64, Error> {{"),
            2 => writeln!(s, "    let value = compute(\"literal {i}\", count * {i});"),
            3 => writeln!(s, "    if value > {i} {{ return Err(Error::TooLarge(value)); }}"),
            4 => writeln!(s, "    Ok(value + {i}) // trailing comment"),
            _ => writeln!(s, "}}"),
        };
    }
    s
}

/// A crude highlighter: keywords, strings, numbers and comments.
fn highlight(source: &str) -> Vec<(SharedString, Vec<(Range<usize>, HighlightStyle)>)> {
    let color = |hex: u32| Some(Hsla::from(rgb(hex)));
    let keyword = HighlightStyle { color: color(0x8a2be2), font_weight: Some(FontWeight::BOLD), ..Default::default() };
    let string = HighlightStyle { color: color(0x0a7d32), ..Default::default() };
    let number = HighlightStyle { color: color(0xb35c00), ..Default::default() };
    let comment = HighlightStyle { color: color(0x8a8a8a), ..Default::default() };
    source
        .lines()
        .map(|line| {
            let mut hl = Vec::new();
            let bytes = line.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    hl.push((i..bytes.len(), comment));
                    break;
                } else if c == b'"' {
                    let end = line[i + 1..].find('"').map_or(bytes.len(), |e| i + e + 2);
                    hl.push((i..end, string));
                    i = end;
                } else if c.is_ascii_digit() {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    hl.push((start..i, number));
                } else if c.is_ascii_alphabetic() {
                    let start = i;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                        i += 1;
                    }
                    if matches!(&line[start..i], "pub" | "fn" | "let" | "if" | "return" | "usize" | "u64" | "str") {
                        hl.push((start..i, keyword));
                    }
                } else {
                    i += 1;
                }
            }
            (SharedString::from(line.to_owned()), hl)
        })
        .collect()
}

fn stats(xs: &[f64]) -> String {
    if xs.is_empty() {
        return "null".into();
    }
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    format!(
        "{{\"n\": {}, \"p50\": {:.3}, \"p95\": {:.3}, \"p99\": {:.3}, \"max\": {:.3}, \"over_16ms\": {}}}",
        v.len(),
        pct(0.5),
        pct(0.95),
        pct(0.99),
        v[v.len() - 1],
        v.iter().filter(|x| **x > 16.0).count()
    )
}

fn report(scenario: Scenario, r: &Recorder) -> String {
    let opt = |x: Option<f64>| x.map_or("null".into(), |v| format!("{v:.3}"));
    let rss = memory_stats::memory_stats().map(|m| m.physical_mem as f64 / 1_048_576.0);
    format!(
        "{{\"scenario\": \"{}\", \"os\": \"{}\", \"arch\": \"{}\", \"first_paint_ms\": {}, \"open_ms\": {}, \"frame_work_ms\": {}, \"frame_interval_ms\": {}, \"input_to_paint_ms\": {}, \"rss_mb\": {}}}",
        if scenario == Scenario::Graph { "graph" } else { "text" },
        std::env::consts::OS,
        std::env::consts::ARCH,
        opt(r.first_paint_ms),
        opt(r.open_ms),
        stats(&r.work_ms),
        stats(&r.interval_ms),
        stats(&r.input_ms),
        opt(rss),
    )
}

fn main() {
    START.get_or_init(Instant::now);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scenario = match args.first().map(String::as_str) {
        Some("graph") => Scenario::Graph,
        Some("text") => Scenario::Text,
        _ => {
            eprintln!("usage: gpui-spike graph|text [--out FILE] [--hold-ms N]");
            std::process::exit(2);
        }
    };
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let out = flag("--out");
    let hold_ms = flag("--hold-ms").and_then(|v| v.parse().ok()).unwrap_or(0);

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let opened = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), focus: true, ..Default::default() },
            |_, cx| cx.new(|_| Spike::new(scenario, out.clone(), hold_ms)),
        );
        if let Err(e) = opened {
            eprintln!("gpui-spike: cannot open a window: {e}");
            std::process::exit(1);
        }
        cx.activate(true);
        // Safety net: never hang CI.
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(120)).await;
            eprintln!("gpui-spike: timed out");
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
