//! Benchmarks for the performance budgets in REQUIREMENTS §4.
//!
//! `bp-bench fetch` checks out the pinned reference repositories and
//! `bp-bench run` measures the commit under test, optionally against a base
//! build on the same machine, and writes JSON and a markdown table.

pub mod budgets;
pub mod floor;
pub mod measure;
pub mod report;
pub mod repos;
