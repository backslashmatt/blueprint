//! `bp`: the headless Blueprint front end. Everything the app can show, this
//! CLI can print (REQUIREMENTS §5.3). See docs/cli.md.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use blueprint_engine::commits;
use blueprint_engine::l1::Diagnostic;
use blueprint_engine::source::Directory;
use blueprint_model::{EdgeKind, Level, Model, RepoPath};

/// Exit code for a command that ran but could not do its job.
const EXIT_FAILURE: u8 = 1;
/// Exit code for a usage error.
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "\
usage: bp model <dir> [--rev <rev>] [--level 1|2|3|4] [--json]
       bp index <repo> [--rev <rev>] [--level 2|4]
       bp diff <repo> <base> <head> [--json]
       bp review <repo> <base> <head> [--mark <node> [--note <text>] | --unmark <node>] [--json]
       bp --version
       bp --help

commands:
  model <dir>   Print the architecture model of a directory (L1 systems, L2 modules)

  index <repo>  Analyse a commit (default HEAD) and store its model in the
                index, so that `bp model --rev` reopens it without analysing

  diff <repo> <base> <head>
                Print the architectural diff of two commits: findings (new
                dependencies and cycles), then changed nodes and edges

  review <repo> <base> <head>
                Print each changed node of the diff and whether it is reviewed
                (docs/cli.md), after storing a mark if one is given

options for diff:
  --json        Print the diff as JSON (docs/diff-json.md) instead of text

options for review:
  --mark <node> Mark the node (an id as `bp diff` prints it) reviewed
  --note <text> With --mark, attach a note; with an empty text, remove it
  --unmark <node>
                Mark the node not reviewed, keeping its note
  --json        Print the review as JSON instead of text

options for index:
  --rev <rev>   The commit to index (default HEAD)
  --level N     2 (default) stores the L1–L2 model; 4 also parses every Rust
                file and stores its facts, so `bp model --rev --level 3` or
                `4` parses only files that changed since

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
        /// Whether to store L3–L4 facts too (`--level 4`).
        deep: bool,
    },
    Help,
    Diff {
        path: PathBuf,
        base: String,
        head: String,
        json: bool,
    },
    Model {
        path: PathBuf,
        rev: Option<String>,
        level: Depth,
        json: bool,
    },
    Review {
        path: PathBuf,
        base: String,
        head: String,
        change: Option<MarkChange>,
        json: bool,
    },
}

/// What `bp review` changes before printing.
#[derive(Debug, PartialEq, Eq)]
struct MarkChange {
    node: String,
    reviewed: bool,
    /// `None` keeps the node's note; `Some("")` removes it.
    note: Option<String>,
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
        "diff" => {
            let mut json = false;
            let mut positional = Vec::new();
            for arg in rest {
                match arg.as_str() {
                    "--json" => json = true,
                    "--help" | "-h" => return Ok(Command::Help),
                    flag if flag.starts_with('-') && flag != "-" => {
                        return Err(format!("unknown option `{flag}`"));
                    }
                    p => positional.push(p.to_owned()),
                }
            }
            match <[String; 3]>::try_from(positional) {
                Ok([path, base, head]) => Ok(Command::Diff {
                    path: PathBuf::from(path),
                    base,
                    head,
                    json,
                }),
                Err(_) => {
                    Err("diff needs a repository, a base revision and a head revision".into())
                }
            }
        }
        "index" => {
            let (mut path, mut rev, mut deep) = (None, None, false);
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--level" => {
                        deep = match it.next().map(String::as_str) {
                            Some("2") => false,
                            Some("4") => true,
                            Some(v) => {
                                return Err(format!("--level for index takes 2 or 4, not `{v}`"));
                            }
                            None => return Err("--level needs a value: 2 or 4".into()),
                        }
                    }
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
                deep,
            })
        }
        "review" => {
            let (mut json, mut mark, mut unmark, mut note) = (false, None, None, None);
            let mut positional = Vec::new();
            let mut it = rest.iter();
            let value = |v: Option<&String>, flag: &str| -> Result<String, String> {
                v.cloned().ok_or_else(|| format!("{flag} needs a value"))
            };
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--json" => json = true,
                    "--mark" => mark = Some(value(it.next(), "--mark")?),
                    "--unmark" => unmark = Some(value(it.next(), "--unmark")?),
                    "--note" => note = Some(value(it.next(), "--note")?),
                    "--help" | "-h" => return Ok(Command::Help),
                    flag if flag.starts_with('-') && flag != "-" => {
                        return Err(format!("unknown option `{flag}`"));
                    }
                    p => positional.push(p.to_owned()),
                }
            }
            let change = match (mark, unmark) {
                (Some(_), Some(_)) => return Err("give --mark or --unmark, not both".into()),
                (Some(node), None) => Some(MarkChange {
                    node,
                    reviewed: true,
                    note,
                }),
                (None, Some(node)) if note.is_none() => Some(MarkChange {
                    node,
                    reviewed: false,
                    note: None,
                }),
                (None, _) if note.is_some() => return Err("--note goes with --mark".into()),
                (None, _) => None,
            };
            match <[String; 3]>::try_from(positional) {
                Ok([path, base, head]) => Ok(Command::Review {
                    path: PathBuf::from(path),
                    base,
                    head,
                    change,
                    json,
                }),
                Err(_) => {
                    Err("review needs a repository, a base revision and a head revision".into())
                }
            }
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
        Command::Index { path, rev, deep } => index(&path, &rev, deep),
        Command::Diff {
            path,
            base,
            head,
            json,
        } => diff(&path, &base, &head, json),
        Command::Model {
            path,
            rev,
            level,
            json,
        } => model(&path, rev.as_deref(), level, json),
        Command::Review {
            path,
            base,
            head,
            change,
            json,
        } => review(&path, &base, &head, change.as_ref(), json),
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
        // Deeper levels come from the stored L1–L4 model, or are built from
        // the facts the index holds where it has them.
        Some(rev) if deep => {
            let (index, warnings) = commits::existing_index(path);
            warn_index(&warnings);
            match commits::l4_at(path, rev, index.as_ref()) {
                Ok(built) => built,
                Err(e) => {
                    eprintln!("bp: {}: {e}", path.display());
                    return ExitCode::from(EXIT_FAILURE);
                }
            }
        }
        Some(rev) => match commits::l2_at(path, rev, false) {
            Ok(r) => {
                warn_index(&r.warnings);
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

/// `bp diff`: builds the L1–L4 model of both commits, with stored facts
/// where the index has them, and prints the architectural diff.
fn diff(path: &Path, base: &str, head: &str, json: bool) -> ExitCode {
    let (index, warnings) = commits::existing_index(path);
    warn_index(&warnings);
    let d = match commits::diff_commits(path, base, head, index.as_ref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("bp: {}: {e}", path.display());
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    warn(&d.base.1);
    warn(&d.head.1);
    if let Some(e) = &d.weight_error {
        eprintln!("warning: change weight not computed: {e}");
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let written = if json {
        serde_json::to_writer_pretty(&mut out, &d.diff)
            .map_err(std::io::Error::from)
            .and_then(|()| out.write_all(b"\n"))
    } else {
        out.write_all(diff_text(&d.diff).as_bytes())
    };
    if let Err(e) = written.and_then(|()| out.flush()) {
        eprintln!("bp: cannot write the diff: {e}");
        return ExitCode::from(EXIT_FAILURE);
    }
    // The models are large, and the process ends here: freeing them would
    // take longer than the diff (DR 0009).
    std::mem::forget(d);
    std::mem::forget(index);
    ExitCode::SUCCESS
}

/// `bp review`: the diff's changed nodes and their review state, after
/// storing `change` if given (DR 0011).
fn review(
    path: &Path,
    base: &str,
    head: &str,
    change: Option<&MarkChange>,
    json: bool,
) -> ExitCode {
    use blueprint_engine::reviews::Reviews;
    use blueprint_model::NodeId;
    use blueprint_model::review::{Mark, ReviewState};
    let fail = |e: &dyn std::fmt::Display| {
        eprintln!("bp: {}: {e}", path.display());
        ExitCode::from(EXIT_FAILURE)
    };
    let (index, warnings) = commits::existing_index(path);
    warn_index(&warnings);
    let d = match commits::diff_commits(path, base, head, index.as_ref()) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };
    warn(&d.base.1);
    warn(&d.head.1);
    if let Some(e) = &d.weight_error {
        // Without weights the keys cannot match marks made with them.
        return fail(&format!(
            "change weight not computed, so marks cannot be matched: {e}"
        ));
    }
    let mut reviews = match Reviews::open(path) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    if let Some(w) = reviews.warning() {
        eprintln!("warning: {w}");
    }
    let state = |r: &Reviews| ReviewState::new(&d.diff, &d.base.0, &d.head.0, r.marks());
    let mut current = state(&reviews);
    if let Some(change) = change {
        let node = match NodeId::try_from(change.node.clone()) {
            Ok(id) => id,
            Err(_) => return fail(&format!("`{}` is not a node id", change.node)),
        };
        let Some(key) = current.key(&node) else {
            return fail(&format!("{node} did not change between {base} and {head}"));
        };
        let old = current.mark(&node).and_then(|m| m.note.clone());
        let note = match &change.note {
            None => old,
            Some(n) if n.is_empty() => None,
            Some(n) => Some(n.clone()),
        };
        let mark = Mark {
            node,
            key,
            reviewed: change.reviewed,
            note,
        };
        if let Err(e) = reviews.set(mark) {
            return fail(&e);
        }
        current = state(&reviews);
        let still = NodeId::try_from(change.node.clone())
            .ok()
            .is_some_and(|id| current.is_reviewed(&id));
        if !change.reviewed && still {
            eprintln!(
                "note: {} is still reviewed: every changed node under it is",
                change.node
            );
        }
    }
    let (done, total) = current.progress();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let written = if json {
        let nodes: Vec<serde_json::Value> = current
            .changed()
            .map(|id| {
                serde_json::json!({
                    "node": id.to_string(),
                    "reviewed": current.is_reviewed(id),
                    "needs_own_mark": current.needs_own_mark(id),
                    "note": current.mark(id).and_then(|m| m.note.clone()),
                })
            })
            .collect();
        let doc = serde_json::json!({
            "progress": { "reviewed": done, "of": total },
            "nodes": nodes,
        });
        serde_json::to_writer_pretty(&mut out, &doc)
            .map_err(std::io::Error::from)
            .and_then(|()| out.write_all(b"\n"))
    } else {
        let mut text = format!("reviewed {done} of {total}\n");
        for id in current.changed() {
            let status = if current.is_reviewed(id) {
                "reviewed"
            } else {
                "open"
            };
            text.push_str(&format!("{status}\t{id}"));
            if let Some(note) = current.mark(id).and_then(|m| m.note.as_deref()) {
                text.push('\t');
                text.push_str(&note.replace(['\n', '\r', '\t'], " "));
            }
            text.push('\n');
        }
        out.write_all(text.as_bytes())
    };
    if let Err(e) = written.and_then(|()| out.flush()) {
        return fail(&format!("cannot write the review: {e}"));
    }
    std::mem::forget(d);
    std::mem::forget(index);
    ExitCode::SUCCESS
}

fn warn_index(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

/// The text form of a diff: one tab-separated line per finding, node change
/// and edge change, in that order.
fn diff_text(d: &blueprint_engine::diff::Diff) -> String {
    use blueprint_engine::diff::{Finding, NodeChange};
    use std::fmt::Write as _;
    let edge =
        |k: &blueprint_model::EdgeKey| format!("{} -> {}\t{}", k.from, k.to, edge_kind(k.kind));
    let mut s = String::new();
    for f in &d.findings {
        let _ = match f {
            Finding::NewDependency { edge: k } => {
                writeln!(s, "finding\tnew-dependency\t{}", edge(k))
            }
            Finding::NewCycle {
                level,
                nodes,
                new_edges,
            } => {
                let nodes: Vec<String> = nodes.iter().map(ToString::to_string).collect();
                let new: Vec<String> = new_edges
                    .iter()
                    .map(|k| format!("{} -> {}", k.from, k.to))
                    .collect();
                writeln!(
                    s,
                    "finding\tnew-cycle\tL{level}\t{}\tnew: {}",
                    nodes.join(", "),
                    new.join(", ")
                )
            }
        };
    }
    for n in &d.nodes {
        let _ = match n {
            NodeChange::Added { id } => writeln!(s, "added\t{id}"),
            NodeChange::Removed { id } => writeln!(s, "removed\t{id}"),
            NodeChange::Moved { from, to } => writeln!(s, "moved\t{from} -> {to}"),
            NodeChange::Renamed { from, to } => writeln!(s, "renamed\t{from} -> {to}"),
            NodeChange::Surface { id } => writeln!(s, "surface\t{id}"),
            NodeChange::Contained { id } => writeln!(s, "contained\t{id}"),
            NodeChange::Changed { id } => writeln!(s, "changed\t{id}"),
        };
    }
    for k in &d.added_edges {
        let _ = writeln!(s, "+edge\t{}", edge(k));
    }
    for k in &d.removed_edges {
        let _ = writeln!(s, "-edge\t{}", edge(k));
    }
    for w in &d.weights {
        let _ = writeln!(
            s,
            "weight\t{}\t{}\t{}",
            w.id, w.lines.signature, w.lines.body
        );
    }
    s
}

fn index(path: &Path, rev: &str, deep: bool) -> ExitCode {
    let r = match commits::l2_at(path, rev, true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bp: {}: {e}", path.display());
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    warn_index(&r.warnings);
    let nodes = r.indexed.model.nodes().count();
    // Facts are stored only for a commit whose L1–L2 model could be.
    if deep && (r.from_index || r.stored) {
        return match commits::index_l4(path, &r.commit.to_string()) {
            Ok(done) => {
                warn(&done.diags);
                println!(
                    "{} indexed to level 4 ({nodes} nodes at L1–L2; {} files parsed, {} reused)",
                    r.commit, done.parsed, done.reused
                );
                // Large, and the process ends here (DR 0009).
                std::mem::forget(done);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("bp: {}: {e}", path.display());
                ExitCode::from(EXIT_FAILURE)
            }
        };
    }
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
                rev: "HEAD".into(),
                deep: false
            })
        );
        assert_eq!(
            parse(&args(&["index", "--rev", "v1", "r"])),
            Ok(Command::Index {
                path: "r".into(),
                rev: "v1".into(),
                deep: false
            })
        );
        assert_eq!(
            parse(&args(&["index", "r", "--level", "4"])),
            Ok(Command::Index {
                path: "r".into(),
                rev: "HEAD".into(),
                deep: true
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
        assert_eq!(
            parse(&args(&["diff", "r", "main", "HEAD", "--json"])),
            Ok(Command::Diff {
                path: "r".into(),
                base: "main".into(),
                head: "HEAD".into(),
                json: true
            })
        );
        assert_eq!(
            parse(&args(&[
                "review", "r", "a", "b", "--mark", "n", "--note", "ok"
            ])),
            Ok(Command::Review {
                path: "r".into(),
                base: "a".into(),
                head: "b".into(),
                change: Some(MarkChange {
                    node: "n".into(),
                    reviewed: true,
                    note: Some("ok".into()),
                }),
                json: false
            })
        );
        assert_eq!(
            parse(&args(&["review", "r", "a", "b", "--unmark", "n", "--json"])),
            Ok(Command::Review {
                path: "r".into(),
                base: "a".into(),
                head: "b".into(),
                change: Some(MarkChange {
                    node: "n".into(),
                    reviewed: false,
                    note: None,
                }),
                json: true
            })
        );
        for bad in [
            &["review", "r", "a"][..],
            &["review", "r", "a", "b", "--mark"],
            &["review", "r", "a", "b", "--mark", "n", "--unmark", "n"],
            &["review", "r", "a", "b", "--note", "x"],
            &["review", "r", "a", "b", "--unmark", "n", "--note", "x"],
            &["model"],
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
            &["diff", "r", "a"],
            &["diff", "r", "a", "b", "c"],
            &["diff", "r", "a", "b", "--x"],
            &["index", "d", "--level", "3"],
            &["index", "d", "--level"],
            &["model", "d", "--x"],
            &["nope"],
            &["--version", "x"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
