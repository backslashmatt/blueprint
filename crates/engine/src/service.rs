//! The engine service: answers [`protocol`](blueprint_protocol) requests on
//! its own threads, so a front end never does I/O, parsing or diffing on its
//! UI thread (REQUIREMENTS §4, #89).
//!
//! Each [`Lane`] has one worker thread. A request supersedes every earlier
//! request in its lane, and [`Request::Open`] supersedes every earlier
//! request in every lane, since they were about another repository. A
//! superseded request still queued is answered [`Response::Superseded`]
//! without running; one already running is checked between stages (after
//! the coarse answer, before the full one) and stops there. A stage itself
//! is not interrupted.
//!
//! Models and diffs go through the index: the L1–L2 model of a commit is
//! stored the first time it is built, as by `bp index`, and the L1–L4 model
//! as by `bp index --level 4`, so the next request for the same commit only
//! reads it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;

use blueprint_model::{Model, RepoPath};
use blueprint_protocol::{
    CommitSummary, Detail, Lane, Ref, RefKind, Reply, Request, RequestId, Response,
};

use crate::commits;
use crate::git::{CommitInfo, ObjectId, Repo};
use crate::l1::Diagnostic;

const LANES: [Lane; 4] = [Lane::Repository, Lane::Model, Lane::Diff, Lane::Source];

/// A request on its way to a worker, with the lane generation it was sent in.
struct Job {
    id: RequestId,
    request: Request,
    generation: u64,
}

/// What every worker shares.
struct Shared {
    /// The open repository, if any.
    repo: RwLock<Option<PathBuf>>,
    /// Per lane, the generation of the newest request sent to it.
    generations: HashMap<Lane, AtomicU64>,
    replies: Sender<Reply>,
}

impl Shared {
    fn current(&self, lane: Lane) -> u64 {
        self.generations[&lane].load(Ordering::SeqCst)
    }

    fn reply(&self, id: RequestId, response: Response) {
        // A front end that has gone away has no use for replies.
        let _ = self.replies.send(Reply { id, response });
    }

    fn repo(&self) -> Result<PathBuf, String> {
        self.repo
            .read()
            .map_err(|_| "the engine failed".to_string())?
            .clone()
            .ok_or_else(|| "no repository is open".to_string())
    }
}

/// A running engine service. Dropping it stops the workers once they finish
/// what they are doing.
pub struct Service {
    shared: Arc<Shared>,
    lanes: HashMap<Lane, Sender<Job>>,
    workers: Vec<JoinHandle<()>>,
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
            repo: RwLock::new(None),
            generations: LANES.iter().map(|l| (*l, AtomicU64::new(0))).collect(),
            replies,
        });
        let mut lanes = HashMap::new();
        let mut workers = Vec::new();
        for lane in LANES {
            let (tx, rx) = mpsc::channel::<Job>();
            let shared = Arc::clone(&shared);
            workers.push(
                std::thread::Builder::new()
                    .name(format!("engine-{lane:?}").to_lowercase())
                    .spawn(move || work(&shared, lane, &rx))?,
            );
            lanes.insert(lane, tx);
        }
        Ok((
            Service {
                shared,
                lanes,
                workers,
            },
            received,
        ))
    }

    /// Queues `request`. Its answer, and a [`Response::Superseded`] for any
    /// request it replaces, arrive on the reply receiver.
    pub fn send(&self, id: RequestId, request: Request) {
        let lane = request.lane();
        if matches!(request, Request::Open { .. }) {
            for l in LANES {
                self.shared.generations[&l].fetch_add(1, Ordering::SeqCst);
            }
        }
        let generation = self.shared.generations[&lane].fetch_add(1, Ordering::SeqCst) + 1;
        let job = Job {
            id,
            request,
            generation,
        };
        if self.lanes[&lane].send(job).is_err() {
            self.shared.reply(
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
        self.lanes.clear();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

/// One lane's worker: runs the newest queued job, answers the rest
/// superseded, until the service is dropped.
fn work(shared: &Shared, lane: Lane, rx: &Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        let stale = || shared.current(lane) != job.generation;
        if stale() {
            shared.reply(job.id, Response::Superseded);
            continue;
        }
        let reply = |r: Response| shared.reply(job.id, r);
        if let Err(error) = run(shared, &job.request, &stale, &reply) {
            reply(if stale() {
                Response::Superseded
            } else {
                Response::Failed { error }
            });
        }
    }
}

/// Answers one request through `reply`, checking `stale` between stages.
fn run(
    shared: &Shared,
    request: &Request,
    stale: &dyn Fn() -> bool,
    reply: &dyn Fn(Response),
) -> Result<(), String> {
    match request {
        Request::Open { path } => {
            let path = PathBuf::from(path);
            let repo = Repo::open(&path).map_err(|e| e.to_string())?;
            let head = repo
                .head()
                .map(|c| repo.commit_info(c).map(summary))
                .transpose()
                .map_err(|e| e.to_string())?;
            *shared
                .repo
                .write()
                .map_err(|_| "the engine failed".to_string())? = Some(path.clone());
            reply(Response::Opened {
                path: path.display().to_string(),
                head,
            });
        }
        Request::Refs { limit } => {
            let repo = Repo::open(&shared.repo()?).map_err(|e| e.to_string())?;
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
            reply(Response::Refs { refs, commits });
        }
        Request::Model { rev } => {
            let path = shared.repo()?;
            let coarse = commits::l2_at(&path, rev, true)?;
            let commit = coarse.commit.to_string();
            let warnings = text(&coarse.indexed.diagnostics());
            reply(Response::Model {
                commit: commit.clone(),
                detail: Detail::Coarse,
                model: Arc::new(coarse.indexed.model),
                warnings,
            });
            if stale() {
                return Err("superseded".into());
            }
            let (model, diags) = full_model(&path, &commit)?;
            reply(Response::Model {
                commit,
                detail: Detail::Full,
                model: Arc::new(model),
                warnings: text(&diags),
            });
        }
        Request::Diff { base, head } => {
            let path = shared.repo()?;
            let (b, h) = (
                commits::l2_at(&path, base, true)?,
                commits::l2_at(&path, head, true)?,
            );
            let (base, head) = (b.commit.to_string(), h.commit.to_string());
            let mut warnings = text(&b.indexed.diagnostics());
            warnings.extend(text(&h.indexed.diagnostics()));
            let diff = crate::diff::diff(&b.indexed.model, &h.indexed.model);
            reply(Response::Diff {
                base: base.clone(),
                head: head.clone(),
                detail: Detail::Coarse,
                diff: Arc::new(diff),
                base_model: Arc::new(b.indexed.model),
                head_model: Arc::new(h.indexed.model),
                warnings,
            });
            if stale() {
                return Err("superseded".into());
            }
            // Both stored at level 4 first, so the diff reads them.
            let (bf, hf) = std::thread::scope(|s| {
                let b = s.spawn(|| full_model(&path, &base));
                let h = full_model(&path, &head);
                (
                    b.join().unwrap_or_else(|_| Err("analysis failed".into())),
                    h,
                )
            });
            let (bf, hf) = (bf?, hf?);
            if stale() {
                return Err("superseded".into());
            }
            let (index, _) = commits::existing_index(&path);
            let d = commits::diff_commits(&path, &base, &head, index.as_ref())?;
            let mut warnings = text(&bf.1);
            warnings.extend(text(&hf.1));
            if let Some(e) = d.weight_error {
                warnings.push(format!("change weight not computed: {e}"));
            }
            reply(Response::Diff {
                base,
                head,
                detail: Detail::Full,
                diff: Arc::new(d.diff),
                base_model: Arc::new(d.base.0),
                head_model: Arc::new(d.head.0),
                warnings,
            });
        }
        Request::Source {
            rev,
            path: file,
            first,
            last,
        } => {
            let path = shared.repo()?;
            let file = RepoPath::parse(file).map_err(|e| format!("{file}: {e}"))?;
            reply(Response::Source {
                text: source_lines(&path, rev, &file, *first, *last)?,
            });
        }
    }
    Ok(())
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
    drop(index);
    match commits::index_l4(path, commit) {
        Ok(done) => Ok((done.model, done.model_diags)),
        Err(_) => commits::l4_at(path, commit, None),
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
