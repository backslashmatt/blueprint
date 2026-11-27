//! Asks GitHub for the latest release, as the app will (DR 0019):
//! `cargo run -p blueprint-update --features net --example check -- <current> <platform>`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let current = args.next().unwrap_or_else(|| "0.1.0".into());
    let platform = args.next().unwrap_or_else(|| "linux-x86_64-deb".into());
    // Any key will do while nothing is released: a 404 is answered before any signature.
    let (_, public) = blueprint_update::keypair_from_seed(&[0; 32]);
    let https = blueprint_update::net::Https::new(&current);
    match blueprint_update::check(&https, &current, &platform, &public) {
        Ok(found) => {
            println!("{found:?}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
