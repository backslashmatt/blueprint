//! The window's root view once a repository is opened: the review list,
//! then the review of the chosen commits. It reaches the engine only by
//! sending protocol requests through the function it is given and
//! receiving replies in [`Workspace::receive`] (REQUIREMENTS §5.3).

use std::collections::HashMap;
use std::rc::Rc;

use blueprint_protocol::model::NodeId;
use blueprint_protocol::model::diff::{FileText, RowKind};
use blueprint_protocol::model::lang::TokenClass;
use blueprint_protocol::{Detail, Reply, Request, RequestId, Response};
use gpui::{
    Context, FocusHandle, HighlightStyle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollStrategy, Styled, StyledText, UniformListScrollHandle, Window,
    div, px, rgb, uniform_list,
};

use crate::Theme;
use crate::code_view::CodeView;
use crate::layout::{Layout, Metrics, Seed, layout, relayout};
use crate::map::{EdgeStatus, Map, Status, nearest_towards};
use crate::review_list::{Effect, ReviewList, action_for_key, short};

/// Sends one request to the engine.
pub type Send = Rc<dyn Fn(RequestId, Request)>;

/// A review in progress: what the engine has answered so far.
#[derive(Debug, Clone)]
pub struct Review {
    /// The base commit.
    pub base: String,
    /// The head commit.
    pub head: String,
    /// How much of the diff has arrived; `None` before the first answer.
    pub detail: Option<Detail>,
    /// Findings in the diff so far.
    pub findings: usize,
    /// Changed nodes in the diff so far.
    pub changed: usize,
    /// The map of the diff so far.
    pub map: Option<Map>,
    /// How much of the diff the map shows.
    pub mapped: Option<Detail>,
    /// The current view's layout, once computed.
    pub layout: Option<Layout>,
    /// The code view, when one is open over the map (#93).
    pub code: Option<CodeView>,
    /// The file diff request the code view waits for.
    code_request: Option<RequestId>,
}

/// What the workspace shows.
#[derive(Debug, Clone)]
pub enum Screen {
    /// Waiting for the repository's refs.
    Opening,
    /// Choosing base and head.
    List(ReviewList),
    /// Reviewing base against head, on the map (#92).
    Review(Box<Review>),
    /// The engine could not answer.
    Failed(String),
}

/// The workspace's state and its link to the engine.
pub struct Workspace {
    /// What is on screen.
    pub screen: Screen,
    list: Option<ReviewList>,
    send: Send,
    next_id: RequestId,
    /// The request whose answers the current screen waits for.
    waiting: Option<RequestId>,
    /// Keys to apply once the list arrives (`--script`), for app tests.
    script: Vec<String>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    code_scroll: UniformListScrollHandle,
    /// Each view's last layout, so a new one keeps nodes in place (DR 0010).
    seeds: HashMap<Option<NodeId>, Seed>,
    /// Bumped for every layout asked for; only the newest is kept.
    layout_generation: u64,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// Opens the repository at `path` and asks for its refs.
    pub fn new(path: &str, send: Send, script: Vec<String>, cx: &mut Context<Self>) -> Self {
        let mut w = Workspace {
            screen: Screen::Opening,
            list: None,
            send,
            next_id: 0,
            waiting: None,
            script,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            code_scroll: UniformListScrollHandle::new(),
            seeds: HashMap::new(),
            layout_generation: 0,
        };
        w.request(Request::Open { path: path.into() });
        let refs = w.request(Request::Refs { limit: 200 });
        w.waiting = Some(refs);
        w
    }

    /// The focus handle keys arrive through; the app focuses it.
    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn request(&mut self, r: Request) -> RequestId {
        self.next_id += 1;
        (self.send)(self.next_id, r);
        self.next_id
    }

    /// Takes in one engine reply.
    pub fn receive(&mut self, reply: Reply, cx: &mut Context<Self>) {
        if let Screen::Review(review) = &mut self.screen
            && review.code_request == Some(reply.id)
        {
            match (&reply.response, review.code.as_mut()) {
                (Response::FileDiff { detail, diff, .. }, Some(code)) => {
                    let first = code.diff.is_none();
                    code.receive(*detail, diff.clone());
                    if first {
                        self.code_scroll
                            .scroll_to_item(code.cursor, ScrollStrategy::Center);
                    }
                }
                (Response::Failed { error }, Some(code)) => code.error = Some(error.clone()),
                _ => {}
            }
            self.replay(cx);
            cx.notify();
            return;
        }
        if let Response::Failed { error } = &reply.response {
            // An Open that fails is reported by the Refs request after it,
            // whose reply is the one awaited.
            if self.waiting == Some(reply.id) {
                self.screen = Screen::Failed(error.clone());
                cx.notify();
            }
            return;
        }
        if self.waiting != Some(reply.id) {
            return;
        }
        match (&mut self.screen, &reply.response) {
            (Screen::Opening, r) => {
                if let Some(list) = ReviewList::from_response(r) {
                    self.screen = Screen::List(list);
                    // Keys up to the one that opens a review; the rest wait
                    // for its map.
                    while !self.script.is_empty() && matches!(self.screen, Screen::List(_)) {
                        let key = self.script.remove(0);
                        self.key(&key, cx);
                    }
                }
            }
            (
                Screen::Review(review),
                Response::Diff {
                    detail,
                    diff,
                    base_model,
                    head_model,
                    ..
                },
            ) => {
                review.detail = Some(*detail);
                review.findings = diff.findings.len();
                review.changed = diff.nodes.len();
                // Building the map rolls up every edge once: off the UI
                // thread (§4).
                let (base, head, diff) = (base_model.clone(), head_model.clone(), diff.clone());
                let detail = *detail;
                let request = reply.id;
                let task = cx
                    .background_executor()
                    .spawn(async move { Map::new(base, head, &diff) });
                cx.spawn(async move |this, cx| {
                    let map = task.await;
                    let _ = this.update(cx, |w, cx| w.map_ready(request, detail, map, cx));
                })
                .detach();
            }
            _ => {}
        }
        cx.notify();
    }

    /// Puts a newly built map on screen, keeping the place the reviewer had
    /// reached on the previous one, and lays it out.
    fn map_ready(
        &mut self,
        request: RequestId,
        detail: Detail,
        mut map: Map,
        cx: &mut Context<Self>,
    ) {
        if self.waiting != Some(request) {
            return;
        }
        let Screen::Review(review) = &mut self.screen else {
            return;
        };
        if let Some(old) = &review.map {
            for id in &old.path {
                map.selected = map.view.nodes.iter().position(|n| &n.id == id);
                if map.selected.is_none() || !map.drill_down() {
                    break;
                }
            }
            if let Some(sel) = old.selected.and_then(|i| old.view.nodes.get(i))
                && let Some(i) = map.view.nodes.iter().position(|n| n.id == sel.id)
            {
                map.selected = Some(i);
            }
        }
        review.map = Some(map);
        review.mapped = Some(detail);
        // The old layout belongs to the old map.
        review.layout = None;
        self.lay_out(cx);
        cx.notify();
    }

    /// Whether `--script` keys are still waiting to be pressed.
    #[must_use]
    pub fn script_pending(&self) -> bool {
        !self.script.is_empty()
    }

    /// Presses waiting `--script` keys on the full map, each once the view
    /// it acts on is laid out.
    fn replay(&mut self, cx: &mut Context<Self>) {
        loop {
            let ready = matches!(
                &self.screen,
                Screen::Review(r) if r.mapped == Some(Detail::Full)
                    && r.layout.is_some()
                    && r.code.as_ref().is_none_or(|c| c.detail == Some(Detail::Full) || c.error.is_some())
            );
            if !ready || self.script.is_empty() {
                return;
            }
            let key = self.script.remove(0);
            self.key(&key, cx);
        }
    }

    /// Lays out the current view on a background thread, seeded from its
    /// last layout; the newest request wins.
    fn lay_out(&mut self, cx: &mut Context<Self>) {
        let Screen::Review(review) = &self.screen else {
            return;
        };
        let Some(map) = &review.map else {
            return;
        };
        let parent = map.view.parent.clone();
        let ids = map.view.ids();
        let pairs = map.view.pairs();
        let seed = self.seeds.get(&parent).cloned();
        self.layout_generation += 1;
        let generation = self.layout_generation;
        let task = cx.background_executor().spawn(async move {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let l = match &seed {
                Some(s) => relayout(&refs, &pairs, Metrics::default(), s),
                None => layout(&refs, &pairs, Metrics::default()),
            };
            let seed = l.seed(&refs);
            (l, seed)
        });
        cx.spawn(async move |this, cx| {
            let (l, seed) = task.await;
            let _ = this.update(cx, |w, cx| {
                if w.layout_generation != generation {
                    return;
                }
                w.seeds.insert(parent, seed);
                if let Screen::Review(r) = &mut w.screen {
                    r.layout = Some(l);
                }
                w.replay(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Applies one key, by its GPUI name.
    pub fn key(&mut self, key: &str, cx: &mut Context<Self>) {
        match &mut self.screen {
            Screen::List(list) => {
                let Some(action) = action_for_key(key) else {
                    return;
                };
                let before = list.cursor;
                let effect = list.apply(action);
                if list.cursor != before {
                    self.scroll
                        .scroll_to_item(list.cursor, ScrollStrategy::Nearest);
                }
                if let Effect::Review { base, head } = effect {
                    self.list = Some(list.clone());
                    let id = self.request(Request::Diff {
                        base: base.clone(),
                        head: head.clone(),
                    });
                    self.waiting = Some(id);
                    self.screen = Screen::Review(Box::new(Review {
                        base,
                        head,
                        detail: None,
                        findings: 0,
                        changed: 0,
                        map: None,
                        mapped: None,
                        layout: None,
                        code: None,
                        code_request: None,
                    }));
                }
            }
            Screen::Review(review) if review.code.is_some() => {
                if matches!(key, "backspace" | "escape") {
                    review.code = None;
                    review.code_request = None;
                } else if let Some(code) = review.code.as_mut()
                    && code.key(key)
                {
                    self.code_scroll
                        .scroll_to_item(code.cursor, ScrollStrategy::Nearest);
                }
            }
            Screen::Review(review) if key != "escape" && review.map.is_some() => {
                // Enter on a node with nothing inside, or `c` on any node,
                // opens its code.
                let leaf = review
                    .map
                    .as_ref()
                    .and_then(|m| m.selected.and_then(|i| m.view.nodes.get(i)))
                    .map(|n| (n.clone(), !n.has_children));
                if let Some((node, is_leaf)) = leaf
                    && ((key == "enter" && is_leaf) || key == "c")
                    && !node.source.is_empty()
                {
                    let (base, head) = (review.base.clone(), review.head.clone());
                    review.code = Some(CodeView::new(
                        node.source.clone(),
                        node.name.clone(),
                        node.lines,
                    ));
                    self.next_id += 1;
                    let id = self.next_id;
                    (self.send)(
                        id,
                        Request::FileDiff {
                            base,
                            head,
                            path: node.source,
                        },
                    );
                    if let Screen::Review(review) = &mut self.screen {
                        review.code_request = Some(id);
                    }
                    return;
                }
                let Screen::Review(review) = &mut self.screen else {
                    return;
                };
                let Review { map, layout, .. } = &mut **review;
                let Some(map) = map.as_mut() else {
                    return;
                };
                let moved = match key {
                    "enter" => map.drill_down(),
                    "backspace" => map.drill_up(),
                    "tab" => {
                        map.cycle(true);
                        false
                    }
                    "left" | "right" | "up" | "down" => {
                        let (dx, dy) = match key {
                            "left" => (-1.0, 0.0),
                            "right" => (1.0, 0.0),
                            "up" => (0.0, -1.0),
                            _ => (0.0, 1.0),
                        };
                        if let (Some(l), Some(from)) = (layout.as_ref(), map.selected) {
                            let pts: Vec<(f32, f32)> = l.nodes.iter().map(|p| (p.x, p.y)).collect();
                            if let Some(i) = nearest_towards(&pts, from, dx, dy) {
                                map.selected = Some(i);
                            }
                        }
                        false
                    }
                    _ => false,
                };
                if moved {
                    *layout = None;
                    self.lay_out(cx);
                }
            }
            Screen::Review(_) | Screen::Failed(_) if key == "escape" => {
                if let Some(list) = self.list.take() {
                    self.waiting = None;
                    // Layouts still running belong to the review left.
                    self.layout_generation += 1;
                    self.seeds.clear();
                    self.screen = Screen::List(list);
                }
            }
            _ => {}
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        let dark = theme == Theme::DARK;
        let root = div()
            .id("workspace")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                this.key(&ev.keystroke.key, cx);
                cx.notify();
            }))
            .size_full()
            .flex()
            .flex_col()
            .gap_1()
            .p_6()
            .bg(rgb(theme.background))
            .text_color(rgb(theme.text));
        match &self.screen {
            Screen::Opening => root.child("Opening the repository…"),
            Screen::Failed(e) => root
                .child(format!("The engine could not answer: {e}"))
                .child(
                    div()
                        .text_color(rgb(theme.muted))
                        .child(if self.list.is_some() {
                            "Esc: back to the list"
                        } else {
                            ""
                        }),
                ),
            Screen::List(list) => {
                let header = format!(
                    "On {}. ↑↓ Home End move, b base, h head, s swap, Enter review.",
                    list.current.as_deref().unwrap_or("a detached HEAD")
                );
                let lines: Vec<String> = list
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, row)| {
                        let mark = match (
                            list.base.as_deref() == Some(row.commit()),
                            list.head.as_deref() == Some(row.commit()),
                        ) {
                            (true, true) => "base+head",
                            (true, false) => "base",
                            (false, true) => "head",
                            (false, false) => "",
                        };
                        let cursor = if i == list.cursor { "›" } else { " " };
                        format!("{cursor} {mark:<9} {}", row.label())
                    })
                    .collect();
                // Only the rows in view are built; the cursor's row is
                // scrolled into view as it moves.
                root.child(div().text_color(rgb(theme.muted)).child(header))
                    .child(
                        uniform_list("review-list", lines.len(), move |range, _, _| {
                            range
                                .map(|i| div().h(px(22.0)).child(lines[i].clone()))
                                .collect()
                        })
                        .track_scroll(&self.scroll)
                        .flex_1(),
                    )
            }
            Screen::Review(r) => {
                let state = match r.detail {
                    None => "loading…",
                    Some(Detail::Coarse) => "systems and modules; code still loading…",
                    Some(Detail::Full) => "complete",
                };
                let crumbs = r.map.as_ref().map(Map::breadcrumb).unwrap_or_default();
                let place = if crumbs.is_empty() {
                    "systems".to_string()
                } else {
                    crumbs.join(" › ")
                };
                let root = root
                    .child(format!(
                        "Reviewing {} → {}: {} findings, {} changed nodes ({state})",
                        short(&r.base),
                        short(&r.head),
                        r.findings,
                        r.changed
                    ))
                    .child(div().text_color(rgb(theme.muted)).child(format!(
                        "{place}. Arrows or Tab select, Enter drill down (or open the code of a leaf), c code, Backspace up, Esc back to the list."
                    )));
                if let Some(code) = &r.code {
                    return root.child(code_view(code, &self.code_scroll, dark));
                }
                match (&r.map, &r.layout) {
                    (Some(map), Some(l)) => {
                        let size = window.viewport_size();
                        root.child(canvas(
                            map,
                            l,
                            (f32::from(size.width), f32::from(size.height)),
                            dark,
                        ))
                    }
                    _ => root,
                }
            }
        }
    }
}

/// A node's fill for its status: the diff overlay (§5.4).
fn fill(status: Status, dark: bool) -> u32 {
    match (status, dark) {
        (Status::Added, false) => 0xd8f3dc,
        (Status::Added, true) => 0x1f4d2b,
        (Status::Removed, false) => 0xfde2e1,
        (Status::Removed, true) => 0x5c1f1f,
        (Status::Surface, false) => 0xffe8cc,
        (Status::Surface, true) => 0x5c3d12,
        (Status::Moved, false) => 0xe9e3ff,
        (Status::Moved, true) => 0x3b2f66,
        (Status::Changed, false) => 0xfff4c2,
        (Status::Changed, true) => 0x4d4214,
        (Status::Contained | Status::Inside | Status::Unchanged, false) => 0xf1f3f5,
        (Status::Contained | Status::Inside | Status::Unchanged, true) => 0x2a2d31,
    }
}

fn edge_colour(status: EdgeStatus, dark: bool) -> u32 {
    match (status, dark) {
        (EdgeStatus::Added, _) => 0x2f9e44,
        (EdgeStatus::Removed, _) => 0xe03131,
        (EdgeStatus::Unchanged, false) => 0xadb5bd,
        (EdgeStatus::Unchanged, true) => 0x5c636a,
    }
}

/// The view as positioned boxes and edge segments (quads, DR 0003),
/// centred on the selected node. Only what falls inside `viewport` is built
/// (§4: no layout of off-screen content).
fn canvas(map: &Map, l: &Layout, viewport: (f32, f32), dark: bool) -> gpui::Div {
    let m = Metrics::default();
    let (vw, vh) = (viewport.0, (viewport.1 - 120.0).max(100.0));
    let centre = map
        .selected
        .and_then(|i| l.nodes.get(i))
        .map_or((l.width / 2.0, l.height / 2.0), |p| {
            (p.x + m.width / 2.0, p.y + m.height / 2.0)
        });
    let (ox, oy) = (vw / 2.0 - centre.0, vh / 2.0 - centre.1);
    let visible = |x: f32, y: f32, w: f32, h: f32| {
        x + ox + w >= 0.0 && x + ox <= vw && y + oy + h >= 0.0 && y + oy <= vh
    };
    let mut c = div().relative().w(px(vw)).h(px(vh)).overflow_hidden();
    for (edge, points) in map.view.edges.iter().zip(&l.edges) {
        let colour = edge_colour(edge.status, dark);
        for seg in points.windows(2) {
            let (p, q) = (seg[0], seg[1]);
            let (x, y) = (p.x.min(q.x), p.y.min(q.y));
            let (w, h) = ((p.x - q.x).abs().max(2.0), (p.y - q.y).abs().max(2.0));
            if visible(x, y, w, h) {
                c = c.child(
                    div()
                        .absolute()
                        .left(px(x + ox - 1.0))
                        .top(px(y + oy - 1.0))
                        .w(px(w))
                        .h(px(h))
                        .bg(rgb(colour)),
                );
            }
        }
    }
    for (i, (node, p)) in map.view.nodes.iter().zip(&l.nodes).enumerate() {
        if !visible(p.x, p.y, m.width, m.height) {
            continue;
        }
        let selected = map.selected == Some(i);
        let border = if selected {
            0x1c7ed6
        } else if node.status == Status::Inside {
            0xf08c00
        } else if dark {
            0x495057
        } else {
            0xced4da
        };
        let mut label = node.name.clone();
        if node.has_children {
            label.push_str(" ›");
        }
        c = c.child(
            div()
                .absolute()
                .left(px(p.x + ox))
                .top(px(p.y + oy))
                .w(px(m.width))
                .h(px(m.height))
                .px_2()
                .overflow_hidden()
                .bg(rgb(fill(node.status, dark)))
                .border_2()
                .border_color(rgb(border))
                .rounded_md()
                .text_sm()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label),
        );
    }
    c
}

/// A monospace family that every platform has (DR 0003: the generic
/// `monospace` does not resolve in GPUI).
fn mono_font() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

fn token_colour(class: TokenClass, dark: bool) -> u32 {
    match (class, dark) {
        (TokenClass::Keyword, false) => 0x8250df,
        (TokenClass::Keyword, true) => 0xd2a8ff,
        (TokenClass::Type, false) => 0x0a7a7a,
        (TokenClass::Type, true) => 0x56d4dd,
        (TokenClass::Function, false) => 0x0550ae,
        (TokenClass::Function, true) => 0x79c0ff,
        (TokenClass::String, false) => 0x116329,
        (TokenClass::String, true) => 0xa5d6a7,
        (TokenClass::Number, false) => 0x953800,
        (TokenClass::Number, true) => 0xffa657,
        (TokenClass::Comment, false) => 0x6e7781,
        (TokenClass::Comment, true) => 0x8b949e,
    }
}

/// One side of a row: its line number and its highlighted text.
fn side(text: &FileText, line: Option<u32>, tint: Option<u32>, dark: bool) -> gpui::Div {
    let mut cell = div()
        .w_1_2()
        .flex()
        .flex_row()
        .overflow_hidden()
        .whitespace_nowrap();
    if let Some(t) = tint {
        cell = cell.bg(rgb(t));
    }
    let Some(i) = line.map(|l| l as usize) else {
        return cell;
    };
    let body = text.lines.get(i).cloned().unwrap_or_default();
    let spans: Vec<(std::ops::Range<usize>, HighlightStyle)> = text
        .highlights
        .get(i)
        .into_iter()
        .flatten()
        .filter(|s| body.get(s.start as usize..s.end as usize).is_some())
        .map(|s| {
            (
                s.start as usize..s.end as usize,
                HighlightStyle {
                    color: Some(rgb(token_colour(s.class, dark)).into()),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect();
    cell.child(
        div()
            .w(px(48.0))
            .flex_none()
            .text_color(rgb(if dark { 0x6e7681 } else { 0x8c959f }))
            .child(format!("{:>5} ", i + 1)),
    )
    .child(StyledText::new(body).with_highlights(spans))
}

/// The code view: a header, then the rows, of which only those on screen
/// are built (§4).
fn code_view(code: &CodeView, scroll: &UniformListScrollHandle, dark: bool) -> gpui::Div {
    let state = match (&code.error, code.detail) {
        (Some(e), _) => format!("cannot open: {e}"),
        (None, None) => "loading…".to_string(),
        (None, Some(Detail::Coarse)) => "highlighting…".to_string(),
        (None, Some(Detail::Full)) => format!("{} rows", code.rows()),
    };
    let header = div().child(format!(
        "{} — {} ({state}). ↑↓ PgUp PgDn Home End move, n p next or previous change, Backspace back to the map.",
        code.path, code.title
    ));
    let Some(diff) = code.diff.clone() else {
        return div().flex().flex_col().flex_1().child(header);
    };
    let cursor = code.cursor;
    let (removed, added, cursor_bg) = if dark {
        (0x3d1d1d, 0x1d3d24, 0x2d333b)
    } else {
        (0xffebe9, 0xe6ffec, 0xeaeef2)
    };
    let list = uniform_list("code-rows", diff.rows.len(), move |range, _, _| {
        range
            .filter_map(|i| Some((i, *diff.rows.get(i)?)))
            .map(|(i, r)| {
                let (left, right) = match r.kind {
                    RowKind::Same => (None, None),
                    RowKind::Changed => (Some(removed), Some(added)),
                    RowKind::Removed => (Some(removed), None),
                    RowKind::Added => (None, Some(added)),
                };
                let mut row = div()
                    .w_full()
                    .h(px(18.0))
                    .flex()
                    .flex_row()
                    .text_size(px(13.0))
                    .font_family(mono_font())
                    .child(side(&diff.base, r.base, left, dark))
                    .child(side(&diff.head, r.head, right, dark));
                if i == cursor {
                    row = row.bg(rgb(cursor_bg));
                }
                row
            })
            .collect()
    })
    .track_scroll(scroll)
    .flex_1();
    div().flex().flex_col().flex_1().child(header).child(list)
}
