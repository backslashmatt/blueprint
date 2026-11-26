//! Repository tooling. The layering rules of REQUIREMENTS §5.3 are enforced
//! by this crate's tests (`cargo test -p xtask`).
//!
//! ```text
//! cargo run -p xtask -- update-keygen
//! cargo run -p xtask -- release-manifest <version> <notes-file> <installer>...
//! cargo run -p xtask -- release-sign <manifest.json>
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
//! variable `UPDATE_SIGNING_KEY` and writes `manifest.json.sig`. When
//! `crates/update/release-key.txt` exists, the key must be its pair, so a
//! wrong secret fails the release instead of shipping updates no app accepts.

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("update-keygen") => update_keygen(),
        Some("release-manifest") => report(release_manifest()),
        Some("release-sign") => report(release_sign()),
        _ => {
            eprintln!(
                "usage: cargo run -p xtask -- update-keygen | release-manifest <version> <notes-file> <installer>... | release-sign <manifest.json>"
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
    println!("Secret (add as the repository secret UPDATE_SIGNING_KEY; keep it nowhere else):");
    println!("{secret_hex}");
    println!();
    println!("Public (commit as crates/update/release-key.txt):");
    println!("{}", blueprint_update::public_key_hex(&public));
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
    let path = std::env::args()
        .nth(2)
        .ok_or("release-sign <manifest.json>")?;
    let seed = std::env::var("UPDATE_SIGNING_KEY")
        .map_err(|_| "UPDATE_SIGNING_KEY is not set".to_owned())?;
    let secret = blueprint_update::parse_secret_key(&seed).map_err(|e| e.to_string())?;
    let public = secret.verifying_key();
    let committed = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../crates/update/release-key.txt"
    );
    if let Ok(text) = std::fs::read_to_string(committed) {
        let key = blueprint_update::parse_public_key(&text).map_err(|e| e.to_string())?;
        if key != public {
            return Err(
                "UPDATE_SIGNING_KEY is not the pair of crates/update/release-key.txt".into(),
            );
        }
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
