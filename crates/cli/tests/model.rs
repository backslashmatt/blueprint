//! `bp model` against a fixture repository built at test time, with the JSON
//! snapshotted in `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture() -> PathBuf {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("bp-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let files: &[(&str, &str)] = &[
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\n[dependencies]\ncore = { path = \"../core\" }\n",
        ),
        ("crates/app/src/main.rs", "mod cli;\nfn main() {}\n"),
        ("crates/app/src/cli.rs", ""),
        ("crates/core/Cargo.toml", "[package]\nname = \"core\"\n"),
        (
            "crates/core/src/lib.rs",
            "pub mod graph;\npub(crate) mod util;\n",
        ),
        ("crates/core/src/graph.rs", ""),
        ("crates/core/src/util.rs", ""),
        ("web/package.json", r#"{ "name": "web" }"#),
        ("docs/guide.md", "# guide\n"),
        // Skipped: version control and build output.
        (".git/HEAD", "ref: refs/heads/main\n"),
        (
            "target/debug/junk/Cargo.toml",
            "[package]\nname = \"junk\"\n",
        ),
        ("web/node_modules/dep/package.json", r#"{ "name": "dep" }"#),
    ];
    for (path, body) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    dir
}

fn bp(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bp"))
        .args(args)
        .output()
        .expect("run bp")
}

fn snapshot(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "no snapshot {}; run with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "snapshot {name} differs; run with UPDATE_SNAPSHOTS=1 if intended"
    );
}

#[test]
fn model_json_and_text_are_deterministic_and_snapshotted() {
    let dir = fixture();
    let d = dir.to_str().unwrap();
    let json = bp(&["model", d, "--json"]);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let json = String::from_utf8(json.stdout).unwrap();
    snapshot("model_fixture.json", &json);
    assert_eq!(
        json,
        String::from_utf8(bp(&["model", d, "--json"]).stdout).unwrap(),
        "same input, same bytes"
    );
    // The JSON is a valid model.
    serde_json::from_str::<blueprint_model::Model>(&json).unwrap();
    assert!(
        !json.contains("junk") && !json.contains("\"dep\""),
        "target/ and node_modules/ are skipped"
    );

    let l1 = String::from_utf8(bp(&["model", d, "--level", "1"]).stdout).unwrap();
    snapshot("model_fixture_l1.txt", &l1);
    assert!(l1.lines().all(|l| l.starts_with("L1:")), "{l1}");
}

#[test]
fn missing_directory_fails_with_exit_1_and_bad_usage_with_exit_2() {
    let out = bp(&["model", "/no/such/directory/for/bp"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot read"));
    let out = bp(&["model", ".", "--level", "7"]);
    assert_eq!(out.status.code(), Some(2));
}
