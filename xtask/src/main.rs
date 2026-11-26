//! Repository tooling. The layering rules of REQUIREMENTS §5.3 are enforced
//! by this crate's tests (`cargo test -p xtask`).
//!
//! ```text
//! cargo run -p xtask -- update-keygen
//! cargo run -p xtask -- release-manifest <version> <notes-file> <installer>...
//! cargo run -p xtask -- release-sign [--expect-public <hex>] <manifest.json>
//! ```
//!
//! `update-keygen` makes the key pair that signs update manifests (#179,
//! DR 0018), from the operating system's randomness. It prints the secret
//! seed, for the repository secret `UPDATE_SIGNING_KEY`, and the public key,
//! for `crates/update/release-key.txt`. Nothing is written to disk.
//!
//! `release-manifest` writes `manifest.json` beside the installers (#179):
//! one per platform, hashed and sized, with its URL on the release
//! `v<version>`. `release-sign` signs it with the seed in the environment
//! variable `UPDATE_SIGNING_KEY` and writes `manifest.json.sig`. The key must
//! be the pair of `crates/update/release-key.txt`, which must exist, so a
//! wrong or missing key fails the release instead of shipping updates no app
//! accepts; CI's dry run names its throwaway public key with
//! `--expect-public` instead.

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("update-keygen") => update_keygen(),
        Some("release-manifest") => report(release_manifest()),
        Some("release-sign") => report(release_sign()),
        _ => {
            eprintln!(
                "usage: cargo run -p xtask -- update-keygen | release-manifest <version> <notes-file> <installer>... | release-sign [--expect-public <hex>] <manifest.json>"
            );
            ExitCode::from(2)
        }
    }
}

fn update_keygen() -> ExitCode {
    let mut seed = [0u8; 32];
    if let Err(e) = getrandom::fill(&mut seed) {
        eprintln!("xtask: no randomness from the operating system: {e}");
        return ExitCode::FAILURE;
    }
    let (_, public) = blueprint_update::keypair_from_seed(&seed);
    let secret_hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    // One labelled value per line, so scripts read them by label, never by position.
    println!("# secret: add as the repository secret UPDATE_SIGNING_KEY; keep it nowhere else");
    println!("secret {secret_hex}");
    println!("# public: commit as crates/update/release-key.txt");
    println!("public {}", blueprint_update::public_key_hex(&public));
    seed.fill(0);
    ExitCode::SUCCESS
}

fn report(r: Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn release_manifest() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let [version, notes, installers @ ..] = args.as_slice() else {
        return Err("release-manifest <version> <notes-file> <installer>...".into());
    };
    if installers.is_empty() {
        return Err("no installers".into());
    }
    let notes = std::fs::read_to_string(notes).map_err(|e| format!("{notes}: {e}"))?;
    let m = xtask::release::write_manifest(version, notes.trim(), installers)?;
    let dir = std::path::Path::new(&installers[0])
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let out = dir.join("manifest.json");
    std::fs::write(&out, m.to_json().map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "{}: {} installers of {version}",
        out.display(),
        m.assets.len()
    );
    Ok(())
}

fn release_sign() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    // The public key the signing key must pair with: the committed one, or for CI's dry run
    // a throwaway one given explicitly. There is no way to skip the check.
    let (path, expected) = match args.as_slice() {
        [path] => {
            let committed = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../crates/update/release-key.txt"
            );
            let text = std::fs::read_to_string(committed).map_err(|e| {
                format!("crates/update/release-key.txt (the release public key): {e}")
            })?;
            (path.clone(), text)
        }
        [flag, key, path] if flag == "--expect-public" => (path.clone(), key.clone()),
        _ => return Err("release-sign [--expect-public <hex>] <manifest.json>".into()),
    };
    let expected = blueprint_update::parse_public_key(&expected).map_err(|e| e.to_string())?;
    let seed = std::env::var("UPDATE_SIGNING_KEY")
        .map_err(|_| "UPDATE_SIGNING_KEY is not set".to_owned())?;
    let secret = blueprint_update::parse_secret_key(&seed).map_err(|e| e.to_string())?;
    let public = secret.verifying_key();
    if public != expected {
        return Err("UPDATE_SIGNING_KEY is not the pair of the expected public key".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let sig = blueprint_update::sign(&bytes, &secret);
    // What the app will do with it.
    blueprint_update::verify(&bytes, &sig, &public).map_err(|e| e.to_string())?;
    let out = format!("{path}.sig");
    std::fs::write(&out, format!("{sig}\n")).map_err(|e| format!("{out}: {e}"))?;
    println!(
        "{out}: signed by {}",
        blueprint_update::public_key_hex(&public)
    );
    Ok(())
}
