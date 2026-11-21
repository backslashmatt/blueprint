//! Which language analyser reads a file (REQUIREMENTS §3.4, DR 0008).

use blueprint_lang_rust::Rust;
use blueprint_lang_ts::{Tsx, TypeScript};
use blueprint_model::RepoPath;
use blueprint_model::lang::LanguageAnalyser;

/// Every analyser, in the order they are asked whether they handle a file.
pub static ANALYSERS: [&dyn LanguageAnalyser; 3] = [&Rust, &TypeScript, &Tsx];

/// The analyser that reads the file at `path`, if any.
#[must_use]
pub fn analyser_for(path: &RepoPath) -> Option<&'static dyn LanguageAnalyser> {
    ANALYSERS.iter().copied().find(|a| a.handles(path))
}

/// The analyser named `name` (its [`LanguageAnalyser::name`]).
#[must_use]
pub fn by_name(name: &str) -> Option<&'static dyn LanguageAnalyser> {
    ANALYSERS.iter().copied().find(|a| a.name() == name)
}

/// One number for every analyser's name and version together: it changes
/// whenever any analyser's output would.
#[must_use]
pub fn versions() -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for a in ANALYSERS {
        for b in a.name().bytes().chain(a.version().to_le_bytes()) {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
    }
    h
}

/// Whether the file at `path` is Rust: only Rust files have an L2 shape
/// that depends on their contents (`mod` items).
#[must_use]
pub fn is_rust(path: &RepoPath) -> bool {
    Rust.handles(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> RepoPath {
        RepoPath::parse(s).unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn each_file_goes_to_its_languages_analyser() {
        let name = |s: &str| analyser_for(&p(s)).map(|a| a.name());
        assert_eq!(name("src/lib.rs"), Some("rust"));
        assert_eq!(name("src/a.ts"), Some("ts"));
        assert_eq!(name("src/a.tsx"), Some("tsx"));
        assert_eq!(name("src/a.js"), Some("tsx"));
        assert_eq!(name("README.md"), None);
        for a in ANALYSERS {
            assert!(by_name(a.name()).is_some());
        }
    }
}
