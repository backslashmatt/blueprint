//! `bp`: the headless Blueprint front end. Everything the app can show, this
//! CLI can print (REQUIREMENTS §5.3). See docs/cli.md.

use std::path::PathBuf;
use std::process::ExitCode;

use blueprint_engine::source::Directory;
use blueprint_model::{Level, Model};
use clap::{Parser, Subcommand, ValueEnum};

/// Exit code for a command that ran but could not do its job.
const EXIT_FAILURE: u8 = 1;

#[derive(Parser)]
#[command(
    name = "bp",
    version,
    about = "Blueprint: review code changes as architecture changes"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the architecture model of a directory (its L1 systems and L2 modules).
    Model {
        /// The directory to analyse, usually a repository's working tree.
        path: PathBuf,
        /// The deepest level to include.
        #[arg(long, value_enum, default_value_t = Depth::L2)]
        level: Depth,
        /// Print the model as JSON (docs/model-json.md) instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Depth {
    #[value(name = "1")]
    L1,
    #[value(name = "2")]
    L2,
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
    // clap prints usage errors itself and exits with code 2.
    let cli = Cli::parse();
    match cli.command {
        Command::Model { path, level, json } => {
            let (dir, mut diags) = match Directory::open(&path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("bp: cannot read {}: {e}", path.display());
                    return ExitCode::from(EXIT_FAILURE);
                }
            };
            let (model, more) = blueprint_engine::analyse(dir.paths(), &|p| dir.read(p));
            diags.extend(more);
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
    }
}
