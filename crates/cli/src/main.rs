//! `bp`: the headless Blueprint front end. Everything the app can show, this
//! CLI can print (REQUIREMENTS §5.3). See docs/cli.md.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use blueprint_engine::git::{ObjectId, Repo};
use blueprint_engine::index::{Index, Indexed};
use blueprint_engine::l1::Diagnostic;
use blueprint_engine::source::{Commit, Directory};
use blueprint_model::{EdgeKind, Level, Model, RepoPath};

/// Exit code for a command that ran but could not do its job.
const EXIT_FAILURE: u8 = 1;
/// Exit code for a usage error.
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "\
usage: bp model <dir> [--rev <rev>] [--level 1|2|3|4] [--json]
       bp index <repo> [--rev <rev>]
       bp --version
       bp --help

commands:
  model <dir>   Print the architecture model of a directory (L1 systems, L2 modules)

  index <repo>  Analyse a commit (default HEAD) and store its model in the
                index, so that `bp model --rev` reopens it without analysing

options for index:
  --rev <rev>   The commit to index (default HEAD)

options for model:
  --rev <rev>   Read the commit <rev> of the git repository at <dir> instead of
                its working tree (a branch, tag, commit id or HEAD)
  --level N     The deepest level to include, 1 to 4 (default 2)
  --json        Print the model as JSON (docs/model-json.md) instead of text";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    L1,
    L2,
    L3,
    L4,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Version,
    Index {
        path: PathBuf,
        rev: String,
    },
    Help,
    Model {
        path: PathBuf,
        rev: Option<String>,
        level: Depth,
        json: bool,
    },
}

/// Parses the arguments after the program name. Hand-written rather than a
/// parser crate: `bp` start-up is benchmarked (`cli_process_start`), and the
/// surface is small.
fn parse(args: &[String]) -> Result<Command, String> {
    let (first, rest) = match args.split_first() {
        None => return Ok(Command::Help),
        Some(split) => split,
    };
    match first.as_str() {
        "--version" | "-V" if rest.is_empty() => Ok(Command::Version),
        "--help" | "-h" | "help" if rest.is_empty() || first == "help" => Ok(Command::Help),
        "model" => {
            let (mut path, mut rev, mut level, mut json) = (None, None, Depth::L2, false);
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--json" => json = true,
                    "--rev" => match it.next() {
                        Some(r) if !r.is_empty() && !r.starts_with('-') => rev = Some(r.clone()),
                        _ => return Err("--rev needs a revision, such as HEAD".into()),
                    },
                    "--level" => {
                        level = match it.next().map(String::as_str) {
                            Some("1") => Depth::L1,
                            Some("2") => Depth::L2,
                            Some("3") => Depth::L3,
                            Some("4") => Depth::L4,
                            Some(v) => return Err(format!("--level takes 1 to 4, not `{v}`")),
                            None => return Err("--level needs a value: 1 to 4".into()),
                        }
                    }
                    "--help" | "-h" => return Ok(Command::Help),
                    flag if flag.starts_with('-') && flag != "-" => {
                        return Err(format!("unknown option `{flag}`"));
                    }
                    p if path.is_none() => path = Some(PathBuf::from(p)),
                    extra => return Err(format!("unexpected argument `{extra}`")),
                }
            }
            let path = path.ok_or("model needs a directory")?;
            Ok(Command::Model {
                path,
                rev,
                level,
                json,
            })
        }
        "index" => {
            let (mut path, mut rev) = (None, None);
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--rev" => match it.next() {
                        Some(r) if !r.is_empty() && !r.starts_with('-') => rev = Some(r.clone()),
                        _ => return Err("--rev needs a revision, such as HEAD".into()),
                    },
                    "--help" | "-h" => return Ok(Command::Help),
                    flag if flag.starts_with('-') && flag != "-" => {
                        return Err(format!("unknown option `{flag}`"));
                    }
                    p if path.is_none() => path = Some(PathBuf::from(p)),
                    extra => return Err(format!("unexpected argument `{extra}`")),
                }
            }
            let path = path.ok_or("index needs a repository")?;
            Ok(Command::Index {
                path,
                rev: rev.unwrap_or_else(|| "HEAD".into()),
            })
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

/// The nodes up to `depth` and the edges between them.
fn up_to(model: &Model, depth: Depth) -> Model {
    let max = match depth {
        Depth::L1 => Level::System,
        Depth::L2 => Level::Module,
        Depth::L3 => Level::Component,
        Depth::L4 => Level::Code,
    };
    let mut out = Model::default();
    // Nodes come out parents first (ids sort by level), so every parent is
    // already present when its child is added. Errors cannot happen for a
    // subset of a valid model; they would only drop the node.
    for n in model.nodes().filter(|n| n.level() <= max) {
        let _ = out.add_node(n.clone());
    }
    // Edges go in lowest level first, because a roll-up's evidence (one
    // level down) must already be present; evidence below `max` is dropped
    // with the edges it names.
    let mut edges: Vec<_> = model
        .edges()
        .filter(|e| e.key.from.level() <= max)
        .collect();
    edges.sort_by_key(|e| std::cmp::Reverse(e.key.from.level()));
    for e in edges {
        let mut e = e.clone();
        e.evidence.retain(|ev| ev.from.level() <= max);
        let _ = out.add_edge(e);
    }
    out
}

/// An edge kind as the JSON form spells it (docs/model-json.md).
fn edge_kind(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::DependsOn => "depends-on",
        EdgeKind::Calls => "calls",
        EdgeKind::Implements => "implements",
        EdgeKind::Extends => "extends",
        EdgeKind::Reads => "reads",
        EdgeKind::Writes => "writes",
        EdgeKind::Exposes => "exposes",
    }
}

fn text(model: &Model) -> String {
    let mut s = String::new();
    for n in model.nodes() {
        let vis = n
            .visibility
            .map(|v| format!("\t{}", format!("{v:?}").to_lowercase()))
            .unwrap_or_default();
        let conf = format!("{:?}", n.confidence).to_lowercase();
        let kind = format!("{:?}", n.kind).to_lowercase();
        s += &format!(
            "{}\t{kind}\t{}\t{conf}{vis}\n",
            String::from(n.id.clone()),
            n.name
        );
    }
    for e in model.edges() {
        let conf = format!("{:?}", e.confidence).to_lowercase();
        let deps: Vec<String> = e
            .dependency
            .iter()
            .map(|d| format!("{d:?}").to_lowercase())
            .collect();
        let deps = if deps.is_empty() {
            String::new()
        } else {
            format!("\t{}", deps.join(","))
        };
        s += &format!(
            "{} -> {}\t{}\t{conf}{deps}\n",
            String::from(e.key.from.clone()),
            String::from(e.key.to.clone()),
            edge_kind(e.key.kind)
        );
    }
    s
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bp: {e}\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let code = match command {
        Command::Version => {
            println!("bp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Index { path, rev } => index(&path, &rev),
        Command::Model {
            path,
            rev,
            level,
            json,
        } => model(&path, rev.as_deref(), level, json),
    };
    if std::env::var_os(PEAK_RSS_ENV).is_some() {
        report_peak_rss();
    }
    code
}

/// When set, `bp` prints its peak resident memory to stderr as it exits, as
/// `peak_rss_kb: <n>`. For bp-bench (#30); Linux only, as it reads `/proc`.
const PEAK_RSS_ENV: &str = "BLUEPRINT_REPORT_PEAK_RSS";

fn report_peak_rss() {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let peak = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().strip_suffix("kB"))
        .and_then(|v| v.trim().parse::<u64>().ok());
    if let Some(kb) = peak {
        eprintln!("peak_rss_kb: {kb}");
    }
}

fn model(path: &Path, rev: Option<&str>, level: Depth, json: bool) -> ExitCode {
    let deep = matches!(level, Depth::L3 | Depth::L4);
    let (model, diags) = match rev {
        None => match Directory::open(path) {
            Ok((dir, mut diags)) => {
                let read = |p: &RepoPath| dir.read(p);
                let (model, more) = if deep {
                    blueprint_engine::analyse_deep(dir.paths(), &read)
                } else {
                    blueprint_engine::analyse(dir.paths(), &read)
                };
                diags.extend(more);
                (model, diags)
            }
            Err(e) => {
                eprintln!("bp: cannot read {}: {e}", path.display());
                return ExitCode::from(EXIT_FAILURE);
            }
        },
        // The index holds L1–L2 only, so deeper levels are analysed.
        Some(rev) if deep => match Commit::open(path, rev) {
            Ok((source, mut diags)) => {
                let (model, more) =
                    blueprint_engine::analyse_deep(&source.paths(), &|p| source.read(p));
                diags.extend(more);
                diags.extend(unreadable(&source));
                diags.sort();
                (model, diags)
            }
            Err(e) => {
                eprintln!("bp: {}: {e}", path.display());
                return ExitCode::from(EXIT_FAILURE);
            }
        },
        Some(rev) => match at_rev(path, rev, false) {
            Ok(r) => {
                let diags = r.indexed.diagnostics();
                (r.indexed.model, diags)
            }
            Err(e) => {
                eprintln!("bp: {}: {e}", path.display());
                return ExitCode::from(EXIT_FAILURE);
            }
        },
    };
    // Level 4 is the whole model: no copy needed.
    let model = if level == Depth::L4 {
        model
    } else {
        up_to(&model, level)
    };
    // Written straight to stdout: the level-4 JSON of a large repository is
    // over 100 MB, and building it as one string first doubles the memory.
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let written = if json {
        serde_json::to_writer_pretty(&mut out, &model)
            .map_err(std::io::Error::from)
            .and_then(|()| out.write_all(b"\n"))
    } else {
        out.write_all(text(&model).as_bytes())
    };
    if let Err(e) = written.and_then(|()| out.flush()) {
        eprintln!("bp: cannot write the model: {e}");
        return ExitCode::from(EXIT_FAILURE);
    }
    warn(&diags);
    ExitCode::SUCCESS
}

fn warn(diags: &[Diagnostic]) {
    for d in diags {
        eprintln!(
            "warning: {}: {}",
            d.path,
            d.message.lines().next().unwrap_or("")
        );
    }
}

/// The model of a commit, and where it came from.
struct AtRev {
    commit: ObjectId,
    indexed: Indexed,
    /// Whether it was read from the index rather than analysed.
    from_index: bool,
    /// Whether it was analysed and then written to the index.
    stored: bool,
}

/// The model of `rev` in the repository at `path`: from the index if it holds
/// the commit's tree, otherwise analysed, and then stored if `store` is set.
/// An index that cannot be opened is a warning, not a failure, unless it has
/// to be written.
fn at_rev(path: &Path, rev: &str, store: bool) -> Result<AtRev, String> {
    let repo = Repo::open(path).map_err(|e| e.to_string())?;
    let commit = repo.resolve(rev).map_err(|e| e.to_string())?;
    let tree = repo.tree_id(commit).map_err(|e| e.to_string())?;
    let opened = if store {
        Index::open(path).map(Some)
    } else {
        Index::open_existing(path)
    };
    let index = match opened {
        Ok(index) => {
            for d in index.iter().flat_map(Index::dropped) {
                eprintln!("warning: index segment dropped and will be rebuilt: {d}");
            }
            index
        }
        Err(e) if store => return Err(format!("cannot open the index: {e}")),
        Err(e) => {
            eprintln!("warning: cannot open the index, analysing instead: {e}");
            None
        }
    };
    if let Some(indexed) = index.as_ref().and_then(|i| i.get(tree)) {
        return Ok(AtRev {
            commit,
            indexed,
            from_index: true,
            stored: false,
        });
    }
    let commit_id = commit.to_string();
    let (source, mut diags) = Commit::open(path, &commit_id).map_err(|e| e.to_string())?;
    let (model, more) = blueprint_engine::analyse(&source.paths(), &|p| source.read(p));
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
    })
}

/// A warning for each file of `source` whose object could not be read.
fn unreadable(source: &Commit) -> Vec<Diagnostic> {
    source
        .unreadable()
        .into_iter()
        .map(|path| Diagnostic {
            path,
            message: "the file's object could not be read (a partial clone?)".into(),
        })
        .collect()
}

fn index(path: &Path, rev: &str) -> ExitCode {
    match at_rev(path, rev, true) {
        Ok(r) => {
            let nodes = r.indexed.model.nodes().count();
            if r.from_index {
                println!("{} already indexed ({nodes} nodes)", r.commit);
                return ExitCode::SUCCESS;
            }
            warn(&r.indexed.diagnostics());
            if r.stored {
                println!("{} indexed ({nodes} nodes)", r.commit);
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "bp: {}: {} analysed but not indexed: some files could not be read, or its object ids are not SHA-1",
                    path.display(),
                    r.commit
                );
                ExitCode::from(EXIT_FAILURE)
            }
        }
        Err(e) => {
            eprintln!("bp: {}: {e}", path.display());
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_every_form() {
        assert_eq!(parse(&args(&[])), Ok(Command::Help));
        assert_eq!(parse(&args(&["-V"])), Ok(Command::Version));
        assert_eq!(parse(&args(&["help"])), Ok(Command::Help));
        assert_eq!(
            parse(&args(&["index", "r"])),
            Ok(Command::Index {
                path: "r".into(),
                rev: "HEAD".into()
            })
        );
        assert_eq!(
            parse(&args(&["index", "--rev", "v1", "r"])),
            Ok(Command::Index {
                path: "r".into(),
                rev: "v1".into()
            })
        );
        assert_eq!(
            parse(&args(&["model", "d", "--level", "1", "--json"])),
            Ok(Command::Model {
                path: "d".into(),
                rev: None,
                level: Depth::L1,
                json: true
            })
        );
        assert_eq!(
            parse(&args(&["model", "d", "--rev", "main~2"])),
            Ok(Command::Model {
                path: "d".into(),
                rev: Some("main~2".into()),
                level: Depth::L2,
                json: false
            })
        );
        assert_eq!(
            parse(&args(&["model", "--json", "d"])),
            Ok(Command::Model {
                path: "d".into(),
                rev: None,
                level: Depth::L2,
                json: true
            })
        );
        for bad in [
            &["model"][..],
            &["model", "d", "e"],
            &["model", "d", "--level"],
            &["model", "d", "--level", "5"],
            &["model", "d", "--rev"],
            &["model", "d", "--rev", ""],
            &["model", "d", "--rev", "--json"],
            &["index"],
            &["index", "d", "e"],
            &["index", "d", "--rev"],
            &["index", "d", "--level", "1"],
            &["model", "d", "--x"],
            &["nope"],
            &["--version", "x"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
