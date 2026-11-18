//! The window's root view once a repository is opened: the review list,
//! then the review of the chosen commits. It reaches the engine only by
//! sending protocol requests through the function it is given and
//! receiving replies in [`Workspace::receive`] (REQUIREMENTS §5.3).

use std::rc::Rc;

use blueprint_protocol::{Detail, Reply, Request, RequestId, Response};
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    Styled, Window, div, rgb,
};

use crate::Theme;
use crate::review_list::{Effect, ReviewList, action_for_key};

/// Sends one request to the engine.
pub type Send = Rc<dyn Fn(RequestId, Request)>;

/// A review in progress: what the engine has answered so far.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// What the workspace shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Waiting for the repository's refs.
    Opening,
    /// Choosing base and head.
    List(ReviewList),
    /// Reviewing base against head. The map view (#92) replaces the summary.
    Review(Review),
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
                    for key in std::mem::take(&mut self.script) {
                        self.key(&key);
                    }
                }
            }
            (Screen::Review(review), Response::Diff { detail, diff, .. }) => {
                review.detail = Some(*detail);
                review.findings = diff.findings.len();
                review.changed = diff.nodes.len();
            }
            _ => {}
        }
        cx.notify();
    }

    /// Applies one key, by its GPUI name.
    pub fn key(&mut self, key: &str) {
        match &mut self.screen {
            Screen::List(list) => {
                let Some(action) = action_for_key(key) else {
                    return;
                };
                if let Effect::Review { base, head } = list.apply(action) {
                    self.list = Some(list.clone());
                    let id = self.request(Request::Diff {
                        base: base.clone(),
                        head: head.clone(),
                    });
                    self.waiting = Some(id);
                    self.screen = Screen::Review(Review {
                        base,
                        head,
                        detail: None,
                        findings: 0,
                        changed: 0,
                    });
                }
            }
            Screen::Review(_) if key == "escape" => {
                if let Some(list) = self.list.take() {
                    self.waiting = None;
                    self.screen = Screen::List(list);
                }
            }
            _ => {}
        }
    }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(10)]
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        let root = div()
            .id("workspace")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                this.key(&ev.keystroke.key);
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
            Screen::Failed(e) => root.child(format!("Cannot open: {e}")),
            Screen::List(list) => {
                let header = format!(
                    "On {}. ↑↓ move, b base, h head, s swap, Enter review.",
                    list.current.as_deref().unwrap_or("a detached HEAD")
                );
                let mut root = root.child(div().text_color(rgb(theme.muted)).child(header));
                for (i, row) in list.rows.iter().enumerate() {
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
                    root = root.child(format!("{cursor} {:<9} {}", mark, row.label()));
                }
                root
            }
            Screen::Review(r) => {
                let state = match r.detail {
                    None => "loading…",
                    Some(Detail::Coarse) => "systems and modules; code still loading…",
                    Some(Detail::Full) => "complete",
                };
                root.child(format!("Reviewing {} → {}", short(&r.base), short(&r.head)))
                    .child(format!(
                        "{} findings, {} changed nodes ({state})",
                        r.findings, r.changed
                    ))
                    .child(
                        div()
                            .text_color(rgb(theme.muted))
                            .child("Esc: back to the list"),
                    )
            }
        }
    }
}
