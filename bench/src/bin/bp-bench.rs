//! `bp-bench`: fetch the reference repositories, run the benchmarks, and
//! compare them with the §4 budgets and with a base build measured on the same
//! machine in the same run (DR 0002).
//!
//! ```text
//! bp-bench fetch [--manifest bench/repos.json] [--dest bench/repos]
//! bp-bench run   [--manifest ..] [--dest ..] --bp <bp under test> --out results.json
//!                [--base-bp <bp built from the base commit>] [--base-commit <sha>]
//!                [--commit <sha>] [--summary summary.md]
//! ```
//!
//! `run` exits 1 if any measurement fails after re-measuring, 2 on errors.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use blueprint_bench::floor::read_tree;
use blueprint_bench::measure::{Error, best_ms, confirm, parse_opts};
use blueprint_bench::report::{Measurement, Report, SCHEMA, failures, markdown};
use blueprint_bench::repos::{self, RefRepo};
use blueprint_engine::git::Repo;

const USAGE: &str = "usage: bp-bench fetch [--manifest F] [--dest D]\n       bp-bench run [--manifest F] [--dest D] --bp PATH --out F [--base-bp PATH] [--base-commit SHA] [--commit SHA] [--summary F]";

const OPTIONS: &[&str] = &[
    "manifest",
    "dest",
    "bp",
    "base-bp",
    "base-commit",
    "commit",
    "out",
    "summary",
];

/// Re-measurements allowed before a failure counts (DR 0002).
const RETRIES: usize = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run_cli(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("bp-bench: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_cli(args: &[String]) -> Result<ExitCode, Error> {
    let (cmd, rest) = args.split_first().ok_or("no command")?;
    let opts = parse_opts(rest, OPTIONS)?;
    let path =
        |key: &str, default: &str| PathBuf::from(opts.get(key).map_or(default, String::as_str));
    let manifest = path("manifest", "bench/repos.json");
    let dest = path("dest", "bench/repos");
    let repos = repos::load(&manifest)?;
    match cmd.as_str() {
        "fetch" => {
            for r in &repos {
                eprintln!("fetching {} ({}) at {}", r.name, r.url, r.commit);
                repos::fetch(&dest, r)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        "run" => {
            for r in &repos {
                repos::verify(&dest, r)?;
            }
            let bp = PathBuf::from(opts.get("bp").ok_or("--bp is required")?);
            let base_bp = opts.get("base-bp").map(PathBuf::from);
            if base_bp.is_some() != opts.contains_key("base-commit") {
                return Err("--base-bp and --base-commit go together".into());
            }
            let out = PathBuf::from(opts.get("out").ok_or("--out is required")?);
            let mut report = Report {
                schema: SCHEMA,
                commit: opts.get("commit").cloned(),
                base_commit: opts.get("base-commit").cloned(),
                runner: runner(),
                measurements: measure(&bp, base_bp.as_deref(), &dest, &repos)?,
            };
            confirm(&mut report, RETRIES, || {
                measure(&bp, base_bp.as_deref(), &dest, &repos)
            })?;
            std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
            let table = markdown(&report);
            println!("{table}");
            if let Some(s) = opts.get("summary") {
                std::fs::write(s, &table)?;
            }
            let failed = failures(&report);
            for f in &failed {
                eprintln!("FAIL {f}");
            }
            Ok(if failed.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        other => Err(format!("unknown command `{other}`").into()),
    }
}

fn runner() -> String {
    let cpus = std::thread::available_parallelism().map_or(0, |n| n.get());
    let image = std::env::var("ImageOS").unwrap_or_else(|_| "local".into());
    format!(
        "{} {} ({image}), {cpus} logical CPUs",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn spawn_version(bp: &Path) -> impl FnMut() -> Result<(), Error> + '_ {
    move || {
        let status = Command::new(bp).arg("--version").output()?.status;
        if status.success() {
            Ok(())
        } else {
            Err(format!("`{} --version` failed: {status}", bp.display()).into())
        }
    }
}

fn measure(
    bp: &Path,
    base_bp: Option<&Path>,
    dest: &Path,
    repos: &[RefRepo],
) -> Result<Vec<Measurement>, Error> {
    let mut out = Vec::new();

    // 40 spawns per sample keep one sample around 50 ms, so a 5% change is
    // well above timer and scheduler jitter (DR 0002).
    let (samples, batch) = (11, 40);
    let mut head = spawn_version(bp);
    let mut base = base_bp.map(spawn_version);
    let (value, base) = best_ms(
        samples,
        batch,
        &mut head,
        base.as_mut()
            .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
    )?;
    out.push(Measurement {
        name: "cli_process_start".into(),
        repo: None,
        value,
        base,
        unit: "ms".into(),
        budget: None,
        samples,
        note: Some(format!(
            "`bp --version`, fastest of {samples} batches of {batch}; not the app cold-start budget"
        )),
        gated: true,
    });

    for r in repos {
        let dir = repos::checkout_dir(dest, r);
        let samples = 11;
        let mut stats = None;
        let mut read = || -> Result<(), Error> {
            stats = Some(read_tree(&dir)?);
            Ok(())
        };
        let (value, _) = best_ms(samples, 1, &mut read, None)?;
        let s = stats.ok_or("read_tree produced no result")?;
        out.push(Measurement {
            name: "io_floor_read_tree".into(),
            repo: Some(r.name.clone()),
            value,
            base: None,
            unit: "ms".into(),
            budget: None,
            samples,
            note: Some(format!(
                "warm cache; reads {} files, {} MB (symlinks and submodules skipped)",
                s.files,
                s.bytes / 1_000_000
            )),
            gated: false,
        });

        // The first step of indexing (#24): list the pinned commit's tree
        // through the object database. In-process, so there is no base build
        // to compare with; it is reported, not gated.
        let repo = Repo::open(&dir)?;
        let commit = repo.resolve(&r.commit)?;
        let mut listed = 0;
        let mut list = || -> Result<(), Error> {
            listed = repo.tree(commit)?.len();
            Ok(())
        };
        let (value, _) = best_ms(samples, 1, &mut list, None)?;
        out.push(Measurement {
            name: "git_list_tree".into(),
            repo: Some(r.name.clone()),
            value,
            base: None,
            unit: "ms".into(),
            budget: None,
            samples,
            note: Some(format!(
                "warm cache; {listed} entries at the pinned commit, via gix; a step towards index_large_l2"
            )),
            gated: false,
        });
    }
    Ok(out)
}
