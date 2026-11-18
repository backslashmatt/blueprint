//! The serialisable request/response API between the engine and any front
//! end (REQUIREMENTS §5.3). The UI reaches the engine only through this crate.
//!
//! A front end sends [`Request`]s, each with a [`RequestId`] it chooses, and
//! receives [`Reply`]s carrying the same id. Requests in the same [`Lane`]
//! supersede each other: when a newer one arrives, an older one that has not
//! finished is answered with [`Response::Superseded`] instead (REQUIREMENTS
//! §4: new input cancels stale work). Requests in different lanes run
//! independently. A model or diff is answered twice: first at
//! [`Detail::Coarse`] (L1–L2), then at [`Detail::Full`] (L1–L4), so coarse
//! levels render before fine ones are ready.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Version of the protocol. Bumped on any incompatible change; 1 added the
/// model types (`docs/model-json.md`), 2 the requests and responses.
pub const PROTOCOL_VERSION: u32 = 2;

/// The architecture model types, re-exported so front ends depend on
/// `protocol` only (REQUIREMENTS §5.3).
pub use blueprint_model as model;

use blueprint_model::Model;
use blueprint_model::diff::Diff;

/// Chosen by the front end; echoed in the [`Reply`].
pub type RequestId = u64;

/// Something a front end asks of the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case")]
pub enum Request {
    /// Open the git repository at `path`. Later requests read it.
    Open {
        /// A working tree or a bare repository.
        path: String,
    },
    /// The branches and tags, and up to `limit` recent commits from `HEAD`.
    Refs {
        /// The most commits to list.
        limit: usize,
    },
    /// The model of one commit: coarse first, then full.
    Model {
        /// A branch, tag, commit id or any revision expression.
        rev: String,
    },
    /// The architectural diff of two commits: coarse first, then full.
    Diff {
        /// The base revision.
        base: String,
        /// The head revision.
        head: String,
    },
    /// Lines of one file at one commit, for the code view at L4.
    Source {
        /// The revision.
        rev: String,
        /// The file, relative to the repository root, `/`-separated.
        path: String,
        /// The first line, counted from 1.
        first: u32,
        /// The last line, inclusive.
        last: u32,
    },
}

/// Requests in the same lane supersede each other; requests in different
/// lanes do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    /// [`Request::Open`] and [`Request::Refs`].
    Repository,
    /// [`Request::Model`].
    Model,
    /// [`Request::Diff`].
    Diff,
    /// [`Request::Source`].
    Source,
}

impl Request {
    /// The lane this request runs in.
    #[must_use]
    pub fn lane(&self) -> Lane {
        match self {
            Request::Open { .. } | Request::Refs { .. } => Lane::Repository,
            Request::Model { .. } => Lane::Model,
            Request::Diff { .. } => Lane::Diff,
            Request::Source { .. } => Lane::Source,
        }
    }
}

/// How much of the model an answer covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Detail {
    /// L1–L2: systems and modules. Sent first.
    Coarse,
    /// L1–L4: everything. Sent last.
    Full,
}

/// A branch or tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ref {
    /// The full name, such as `refs/heads/main`.
    pub name: String,
    /// `branch`, `remote-branch`, `tag` or `pull-request`.
    pub kind: RefKind,
    /// The commit it leads to, as hex.
    pub commit: String,
}

/// What kind of ref a [`Ref`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    /// A local branch.
    Branch,
    /// A remote-tracking branch.
    RemoteBranch,
    /// A tag.
    Tag,
    /// A pull request's ref, `refs/pull/…`.
    PullRequest,
}

/// A commit, as a review list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitSummary {
    /// The commit id, as hex.
    pub id: String,
    /// The first line of its message.
    pub summary: String,
    /// The author's name.
    pub author: String,
    /// The commit time, in seconds since the Unix epoch.
    pub time: i64,
    /// Its parents' ids, in order.
    pub parents: Vec<String>,
}

/// The engine's answer to one request.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "response", rename_all = "kebab-case")]
pub enum Response {
    /// The repository is open.
    Opened {
        /// The path it was opened at.
        path: String,
        /// The commit `HEAD` points at; `None` before the first commit.
        head: Option<CommitSummary>,
    },
    /// The answer to [`Request::Refs`].
    Refs {
        /// Branches and tags, sorted by name.
        refs: Vec<Ref>,
        /// Recent commits from `HEAD`, newest first.
        commits: Vec<CommitSummary>,
        /// The branch `HEAD` is on (`refs/heads/…`); `None` when detached.
        current: Option<String>,
        /// The review to start from, `(base, head)` commit ids: `HEAD`
        /// against its merge base with the default branch, or against its
        /// first parent when that merge base is `HEAD` itself.
        suggested: Option<(String, String)>,
    },
    /// A commit's model, at `detail`.
    Model {
        /// The commit id, as hex.
        commit: String,
        /// How much of the model this is.
        detail: Detail,
        /// The model. Shared, as models are large and front ends only read them.
        model: Arc<Model>,
        /// Analysis warnings, one per line, `path: message`.
        warnings: Vec<String>,
    },
    /// The diff of two commits, at `detail`, with both models.
    Diff {
        /// The base commit id, as hex.
        base: String,
        /// The head commit id, as hex.
        head: String,
        /// How much of the models this diff covers.
        detail: Detail,
        /// The diff. Change weights are filled in at [`Detail::Full`] only.
        diff: Arc<Diff>,
        /// The base commit's model.
        base_model: Arc<Model>,
        /// The head commit's model.
        head_model: Arc<Model>,
        /// Analysis warnings for either commit, one per line.
        warnings: Vec<String>,
    },
    /// The lines asked for, joined with `\n`. Fewer lines when the file is
    /// shorter; invalid UTF-8 replaced.
    Source {
        /// The text.
        text: String,
    },
    /// A newer request in the same lane replaced this one before it finished.
    Superseded,
    /// The request could not be answered.
    Failed {
        /// Why, in one line.
        error: String,
    },
}

impl Response {
    /// Whether no further reply will follow for this request: everything
    /// but a coarse model or diff.
    #[must_use]
    pub fn is_last(&self) -> bool {
        !matches!(
            self,
            Response::Model {
                detail: Detail::Coarse,
                ..
            } | Response::Diff {
                detail: Detail::Coarse,
                ..
            }
        )
    }
}

/// One answer, tagged with the request it answers.
#[derive(Debug, Clone, Serialize)]
pub struct Reply {
    /// The request's id.
    pub id: RequestId,
    /// The answer.
    pub response: Response,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_group_what_supersedes_what() {
        let open = Request::Open { path: "r".into() };
        let refs = Request::Refs { limit: 10 };
        assert_eq!(open.lane(), refs.lane());
        let model = Request::Model { rev: "HEAD".into() };
        let diff = Request::Diff {
            base: "a".into(),
            head: "b".into(),
        };
        assert_ne!(model.lane(), diff.lane());
    }

    #[test]
    fn only_coarse_answers_are_followed_by_more() {
        let coarse = Response::Model {
            commit: "c".into(),
            detail: Detail::Coarse,
            model: Arc::default(),
            warnings: vec![],
        };
        assert!(!coarse.is_last());
        assert!(Response::Superseded.is_last());
        assert!(Response::Failed { error: "e".into() }.is_last());
    }

    #[test]
    fn requests_round_trip_as_json() -> Result<(), serde_json::Error> {
        let r = Request::Source {
            rev: "HEAD".into(),
            path: "src/lib.rs".into(),
            first: 1,
            last: 20,
        };
        let json = serde_json::to_string(&r)?;
        assert!(json.contains(r#""request":"source""#), "{json}");
        assert_eq!(serde_json::from_str::<Request>(&json)?, r);
        Ok(())
    }
}
