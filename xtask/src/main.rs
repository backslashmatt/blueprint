//! Repository tooling. The layering rules of REQUIREMENTS §5.3 are enforced
//! by this crate's tests (`cargo test -p xtask`).
//!
//! ```text
//! cargo run -p xtask -- update-keygen
//! ```
//!
//! `update-keygen` makes the key pair that signs update manifests (#179,
//! DR 0018), from the operating system's randomness. It prints the secret
//! seed, for the repository secret `UPDATE_SIGNING_KEY`, and the public key,
//! for `crates/update/release-key.txt`. Nothing is written to disk.

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("update-keygen") => update_keygen(),
        _ => {
            eprintln!("usage: cargo run -p xtask -- update-keygen");
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
