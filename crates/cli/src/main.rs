//! `bp`: the headless Blueprint front end. Everything the app can show, this
//! CLI can print (REQUIREMENTS §5.3). See docs/cli.md.

use std::path::PathBuf;
use std::process::ExitCode;

use blueprint_engine::source::{Commit, Directory};
use blueprint_model::{Level, Model};

/// Exit code for a command that ran but could not do its job.
const EXIT_FAILURE: u8 = 1;
/// Exit code for a usage error.
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "\
usage: bp model <dir> [--rev <rev>] [--level 1|2] [--json]
       bp --version
       bp --help

commands:
  model <dir>   Print the architecture model of a directory (L1 systems, L2 modules)

options for model:
  --rev <rev>   Read the commit <rev> of the git repository at <dir> instead of
                its working tree (a branch, tag, commit id or HEAD)
  --level 1|2   The deepest level to include (default 2)
  --json        Print the model as JSON (docs/model-json.md) instead of text";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    L1,
    L2,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Version,
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
                            Some(v) => return Err(format!("--level takes 1 or 2, not `{v}`")),
                            None => return Err("--level needs a value: 1 or 2".into()),
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
        other => Err(format!("unknown command `{other}`")),
    }
}

/// The nodes up to `depth` and the edges between them.
fn up_to(model: &Model, depth: Depth) -> Model {
    let max = match depth {
        Depth::L1 => Level::System,
        Depth::L2 => Level::Module,
    };
    let mut out = Model::default();
    // Nodes come out parents first (ids sort by level), so every parent is
    // already present when its child is added. Errors cannot happen for a
    // subset of a valid model; they would only drop the node.
    for n in model.nodes().filter(|n| n.level() <= max) {
        let _ = out.add_node(n.clone());
    }
    for e in model.edges().filter(|e| e.key.from.level() <= max) {
        let _ = out.add_edge(e.clone());
    }
    out
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
            "{} -> {}\tdepends-on\t{conf}{deps}\n",
            String::from(e.key.from.clone()),
            String::from(e.key.to.clone())
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
    match command {
        Command::Version => {
            println!("bp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Model {
            path,
            rev,
            level,
            json,
        } => model(&path, rev.as_deref(), level, json),
    }
}

fn model(path: &std::path::Path, rev: Option<&str>, level: Depth, json: bool) -> ExitCode {
    let (model, diags) = match rev {
        None => match Directory::open(path) {
            Ok((dir, mut diags)) => {
                let (model, more) = blueprint_engine::analyse(dir.paths(), &|p| dir.read(p));
                diags.extend(more);
                (model, diags)
            }
            Err(e) => {
                eprintln!("bp: cannot read {}: {e}", path.display());
                return ExitCode::from(EXIT_FAILURE);
            }
        },
        Some(rev) => match Commit::open(path, rev) {
            Ok((commit, mut diags)) => {
                let (model, more) = blueprint_engine::analyse(&commit.paths(), &|p| commit.read(p));
                diags.extend(more);
                (model, diags)
            }
            Err(e) => {
                eprintln!("bp: {}: {e}", path.display());
                return ExitCode::from(EXIT_FAILURE);
            }
        },
    };
    let model = up_to(&model, level);
    let out = if json {
        match serde_json::to_string_pretty(&model) {
            Ok(s) => s + "\n",
            Err(e) => {
                eprintln!("bp: cannot serialise the model: {e}");
                return ExitCode::from(EXIT_FAILURE);
            }
        }
    } else {
        text(&model)
    };
    print!("{out}");
    for d in &diags {
        eprintln!(
            "warning: {}: {}",
            d.path,
            d.message.lines().next().unwrap_or("")
        );
    }
    ExitCode::SUCCESS
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
            &["model", "d", "--level", "3"],
            &["model", "d", "--rev"],
            &["model", "d", "--rev", ""],
            &["model", "d", "--rev", "--json"],
            &["model", "d", "--x"],
            &["nope"],
            &["--version", "x"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
