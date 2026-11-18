//! The engine service: answers [`protocol`](blueprint_protocol) requests on
//! its own threads, so a front end never does I/O, parsing or diffing on its
//! UI thread (REQUIREMENTS §4, #89).
//!
//! Each [`Lane`] has one worker thread. A request supersedes every earlier
//! request in its lane, and [`Request::Open`] supersedes every earlier
//! request in every lane, since they were about another repository. A
//! superseded request still queued is answered [`Response::Superseded`]
//! without running; one already running stops at its next stage boundary,
//! and a stage's answer is never sent once its request is superseded. A
//! stage itself is not interrupted. Every request gets exactly one final
//! reply ([`Response::is_last`]); a panic while answering one becomes
//! [`Response::Failed`].
//!
//! Models and diffs go through the index: the L1–L2 model of a commit is
//! stored the first time it is built, as by `bp index`, and the L1–L4 model
//! as by `bp index --level 4`, so the next request for the same commit only
//! reads it.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use blueprint_model::{Model, RepoPath};
use blueprint_protocol::{
    CommitSummary, Detail, Lane, Ref, RefKind, Reply, Request, RequestId, Response,
};

use crate::commits;
use crate::diff::ChangedFile;
use crate::git::{CommitInfo, ObjectId, Repo};
use crate::l1::Diagnostic;

const LANES: [Lane; 4] = [Lane::Repository, Lane::Model, Lane::Diff, Lane::Source];

/// Returned by a stage whose request was superseded; never shown.
const SUPERSEDED: &str = "superseded";

/// A request on its way to a worker: the lane generation it was sent in,
/// and the repository that was open when it was sent.
struct Job {
    id: RequestId,
    request: Request,
    generation: u64,
    repo: Option<PathBuf>,
}

/// What every worker shares.
struct Shared {
    /// Per lane, the generation of the newest request sent to it. A reply
    /// is sent only while holding the lane's lock and only if its request
    /// is still the newest, so a superseded request never answers after
    /// the request that superseded it was sent.
    generations: HashMap<Lane, Mutex<u64>>,
    replies: Sender<Reply>,
}

impl Shared {
    fn lane(&self, lane: Lane) -> MutexGuard<'_, u64> {
        // A poisoned lock still holds a valid counter.
        self.generations[&lane]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn send(&self, id: RequestId, response: Response) {
        // A front end that has gone away has no use for replies.
        let _ = self.replies.send(Reply { id, response });
    }

    /// Sends `response` if the request is still the newest in its lane.
    fn emit(&self, lane: Lane, job: &Job, response: Response) -> bool {
        let current = self.lane(lane);
        if *current != job.generation {
            return false;
        }
        self.send(job.id, response);
        true
    }
}

/// A running engine service. Dropping it supersedes everything queued and
/// returns at once; a stage already running finishes on its own thread and
/// its answer is dropped.
pub struct Service {
    shared: Arc<Shared>,
    lanes: HashMap<Lane, Sender<Job>>,
    /// The repository the last [`Request::Open`] named, as later requests
    /// see it.
    repo: Mutex<Option<PathBuf>>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

impl Service {
    /// Starts the workers. Replies arrive on the returned receiver, in the
    /// order they are produced; replies in one lane keep request order.
    ///
    /// # Errors
    /// If a worker thread cannot be started.
    pub fn start() -> std::io::Result<(Service, Receiver<Reply>)> {
        let (replies, received) = mpsc::channel();
        let shared = Arc::new(Shared {
            generations: LANES.iter().map(|l| (*l, Mutex::new(0))).collect(),
            replies,
        });
        let mut lanes = HashMap::new();
        for lane in LANES {
            let (tx, rx) = mpsc::channel::<Job>();
            let shared = Arc::clone(&shared);
            // Detached: it ends when the service, and so the sender, is dropped.
            std::thread::Builder::new()
                .name(format!("engine-{lane:?}").to_lowercase())
                .spawn(move || work(&shared, lane, &rx))?;
            lanes.insert(lane, tx);
        }
        Ok((
            Service {
                shared,
                lanes,
                repo: Mutex::new(None),
            },
            received,
        ))
    }

    /// Queues `request`. Its answer, and a [`Response::Superseded`] for any
    /// request it replaces, arrive on the reply receiver. A request is
    /// answered about the repository named by the last [`Request::Open`]
    /// sent before it, even if that Open has not finished yet.
    pub fn send(&self, id: RequestId, request: Request) {
        let lane = request.lane();
        let repo = {
            let mut repo = self.repo.lock().unwrap_or_else(PoisonError::into_inner);
            if let Request::Open { path } = &request {
                *repo = Some(PathBuf::from(path));
                for l in LANES {
                    *self.shared.lane(l) += 1;
                }
            }
            repo.clone()
        };
        let generation = {
            let mut g = self.shared.lane(lane);
            *g += 1;
            *g
        };
        let job = Job {
            id,
            request,
            generation,
            repo,
        };
        if self.lanes[&lane].send(job).is_err() {
            self.shared.send(
                id,
                Response::Failed {
                    error: "the engine failed".into(),
                },
            );
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        for l in LANES {
            *self.shared.lane(l) += 1;
        }
    }
}

/// One lane's worker: runs each job that is still the newest, answers the
/// rest superseded, until the service is dropped. Every request gets
/// exactly one final reply.
fn work(shared: &Shared, lane: Lane, rx: &Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        let emit = |r: Response| shared.emit(lane, &job, r);
        let stale = || *shared.lane(lane) != job.generation;
        let outcome = if stale() {
            Err(SUPERSEDED.to_string())
        } else {
            // An analyser panic on hostile input fails this request, not
            // the lane.
            std::panic::catch_unwind(AssertUnwindSafe(|| run(&job, &stale, &emit)))
                .unwrap_or_else(|_| Err("the engine failed on this input".into()))
        };
        if let Err(error) = outcome
            && (error == SUPERSEDED || !emit(Response::Failed { error }))
        {
            shared.send(job.id, Response::Superseded);
        }
    }
}

/// Fails with [`SUPERSEDED`] unless `emit` sent the response.
fn sent(delivered: bool) -> Result<(), String> {
    if delivered {
        Ok(())
    } else {
        Err(SUPERSEDED.into())
    }
}

/// Answers one request through `emit`, stopping between stages once
/// `stale`.
fn run(job: &Job, stale: &dyn Fn() -> bool, emit: &dyn Fn(Response) -> bool) -> Result<(), String> {
    let repo = || {
        job.repo
            .clone()
            .ok_or_else(|| "no repository is open".to_string())
    };
    let check = || {
        if stale() {
            Err(SUPERSEDED.to_string())
        } else {
            Ok(())
        }
    };
    match &job.request {
        Request::Open { path } => {
            let path = PathBuf::from(path);
            let repo = Repo::open(&path).map_err(|e| e.to_string())?;
            let head = repo
                .head()
                .map(|c| repo.commit_info(c).map(summary))
                .transpose()
                .map_err(|e| e.to_string())?;
            sent(emit(Response::Opened {
                path: path.display().to_string(),
                head,
            }))
        }
        Request::Refs { limit } => {
            let repo = Repo::open(&repo()?).map_err(|e| e.to_string())?;
            let refs = repo
                .refs()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|r| Ref {
                    name: r.name,
                    kind: match r.kind {
                        crate::git::RefKind::Branch => RefKind::Branch,
                        crate::git::RefKind::RemoteBranch => RefKind::RemoteBranch,
                        crate::git::RefKind::Tag => RefKind::Tag,
                        crate::git::RefKind::PullRequest => RefKind::PullRequest,
                    },
                    commit: r.commit.to_string(),
                })
                .collect();
            let commits = match repo.head() {
                Some(head) => repo
                    .log(head, *limit)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(summary)
                    .collect(),
                None => Vec::new(),
            };
            sent(emit(Response::Refs {
                refs,
                commits,
                current: repo.head_branch(),
                suggested: repo
                    .suggested_review()
                    .map(|(b, h)| (b.to_string(), h.to_string())),
            }))
        }
        Request::Model { rev } => {
            let path = repo()?;
            let coarse = coarse_model(&path, rev)?;
            let commit = coarse.commit.to_string();
            let warnings = text(&coarse.indexed.diagnostics());
            sent(emit(Response::Model {
                commit: commit.clone(),
                detail: Detail::Coarse,
                model: Arc::new(coarse.indexed.model),
                warnings,
            }))?;
            check()?;
            let (model, diags) = full_model(&path, &commit)?;
            sent(emit(Response::Model {
                commit,
                detail: Detail::Full,
                model: Arc::new(model),
                warnings: text(&diags),
            }))
        }
        Request::Diff { base, head } => {
            let path = repo()?;
            let b = coarse_model(&path, base)?;
            check()?;
            let h = coarse_model(&path, head)?;
            let (base, head) = (b.commit.to_string(), h.commit.to_string());
            let mut warnings = text(&b.indexed.diagnostics());
            warnings.extend(text(&h.indexed.diagnostics()));
            let diff = crate::diff::diff(&b.indexed.model, &h.indexed.model);
            sent(emit(Response::Diff {
                base: base.clone(),
                head: head.clone(),
                detail: Detail::Coarse,
                diff: Arc::new(diff),
                base_model: Arc::new(b.indexed.model),
                head_model: Arc::new(h.indexed.model),
                warnings,
            }))?;
            check()?;
            // Both models at once, and the changed files for weights.
            let (bf, hf, changed) = std::thread::scope(|s| {
                let b = s.spawn(|| full_model(&path, &base));
                let c = s.spawn(|| commits::changed_files(&path, &base, &head));
                let h = full_model(&path, &head);
                const FAILED: &str = "the engine failed on this input";
                (
                    b.join().unwrap_or_else(|_| Err(FAILED.into())),
                    h,
                    c.join().unwrap_or_else(|_| Err(FAILED.into())),
                )
            });
            let ((base_model, base_diags), (head_model, head_diags)) = (bf?, hf?);
            check()?;
            let mut diff = crate::diff::diff(&base_model, &head_model);
            let mut warnings = text(&base_diags);
            warnings.extend(text(&head_diags));
            match changed {
                Ok(files) => {
                    let files: Vec<ChangedFile<'_>> = files
                        .iter()
                        .map(|(p, b, h)| ChangedFile {
                            path: p,
                            base: b.as_deref(),
                            head: h.as_deref(),
                        })
                        .collect();
                    crate::diff::weigh(&mut diff, &base_model, &head_model, &files);
                }
                Err(e) => warnings.push(format!("change weight not computed: {e}")),
            }
            sent(emit(Response::Diff {
                base,
                head,
                detail: Detail::Full,
                diff: Arc::new(diff),
                base_model: Arc::new(base_model),
                head_model: Arc::new(head_model),
                warnings,
            }))
        }
        Request::Source {
            rev,
            path: file,
            first,
            last,
        } => {
            let path = repo()?;
            let file = RepoPath::parse(file).map_err(|e| format!("{file}: {e}"))?;
            sent(emit(Response::Source {
                text: source_lines(&path, rev, &file, *first, *last)?,
            }))
        }
    }
}

/// The L1–L2 model of `rev`, stored in the index as by `bp index`; if the
/// index cannot be opened or written, analysed without it, as `bp model`
/// does.
fn coarse_model(path: &Path, rev: &str) -> Result<commits::AtRev, String> {
    commits::l2_at(path, rev, true).or_else(|_| commits::l2_at(path, rev, false))
}

/// The L1–L4 model of `commit`: read from the index, or built and stored
/// there (`bp index --level 4`). A commit that cannot be stored (unreadable
/// files, object ids that are not SHA-1) is built without storing.
fn full_model(path: &Path, commit: &str) -> Result<(Model, Vec<Diagnostic>), String> {
    let (index, _) = commits::existing_index(path);
    let repo = Repo::open(path).map_err(|e| e.to_string())?;
    let tree = repo
        .resolve(commit)
        .and_then(|c| repo.tree_id(c))
        .map_err(|e| e.to_string())?;
    if let Some(stored) = index.as_ref().and_then(|i| i.get_l4(tree)) {
        return Ok(stored);
    }
    match commits::index_l4(path, commit) {
        Ok(done) => Ok((done.model, done.model_diags)),
        // Built without storing, with whatever facts the index holds.
        Err(_) => commits::l4_at(path, commit, index.as_ref()),
    }
}

/// Lines `first..=last` (from 1) of `file` at `rev`.
fn source_lines(
    path: &Path,
    rev: &str,
    file: &RepoPath,
    first: u32,
    last: u32,
) -> Result<String, String> {
    let repo = Repo::open(path).map_err(|e| e.to_string())?;
    let commit = repo.resolve(rev).map_err(|e| e.to_string())?;
    let entry = repo
        .tree(commit)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|e| e.path_str() == Some(&file.to_string()))
        .ok_or_else(|| format!("{file}: not in {rev}"))?;
    let bytes = repo.blob(entry.id).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let first = first.max(1) as usize;
    let take = (last as usize + 1).saturating_sub(first);
    Ok(text
        .lines()
        .skip(first - 1)
        .take(take)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn summary(c: CommitInfo) -> CommitSummary {
    CommitSummary {
        id: c.id.to_string(),
        summary: c.summary,
        author: c.author,
        time: c.time,
        parents: c.parents.iter().map(ObjectId::to_string).collect(),
    }
}

fn text(diags: &[Diagnostic]) -> Vec<String> {
    diags
        .iter()
        .map(|d| format!("{}: {}", d.path, d.message.lines().next().unwrap_or("")))
        .collect()
}
