//! The window's root view once a repository is opened: the review list,
//! then the review of the chosen commits. It reaches the engine only by
//! sending protocol requests through the function it is given and
//! receiving replies in [`Workspace::receive`] (REQUIREMENTS §5.3).

use std::collections::HashMap;
use std::rc::Rc;

use blueprint_protocol::model::NodeId;
use blueprint_protocol::model::diff::{FileText, RowKind};
use blueprint_protocol::model::lang::TokenClass;
use blueprint_protocol::model::review::{Mark, ReviewBasis, ReviewState};
use blueprint_protocol::{Detail, Reply, Request, RequestId, Response};
use gpui::prelude::FluentBuilder;
use gpui::{
    Context, FocusHandle, HighlightStyle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollStrategy, Styled, StyledText, UniformListScrollHandle, Window,
    div, px, rgb, uniform_list,
};

use crate::Theme;
use crate::actions::{self, Palette, PaletteEffect, Scope};
use crate::code_view::CodeView;
use crate::findings::{self, Findings, Item};
use crate::layout::{Layout, Metrics, Seed, layout, relayout};
use crate::map::{Camera, EdgeStatus, Map, Status, nearest_towards};
use crate::outline::{self, Outline};
use crate::review_list::{self, Effect, ReviewList, short};

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
    /// How the map is looked at: pan and zoom (#92).
    pub camera: Camera,
    /// The findings list (#114), once the diff has arrived.
    pub list: Option<Findings>,
    /// Whether the map is on screen; a review opens on its findings.
    pub on_map: bool,
    /// Which changed nodes are reviewed (#115), once the full diff and the
    /// stored marks have both arrived.
    pub reviewed: Option<ReviewState>,
    /// The outline tree, when it is shown in place of the map (#117).
    pub outline: Option<Outline>,
    /// The diff as the engine last answered it, for the export (#116).
    pub diff: Option<std::sync::Arc<blueprint_protocol::model::diff::Diff>>,
    /// The file diff request the code view waits for.
    code_request: Option<RequestId>,
}

/// The one-line editor for a node's review note (#115).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteEditor {
    /// The node, and the key its mark is stored under.
    pub node: NodeId,
    /// [`ReviewState::key`] of the node.
    pub key: u64,
    /// The node's name, for the prompt.
    pub name: String,
    /// The text so far.
    pub text: String,
}

/// One timed `--script` key (#95).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyTiming {
    /// The key, by its GPUI name.
    pub key: String,
    /// The action it ran (`crate::actions` id), or `palette.key` for a key
    /// typed into the palette.
    pub action: Option<&'static str>,
    /// Whether it opened a code view (a drill on a leaf, or `c`).
    pub opened_code: bool,
    /// From the key to the end of the next frame's paint: its input
    /// latency, and for a scroll its frame time.
    pub first_frame_ms: f64,
    /// From the key to when its effect was complete (a drill's view laid
    /// out, a code view's diff arrived), plus the paint of the frame that
    /// shows it; not the wait for the display's tick in between.
    pub settled_ms: f64,
}

#[derive(Debug)]
struct Pending {
    key: String,
    action: Option<&'static str>,
    opened_code: bool,
    start: std::time::Instant,
    /// When its effect was complete.
    settled_at: Option<std::time::Instant>,
    /// When the frame the frame callback was registered in began.
    frame_started: std::time::Instant,
    first_frame_ms: Option<f64>,
    /// A frame callback is registered.
    armed: bool,
    /// The screen had settled when it was registered.
    armed_settled: bool,
}

/// What the workspace shows.
#[derive(Debug, Clone)]
pub enum Screen {
    /// No repository open yet: the app was started without one.
    Empty,
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
    findings_scroll: UniformListScrollHandle,
    outline_scroll: UniformListScrollHandle,
    /// The command palette, when open (#94).
    pub palette: Option<Palette>,
    /// The note being written, when the note editor is open (#115).
    pub note: Option<NoteEditor>,
    /// Whether `--script` keys are timed (#95): pressed one at a time, each
    /// once the last one's effect is on screen.
    timed: bool,
    /// The timed key in flight.
    pending: Option<Pending>,
    /// The timed keys so far, in order.
    pub timings: Vec<KeyTiming>,
    /// The next timed key waits for the start of a frame.
    press_wanted: bool,
    /// When the frame being drawn began.
    frame_started: std::time::Instant,
    /// When the last frame finished painting, while keys are timed.
    painted: Rc<std::cell::Cell<Option<std::time::Instant>>>,
    /// A message about the last action that went wrong outside the engine
    /// (the folder dialog), shown above the screen until the next key.
    pub notice: Option<String>,
    /// Each view's last layout, so a new one keeps nodes in place (DR 0010).
    seeds: HashMap<Option<NodeId>, Seed>,
    /// Every review mark stored for the open repository (#115), as the
    /// engine last answered plus the reviewer's marks since.
    marks: Vec<Mark>,
    /// The request for the stored marks, while it is unanswered.
    marks_request: Option<RequestId>,
    /// Marks sent to be stored and not yet answered.
    mark_requests: HashMap<RequestId, Mark>,
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
    /// A workspace on the repository at `path`, asking for its refs, or an
    /// empty one without a path.
    pub fn new(
        path: Option<&str>,
        send: Send,
        script: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut w = Workspace {
            screen: Screen::Empty,
            list: None,
            send,
            next_id: 0,
            waiting: None,
            script,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            code_scroll: UniformListScrollHandle::new(),
            findings_scroll: UniformListScrollHandle::new(),
            outline_scroll: UniformListScrollHandle::new(),
            palette: None,
            note: None,
            notice: None,
            timed: false,
            pending: None,
            timings: Vec::new(),
            painted: Rc::default(),
            press_wanted: false,
            frame_started: std::time::Instant::now(),
            seeds: HashMap::new(),
            layout_generation: 0,
            marks: Vec::new(),
            marks_request: None,
            mark_requests: HashMap::new(),
        };
        if let Some(path) = path {
            w.open(path);
        }
        w
    }

    /// Opens the repository at `path` in place of whatever was open, and
    /// asks for its refs. The engine drops its work on the previous one.
    pub fn open(&mut self, path: &str) {
        self.list = None;
        self.marks.clear();
        self.marks_request = None;
        self.mark_requests.clear();
        self.palette = None;
        // Layouts still running belong to the repository left.
        self.layout_generation += 1;
        self.seeds.clear();
        self.notice = None;
        // The new list starts at its top, and its code views at theirs.
        self.scroll = UniformListScrollHandle::new();
        self.code_scroll = UniformListScrollHandle::new();
        self.findings_scroll = UniformListScrollHandle::new();
        self.screen = Screen::Opening;
        self.request(Request::Open { path: path.into() });
        let refs = self.request(Request::Refs { limit: 200 });
        self.waiting = Some(refs);
    }

    /// Asks for a folder with the platform's dialog and opens it.
    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open repository".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(e)) => {
                    // What was on screen stays: only the dialog failed.
                    let _ = this.update(cx, |w, cx| {
                        w.notice = Some(format!("Cannot show the folder dialog: {e}"));
                        cx.notify();
                    });
                    None
                }
                _ => None,
            };
            if let Some(path) = path {
                let _ = this.update(cx, |w, cx| {
                    let Some(path) = path.to_str() else {
                        w.notice = Some(format!(
                            "{} is not a UTF-8 path, which the engine cannot open",
                            path.display()
                        ));
                        cx.notify();
                        return;
                    };
                    // Test keys belong to the repository they were given for.
                    w.script.clear();
                    w.open(path);
                    cx.notify();
                });
            }
        })
        .detach();
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
        match &reply.response {
            Response::Marks { marks, warning } if self.marks_request == Some(reply.id) => {
                self.marks_request = None;
                self.marks.clone_from(marks);
                // Marks still being stored are newer than what was loaded.
                let unsaved: Vec<Mark> = self.mark_requests.values().cloned().collect();
                for m in unsaved {
                    self.marks.retain(|o| !(o.node == m.node && o.key == m.key));
                    self.marks.push(m);
                }
                if let Some(w) = warning {
                    self.notice = Some(w.clone());
                }
                self.refresh_reviewed();
                self.replay(cx);
                cx.notify();
                return;
            }
            Response::Marked { warning, .. } => {
                self.mark_requests.remove(&reply.id);
                if let Some(w) = warning {
                    self.notice = Some(w.clone());
                }
                // Observers wait for marks to be stored (`marks_pending`).
                cx.notify();
                return;
            }
            Response::Failed { error } if self.mark_requests.remove(&reply.id).is_some() => {
                self.notice = Some(format!("The review mark was not saved: {error}"));
                cx.notify();
                return;
            }
            Response::Failed { error } if self.marks_request == Some(reply.id) => {
                self.marks_request = None;
                self.notice = Some(format!("Review marks could not be loaded: {error}"));
                // Scripts wait for the load to end, either way.
                self.replay(cx);
                cx.notify();
                return;
            }
            // A mark sent meanwhile replaced the load: load again, after it.
            Response::Superseded if self.marks_request == Some(reply.id) => {
                self.marks_request = Some(self.request(Request::Marks));
                return;
            }
            _ => {}
        }
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
                    // for its map (`replay`).
                    self.replay(cx);
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
                review.diff = Some(diff.clone());
                review.findings = diff.findings.len();
                review.changed = diff.nodes.len();
                // Building the map rolls up every edge once: off the UI
                // thread (§4).
                let (base, head, diff) = (base_model.clone(), head_model.clone(), diff.clone());
                let detail = *detail;
                let request = reply.id;
                // Review state needs the change weights, which only the full
                // diff has; it reads both models, beside the map (#115).
                if detail == Detail::Full {
                    let (base, head, diff) = (base.clone(), head.clone(), diff.clone());
                    let task = cx.background_executor().spawn(async move {
                        std::sync::Arc::new(ReviewBasis::new(&diff, &base, &head))
                    });
                    cx.spawn(async move |this, cx| {
                        let basis = task.await;
                        let _ = this.update(cx, |w, cx| w.basis_ready(request, basis, cx));
                    })
                    .detach();
                }
                let task = cx.background_executor().spawn(async move {
                    let map = Map::new(base, head, &diff);
                    // The list with the map it is labelled and shown on.
                    let list = Findings::new(&diff, &map);
                    (map, list)
                });
                cx.spawn(async move |this, cx| {
                    let (map, list) = task.await;
                    let _ = this.update(cx, |w, cx| w.map_ready(request, detail, map, list, cx));
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
        mut list: Findings,
        cx: &mut Context<Self>,
    ) {
        if self.waiting != Some(request) {
            return;
        }
        let Screen::Review(review) = &mut self.screen else {
            return;
        };
        // The coarse map, built in its own task, can finish after the full
        // one: it never replaces it.
        if detail == Detail::Coarse && review.mapped == Some(Detail::Full) {
            return;
        }
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
        // The camera stays only on the same view and selection.
        let same = |m: &Map| {
            (
                m.path.clone(),
                m.selected
                    .and_then(|i| m.view.nodes.get(i))
                    .map(|n| n.id.clone()),
            )
        };
        match review.map.as_ref().map(same) {
            Some((path, sel)) if path == map.path => {
                if sel != same(&map).1 {
                    review.camera.pan = (0.0, 0.0);
                }
            }
            _ => review.camera = Camera::default(),
        }
        // The full list keeps the finding the coarse one had highlighted.
        if let Some(old) = &review.list {
            list.keep(old.current(), old.cursor);
        }
        review.list = Some(list);
        // An open outline follows the new map, in place.
        if let Some(o) = review.outline.as_mut() {
            o.refresh(&map);
        }
        review.map = Some(map);
        review.mapped = Some(detail);

        // The old layout belongs to the old map.
        review.layout = None;
        self.lay_out(cx);
        cx.notify();
    }

    /// Takes in the review basis of the full diff (#115).
    fn basis_ready(
        &mut self,
        request: RequestId,
        basis: std::sync::Arc<ReviewBasis>,
        cx: &mut Context<Self>,
    ) {
        if self.waiting != Some(request) {
            return;
        }
        if let Screen::Review(r) = &mut self.screen {
            r.reviewed = Some(basis.state(&self.marks));
            self.replay(cx);
            cx.notify();
        }
    }

    /// Whether the stored review marks, or a mark being stored, are still
    /// awaited.
    #[must_use]
    pub fn marks_pending(&self) -> bool {
        self.marks_request.is_some() || !self.mark_requests.is_empty()
    }

    /// Whether `--script` keys are still waiting to be pressed.
    #[must_use]
    pub fn script_pending(&self) -> bool {
        !self.script.is_empty() || self.pending.is_some()
    }

    /// Whether the screen is settled: the list, or a review whose full map
    /// is laid out and whose code view, if open, has its full diff.
    fn settled(&self) -> bool {
        match &self.screen {
            Screen::List(_) => true,
            Screen::Review(r) => {
                r.mapped == Some(Detail::Full)
                    && r.layout.is_some()
                    && r.reviewed.is_some()
                    && self.marks_request.is_none()
                    && r.code
                        .as_ref()
                        .is_none_or(|c| c.detail == Some(Detail::Full) || c.error.is_some())
            }
            _ => false,
        }
    }

    /// Presses waiting `--script` keys, each once the screen it acts on has
    /// settled: list keys at once, the keys after the one that opens a
    /// review once its full map is laid out, and so on. Timed keys are
    /// pressed one at a time, at the start of a frame (`press_timed`), so
    /// that none waits for the display's next tick before it is drawn.
    fn replay(&mut self, cx: &mut Context<Self>) {
        if self.timed {
            let settled = self.settled();
            if let Some(p) = self.pending.as_mut() {
                if settled && p.settled_at.is_none() {
                    p.settled_at = Some(std::time::Instant::now());
                }
            } else if settled && !self.script.is_empty() {
                self.press_wanted = true;
                cx.notify();
            }
            return;
        }
        while !self.script.is_empty() && self.settled() {
            let key = self.script.remove(0);
            self.key(&key, cx);
        }
    }

    /// Presses the next timed key, from a next-frame callback: the frame it
    /// changes is drawn straight after.
    fn press_timed(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some() || self.script.is_empty() || !self.settled() {
            return;
        }
        let key = self.script.remove(0);
        let action = match &self.palette {
            Some(_) => Some("palette.key"),
            None => actions::for_key(&self.scopes(), &key).map(|a| a.id),
        };
        let had_code = matches!(&self.screen, Screen::Review(r) if r.code.is_some());
        let start = std::time::Instant::now();
        self.key(&key, cx);
        let opened_code =
            !had_code && matches!(&self.screen, Screen::Review(r) if r.code.is_some());
        let settled_at = self.settled().then(std::time::Instant::now);
        self.pending = Some(Pending {
            key,
            action,
            opened_code,
            start,
            settled_at,
            first_frame_ms: None,
            armed: false,
            armed_settled: false,
            frame_started: start,
        });
        cx.notify();
    }

    /// After a frame is painted with a timed key in flight: its first frame,
    /// and if the screen had settled for that frame, the key is done.
    fn frame_painted(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.pending.as_mut() else {
            return;
        };
        // The end of the frame's paint; presenting it is the platform's.
        let end = self
            .painted
            .get()
            .filter(|t| *t >= p.frame_started)
            .unwrap_or_else(std::time::Instant::now);
        let ms = |from: std::time::Instant, to: std::time::Instant| {
            to.saturating_duration_since(from).as_secs_f64() * 1000.0
        };
        let first = *p.first_frame_ms.get_or_insert(ms(p.start, end));
        p.armed = false;
        if p.armed_settled {
            // Up to when the effect was complete (a layout or a reply
            // arrived), plus the paint of the frame showing it: the wait
            // for the display's next tick in between is not the app's.
            let settled_at = p.settled_at.unwrap_or(p.frame_started);
            let settled = ms(p.start, settled_at) + ms(p.frame_started, end);
            if let Some(p) = self.pending.take() {
                self.timings.push(KeyTiming {
                    key: p.key,
                    action: p.action,
                    opened_code: p.opened_code,
                    first_frame_ms: first,
                    settled_ms: settled.max(first),
                });
            }
            self.replay(cx);
        }
        cx.notify();
    }

    /// Times `--script` keys from now on (#95).
    pub fn time_keys(&mut self) {
        self.timed = true;
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

    /// The action scopes of what is on screen, narrowest first.
    #[must_use]
    pub fn scopes(&self) -> Vec<Scope> {
        match &self.screen {
            Screen::Empty | Screen::Opening => vec![Scope::Global],
            Screen::List(_) => vec![Scope::List, Scope::Global],
            Screen::Review(r) if r.code.is_some() => {
                vec![Scope::Code, Scope::Review, Scope::Global]
            }
            Screen::Review(r) if r.outline.is_some() && r.code.is_none() => {
                vec![Scope::Outline, Scope::Review, Scope::Global]
            }
            Screen::Review(r) if r.map.is_some() && r.on_map => {
                vec![Scope::Map, Scope::Review, Scope::Global]
            }
            Screen::Review(r) if r.map.is_some() && r.list.is_some() => {
                vec![Scope::Findings, Scope::Review, Scope::Global]
            }
            // Back to the list only when there is a list to go back to.
            Screen::Review(_) | Screen::Failed(_) if self.list.is_some() => {
                vec![Scope::Review, Scope::Global]
            }
            Screen::Review(_) | Screen::Failed(_) => vec![Scope::Global],
        }
    }

    /// Applies one key, by its GPUI name (modifiers first, as in
    /// `ctrl-shift-p`): to the palette when it is open, else the action
    /// bound to it on this screen (`crate::actions`).
    pub fn key(&mut self, key: &str, cx: &mut Context<Self>) {
        self.notice = None;
        if self.note.is_some() {
            let typed = match key {
                "space" => Some(" ".to_owned()),
                k if k.chars().count() == 1 => Some(k.to_owned()),
                _ => None,
            };
            self.note_key(key, typed.as_deref());
            return;
        }
        // Opening a repository works over the palette too.
        if actions::for_key(&[Scope::Global], key).is_some_and(|a| a.id == "global.open") {
            self.run("global.open", cx);
            return;
        }
        let scopes = self.scopes();
        if let Some(palette) = self.palette.as_mut() {
            palette.rescope(scopes);
            match palette.key(key) {
                PaletteEffect::Stay => {}
                PaletteEffect::Close => self.palette = None,
                PaletteEffect::Run(action) => {
                    self.palette = None;
                    self.run(action.id, cx);
                }
            }
            return;
        }
        if let Some(action) = actions::for_key(&self.scopes(), key) {
            self.run(action.id, cx);
        }
    }

    /// Runs the action with this registry id, if it applies on this screen.
    pub fn run(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(def) = actions::by_id(id) else {
            return;
        };
        if !self.scopes().contains(&def.scope) {
            return;
        }
        match def.scope {
            Scope::Global if id == "global.open" => {
                self.palette = None;
                self.choose_folder(cx);
            }
            Scope::Global => self.palette = Some(Palette::new(self.scopes())),
            Scope::List => self.run_list(id),
            Scope::Code => self.run_code(id),
            Scope::Findings => self.run_findings(id, cx),
            Scope::Map => self.run_map(id, cx),
            Scope::Outline => self.run_outline(id, cx),
            Scope::Review if id == "review.reviewed" => self.toggle_reviewed(),
            Scope::Review if id == "review.note" => self.open_note(),
            Scope::Review if id == "review.export" => match self.export() {
                Some(text) => {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    self.notice = Some("The review is on the clipboard as Markdown.".into());
                }
                None => {
                    self.notice =
                        Some("The review can be exported once the whole diff has loaded.".into());
                }
            },
            Scope::Review => self.back_to_list(),
        }
    }

    /// The node `r` marks: the code's, else the highlighted finding's on
    /// the findings list, else the map's selected node.
    fn review_target(&self) -> Option<NodeId> {
        let Screen::Review(r) = &self.screen else {
            return None;
        };
        let map = r.map.as_ref()?;
        if r.code.is_none()
            && let Some(o) = &r.outline
        {
            return o.current().cloned();
        }
        let selected = || {
            map.selected
                .and_then(|i| map.view.nodes.get(i))
                .map(|n| n.id.clone())
        };
        if r.code.is_some() || r.on_map {
            return selected();
        }
        r.list.as_ref()?.current().and_then(Item::target).cloned()
    }

    /// Marks the target node reviewed, or, if its own mark says reviewed,
    /// not reviewed (its note stays). Applied at once; the engine stores it.
    fn toggle_reviewed(&mut self) {
        let target = self.review_target();
        let Screen::Review(r) = &mut self.screen else {
            return;
        };
        let Some(state) = r.reviewed.as_ref() else {
            self.notice = Some("Review marks are available once the whole diff has loaded.".into());
            return;
        };
        if self.marks_request.is_some() {
            // A mark made now would not know the stored note, and the load
            // would hide it.
            self.notice = Some("Review marks are still loading.".into());
            return;
        }
        let Some(node) = target else {
            self.notice = Some("Nothing is selected to mark.".into());
            return;
        };
        let Some(key) = state.key(&node) else {
            self.notice = Some("That node did not change in this review.".into());
            return;
        };
        let own = state.mark(&node);
        let marked = own.is_some_and(|m| m.reviewed);
        if !marked && state.is_reviewed(&node) {
            self.notice =
                Some("Already reviewed, by a mark above it or on every change under it.".into());
            return;
        }
        let mark = Mark {
            node,
            key,
            reviewed: !marked,
            note: own.and_then(|m| m.note.clone()),
        };
        let node = mark.node.clone();
        self.store_mark(mark);
        if marked
            && let Screen::Review(r) = &self.screen
            && r.reviewed.as_ref().is_some_and(|st| st.is_reviewed(&node))
        {
            self.notice = Some(
                "Unmarked, but still reviewed: a mark above it, or on every change under it, covers it."
                    .into(),
            );
        }
    }

    /// The review as Markdown (#116), once its review state is known: the
    /// same text as `bp review export`.
    #[must_use]
    pub fn export(&self) -> Option<String> {
        let Screen::Review(r) = &self.screen else {
            return None;
        };
        // The full map's models: the coarse map has only L1–L2.
        if r.mapped != Some(Detail::Full) {
            return None;
        }
        let (state, diff, map) = (r.reviewed.as_ref()?, r.diff.as_ref()?, r.map.as_ref()?);
        Some(blueprint_protocol::model::review::export(
            state,
            diff,
            map.base(),
            map.head(),
            &r.base,
            &r.head,
        ))
    }

    /// Opens the note editor on the node `r` would mark, with its note.
    fn open_note(&mut self) {
        let target = self.review_target();
        let Screen::Review(r) = &self.screen else {
            return;
        };
        let (Some(state), Some(node)) = (r.reviewed.as_ref(), target) else {
            self.notice = Some("Notes are available once the whole diff has loaded.".into());
            return;
        };
        if self.marks_request.is_some() {
            self.notice = Some("Review marks are still loading.".into());
            return;
        }
        let Some(key) = state.key(&node) else {
            self.notice = Some("That node did not change in this review.".into());
            return;
        };
        let text = state
            .mark(&node)
            .and_then(|m| m.note.clone())
            .unwrap_or_default();
        let name = r
            .map
            .as_ref()
            .map_or_else(|| node.to_string(), |m| m.name(&node));
        self.note = Some(NoteEditor {
            node,
            key,
            name,
            text,
        });
    }

    /// One key in the note editor: `typed` is the character it types, if
    /// any.
    fn note_key(&mut self, key: &str, typed: Option<&str>) {
        let Some(editor) = self.note.as_mut() else {
            return;
        };
        match key {
            "escape" => self.note = None,
            "enter" => {
                let Some(editor) = self.note.take() else {
                    return;
                };
                let reviewed = match &self.screen {
                    Screen::Review(r) => r
                        .reviewed
                        .as_ref()
                        .and_then(|st| st.mark(&editor.node))
                        .is_some_and(|m| m.reviewed),
                    _ => false,
                };
                let text = editor.text.trim();
                self.store_mark(Mark {
                    node: editor.node,
                    key: editor.key,
                    reviewed,
                    note: (!text.is_empty()).then(|| text.to_owned()),
                });
            }
            "backspace" => {
                editor.text.pop();
            }
            _ => {
                if let Some(t) = typed.filter(|t| !t.chars().any(char::is_control)) {
                    editor.text.push_str(t);
                }
            }
        }
    }

    /// Applies `mark` at once and sends it to the engine to store.
    fn store_mark(&mut self, mark: Mark) {
        self.marks
            .retain(|m| !(m.node == mark.node && m.key == mark.key));
        self.marks.push(mark.clone());
        self.refresh_reviewed();
        let id = self.request(Request::Mark { mark: mark.clone() });
        self.mark_requests.insert(id, mark);
    }

    /// Recomputes the review state from the marks: cheap, as the basis is
    /// kept (DR 0011).
    fn refresh_reviewed(&mut self) {
        if let Screen::Review(r) = &mut self.screen
            && let Some(state) = r.reviewed.as_ref()
        {
            let basis = std::sync::Arc::clone(state.basis());
            r.reviewed = Some(basis.state(&self.marks));
        }
    }

    fn run_findings(&mut self, id: &str, cx: &mut Context<Self>) {
        let Screen::Review(review) = &mut self.screen else {
            return;
        };
        let Review {
            list,
            map,
            camera,
            layout,
            on_map,
            ..
        } = &mut **review;
        let (Some(list), Some(map)) = (list.as_mut(), map.as_mut()) else {
            return;
        };
        let before = list.cursor;
        let mut relayout = false;
        match id {
            "findings.up" => list.move_by(-1),
            "findings.down" => list.move_by(1),
            "findings.first" => list.to_end(false),
            "findings.last" => list.to_end(true),
            "findings.show" | "findings.map" => {
                let path = map.path.clone();
                if id == "findings.show" {
                    let target = list.current().and_then(Item::target).cloned();
                    if !target.is_some_and(|t| map.reveal(&t)) {
                        self.notice = Some("That finding's node is not on the map.".into());
                        return;
                    }
                } else {
                    while map.drill_up() {}
                }
                *on_map = true;
                *camera = Camera::default();
                if map.path != path || layout.is_none() {
                    *layout = None;
                    relayout = true;
                }
            }
            _ => {}
        }
        let after = list.cursor;
        if after != before {
            self.findings_scroll
                .scroll_to_item(after, ScrollStrategy::Nearest);
        }
        if relayout {
            self.lay_out(cx);
        }
    }

    fn run_outline(&mut self, id: &str, cx: &mut Context<Self>) {
        let Screen::Review(review) = &mut self.screen else {
            return;
        };
        let (Some(o), Some(map)) = (review.outline.as_mut(), review.map.as_mut()) else {
            return;
        };
        let op = match id {
            "outline.up" => Some(outline::Op::Up),
            "outline.down" => Some(outline::Op::Down),
            "outline.first" => Some(outline::Op::First),
            "outline.last" => Some(outline::Op::Last),
            "outline.expand" => Some(outline::Op::Expand),
            "outline.collapse" => Some(outline::Op::Collapse),
            _ => None,
        };
        if let Some(op) = op {
            if o.apply(map, op) {
                self.outline_scroll
                    .scroll_to_item(o.cursor, ScrollStrategy::Nearest);
            }
            return;
        }
        // Back to the map, at the highlighted node.
        let path = map.path.clone();
        if let Some(node) = o.current().cloned() {
            map.reveal(&node);
        }
        review.outline = None;
        review.on_map = true;
        if map.path != path || review.layout.is_none() {
            review.camera = Camera::default();
            review.layout = None;
            self.lay_out(cx);
        } else {
            review.camera.pan = (0.0, 0.0);
        }
    }

    fn run_list(&mut self, id: &str) {
        let Screen::List(list) = &mut self.screen else {
            return;
        };
        let Some(action) = review_list::Action::from_id(id) else {
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
            // The stored marks, for the review state once the diff is in.
            self.marks_request = Some(self.request(Request::Marks));
            self.findings_scroll = UniformListScrollHandle::new();
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
                camera: Camera::default(),
                list: None,
                on_map: false,
                reviewed: None,
                diff: None,
                outline: None,
            }));
        }
    }

    fn run_code(&mut self, id: &str) {
        let Screen::Review(review) = &mut self.screen else {
            return;
        };
        if id == "code.close" {
            review.code = None;
            review.code_request = None;
            return;
        }
        let Some(key) = code_key(id) else {
            return;
        };
        if let Some(code) = review.code.as_mut()
            && code.key(key)
        {
            self.code_scroll
                .scroll_to_item(code.cursor, ScrollStrategy::Nearest);
        }
    }

    fn run_map(&mut self, id: &str, cx: &mut Context<Self>) {
        let Screen::Review(review) = &mut self.screen else {
            return;
        };
        if id == "map.findings" {
            review.on_map = false;
            return;
        }
        if id == "map.outline" {
            review.outline = review.map.as_ref().map(Outline::new);
            // A fresh scroll position, at the highlighted row.
            self.outline_scroll = UniformListScrollHandle::new();
            if let Some(o) = &review.outline {
                self.outline_scroll
                    .scroll_to_item(o.cursor, ScrollStrategy::Center);
            }
            return;
        }
        // Enter on a node with nothing inside, or `c` on any node, opens
        // its code.
        let leaf = review
            .map
            .as_ref()
            .and_then(|m| m.selected.and_then(|i| m.view.nodes.get(i)))
            .map(|n| (n.clone(), !n.has_children));
        if let Some((node, is_leaf)) = leaf
            && ((id == "map.drill_down" && is_leaf) || id == "map.code")
            && !node.source.is_empty()
        {
            let (base, head) = (review.base.clone(), review.head.clone());
            review.code = Some(CodeView::new(
                node.source.clone(),
                node.name.clone(),
                node.lines,
                node.lines_in_base,
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
        let Review {
            map,
            layout,
            camera,
            ..
        } = &mut **review;
        let Some(map) = map.as_mut() else {
            return;
        };
        let before = map.selected;
        let moved = match map_op(id) {
            Some(MapOp::Pan(dx, dy)) => {
                camera.pan_by(dx, dy);
                false
            }
            Some(MapOp::Zoom(steps)) => {
                camera.zoom_by(steps);
                false
            }
            Some(MapOp::ResetCamera) => {
                *camera = Camera::default();
                false
            }
            Some(MapOp::DrillDown) => map.drill_down(),
            Some(MapOp::DrillUp) => map.drill_up(),
            Some(MapOp::Next) => {
                map.cycle(true);
                false
            }
            Some(MapOp::Towards(dx, dy)) => {
                if let (Some(l), Some(from)) = (layout.as_ref(), map.selected) {
                    let pts: Vec<(f32, f32)> = l.nodes.iter().map(|p| (p.x, p.y)).collect();
                    if let Some(i) = nearest_towards(&pts, from, dx, dy) {
                        map.selected = Some(i);
                    }
                }
                false
            }
            // `map.code` was handled above; on a node without source it
            // does nothing.
            Some(MapOp::Code) | None => false,
        };
        if moved {
            // A new view: looked at afresh.
            *camera = Camera::default();
            *layout = None;
            self.lay_out(cx);
        } else if map.selected != before {
            // The view follows the selection, at the same zoom.
            camera.pan = (0.0, 0.0);
        }
    }

    fn back_to_list(&mut self) {
        if let Some(list) = self.list.take() {
            self.waiting = None;
            // Layouts still running belong to the review left.
            self.layout_generation += 1;
            self.seeds.clear();
            self.screen = Screen::List(list);
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.frame_started = std::time::Instant::now();
        let timed = self.timed;
        let painted = Rc::clone(&self.painted);
        let screen = self.render_screen(window, cx);
        // Timed keys (#95): an empty canvas painted last marks when this
        // frame's paint is done.
        div().size_full().child(screen).when(timed, |d| {
            d.child(
                gpui::canvas(
                    |_, _, _| (),
                    move |_, (), _, _| {
                        painted.set(Some(std::time::Instant::now()));
                    },
                )
                .size_0(),
            )
        })
    }
}

impl Workspace {
    fn render_screen(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = Theme::for_appearance(window.appearance());
        let dark = theme == Theme::DARK;
        let scopes = self.scopes();
        if let Some(p) = self.palette.as_mut() {
            p.rescope(scopes);
        }
        let settled = self.settled();
        if let Some(p) = self.pending.as_mut()
            && !p.armed
        {
            p.armed = true;
            p.armed_settled = settled;
            p.frame_started = self.frame_started;
            let ws = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = ws.update(cx, |w, cx| w.frame_painted(cx));
            });
        } else if self.press_wanted && self.pending.is_none() {
            self.press_wanted = false;
            let ws = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = ws.update(cx, |w, cx| w.press_timed(cx));
            });
        }
        let root = div()
            .id("workspace")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                let k = &ev.keystroke;
                let m = &k.modifiers;
                if this.note.is_some() && !(m.control || m.alt || m.platform) {
                    // The character typed, shift and layout applied.
                    this.notice = None;
                    this.note_key(&key_name(k), k.key_char.as_deref());
                } else {
                    this.key(&key_name(k), cx);
                }
                cx.notify();
            }))
            .size_full()
            .flex()
            .flex_col()
            .gap_1()
            .p_6()
            .bg(rgb(theme.background))
            .text_color(rgb(theme.text));
        let root = match &self.notice {
            Some(n) => root.child(div().text_color(rgb(theme.muted)).child(n.clone())),
            None => root,
        };
        let root = match &self.palette {
            Some(p) => root.child(palette_view(p, theme)),
            None => root,
        };
        let root = match &self.note {
            Some(n) => root.child(
                div()
                    .p_2()
                    .border_1()
                    .border_color(rgb(theme.muted))
                    .child(format!("Note on {}: {}▏", n.name, n.text))
                    .child(
                        div()
                            .text_color(rgb(theme.muted))
                            .child("Enter saves, Esc cancels; an empty note removes it."),
                    ),
            ),
            None => root,
        };
        match &self.screen {
            Screen::Empty => root.child("No repository is open.").child(
                div()
                    .text_color(rgb(theme.muted))
                    .child("Ctrl+O (⌘O on macOS) opens one; F1 lists every action."),
            ),
            Screen::Opening => root.child("Opening the repository…"),
            Screen::Failed(e) => root
                .child(format!("The engine could not answer: {e}"))
                .child(
                    div()
                        .text_color(rgb(theme.muted))
                        .child(if self.list.is_some() {
                            "Esc: back to the list; Ctrl+O (⌘O) opens another repository."
                        } else {
                            "Ctrl+O (⌘O on macOS) opens another repository."
                        }),
                ),
            Screen::List(list) => {
                let header = format!(
                    "On {}. ↑↓ Home End move, b base, h head, s swap, Enter review, F1 all actions.",
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
                let progress = r.reviewed.as_ref().map_or(String::new(), |st| {
                    let (done, total) = st.progress();
                    format!("; reviewed {done} of {total}")
                });
                let root = root
                    .child(format!(
                        "Reviewing {} → {}: {} findings, {} changed nodes ({state}{progress})",
                        short(&r.base),
                        short(&r.head),
                        r.findings,
                        r.changed
                    ))
                    .child(div().text_color(rgb(theme.muted)).child(if r.outline.is_some() {
                        "Outline: the map as a tree. ↑↓ Home End move, → expand, ← collapse, Enter show on the map, o or Esc the map, r mark ✓ reviewed, e note, x export, F1 all actions.".to_owned()
                    } else if r.on_map || r.list.is_none() || r.map.is_none() {
                        format!(
                        "{place}. Arrows or Tab select, Enter drill down (or open the code of a leaf), c code, o outline, r mark ✓ reviewed, e note, x export, Backspace up, {pan}+arrows pan, = - 0 zoom, f findings, Esc back to the list, F1 all actions.",
                        pan = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" }
                        )
                    } else {
                        "Findings, the review's starting point. ↑↓ Home End choose, Enter show on the map, r mark ✓ reviewed, e note, x export, m the map, Esc back to the list, F1 all actions.".to_owned()
                    }));
                // The note on the node `r` and `e` act on, if it has one.
                let note = self.review_target().and_then(|t| {
                    let st = r.reviewed.as_ref()?;
                    st.mark(&t)?.note.clone()
                });
                let root = match note {
                    Some(n) => root.child(
                        div()
                            .text_color(rgb(theme.muted))
                            .child(format!("Note: {n}")),
                    ),
                    None => root,
                };
                if let Some(code) = &r.code {
                    return root.child(code_view(code, &self.code_scroll, dark));
                }
                if let Some(o) = &r.outline {
                    return outline_view(root, o, r.reviewed.as_ref(), &self.outline_scroll);
                }
                if !r.on_map
                    && let (Some(list), Some(map)) = (&r.list, &r.map)
                {
                    return findings_view(
                        root,
                        list,
                        map,
                        r.reviewed.as_ref(),
                        &self.findings_scroll,
                    );
                }
                match (&r.map, &r.layout) {
                    (Some(map), Some(l)) => {
                        let size = window.viewport_size();
                        root.child(canvas(
                            map,
                            r.reviewed.as_ref(),
                            l,
                            r.camera,
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

/// The findings list (#114) under the header: one row per finding, the
/// highlighted one marked. The labels are made for every row; only the
/// rows on screen are laid out.
fn findings_view<E: ParentElement>(
    root: E,
    list: &Findings,
    map: &Map,
    reviewed: Option<&ReviewState>,
    scroll: &UniformListScrollHandle,
) -> E {
    if list.items.is_empty() {
        return root.child(
            "No findings: nothing crosses a boundary, closes a cycle or changes a public surface. Press m for the map.",
        );
    }
    let rows: Vec<String> = list
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mark = if i == list.cursor { "›" } else { " " };
            let done = reviewed.is_some_and(|r| r.is_finding_reviewed(item));
            // The help line above shows ✓ too, so its glyph is shaped when
            // the view opens: on Windows the first ✓ needs a font fallback
            // that took 22 ms, over the 16 ms input budget, inside `r`.
            let check = if done { "✓" } else { " " };
            format!("{mark} {check} {}", findings::label(item, map))
        })
        .collect();
    root.child(
        uniform_list("findings", rows.len(), move |range, _, _| {
            range
                .map(|i| div().h(px(24.0)).child(rows[i].clone()))
                .collect()
        })
        .track_scroll(scroll)
        .flex_1(),
    )
}

/// The outline tree (#117): one indented row per node shown, with its
/// change, whether it is reviewed and its edges to its siblings, in words.
fn outline_view<E: ParentElement>(
    root: E,
    o: &Outline,
    reviewed: Option<&ReviewState>,
    scroll: &UniformListScrollHandle,
) -> E {
    let rows: Vec<String> = o
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let cursor = if i == o.cursor { "›" } else { " " };
            let fold = match (row.node.has_children, row.expanded) {
                (false, _) => " ",
                (true, false) => "▸",
                (true, true) => "▾",
            };
            let done = reviewed.is_some_and(|r| r.is_reviewed(&row.node.id));
            let check = if done { " ✓ reviewed" } else { "" };
            format!(
                "{cursor} {indent}{fold} {name} — {status}{check}; depends on {out}, used by {into}",
                indent = "    ".repeat(row.depth),
                name = row.node.name,
                status = outline::status_word(row.node.status),
                out = row.out,
                into = row.into,
            )
        })
        .collect();
    root.child(
        uniform_list("outline", rows.len(), move |range, _, _| {
            range
                .map(|i| div().h(px(22.0)).whitespace_nowrap().child(rows[i].clone()))
                .collect()
        })
        .track_scroll(scroll)
        .flex_1(),
    )
}

/// The code view key an action stands for.
fn code_key(id: &str) -> Option<&'static str> {
    Some(match id {
        "code.up" => "up",
        "code.down" => "down",
        "code.page_up" => "pageup",
        "code.page_down" => "pagedown",
        "code.first" => "home",
        "code.last" => "end",
        "code.next_change" => "n",
        "code.previous_change" => "p",
        "code.scroll_left" => "left",
        "code.scroll_right" => "right",
        _ => return None,
    })
}

/// What a map action does.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MapOp {
    DrillDown,
    DrillUp,
    Code,
    Next,
    /// Select the nearest node in this direction.
    Towards(f32, f32),
    /// Pan one step in this direction.
    Pan(f32, f32),
    /// Zoom this many steps in (or out, negative).
    Zoom(i32),
    /// Back to the selection, unscaled.
    ResetCamera,
}

fn map_op(id: &str) -> Option<MapOp> {
    Some(match id {
        "map.drill_down" => MapOp::DrillDown,
        "map.drill_up" => MapOp::DrillUp,
        "map.code" => MapOp::Code,
        "map.next" => MapOp::Next,
        "map.left" => MapOp::Towards(-1.0, 0.0),
        "map.right" => MapOp::Towards(1.0, 0.0),
        "map.up" => MapOp::Towards(0.0, -1.0),
        "map.down" => MapOp::Towards(0.0, 1.0),
        "map.pan_left" => MapOp::Pan(-1.0, 0.0),
        "map.pan_right" => MapOp::Pan(1.0, 0.0),
        "map.pan_up" => MapOp::Pan(0.0, -1.0),
        "map.pan_down" => MapOp::Pan(0.0, 1.0),
        "map.zoom_in" => MapOp::Zoom(1),
        "map.zoom_out" => MapOp::Zoom(-1),
        "map.zoom_reset" => MapOp::ResetCamera,
        _ => return None,
    })
}

/// The command palette: what was typed, then the matching actions with
/// their keys, the highlighted one marked.
fn palette_view(p: &Palette, theme: Theme) -> gpui::Div {
    const SHOWN: usize = 12;
    let start = p.cursor.saturating_sub(SHOWN - 1);
    let mut list = div()
        .flex()
        .flex_col()
        .p_2()
        .mb_2()
        .border_1()
        .border_color(rgb(theme.muted))
        .child(format!("› {}▏", p.query));
    if p.matches.is_empty() {
        list = list.child(
            div()
                .text_color(rgb(theme.muted))
                .child("No matching action"),
        );
    }
    for (i, a) in p.matches.iter().enumerate().skip(start).take(SHOWN) {
        let mark = if i == p.cursor { "›" } else { " " };
        list = list.child(
            div()
                .flex()
                .justify_between()
                .child(format!("{mark} {}", a.title))
                .child(div().text_color(rgb(theme.muted)).child(a.keys.join("  "))),
        );
    }
    list.child(
        div()
            .text_color(rgb(theme.muted))
            .child("Type to search, ↑↓ choose, Enter run, Esc close"),
    )
}

/// A keystroke's name as the action registry spells it: `ctrl-`, `alt-`
/// and `cmd-` first, then `shift-` only alongside one of them (a shifted
/// letter alone is the letter), then the key.
fn key_name(k: &gpui::Keystroke) -> String {
    let m = &k.modifiers;
    let mut name = String::new();
    if m.control {
        name.push_str("ctrl-");
    }
    if m.alt {
        name.push_str("alt-");
    }
    if m.platform {
        name.push_str("cmd-");
    }
    if m.shift && (m.control || m.alt || m.platform) {
        name.push_str("shift-");
    }
    name.push_str(&k.key);
    name
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
fn canvas(
    map: &Map,
    reviewed: Option<&ReviewState>,
    l: &Layout,
    camera: Camera,
    viewport: (f32, f32),
    dark: bool,
) -> gpui::Div {
    let m = Metrics::default();
    let (vw, vh) = (viewport.0, (viewport.1 - 120.0).max(100.0));
    let centre = map
        .selected
        .and_then(|i| l.nodes.get(i))
        .map_or((l.width / 2.0, l.height / 2.0), |p| {
            (p.x + m.width / 2.0, p.y + m.height / 2.0)
        });
    let z = camera.zoom;
    let at = |x: f32, y: f32| camera.to_screen((x, y), centre, (vw, vh));
    let visible = |x: f32, y: f32, w: f32, h: f32| {
        let (sx, sy) = at(x, y);
        sx + w * z >= 0.0 && sx <= vw && sy + h * z >= 0.0 && sy <= vh
    };
    let mut c = div().relative().w(px(vw)).h(px(vh)).overflow_hidden();
    for (edge, points) in map.view.edges.iter().zip(&l.edges) {
        let colour = edge_colour(edge.status, dark);
        for seg in points.windows(2) {
            let (p, q) = (seg[0], seg[1]);
            let (x, y) = (p.x.min(q.x), p.y.min(q.y));
            let (w, h) = ((p.x - q.x).abs().max(2.0), (p.y - q.y).abs().max(2.0));
            if visible(x, y, w, h) {
                let (sx, sy) = at(x, y);
                // Lines keep their 2 px weight at any zoom.
                let (sw, sh) = (
                    if w > 2.0 { w * z } else { 2.0 },
                    if h > 2.0 { h * z } else { 2.0 },
                );
                c = c.child(
                    div()
                        .absolute()
                        .left(px(sx - 1.0))
                        .top(px(sy - 1.0))
                        .w(px(sw))
                        .h(px(sh))
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
        // Reviewed nodes (#115) carry a check: a mark, not only a colour.
        let done = reviewed.is_some_and(|r| r.is_reviewed(&node.id));
        let mut label = if done {
            format!("✓ {}", node.name)
        } else {
            node.name.clone()
        };
        if node.has_children {
            label.push_str(" ›");
        }
        let (sx, sy) = at(p.x, p.y);
        c = c.child(
            div()
                .absolute()
                .left(px(sx))
                .top(px(sy))
                .w(px(m.width * z))
                .h(px(m.height * z))
                .px(px(8.0 * z))
                .overflow_hidden()
                .bg(rgb(fill(node.status, dark)))
                .border_2()
                .border_color(rgb(border))
                .rounded_md()
                .text_size(px(14.0 * z))
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
fn side(
    text: &FileText,
    line: Option<u32>,
    tint: Option<u32>,
    column: usize,
    dark: bool,
) -> gpui::Div {
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
    let full = text.lines.get(i).map(String::as_str).unwrap_or_default();
    // Scrolled sideways by whole characters.
    let skip = full
        .char_indices()
        .nth(column)
        .map_or(full.len(), |(b, _)| b);
    let body = full[skip..].to_owned();
    let spans: Vec<(std::ops::Range<usize>, HighlightStyle)> = text
        .highlights
        .get(i)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let (a, b) = (
                (s.start as usize).max(skip) - skip,
                (s.end as usize).saturating_sub(skip),
            );
            (b > a && body.get(a..b).is_some()).then_some((a, b, s.class))
        })
        .map(|(a, b, class)| {
            (
                a..b,
                HighlightStyle {
                    color: Some(rgb(token_colour(class, dark)).into()),
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
        "{} — {} ({state}). ↑↓ PgUp PgDn Home End move, ←→ scroll sideways, n p next or previous change, r mark ✓ reviewed, Backspace back to the map.",
        code.path, code.title
    ));
    let Some(diff) = code.diff.clone() else {
        return div().flex().flex_col().flex_1().child(header);
    };
    let cursor = code.cursor;
    let column = code.column;
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
                    .child(side(&diff.base, r.base, left, column, dark))
                    .child(side(&diff.head, r.head, right, column, dark));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_action_is_handled() {
        for a in actions::ACTIONS {
            let handled = match a.scope {
                Scope::List => review_list::Action::from_id(a.id).is_some(),
                Scope::Code => a.id == "code.close" || code_key(a.id).is_some(),
                Scope::Map => {
                    matches!(a.id, "map.findings" | "map.outline") || map_op(a.id).is_some()
                }
                Scope::Outline => a.id.starts_with("outline."),
                Scope::Findings => matches!(
                    a.id,
                    "findings.up"
                        | "findings.down"
                        | "findings.first"
                        | "findings.last"
                        | "findings.show"
                        | "findings.map"
                ),
                Scope::Review => {
                    matches!(
                        a.id,
                        "review.back" | "review.reviewed" | "review.note" | "review.export"
                    )
                }
                Scope::Global => matches!(a.id, "global.palette" | "global.open"),
            };
            assert!(handled, "{} has no handler", a.id);
        }
    }
}
