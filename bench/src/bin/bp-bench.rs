//! `bp-bench`: fetch the reference repositories, run the benchmarks, and
//! compare them with the §4 budgets and with a base build measured on the same
//! machine in the same run (DR 0002).
//!
//! ```text
//! bp-bench fetch [--manifest bench/repos.json] [--dest bench/repos]
//! bp-bench run   [--manifest ..] [--dest ..] --bp <bp under test> --out results.json
//!                [--base-bp <bp built from the base commit>] [--base-commit <sha>]
//!                [--commit <sha>] [--summary summary.md]
//!                [--app <blueprint app> [--base-app <app built from the base commit>]]
//! ```
//!
//! With `--app`, the app's cold start is measured too; it needs a display.
//!
//! `run` exits 1 if any measurement fails after re-measuring, 2 on errors.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use blueprint_bench::floor::read_tree;
use blueprint_bench::measure::{Error, best_ms, confirm, parse_opts};
use blueprint_bench::report::{Measurement, Report, SCHEMA, failures, markdown};
use blueprint_bench::repos::{self, RefRepo};
use blueprint_engine::git::Repo;
use blueprint_store::{KEY_LEN, Key, SegmentWriter, Store};

const USAGE: &str = "usage: bp-bench fetch [--manifest F] [--dest D]\n       bp-bench run [--manifest F] [--dest D] --bp PATH --out F [--base-bp PATH] [--base-commit SHA] [--commit SHA] [--summary F] [--app PATH [--base-app PATH]]";

const OPTIONS: &[&str] = &[
    "manifest",
    "dest",
    "bp",
    "base-bp",
    "base-commit",
    "commit",
    "out",
    "summary",
    "app",
    "base-app",
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
            let app = opts.get("app").map(PathBuf::from);
            let base_app = opts.get("base-app").map(PathBuf::from);
            if base_app.is_some() && (app.is_none() || base_bp.is_none()) {
                return Err("--base-app needs --app and --base-bp".into());
            }
            let mut report = Report {
                schema: SCHEMA,
                commit: opts.get("commit").cloned(),
                base_commit: opts.get("base-commit").cloned(),
                runner: runner(),
                measurements: measure(
                    &bp,
                    base_bp.as_deref(),
                    app.as_deref(),
                    base_app.as_deref(),
                    &dest,
                    &repos,
                )?,
            };
            confirm(&mut report, RETRIES, || {
                measure(
                    &bp,
                    base_bp.as_deref(),
                    app.as_deref(),
                    base_app.as_deref(),
                    &dest,
                    &repos,
                )
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

/// Runs `bp model <dir> --json`, discarding the output: the whole L1–L2
/// analysis of a checkout, as the CLI and app run it.
fn spawn_model<'a>(bp: &'a Path, dir: &'a Path) -> impl FnMut() -> Result<(), Error> + 'a {
    move || {
        let status = Command::new(bp)
            .arg("model")
            .arg(dir)
            .arg("--json")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "`{} model {}` failed: {status}",
                bp.display(),
                dir.display()
            )
            .into())
        }
    }
}

/// `open_indexed` for each reference repository, with the caches under
/// `cache`.
fn open_indexed(
    bp: &Path,
    base_bp: Option<&Path>,
    dest: &Path,
    repos: &[RefRepo],
    cache: &Path,
) -> Result<Vec<Measurement>, Error> {
    let mut out = Vec::new();
    for r in repos {
        let dir = repos::checkout_dir(dest, r);
        let head_cache = cache.join(&r.name).join("head");
        let base_cache = cache.join(&r.name).join("base");
        bp_index(bp, &dir, &r.commit, &head_cache)?;
        let base_bp = base_bp.filter(|b| bp_index(b, &dir, &r.commit, &base_cache).is_ok());
        let samples = if r.name == "large" { 5 } else { 11 };
        let mut head = spawn_reopen(bp, &dir, &r.commit, &head_cache);
        let mut base = base_bp.map(|b| spawn_reopen(b, &dir, &r.commit, &base_cache));
        let (value, base) = best_ms(
            samples,
            1,
            &mut head,
            base.as_mut()
                .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
        )?;
        out.push(Measurement {
            name: "open_indexed".into(),
            repo: Some(r.name.clone()),
            value,
            base,
            unit: "ms".into(),
            budget: (r.name != "small").then(|| "open_indexed".into()),
            samples,
            note: Some(format!(
                "`bp model <checkout> --rev <pinned> --json` after `bp index`, fastest of {samples}; warm cache"
            )),
            gated: true,
        });
    }
    Ok(out)
}

/// `first_index` for each reference repository: `bp index` of the pinned
/// commit into an empty cache, so every run analyses the whole commit. It
/// is held to the §4 "L1–L2 on screen" budgets. Then one more run of
/// `bp index` and of the reopen reports peak resident memory (Linux).
fn first_index(
    bp: &Path,
    base_bp: Option<&Path>,
    dest: &Path,
    repos: &[RefRepo],
    cache: &Path,
) -> Result<Vec<Measurement>, Error> {
    let mut out = Vec::new();
    for r in repos {
        let dir = repos::checkout_dir(dest, r);
        let samples = if r.name == "large" { 3 } else { 7 };
        let fresh = |bp: std::path::PathBuf, tag: &'static str| {
            let mut n = 0u32;
            let root = cache.join(&r.name).join(tag);
            let dir = dir.clone();
            let rev = r.commit.clone();
            move || -> Result<(), Error> {
                n += 1;
                let c = root.join(n.to_string());
                let out = Command::new(&bp)
                    .arg("index")
                    .arg(&dir)
                    .args(["--rev", &rev])
                    .env("BLUEPRINT_CACHE_DIR", &c)
                    .stderr(std::process::Stdio::null())
                    .output()?;
                let stdout = String::from_utf8_lossy(&out.stdout);
                if out.status.success()
                    && stdout.contains(" indexed (")
                    && !stdout.contains("already")
                {
                    Ok(())
                } else {
                    Err(format!(
                        "`{} index {}` did not index: {} {stdout}",
                        bp.display(),
                        dir.display(),
                        out.status
                    )
                    .into())
                }
            }
        };
        let mut head = fresh(bp.to_path_buf(), "head");
        let mut base = base_bp.map(|b| fresh(b.to_path_buf(), "base"));
        let (value, base) = best_ms(
            samples,
            1,
            &mut head,
            base.as_mut()
                .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
        )?;
        out.push(Measurement {
            name: "first_index".into(),
            repo: Some(r.name.clone()),
            value,
            base,
            unit: "ms".into(),
            budget: index_budget(&r.name),
            samples,
            note: Some(format!(
                "`bp index <checkout> --rev <pinned>` into an empty cache, fastest of {samples}; warm OS cache"
            )),
            gated: true,
        });

        let peak_cache = cache.join(&r.name).join("peak");
        for (name, args) in [
            (
                "peak_rss_first_index",
                vec!["index", "--rev", r.commit.as_str()],
            ),
            (
                "peak_rss_open_indexed",
                vec!["model", "--rev", r.commit.as_str(), "--json"],
            ),
        ] {
            let run = Command::new(bp)
                .arg(args[0])
                .arg(&dir)
                .args(&args[1..])
                .env("BLUEPRINT_CACHE_DIR", &peak_cache)
                .env("BLUEPRINT_REPORT_PEAK_RSS", "1")
                .stdout(std::process::Stdio::null())
                .output()?;
            let stderr = String::from_utf8_lossy(&run.stderr);
            let kb = stderr
                .lines()
                .find_map(|l| l.strip_prefix("peak_rss_kb: "))
                .and_then(|v| v.trim().parse::<f64>().ok());
            let Some(kb) = kb else {
                // Not Linux, or a build without the report: nothing to say.
                continue;
            };
            out.push(Measurement {
                name: name.into(),
                repo: Some(r.name.clone()),
                value: kb / 1024.0,
                base: None,
                unit: "MB".into(),
                budget: None,
                samples: 1,
                note: Some(format!("peak resident memory of `bp {}`, one run", args[0])),
                gated: false,
            });
        }
    }
    Ok(out)
}

/// `bp index --level 4` of each reference repository's pinned commit into
/// an empty cache (`first_index_l4`), then `bp model --rev --level 4` with
/// that cache, so every Rust file's facts come from the index
/// (`model_l4_indexed`, #63). One sample of each on the large repository,
/// whose runs take seconds. A base build without `index --level 4` is not
/// compared.
fn deep_index(
    bp: &Path,
    base_bp: Option<&Path>,
    dest: &Path,
    repos: &[RefRepo],
    cache: &Path,
) -> Result<Vec<Measurement>, Error> {
    let mut out = Vec::new();
    let base_bp = base_bp.filter(|b| supports_index_level_4(b));
    for r in repos {
        let dir = repos::checkout_dir(dest, r);
        let samples = if r.name == "large" { 1 } else { 3 };
        let index = |bp: std::path::PathBuf, tag: &'static str| {
            let mut n = 0u32;
            let root = cache.join(&r.name).join(tag);
            let dir = dir.clone();
            let rev = r.commit.clone();
            move || -> Result<(), Error> {
                n += 1;
                let out = Command::new(&bp)
                    .arg("index")
                    .arg(&dir)
                    .args(["--rev", &rev, "--level", "4"])
                    .env("BLUEPRINT_CACHE_DIR", root.join(n.to_string()))
                    .stderr(std::process::Stdio::null())
                    .output()?;
                let stdout = String::from_utf8_lossy(&out.stdout);
                if out.status.success() && stdout.contains(" 0 reused)") {
                    Ok(())
                } else {
                    Err(format!(
                        "`{} index {} --level 4` did not index afresh: {} {stdout}",
                        bp.display(),
                        dir.display(),
                        out.status
                    )
                    .into())
                }
            }
        };
        let mut head = index(bp.to_path_buf(), "head");
        let mut base = base_bp.map(|b| index(b.to_path_buf(), "base"));
        let (value, base) = best_ms(
            samples,
            1,
            &mut head,
            base.as_mut()
                .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
        )?;
        out.push(Measurement {
            name: "first_index_l4".into(),
            repo: Some(r.name.clone()),
            value,
            base,
            unit: "ms".into(),
            budget: match r.name.as_str() {
                "medium" => Some("index_medium".into()),
                "large" => Some("index_large".into()),
                _ => None,
            },
            samples,
            note: Some(format!(
                "`bp index <checkout> --rev <pinned> --level 4` into an empty cache, fastest of {samples}; warm OS cache"
            )),
            gated: true,
        });

        // Every sample above indexed into its own cache; the first of each
        // holds every file's facts.
        let head_cache = cache.join(&r.name).join("head").join("1");
        let base_cache = cache.join(&r.name).join("base").join("1");
        let mut head = spawn_l4(bp, &dir, &r.commit, &head_cache);
        let mut base = base_bp.map(|b| spawn_l4(b, &dir, &r.commit, &base_cache));
        let (value, base) = best_ms(
            samples,
            1,
            &mut head,
            base.as_mut()
                .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
        )?;
        out.push(Measurement {
            name: "model_l4_indexed".into(),
            repo: Some(r.name.clone()),
            value,
            base,
            unit: "ms".into(),
            budget: None,
            samples,
            note: Some(format!(
                "`bp model <checkout> --rev <pinned> --level 4 --json` after `bp index --level 4`: no file parsed; fastest of {samples}"
            )),
            gated: true,
        });

        // The architectural diff of the pinned commit's first parent and
        // the commit itself, both indexed to level 4 (#64, DR 0009).
        if r.name == "medium" {
            out.push(diff_medium(
                bp,
                base_bp,
                &dir,
                &r.commit,
                &head_cache,
                &base_cache,
            )?);
        }
    }
    Ok(out)
}

/// `diff_medium`: `bp diff <checkout> <pinned>~1 <pinned>` with both
/// commits indexed to level 4 in the given caches, fastest of 11. A base
/// build without `bp diff` is not compared.
fn diff_medium(
    bp: &Path,
    base_bp: Option<&Path>,
    dir: &Path,
    commit: &str,
    head_cache: &Path,
    base_cache: &Path,
) -> Result<Measurement, Error> {
    let parent = format!("{commit}~1");
    let index = |bp: &Path, cache: &Path| -> Result<(), Error> {
        let out = Command::new(bp)
            .arg("index")
            .arg(dir)
            .args(["--rev", &parent, "--level", "4"])
            .env("BLUEPRINT_CACHE_DIR", cache)
            .stderr(std::process::Stdio::null())
            .output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "`{} index {} --rev {parent} --level 4` failed",
                bp.display(),
                dir.display()
            )
            .into())
        }
    };
    let supports_diff = |bp: &Path| {
        Command::new(bp)
            .arg("--help")
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("bp diff"))
    };
    index(bp, head_cache)?;
    let base_bp = base_bp.filter(|b| supports_diff(b) && index(b, base_cache).is_ok());
    let spawn = |bp: &'_ Path, cache: &'_ Path| {
        let (dir, parent, commit) = (dir.to_path_buf(), parent.clone(), commit.to_owned());
        let (bp, cache) = (bp.to_path_buf(), cache.to_path_buf());
        move || -> Result<(), Error> {
            let status = Command::new(&bp)
                .arg("diff")
                .arg(&dir)
                .args([&parent, &commit])
                .env("BLUEPRINT_CACHE_DIR", &cache)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("`{} diff` failed: {status}", bp.display()).into())
            }
        }
    };
    let samples = 11;
    let mut head = spawn(bp, head_cache);
    let mut base = base_bp.map(|b| spawn(b, base_cache));
    let (value, base) = best_ms(
        samples,
        1,
        &mut head,
        base.as_mut()
            .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
    )?;
    Ok(Measurement {
        name: "diff_medium".into(),
        repo: Some("medium".into()),
        value,
        base,
        unit: "ms".into(),
        budget: Some("diff_medium".into()),
        samples,
        note: Some(format!(
            "`bp diff <checkout> <pinned>~1 <pinned>`, both indexed to level 4; fastest of {samples}"
        )),
        gated: true,
    })
}

/// `reindex_one_file` (#63), in process with the engine resident, as the
/// app will run it (DR 0008 decision 5): build the L1–L4 model of the
/// pinned commit once, then time applying a change to one analysed Rust
/// file, two blank lines added at the top and taken away again, so every
/// item in it moves. The budget row uses the file at the 90th percentile of
/// size, a large but ordinary file; `reindex_one_file_p99` reports the
/// 99th, which on these repositories is generated code.
/// `reindex_one_file_items` adds `pub fn new() {}` to the p90 file and takes
/// it away again, an edit to its items (#81) with a name many files
/// mention, held to the same budget. Medium
/// and large only. The engine is linked into bp-bench, so there is no base.
fn reindex_one_file(dest: &Path, repos: &[RefRepo]) -> Result<Vec<Measurement>, Error> {
    use blueprint_engine::resident::{Resident, Update};
    use blueprint_engine::source::Commit;
    use blueprint_model::RepoPath;

    let mut out = Vec::new();
    for r in repos.iter().filter(|r| r.name != "small") {
        let dir = repos::checkout_dir(dest, r);
        let (commit, _) = Commit::open(&dir, &r.commit).map_err(|e| e.to_string())?;
        let paths = commit.paths();
        let mut resident = Resident::build(&paths, &|p| commit.read(p));
        let analysed: std::collections::BTreeSet<&RepoPath> = resident
            .model()
            .nodes()
            .filter(|n| n.code.is_some())
            .map(|n| &n.source)
            .collect();
        let mut by_size: Vec<(usize, RepoPath)> = analysed
            .into_iter()
            .filter_map(|p| Some((commit.read(p)?.len(), p.clone())))
            .collect();
        by_size.sort();
        for (name, percentile, structural) in [
            ("reindex_one_file", 90, false),
            ("reindex_one_file_p99", 99, false),
            ("reindex_one_file_items", 90, true),
        ] {
            let (_, target) = by_size
                .get(by_size.len() * percentile / 100)
                .ok_or("no analysed Rust file")?
                .clone();
            let original = commit.read(&target).ok_or("unreadable target")?;
            // Inside items: two blank lines at the top. Items: a function
            // added at the end (#81).
            let edited = if structural {
                let mut e = original.clone();
                e.extend_from_slice(b"\npub fn new() {}\n");
                e
            } else {
                let mut e = b"\n\n".to_vec();
                e.extend_from_slice(&original);
                e
            };
            let expected = if structural {
                Update::Items
            } else {
                Update::InPlace
            };
            let samples = 11;
            let mut best = f64::INFINITY;
            for i in 0..samples * 2 {
                let now = if i % 2 == 0 { &edited } else { &original };
                let read = |p: &RepoPath| {
                    if *p == target {
                        Some(now.clone())
                    } else {
                        commit.read(p)
                    }
                };
                let start = std::time::Instant::now();
                let how = resident.update(&target, &read);
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                if how != expected {
                    return Err(
                        format!("{target}: the edit was not applied in place ({how:?})").into(),
                    );
                }
                best = best.min(ms);
            }
            out.push(Measurement {
                name: name.into(),
                repo: Some(r.name.clone()),
                value: best,
                base: None,
                unit: "ms".into(),
                budget: (r.name == "large" && percentile == 90).then(|| "reindex_one_file".into()),
                samples: samples * 2,
                note: Some(format!(
                    "in process, engine resident: {target} (p{percentile} of {} analysed files by size, {} KB) {}; fastest of {}",
                    by_size.len(),
                    original.len() / 1024,
                    if structural {
                        "gains or loses `pub fn new() {}`"
                    } else {
                        "gains or loses two leading blank lines"
                    },
                    samples * 2
                )),
                gated: true,
            });
        }
    }
    Ok(out)
}

/// Runs `bp index <dir> --rev <rev>` twice with its cache in `cache`, and
/// checks that the second run found the first one's entry. That is the same
/// lookup `bp model --rev` makes, so the timed reopen really reads the index.
fn bp_index(bp: &Path, dir: &Path, rev: &str, cache: &Path) -> Result<(), Error> {
    for expect in [" indexed (", " already indexed ("] {
        let out = Command::new(bp)
            .arg("index")
            .arg(dir)
            .args(["--rev", rev])
            .env("BLUEPRINT_CACHE_DIR", cache)
            .stderr(std::process::Stdio::null())
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || !stdout.contains(expect) {
            return Err(format!(
                "`{} index {}` did not report{expect}...: {} {stdout}",
                bp.display(),
                dir.display(),
                out.status
            )
            .into());
        }
    }
    Ok(())
}

/// Whether `bp --help` offers `--level` up to 4: older builds do not.
fn supports_level_4(bp: &Path) -> bool {
    Command::new(bp)
        .arg("--help")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("1 to 4"))
}

/// Whether `bp --help` offers `bp index --level 4`: older builds do not.
fn supports_index_level_4(bp: &Path) -> bool {
    Command::new(bp)
        .arg("--help")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("[--level 2|4]"))
}

/// `bp model --rev <rev> --level 4 --json` with its index in `cache`.
fn spawn_l4<'a>(
    bp: &'a Path,
    dir: &'a Path,
    rev: &'a str,
    cache: &'a Path,
) -> impl FnMut() -> Result<(), Error> + 'a {
    move || {
        let status = Command::new(bp)
            .arg("model")
            .arg(dir)
            .args(["--rev", rev, "--level", "4", "--json"])
            .env("BLUEPRINT_CACHE_DIR", cache)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "`{} model {} --level 4` failed: {status}",
                bp.display(),
                dir.display()
            )
            .into())
        }
    }
}

fn spawn_reopen<'a>(
    bp: &'a Path,
    dir: &'a Path,
    rev: &'a str,
    cache: &'a Path,
) -> impl FnMut() -> Result<(), Error> + 'a {
    move || {
        let status = Command::new(bp)
            .arg("model")
            .arg(dir)
            .args(["--rev", rev, "--json"])
            .env("BLUEPRINT_CACHE_DIR", cache)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "`{} model {} --rev {rev}` failed: {status}",
                bp.display(),
                dir.display()
            )
            .into())
        }
    }
}

/// The §4 budget the L1–L2 model of each reference repository is held to.
fn index_budget(repo: &str) -> Option<String> {
    match repo {
        "medium" => Some("index_medium_l2".into()),
        "large" => Some("index_large_l2".into()),
        _ => None,
    }
}

fn measure(
    bp: &Path,
    base_bp: Option<&Path>,
    app: Option<&Path>,
    base_app: Option<&Path>,
    dest: &Path,
    repos: &[RefRepo],
) -> Result<Vec<Measurement>, Error> {
    let mut out = Vec::new();

    if let Some(app) = app {
        out.push(cold_start(app, base_app)?);
        if let Some(medium) = repos.iter().find(|r| r.name == "medium") {
            out.extend(ui_session(app, &repos::checkout_dir(dest, medium))?);
        }
    }

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

    out.push(store_reopen()?);
    out.extend(open_50k_file());

    // L1–L2 of each reference checkout through `bp model` (#30). Read from
    // the working tree until commits can be read (#24); the budget is the
    // §4 "L1–L2 on screen" one. Fewer samples on the large repository,
    // whose runs take long enough that timer noise is negligible.
    for r in repos {
        let dir = repos::checkout_dir(dest, r);
        let samples = if r.name == "large" { 3 } else { 7 };
        let mut head = spawn_model(bp, &dir);
        let mut base = base_bp.map(|b| spawn_model(b, &dir));
        let (value, base) = best_ms(
            samples,
            1,
            &mut head,
            base.as_mut()
                .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
        )?;
        out.push(Measurement {
            name: "index_l1_l2".into(),
            repo: Some(r.name.clone()),
            value,
            base,
            unit: "ms".into(),
            budget: index_budget(&r.name),
            samples,
            note: Some(format!(
                "`bp model <checkout> --json`, fastest of {samples}; warm cache, read from the working tree"
            )),
            gated: true,
        });
    }

    // Reopening an indexed repository (#28): `bp index` the pinned commit
    // once, then time `bp model --rev` reading it back, against the §4
    // "open a previously indexed repository" budget. Each build gets its own
    // cache. A base build without `bp index` is not compared.
    // The full L1–L4 model of the pinned commit (#61): every Rust file
    // parsed, held to the §4 "first index" budgets. The cache is empty, so
    // no facts are reused. A base build without level 4 is not compared.
    let empty = std::env::temp_dir().join(format!("bp-bench-empty-{}", std::process::id()));
    for r in repos {
        let dir = repos::checkout_dir(dest, r);
        let samples = if r.name == "large" { 1 } else { 3 };
        let base_bp = base_bp.filter(|b| supports_level_4(b));
        let mut head = spawn_l4(bp, &dir, &r.commit, &empty);
        let mut base = base_bp.map(|b| spawn_l4(b, &dir, &r.commit, &empty));
        let (value, base) = best_ms(
            samples,
            1,
            &mut head,
            base.as_mut()
                .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
        )?;
        out.push(Measurement {
            name: "model_l4".into(),
            repo: Some(r.name.clone()),
            value,
            base,
            unit: "ms".into(),
            budget: match r.name.as_str() {
                "medium" => Some("index_medium".into()),
                "large" => Some("index_large".into()),
                _ => None,
            },
            samples,
            note: Some(format!(
                "`bp model <checkout> --rev <pinned> --level 4 --json`, fastest of {samples}; warm OS cache"
            )),
            gated: true,
        });
    }

    let cache = std::env::temp_dir().join(format!("bp-bench-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let reopened = open_indexed(bp, base_bp, dest, repos, &cache).and_then(|mut m| {
        m.extend(first_index(bp, base_bp, dest, repos, &cache.join("first"))?);
        m.extend(deep_index(bp, base_bp, dest, repos, &cache.join("deep"))?);
        Ok(m)
    });
    let _ = std::fs::remove_dir_all(&cache);
    out.extend(reopened?);
    out.extend(reindex_one_file(dest, repos)?);

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

/// Entries in the synthetic store: one per file of the large reference
/// repository.
const STORE_ENTRIES: u32 = 64_465;

/// Reopening the persisted index (#28): open a store directory holding one
/// segment, validate it, and look up every key. This is the storage part of
/// the 200 ms reopen budget; the budget itself needs the L1–L2 model on top.
/// Samples of the app's cold start. Each is a fresh process; the first run
/// after a build also fills the font and shader caches, so `best_ms` runs it
/// once beforehand without counting it.
const COLD_START_SAMPLES: usize = 7;

/// Launches `app --cold-start-report <file>`: the app opens its window, writes
/// the time from `main` to the first presented frame and quits (#88). The
/// wall time of the whole process is what is measured, so process creation,
/// dynamic loading and the shutdown after the frame are included.
fn spawn_app(app: &Path, report: PathBuf) -> impl FnMut() -> Result<(), Error> + '_ {
    move || {
        let _ = std::fs::remove_file(&report);
        let status = Command::new(app)
            .arg("--cold-start-report")
            .arg(&report)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err(format!("`{} --cold-start-report` failed: {status}", app.display()).into());
        }
        // The report proves a frame was presented, not just that the process exited.
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report)?)?;
        json["cold_start_ms"]
            .as_f64()
            .ok_or("cold-start report has no `cold_start_ms`")?;
        Ok(())
    }
}

/// The §4 cold-start row, compared with the base build's app when the base
/// commit has one (DR 0002).
fn cold_start(app: &Path, base_app: Option<&Path>) -> Result<Measurement, Error> {
    let tmp = |which: &str| {
        std::env::temp_dir().join(format!("bp-bench-cold-{which}-{}.json", std::process::id()))
    };
    let mut head = spawn_app(app, tmp("head"));
    let mut base = base_app.map(|b| spawn_app(b, tmp("base")));
    let (value, base) = best_ms(
        COLD_START_SAMPLES,
        1,
        &mut head,
        base.as_mut()
            .map(|b| b as &mut dyn FnMut() -> Result<(), Error>),
    )?;
    let _ = std::fs::remove_file(tmp("head"));
    let _ = std::fs::remove_file(tmp("base"));
    Ok(Measurement {
        name: "cold_start".into(),
        repo: None,
        value,
        base,
        unit: "ms".into(),
        budget: Some("cold_start".into()),
        samples: COLD_START_SAMPLES,
        note: Some(format!(
            "`blueprint --cold-start-report`: process start to first frame and exit, fastest of {COLD_START_SAMPLES} after one unmeasured run"
        )),
        gated: true,
    })
}

/// The scripted UI session (#95): open the review the app suggests on the
/// checkout, drill down three levels and back, open a node's code, scroll
/// it, then move about the map, pan and zoom it, each key pressed once the
/// last one's effect is painted.
const UI_SCRIPT: &str = "down,up,enter,enter,enter,enter,backspace,enter,c,pagedown,pagedown,down,n,p,right,escape,tab,right,left,down,up,-,ctrl-right,ctrl-down,=,=,0,backspace";

/// Sessions measured, after one unmeasured session that indexes the
/// checkout.
const UI_SESSIONS: usize = 3;

/// One timed key: its action, whether it opened a code view, and the
/// milliseconds to its first painted frame and to its settled one.
type KeyTime = (String, bool, f64, f64);

/// One timed session: its `--timings` keys.
fn ui_run(app: &Path, checkout: &Path, cache: &Path) -> Result<Vec<KeyTime>, Error> {
    let dir = std::env::temp_dir();
    let report = dir.join(format!("bp-bench-ui-review-{}.json", std::process::id()));
    let timings = dir.join(format!("bp-bench-ui-timings-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&timings);
    let out = Command::new(app)
        .arg(checkout)
        .args(["--script", UI_SCRIPT, "--review-report"])
        .arg(&report)
        .arg("--timings")
        .arg(&timings)
        .env("BLUEPRINT_CACHE_DIR", cache)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()?;
    let _ = std::fs::remove_file(&report);
    if !out.status.success() {
        let _ = std::fs::remove_file(&timings);
        return Err(format!(
            "`{} --timings` failed: {}: {}",
            app.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&timings)?)?;
    let _ = std::fs::remove_file(&timings);
    let keys = json["keys"].as_array().ok_or("timings have no `keys`")?;
    // Every scripted key must have been pressed and timed, or the session
    // went off script and its rows would mean nothing.
    let expected = UI_SCRIPT.split(',').count();
    if keys.len() != expected {
        return Err(format!("the UI session timed {} of {expected} keys", keys.len()).into());
    }
    keys.iter()
        .map(|k| {
            Ok((
                k["action"].as_str().unwrap_or("").to_owned(),
                k["opened_code"].as_bool().unwrap_or(false),
                k["first_frame_ms"].as_f64().ok_or("no first_frame_ms")?,
                k["settled_ms"].as_f64().ok_or("no settled_ms")?,
            ))
        })
        .collect()
}

/// Actions that scroll or move about without new data: their first frame
/// is a frame time (§4 scrolling and pan budgets).
fn scrolls(action: &str) -> bool {
    matches!(
        action,
        "list.up"
            | "list.down"
            | "list.first"
            | "list.last"
            | "map.left"
            | "map.right"
            | "map.up"
            | "map.down"
            | "map.next"
            | "map.pan_left"
            | "map.pan_right"
            | "map.pan_up"
            | "map.pan_down"
            | "map.zoom_in"
            | "map.zoom_out"
            | "map.zoom_reset"
            | "code.up"
            | "code.down"
            | "code.page_up"
            | "code.page_down"
            | "code.first"
            | "code.last"
            | "code.next_change"
            | "code.previous_change"
            | "code.scroll_left"
            | "code.scroll_right"
    )
}

/// The §4 interaction rows from [`UI_SESSIONS`] scripted sessions on the
/// medium checkout: each row is the worst key of its kind in a session, and
/// the lowest of those over the sessions is kept (DR 0002). Times end with
/// the paint of the frame that shows the key: the scene is built, and the
/// renderer's rasterizing and presenting are not included.
fn ui_session(app: &Path, checkout: &Path) -> Result<Vec<Measurement>, Error> {
    let cache = std::env::temp_dir().join(format!("bp-bench-ui-cache-{}", std::process::id()));
    let result = ui_sessions(app, checkout, &cache);
    let _ = std::fs::remove_dir_all(&cache);
    result
}

fn ui_sessions(app: &Path, checkout: &Path, cache: &Path) -> Result<Vec<Measurement>, Error> {
    ui_run(app, checkout, cache)?;
    let mut best: [f64; 3] = [f64::INFINITY; 3];
    let mut drills = 0;
    for _ in 0..UI_SESSIONS {
        let keys = ui_run(app, checkout, cache)?;
        let worst = |pick: &dyn Fn(&KeyTime) -> Option<f64>| {
            keys.iter().filter_map(pick).fold(0.0_f64, f64::max)
        };
        let drill = worst(&|k| {
            (matches!(k.0.as_str(), "map.drill_down" | "map.drill_up") && !k.1).then_some(k.3)
        });
        let input = worst(&|k| Some(k.2));
        let frame = worst(&|k| scrolls(&k.0).then_some(k.2));
        let session_drills = keys
            .iter()
            .filter(|k| matches!(k.0.as_str(), "map.drill_down" | "map.drill_up") && !k.1)
            .count();
        if session_drills == 0 || !keys.iter().any(|k| scrolls(&k.0)) {
            return Err("the UI session drilled or scrolled nothing: off script".into());
        }
        drills = if drills == 0 {
            session_drills
        } else {
            drills.min(session_drills)
        };
        for (b, v) in best.iter_mut().zip([drill, input, frame]) {
            *b = b.min(v);
        }
    }
    let row = |name: &str, value: f64, note: String| Measurement {
        name: name.into(),
        repo: Some("medium".into()),
        value,
        base: None,
        unit: "ms".into(),
        budget: Some(name.into()),
        samples: UI_SESSIONS,
        note: Some(note),
        gated: false,
    };
    let session = format!("best of {UI_SESSIONS} scripted sessions (`blueprint --timings`)");
    Ok(vec![
        row(
            "drill",
            best[0],
            format!(
                "slowest of at least {drills} drills, from the key to the view laid out, plus the paint of the frame showing it; {session}"
            ),
        ),
        row(
            "input_latency",
            best[1],
            format!(
                "slowest key of the session to the end of the paint of its first frame (scene built; rasterizing and presenting are the renderer's); {session}"
            ),
        ),
        row(
            "frame_time",
            best[2],
            format!("slowest scroll or move key to its painted frame; {session}"),
        ),
        row(
            "frame_time_120hz",
            best[2],
            format!("the frame_time row against the 120 Hz budget; {session}"),
        ),
    ])
}

/// A Rust file of `lines` lines, the same every run: functions of ten lines
/// with comments, strings, calls and types, so highlighting has work.
fn synthetic_rust(lines: usize, variant: bool) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(lines * 40);
    for i in 0..lines / 10 {
        // Every 100th function differs in head: a one-in-a-hundred change.
        let changed = variant && i % 100 == 7;
        let _ = write!(
            s,
            "/// Function {i}, documented.\n\
             pub fn item_{i}(value: u64, name: &str) -> Option<String> {{\n\
             \x20   // A comment inside.\n\
             \x20   let label = format!(\"{{name}}-{i}\");\n\
             \x20   if value > {limit} {{\n\
             \x20       return None;\n\
             \x20   }}\n\
             \x20   helper::check(value, &label);\n\
             \x20   Some(label)\n\
             }}\n",
            limit = if changed { i + 1 } else { i },
        );
    }
    s.into_bytes()
}

/// `open_50k_file` (§4, #93): what opening a 50,000-line file at L4 asks of
/// the engine before the file is on screen, in process: the side-by-side
/// diff of two versions that differ in one function in a hundred. The view
/// lays out only the lines on screen. Highlighting follows in a second
/// answer, as it parses both files whole; `highlight_50k_file` reports it.
fn open_50k_file() -> Vec<Measurement> {
    let lines = 50_000;
    let (base, head) = (synthetic_rust(lines, false), synthetic_rust(lines, true));
    let path = blueprint_model::RepoPath::parse("src/big.rs")
        .unwrap_or_else(|_| blueprint_model::RepoPath::root());
    let mut out = Vec::new();
    for (name, highlighted, budget, samples) in [
        ("open_50k_file", false, Some("open_50k_file"), 11),
        ("highlight_50k_file", true, None, 5),
    ] {
        let mut best = f64::INFINITY;
        let mut rows = 0;
        for _ in 0..samples + 1 {
            let start = std::time::Instant::now();
            let d = blueprint_engine::source_diff::file_diff(
                &path,
                Some(&base),
                Some(&head),
                highlighted,
            );
            best = best.min(start.elapsed().as_secs_f64() * 1000.0);
            rows = d.rows.len();
            std::hint::black_box(d);
        }
        out.push(Measurement {
            name: name.into(),
            repo: None,
            value: best,
            base: None,
            unit: "ms".into(),
            budget: budget.map(Into::into),
            samples,
            note: Some(format!(
                "in process: side-by-side diff of a generated {lines}-line Rust file ({} KB, one function in a hundred changed, {rows} rows){}; fastest of {samples} after one unmeasured run",
                base.len() / 1024,
                if highlighted {
                    ", both sides highlighted (the second answer)"
                } else {
                    ", before highlighting (the first answer)"
                }
            )),
            gated: false,
        });
    }
    out
}

fn store_reopen() -> Result<Measurement, Error> {
    let dir = std::env::temp_dir().join(format!("bp-bench-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let result = store_reopen_in(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn store_reopen_in(dir: &Path) -> Result<Measurement, Error> {
    let key = |i: u32| {
        let mut k = [0u8; KEY_LEN];
        k[..4].copy_from_slice(&i.wrapping_mul(2_654_435_761).to_be_bytes());
        k[4..8].copy_from_slice(&i.to_be_bytes());
        Key(k)
    };
    let mut w = SegmentWriter::new();
    for i in 0..STORE_ENTRIES {
        // About 200 bytes per entry: the size of a small analysis summary.
        w.insert(key(i), vec![(i % 251) as u8; 150 + (i % 100) as usize]);
    }
    let mut store = Store::open(dir)?;
    store.add(&w)?;
    drop(store);
    let bytes: u64 = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok()?.metadata().ok())
        .map(|m| m.len())
        .sum();
    let samples = 11;
    let mut found = 0;
    let mut reopen = || -> Result<(), Error> {
        let store = Store::open(dir)?;
        found = (0..STORE_ENTRIES)
            .filter(|i| store.get(&key(*i)).is_some())
            .count();
        Ok(())
    };
    let (value, _) = best_ms(samples, 1, &mut reopen, None)?;
    if found != STORE_ENTRIES as usize {
        return Err(format!("store_reopen found {found} of {STORE_ENTRIES} keys").into());
    }
    Ok(Measurement {
        name: "store_reopen".into(),
        repo: None,
        value,
        base: None,
        unit: "ms".into(),
        budget: None,
        samples,
        note: Some(format!(
            "warm cache; open and checksum a {} MB segment, then look up all {STORE_ENTRIES} keys; storage share of the 200 ms reopen budget",
            bytes / 1_000_000
        )),
        gated: false,
    })
}
