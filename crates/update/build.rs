//! Compiles the release public key in when `release-key.txt` is committed
//! (#179, DR 0018): without it the app has no key and does not check for
//! updates.

fn main() {
    println!("cargo:rerun-if-changed=release-key.txt");
    let key = std::fs::read_to_string("release-key.txt").unwrap_or_default();
    let key = key.trim();
    // Only 64 hex digits are passed on; parse_public_key checks the point.
    if key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
        println!("cargo:rustc-env=BLUEPRINT_RELEASE_KEY={key}");
    }
}
