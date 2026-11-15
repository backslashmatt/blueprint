//! Benchmark results, their comparison against the §4 budgets and against
//! the base commit measured in the same run, and the markdown table that CI
//! publishes and the agent copies into the agent log.

use serde::{Deserialize, Serialize};

use crate::budgets::{BUDGETS, budget};

/// Format version of [`Report`] JSON.
pub const SCHEMA: u32 = 2;

/// Largest allowed slowdown of a gated benchmark against the base, as a
/// fraction (REQUIREMENTS §9.1).
pub const MAX_REGRESSION: f64 = 0.05;

/// One measured value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// Benchmark name, unique together with `repo`.
    pub name: String,
    /// Reference repository the value was measured on, if any.
    pub repo: Option<String>,
    /// Value for the commit under test: the fastest sample (DR 0002). Lower is better.
    pub value: f64,
    /// Value for the base commit, measured in the same run on the same machine
    /// with samples interleaved, if a base was given.
    pub base: Option<f64>,
    /// Unit of `value` and `base`.
    pub unit: String,
    /// Id of the §4 budget this value is held to, if any.
    pub budget: Option<String>,
    /// Number of samples per commit.
    pub samples: usize,
    /// Free-form context, such as the size of what was measured.
    pub note: Option<String>,
    /// Whether a regression against the base fails the run. Reference
    /// measurements (the I/O floor measures the file system, not Blueprint)
    /// are reported but not gated. Budgets are enforced either way.
    pub gated: bool,
}

/// A full benchmark run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Always [`SCHEMA`].
    pub schema: u32,
    /// Commit under test, if known.
    pub commit: Option<String>,
    /// Base commit it was compared with, if any.
    pub base_commit: Option<String>,
    /// The machine, so numbers from different runner classes are not confused.
    pub runner: String,
    /// Everything that was measured.
    pub measurements: Vec<Measurement>,
}

/// How one measurement compares.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Within budget and not regressed.
    Ok,
    /// The value or base is NaN, infinite or negative.
    Invalid,
    /// The measurement's unit differs from its budget's unit.
    UnitMismatch {
        /// The budget's unit.
        budget_unit: &'static str,
    },
    /// Exceeds its §4 budget.
    OverBudget {
        /// The budget that was exceeded.
        limit: f64,
    },
    /// Slower than the base by more than [`MAX_REGRESSION`].
    Regressed {
        /// Relative change, e.g. 0.06 for 6% slower.
        change: f64,
    },
}

fn valid(x: f64) -> bool {
    x.is_finite() && x >= 0.0
}

/// Relative change of `value` against `base`, if both are comparable.
pub fn change(m: &Measurement) -> Option<f64> {
    m.base
        .filter(|b| valid(*b) && *b > 0.0)
        .map(|b| (m.value - b) / b)
}

/// Judges one measurement against its budget and its base.
pub fn judge(m: &Measurement) -> Verdict {
    if !valid(m.value) || m.base.is_some_and(|b| !valid(b)) {
        return Verdict::Invalid;
    }
    if let Some(b) = m.budget.as_deref().and_then(budget) {
        if b.unit != m.unit {
            return Verdict::UnitMismatch {
                budget_unit: b.unit,
            };
        }
        if m.value > b.limit {
            return Verdict::OverBudget { limit: b.limit };
        }
    }
    match change(m) {
        Some(c) if m.gated && c > MAX_REGRESSION => Verdict::Regressed { change: c },
        _ => Verdict::Ok,
    }
}

fn label(m: &Measurement) -> String {
    match &m.repo {
        Some(r) => format!("{} ({r})", m.name),
        None => m.name.clone(),
    }
}

/// Every measurement that fails, as a human-readable line.
pub fn failures(report: &Report) -> Vec<String> {
    report
        .measurements
        .iter()
        .filter_map(|m| {
            let what = match judge(m) {
                Verdict::Ok => return None,
                Verdict::Invalid => format!("invalid value {} / base {:?}", m.value, m.base),
                Verdict::UnitMismatch { budget_unit } => {
                    format!("measured in {} but its budget is in {budget_unit}", m.unit)
                }
                Verdict::OverBudget { limit } => format!(
                    "{:.2} {} exceeds budget {limit} {}",
                    m.value, m.unit, m.unit
                ),
                Verdict::Regressed { change } => format!(
                    "{:.2} {} is {:+.1}% against base",
                    m.value,
                    m.unit,
                    change * 100.0
                ),
            };
            Some(format!("{}: {what}", label(m)))
        })
        .collect()
}

/// The markdown table: every measurement, then every §4 row not yet measured.
pub fn markdown(report: &Report) -> String {
    let mut out = format!("### Benchmarks\n\nRunner: {}\n\n", report.runner);
    if let Some(base) = &report.base_commit {
        out.push_str(&format!(
            "Compared with base `{base}`, measured in the same run.\n\n"
        ));
    } else {
        out.push_str(
            "No base commit: regressions are **not compared** in this run, only budgets.\n\n",
        );
    }
    out.push_str("| Benchmark | Repo | Value | Base | Change | Budget | Status | Note |\n|---|---|---|---|---|---|---|---|\n");
    for m in &report.measurements {
        let limit = m
            .budget
            .as_deref()
            .and_then(budget)
            .map_or("—".into(), |b| format!("{} {}", b.limit, b.unit));
        let base = m.base.map_or("—".into(), |b| format!("{b:.2} {}", m.unit));
        let delta = match change(m) {
            Some(c) => format!("{:+.1}%", c * 100.0),
            None => "not compared".into(),
        };
        let status = match judge(m) {
            Verdict::Ok if m.gated => "ok",
            Verdict::Ok => "ok (not gated)",
            Verdict::Invalid => "**invalid**",
            Verdict::UnitMismatch { .. } => "**unit mismatch**",
            Verdict::OverBudget { .. } => "**over budget**",
            Verdict::Regressed { .. } => "**regressed**",
        };
        out.push_str(&format!(
            "| {} | {} | {:.2} {} | {base} | {delta} | {limit} | {status} | {} |\n",
            m.name,
            m.repo.as_deref().unwrap_or("—"),
            m.value,
            m.unit,
            m.note.as_deref().unwrap_or("")
        ));
    }
    let pending: Vec<_> = BUDGETS
        .iter()
        .filter(|b| {
            !report
                .measurements
                .iter()
                .any(|m| m.budget.as_deref() == Some(b.id))
        })
        .collect();
    if !pending.is_empty() {
        out.push_str("\n§4 budgets not yet measurable (the operation does not exist yet):\n\n");
        for b in pending {
            out.push_str(&format!("- {} — {} {}\n", b.operation, b.limit, b.unit));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(value: f64, base: Option<f64>, unit: &str, budget: Option<&str>) -> Measurement {
        Measurement {
            name: "x".into(),
            repo: Some("small".into()),
            value,
            base,
            unit: unit.into(),
            budget: budget.map(Into::into),
            samples: 5,
            note: None,
            gated: true,
        }
    }

    fn report(ms: Vec<Measurement>, base_commit: Option<&str>) -> Report {
        Report {
            schema: SCHEMA,
            commit: None,
            base_commit: base_commit.map(Into::into),
            runner: "test".into(),
            measurements: ms,
        }
    }

    #[test]
    fn over_budget_fails_with_or_without_base_and_even_ungated() {
        assert_eq!(
            judge(&m(151.0, None, "ms", Some("diff_medium"))),
            Verdict::OverBudget { limit: 150.0 }
        );
        let mut ungated = m(151.0, Some(151.0), "ms", Some("diff_medium"));
        ungated.gated = false;
        assert!(matches!(judge(&ungated), Verdict::OverBudget { .. }));
    }

    #[test]
    fn any_gated_regression_beyond_five_percent_fails_however_small_the_value() {
        assert!(matches!(
            judge(&m(1.06, Some(1.0), "ms", None)),
            Verdict::Regressed { .. }
        ));
        assert_eq!(judge(&m(1.049, Some(1.0), "ms", None)), Verdict::Ok);
        assert!(matches!(
            judge(&m(3.0, Some(1.0), "ms", None)),
            Verdict::Regressed { .. }
        ));
    }

    #[test]
    fn ungated_measurements_and_improvements_do_not_fail() {
        let mut floor = m(200.0, Some(100.0), "ms", None);
        floor.gated = false;
        assert_eq!(judge(&floor), Verdict::Ok);
        assert_eq!(judge(&m(50.0, Some(100.0), "ms", None)), Verdict::Ok);
    }

    #[test]
    fn nan_infinite_negative_and_unit_mismatch_fail() {
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            assert_eq!(judge(&m(bad, None, "ms", None)), Verdict::Invalid, "{bad}");
            assert_eq!(
                judge(&m(1.0, Some(bad), "ms", None)),
                Verdict::Invalid,
                "{bad}"
            );
        }
        assert_eq!(
            judge(&m(12.0, None, "s", Some("index_medium"))),
            Verdict::UnitMismatch { budget_unit: "ms" }
        );
        assert_eq!(
            failures(&report(vec![m(f64::NAN, None, "ms", None)], None)).len(),
            1
        );
    }

    #[test]
    fn markdown_says_when_nothing_was_compared() {
        let r = report(vec![m(120.0, None, "ms", Some("diff_medium"))], None);
        let md = markdown(&r);
        assert!(md.contains("regressions are **not compared**"), "{md}");
        assert!(
            md.contains("| x | small | 120.00 ms | — | not compared | 150 ms | ok |"),
            "{md}"
        );
        assert!(md.contains("Cold start to interactive window"));
        assert!(!md.contains("- Architectural diff"));
        let r = report(vec![m(1.1, Some(1.0), "ms", None)], Some("abc"));
        assert!(markdown(&r).contains("+10.0% | — | **regressed**"));
    }

    #[test]
    fn json_round_trips() {
        let r = report(vec![m(1.0, Some(2.0), "ms", None)], Some("abc"));
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), r);
    }
}
