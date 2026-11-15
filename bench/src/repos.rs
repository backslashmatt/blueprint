//! The pinned reference repositories (`bench/repos.json`) and fetching them.
//!
//! Reference repositories are untrusted input (REQUIREMENTS §5.5): they are
//! only fetched and read. Hooks, every attribute-driven filter (LFS, and any
//! smudge driver defined in the user's or system git config), fsmonitor,
//! submodules and local-file transports are disabled, so nothing from them runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// One pinned reference repository.
#[derive(Debug, Clone, Deserialize)]
pub struct RefRepo {
    /// `small`, `medium` or `large`.
    pub name: String,
    /// Public clone URL.
    pub url: String,
    /// Full commit SHA the repository is pinned to.
    pub commit: String,
    /// Text lines at that commit, as counted by `git grep -I -c ''`.
    pub text_lines: u64,
    /// Tracked files at that commit.
    pub files: u64,
}

#[derive(Deserialize)]
struct Manifest {
    repos: Vec<RefRepo>,
}

/// Errors from reading the manifest or fetching.
#[derive(Debug)]
pub enum RepoError {
    /// The manifest could not be read.
    Io(std::io::Error),
    /// The manifest is not valid.
    Parse(serde_json::Error),
    /// A pinned commit is not a full 40-character hex SHA.
    BadCommit(String),
    /// A `git` step failed.
    Git(String),
}

impl std::fmt::Display for RepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepoError::Io(e) => write!(f, "reading manifest: {e}"),
            RepoError::Parse(e) => write!(f, "parsing manifest: {e}"),
            RepoError::BadCommit(c) => write!(f, "not a full commit SHA: {c}"),
            RepoError::Git(msg) => write!(f, "git: {msg}"),
        }
    }
}

impl std::error::Error for RepoError {}

/// Reads and validates `bench/repos.json`.
pub fn load(manifest: &Path) -> Result<Vec<RefRepo>, RepoError> {
    let text = std::fs::read_to_string(manifest).map_err(RepoError::Io)?;
    let m: Manifest = serde_json::from_str(&text).map_err(RepoError::Parse)?;
    for r in &m.repos {
        if r.commit.len() != 40 || !r.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RepoError::BadCommit(r.commit.clone()));
        }
    }
    Ok(m.repos)
}

/// Where a repository is checked out under `dest`.
pub fn checkout_dir(dest: &Path, repo: &RefRepo) -> PathBuf {
    dest.join(&repo.name)
}

/// Attributes written to `.git/info/attributes`, which take precedence over
/// the repository's own `.gitattributes`: no filter, end-of-line or encoding
/// conversion runs during checkout.
const NEUTRAL_ATTRIBUTES: &str = "* -filter -text -ident -working-tree-encoding\n";

/// Configuration passed to every git call: no hooks, no fsmonitor, no
/// local-file transports, no submodule recursion.
const SAFE_CONFIG: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.file.allow=never",
    "-c",
    "submodule.recurse=false",
];

/// Makes `dest/<name>` a clean checkout of exactly `repo.commit`, fetching
/// only that commit. Does nothing if it is already there and clean.
pub fn fetch(dest: &Path, repo: &RefRepo) -> Result<(), RepoError> {
    fetch_with(dest, repo, &Git::default())
}

/// Extra configuration and environment for git, used only by tests.
#[derive(Default)]
struct Git<'a> {
    config: &'a [&'a str],
    env: &'a [(&'a str, &'a Path)],
}

fn fetch_with(dest: &Path, repo: &RefRepo, extra: &Git<'_>) -> Result<(), RepoError> {
    if verify_with(dest, repo, extra).is_ok() {
        return Ok(());
    }
    let dir = checkout_dir(dest, repo);
    let io = |e: std::io::Error| RepoError::Git(e.to_string());
    std::fs::create_dir_all(&dir).map_err(io)?;
    git(&dir, extra, &["init", "--quiet"])?;
    std::fs::write(
        dir.join(".git").join("info").join("attributes"),
        NEUTRAL_ATTRIBUTES,
    )
    .map_err(io)?;
    git(
        &dir,
        extra,
        &[
            "fetch",
            "--quiet",
            "--depth",
            "1",
            "--no-recurse-submodules",
            &repo.url,
            &repo.commit,
        ],
    )?;
    git(
        &dir,
        extra,
        &["checkout", "--quiet", "--force", "--detach", "FETCH_HEAD"],
    )?;
    verify_with(dest, repo, extra)
}

/// Checks that `dest/<name>` is a clean checkout of exactly `repo.commit`.
pub fn verify(dest: &Path, repo: &RefRepo) -> Result<(), RepoError> {
    verify_with(dest, repo, &Git::default())
}

fn verify_with(dest: &Path, repo: &RefRepo, extra: &Git<'_>) -> Result<(), RepoError> {
    let dir = checkout_dir(dest, repo);
    let head = output(&dir, extra, &["rev-parse", "HEAD"])?;
    if head != repo.commit {
        return Err(RepoError::Git(format!(
            "{} is at {head}, expected {}",
            repo.name, repo.commit
        )));
    }
    let status = output(
        &dir,
        extra,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    if status.is_empty() {
        Ok(())
    } else {
        Err(RepoError::Git(format!(
            "{} has local modifications",
            repo.name
        )))
    }
}

fn command(dir: &Path, extra: &Git<'_>, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(SAFE_CONFIG)
        .args(extra.config)
        .args(args);
    cmd.env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (k, v) in extra.env {
        cmd.env(k, v);
    }
    cmd
}

fn output(dir: &Path, extra: &Git<'_>, args: &[&str]) -> Result<String, RepoError> {
    let out = command(dir, extra, args)
        .output()
        .map_err(|e| RepoError::Git(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(RepoError::Git(format!(
            "`git {}` failed: {}",
            args.join(" "),
            out.status
        )))
    }
}

fn git(dir: &Path, extra: &Git<'_>, args: &[&str]) -> Result<(), RepoError> {
    let status = command(dir, extra, args)
        .status()
        .map_err(|e| RepoError::Git(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(RepoError::Git(format!(
            "`git {}` failed: {status}",
            args.join(" ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_manifest_is_valid_and_has_three_sizes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("repos.json");
        let repos = load(&path).unwrap();
        let names: Vec<_> = repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["small", "medium", "large"]);
        assert!(repos.windows(2).all(|w| w[0].text_lines < w[1].text_lines));
    }

    /// A global git config defining a smudge filter that touches `marker`.
    /// Forward slashes, because backslashes are escapes in git config values.
    fn smudge_config(marker: &Path) -> String {
        let marker = marker.display().to_string().replace('\\', "/");
        format!("[filter \"evil\"]\n\tsmudge = \"touch '{marker}' && cat\"\n")
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// The repository's `.gitattributes` asks for a smudge filter that the
    /// user's global git config defines. Checking it out must not run it.
    #[test]
    fn fetch_pins_the_commit_and_runs_no_filter_named_by_the_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("filter-ran");
        let global = tmp.path().join("gitconfig");
        let smudge = smudge_config(&marker);
        std::fs::write(&global, smudge).unwrap();

        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        run(&src, &["init", "--quiet"]);
        std::fs::write(src.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        std::fs::write(src.join("a.txt"), "hello\n").unwrap();
        run(&src, &["add", "."]);
        run(
            &src,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "-m",
                "c",
            ],
        );
        let commit = run(&src, &["rev-parse", "HEAD"]);
        let repo = RefRepo {
            name: "r".into(),
            url: src.display().to_string(),
            commit,
            text_lines: 1,
            files: 2,
        };

        // The test fetches from a local path, so it re-allows the file
        // transport; everything else is exactly what `fetch` does.
        let extra = Git {
            config: &["-c", "protocol.file.allow=always"],
            env: &[("GIT_CONFIG_GLOBAL", &global)],
        };
        let dest = tmp.path().join("dest");
        fetch_with(&dest, &repo, &extra).unwrap();
        assert!(
            !marker.exists(),
            "a smudge filter named by the repository ran"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("r/a.txt")).unwrap(),
            "hello\n"
        );
        verify_with(&dest, &repo, &extra).unwrap();

        std::fs::write(dest.join("r/a.txt"), "changed\n").unwrap();
        assert!(
            verify_with(&dest, &repo, &extra).is_err(),
            "a modified checkout must fail verification"
        );
    }

    /// Guards the test above: without the neutral attributes the filter does run.
    #[test]
    fn the_filter_would_run_on_an_ordinary_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("filter-ran");
        let global = tmp.path().join("gitconfig");
        let smudge = smudge_config(&marker);
        std::fs::write(&global, smudge).unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        run(&src, &["init", "--quiet"]);
        std::fs::write(src.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        std::fs::write(src.join("a.txt"), "hello\n").unwrap();
        run(&src, &["add", "."]);
        run(
            &src,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "-m",
                "c",
            ],
        );
        let out = Command::new("git")
            .args(["clone", "--quiet"])
            .arg(&src)
            .arg(tmp.path().join("plain"))
            .env("GIT_CONFIG_GLOBAL", &global)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            marker.exists(),
            "the control checkout should have run the filter"
        );
    }

    #[test]
    fn short_commit_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.json");
        std::fs::write(
            &p,
            r#"{"repos":[{"name":"s","url":"u","commit":"abc","text_lines":1,"files":1}]}"#,
        )
        .unwrap();
        assert!(matches!(load(&p), Err(RepoError::BadCommit(_))));
    }
}
