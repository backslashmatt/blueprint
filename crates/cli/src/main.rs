//! `bp`: the headless Blueprint front end. Everything the app can show, this
//! CLI can print (REQUIREMENTS §5.3).

use std::process::ExitCode;

const USAGE: &str = "usage: bp [--version | --help]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("bp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("bp: unknown command `{other}`\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
