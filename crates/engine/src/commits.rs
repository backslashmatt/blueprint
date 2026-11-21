//! Models and diffs of commits, through the index: what `bp model --rev`,
//! `bp index` and `bp diff` do, and what the app's engine service does
//! (#89). Front ends decide how to report; nothing here prints.

use std::path::Path;

use blueprint_model::lang::FileFacts;
use blueprint_model::{Model, RepoPath};

use crate::diff::{ChangedFile, Diff};
use crate::git::{ObjectId, Repo};
use crate::index::{Index, Indexed};
use crate::l1::Diagnostic;
use crate::source::Commit;

/// The index of the repository at `path` for reading, if there is one, and
/// warnings about it. An index that cannot be opened is a warning: the
/// caller analyses instead.
#[must_use]
pub fn existing_index(path: &Path) -> (Option<Index>, Vec<String>) {
    match Index::open_existing(path) {
        Ok(index) => {
            let warnings = dropped(index.as_ref());
            (index, warnings)
        }
        Err(e) => (
            None,
            vec![format!("cannot open the index, analysing instead: {e}")],
        ),
    }
}

fn dropped(index: Option<&Index>) -> Vec<String> {
    index
        .iter()
        .flat_map(|i| i.dropped())
        .map(|d| format!("index segment dropped and will be rebuilt: {d}"))
        .collect()
}

/// The L1–L2 model of a commit, and where it came from.
#[derive(Debug)]
pub struct AtRev {
    /// The commit.
    pub commit: ObjectId,
    /// Its model and diagnostics.
    pub indexed: Indexed,
    /// Whether it was read from the index rather than analysed.
    pub from_index: bool,
    /// Whether it was analysed and then written to the index.
    pub stored: bool,
    /// Problems with the index that did not stop the work.
    pub warnings: Vec<String>,
}

/// The L1–L2 model of `rev` in the repository at `path`: from the index if
/// it holds the commit's tree, otherwise analysed, and then stored if
/// `store` is set.
///
/// # Errors
/// If the repository or commit cannot be read, or `store` is set and the
/// index cannot be opened or written.
pub fn l2_at(path: &Path, rev: &str, store: bool) -> Result<AtRev, String> {
    let repo = Repo::open(path).map_err(|e| e.to_string())?;
    let commit = repo.resolve(rev).map_err(|e| e.to_string())?;
    let tree = repo.tree_id(commit).map_err(|e| e.to_string())?;
    let (index, warnings) = if store {
        let index = Index::open(path).map_err(|e| format!("cannot open the index: {e}"))?;
        let warnings = dropped(Some(&index));
        (Some(index), warnings)
    } else {
        existing_index(path)
    };
    if let Some(indexed) = index.as_ref().and_then(|i| i.get(tree)) {
        return Ok(AtRev {
            commit,
            indexed,
            from_index: true,
            stored: false,
            warnings,
        });
    }
    let (source, mut diags) = Commit::open(path, &commit.to_string()).map_err(|e| e.to_string())?;
    let (model, more) = crate::analyse(&source.paths(), &|p| source.read(p));
    diags.extend(more);
    // A model built without some of its files describes less than the commit,
    // so it is reported and never stored under the commit's tree.
    let missing = unreadable(&source);
    let complete = missing.is_empty();
    diags.extend(missing);
    diags.sort();
    let indexed = Indexed::new(model, &diags);
    let mut stored = false;
    if store
        && complete
        && let Some(mut index) = index
    {
        stored = index
            .put(tree, &indexed)
            .map_err(|e| format!("cannot write the index: {e}"))?;
    }
    Ok(AtRev {
        commit,
        indexed,
        from_index: false,
        stored,
        warnings,
    })
}

/// The L1–L4 model of `rev` and its diagnostics: the one stored in `index`
/// for the commit's tree if there is one (DR 0009), otherwise built, with
/// each Rust file's facts from the index where it has them.
///
/// # Errors
/// If the repository or commit cannot be read.
pub fn l4_at(
    path: &Path,
    rev: &str,
    index: Option<&Index>,
) -> Result<(Model, Vec<Diagnostic>), String> {
    let repo = Repo::open(path).map_err(|e| e.to_string())?;
    let commit = repo.resolve(rev).map_err(|e| e.to_string())?;
    let tree = repo.tree_id(commit).map_err(|e| e.to_string())?;
    if let Some(stored) = index.and_then(|i| i.get_l4(tree)) {
        return Ok(stored);
    }
    let (source, mut diags) = Commit::open(path, &commit.to_string()).map_err(|e| e.to_string())?;
    let deep = deep_at(&source, index);
    diags.extend(deep.diags);
    diags.extend(unreadable(&source));
    diags.sort();
    Ok((deep.model, diags))
}

/// What [`deep_at`] built.
#[derive(Debug)]
pub struct Deep {
    /// The L1–L4 model.
    pub model: Model,
    /// Its diagnostics, every level's.
    pub diags: Vec<Diagnostic>,
    /// The facts of every file that had to be parsed, by blob, with the
    /// name of the analyser that read it.
    pub parsed: Vec<(ObjectId, &'static str, FileFacts)>,
    /// How many files' facts came from the index.
    pub reused: usize,
}

/// The L1–L4 model of `source`, with each analysed file's facts taken from
/// `index` when it has them for the file's blob.
#[must_use]
pub fn deep_at(source: &Commit, index: Option<&Index>) -> Deep {
    let blobs = source.blob_ids();
    let blob = |p: &RepoPath| {
        blobs
            .binary_search_by(|(q, _)| q.cmp(p))
            .ok()
            .map(|i| blobs[i].1)
    };
    let reused = std::sync::atomic::AtomicUsize::new(0);
    let cached = |p: &RepoPath| {
        let analyser = crate::lang::analyser_for(p)?;
        let facts = index?.get_facts(analyser, blob(p)?)?;
        reused.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(facts)
    };
    let mut parsed = Vec::new();
    let (model, diags) = crate::analyse_deep_cached(
        &source.paths(),
        &|p| source.read(p),
        &cached,
        &mut |p, facts| {
            if let (Some(b), Some(a)) = (blob(p), crate::lang::analyser_for(p)) {
                parsed.push((b, a.name(), facts.clone()));
            }
        },
    );
    Deep {
        model,
        diags,
        parsed,
        reused: reused.into_inner(),
    }
}

/// A warning for each file of `source` whose object could not be read.
#[must_use]
pub fn unreadable(source: &Commit) -> Vec<Diagnostic> {
    source
        .unreadable()
        .into_iter()
        .map(|path| Diagnostic {
            path,
            message: "the file's object could not be read (a partial clone?)".into(),
        })
        .collect()
}

/// What [`index_l4`] did.
#[derive(Debug)]
pub struct IndexedL4 {
    /// Files parsed.
    pub parsed: usize,
    /// Files whose facts came from the index.
    pub reused: usize,
    /// The L3–L4 analysis diagnostics, which `bp index` reports.
    pub diags: Vec<Diagnostic>,
    /// The L1–L4 model that was stored.
    pub model: Model,
    /// All of its diagnostics, sorted, as stored with it.
    pub model_diags: Vec<Diagnostic>,
}

/// Builds the L1–L4 model of `rev`, reusing the facts the index holds, and
/// stores the facts of every file it had to parse, then the whole model
/// (DR 0009).
///
/// # Errors
/// If the commit or the index cannot be opened, a file cannot be read, or
/// nothing that was parsed could be stored (object ids that are not SHA-1).
pub fn index_l4(path: &Path, rev: &str) -> Result<IndexedL4, String> {
    let (source, mut diags) = Commit::open(path, rev).map_err(|e| e.to_string())?;
    let mut index = Index::open(path)?;
    let deep = deep_at(&source, Some(&index));
    let level_diags = deep.diags.clone();
    diags.extend(deep.diags.iter().cloned());
    diags.sort();
    if !source.unreadable().is_empty() {
        return Err(format!(
            "{rev}: some files could not be read, so not every file's facts are stored"
        ));
    }
    let mut stored = 0;
    for analyser in crate::lang::ANALYSERS {
        let mine = deep.parsed.iter().filter(|(_, a, _)| *a == analyser.name());
        stored += index.put_facts(analyser, mine.map(|(b, _, f)| (*b, f)))?;
    }
    if stored == 0 && !deep.parsed.is_empty() {
        return Err(format!(
            "{rev}: facts not stored: its object ids are not SHA-1"
        ));
    }
    let repo = Repo::open(path).map_err(|e| e.to_string())?;
    let tree = repo
        .resolve(rev)
        .and_then(|c| repo.tree_id(c))
        .map_err(|e| e.to_string())?;
    if index.get_l4(tree).is_none() && !index.put_l4(tree, &deep.model, &diags)? {
        return Err(format!(
            "{rev}: model not stored: its object ids are not SHA-1"
        ));
    }
    Ok(IndexedL4 {
        parsed: deep.parsed.len(),
        reused: deep.reused,
        diags: level_diags,
        model: deep.model,
        model_diags: diags,
    })
}

/// Each file whose blob differs between two commits, with its bytes in each
/// (`None` where it does not exist).
pub type Changed = Vec<(RepoPath, Option<Vec<u8>>, Option<Vec<u8>>)>;

/// The files whose blobs differ between `base` and `head`.
///
/// # Errors
/// If the repository, either commit or a tree cannot be read.
pub fn changed_files(path: &Path, base: &str, head: &str) -> Result<Changed, String> {
    let repo = Repo::open(path).map_err(|e| e.to_string())?;
    let (b, h) = (
        repo.resolve(base).map_err(|e| e.to_string())?,
        repo.resolve(head).map_err(|e| e.to_string())?,
    );
    let read = |id: Option<ObjectId>| id.and_then(|id| repo.blob(id).ok());
    let mut out = Vec::new();
    for (p, bid, hid) in repo.changed_blobs(b, h).map_err(|e| e.to_string())? {
        // Paths that are not valid repository paths are not in either model.
        let Some(p) = std::str::from_utf8(&p)
            .ok()
            .and_then(|p| RepoPath::parse(p).ok())
        else {
            continue;
        };
        out.push((p, read(bid), read(hid)));
    }
    Ok(out)
}

/// The architectural diff of two commits, and the models it came from.
#[derive(Debug)]
pub struct CommitDiff {
    /// The diff, weighed unless `weight_error` says why not.
    pub diff: Diff,
    /// The base commit's L1–L4 model and diagnostics.
    pub base: (Model, Vec<Diagnostic>),
    /// The head commit's.
    pub head: (Model, Vec<Diagnostic>),
    /// Why change weights were not computed, if they were not.
    pub weight_error: Option<String>,
}

/// A model and its diagnostics, or why it could not be had.
type Built = Result<(Model, Vec<Diagnostic>), String>;

/// The L1–L4 diff of `base` and `head`, with change weights. Both models
/// are built at once, since decoding stored ones is most of the time, and
/// the changed files are listed on a third thread.
///
/// # Errors
/// If either model cannot be had.
pub fn diff_commits(
    path: &Path,
    base: &str,
    head: &str,
    index: Option<&Index>,
) -> Result<CommitDiff, String> {
    let (built, changed): (Vec<Built>, _) = std::thread::scope(|s| {
        let changed = s.spawn(move || changed_files(path, base, head));
        let jobs: Vec<_> = [base, head]
            .into_iter()
            .map(|rev| s.spawn(move || l4_at(path, rev, index)))
            .collect();
        let built = jobs
            .into_iter()
            .map(|j| j.join().unwrap_or_else(|_| Err("analysis failed".into())))
            .collect();
        let changed = changed
            .join()
            .unwrap_or_else(|_| Err("listing changed files failed".into()));
        (built, changed)
    });
    let mut built = built.into_iter();
    let (Some(base_model), Some(head_model)) = (built.next(), built.next()) else {
        return Err("analysis failed".into());
    };
    let (base_model, head_model) = (base_model?, head_model?);
    let mut diff = crate::diff::diff(&base_model.0, &head_model.0);
    let weight_error = match changed {
        Ok(files) => {
            let files: Vec<ChangedFile<'_>> = files
                .iter()
                .map(|(p, b, h)| ChangedFile {
                    path: p,
                    base: b.as_deref(),
                    head: h.as_deref(),
                })
                .collect();
            crate::diff::weigh(&mut diff, &base_model.0, &head_model.0, &files);
            None
        }
        Err(e) => Some(e),
    };
    Ok(CommitDiff {
        diff,
        base: base_model,
        head: head_model,
        weight_error,
    })
}
