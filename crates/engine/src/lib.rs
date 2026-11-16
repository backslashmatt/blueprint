//! The headless engine (REQUIREMENTS §5.3): indexing, incremental queries,
//! git, storage and diffing. It has no UI dependency and everything it can
//! show is reachable from the CLI.

pub mod git;
