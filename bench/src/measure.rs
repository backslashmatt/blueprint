//! How samples are taken (DR 0002): the fastest of N timed batches, with the
//! commit under test and the base commit sampled alternately on the same
//! machine, and any failure re-measured before it counts.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::report::{Measurement, Report, failures};

/// Boxed error type for measurement callbacks.
pub type Error = Box<dyn std::error::Error>;

/// Times `batch` calls of `f` as one sample and returns milliseconds per call.
fn time_batch(batch: usize, f: &mut dyn FnMut() -> Result<(), Error>) -> Result<f64, Error> {
    let start = Instant::now();
    for _ in 0..batch {
        f()?;
    }
    Ok(start.elapsed().as_secs_f64() * 1000.0 / batch.max(1) as f64)
}

/// Fastest per-call time over `samples` batches, after one untimed warm-up
/// batch each. When `base` is given, head and base batches alternate, and so
/// does which of them goes first, so drift in the machine affects both
/// equally. Returns `(head, base)`.
pub fn best_ms(
    samples: usize,
    batch: usize,
    head: &mut dyn FnMut() -> Result<(), Error>,
    mut base: Option<&mut dyn FnMut() -> Result<(), Error>>,
) -> Result<(f64, Option<f64>), Error> {
    time_batch(1, head)?;
    if let Some(b) = base.as_deref_mut() {
        time_batch(1, b)?;
    }
    let (mut best_head, mut best_base) = (f64::INFINITY, None::<f64>);
    for i in 0..samples.max(1) {
        // Alternate which commit goes first, so neither gains from running
        // second (warmer caches) or loses from it (thermal throttling).
        let base_first = i % 2 == 1;
        let mut sample_base =
            |base: &mut Option<&mut dyn FnMut() -> Result<(), Error>>| -> Result<(), Error> {
                if let Some(b) = base.as_deref_mut() {
                    let t = time_batch(batch, b)?;
                    best_base = Some(best_base.map_or(t, |x| x.min(t)));
                }
                Ok(())
            };
        if base_first {
            sample_base(&mut base)?;
        }
        best_head = best_head.min(time_batch(batch, head)?);
        if !base_first {
            sample_base(&mut base)?;
        }
    }
    Ok((best_head, best_base))
}

/// Re-measures up to `retries` times while `report` has failures, keeping the
/// fastest head and base value seen for each benchmark, so that only a
/// failure that reproduces fails the run.
pub fn confirm(
    report: &mut Report,
    retries: usize,
    mut remeasure: impl FnMut() -> Result<Vec<Measurement>, Error>,
) -> Result<(), Error> {
    for _ in 0..retries {
        if failures(report).is_empty() {
            break;
        }
        for (old, new) in report.measurements.iter_mut().zip(remeasure()?) {
            old.value = old.value.min(new.value);
            old.base = match (old.base, new.base) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            old.samples += new.samples;
        }
    }
    Ok(())
}

/// Parses `--flag value` pairs, rejecting any flag not in `known`, so that a
/// misspelt option cannot silently switch a check off.
pub fn parse_opts(args: &[String], known: &[&str]) -> Result<BTreeMap<String, String>, Error> {
    let mut opts = BTreeMap::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let key = flag
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument `{flag}`"))?;
        if !known.contains(&key) {
            return Err(format!("unknown option `{flag}`").into());
        }
        let value = it.next().ok_or_else(|| format!("`{flag}` needs a value"))?;
        opts.insert(key.to_owned(), value.clone());
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::SCHEMA;

    fn m(value: f64, base: f64) -> Measurement {
        Measurement {
            name: "x".into(),
            repo: None,
            value,
            base: Some(base),
            unit: "ms".into(),
            budget: None,
            samples: 3,
            note: None,
            gated: true,
        }
    }

    #[test]
    fn interleaves_head_and_base_and_counts_calls() {
        let order = std::cell::RefCell::new(Vec::new());
        let mut head = || -> Result<(), Error> {
            order.borrow_mut().push('h');
            Ok(())
        };
        let mut base = || -> Result<(), Error> {
            order.borrow_mut().push('b');
            Ok(())
        };
        let (h, b) = best_ms(2, 3, &mut head, Some(&mut base)).unwrap();
        assert!(h >= 0.0 && b.is_some());
        assert_eq!(
            order.into_inner().into_iter().collect::<String>(),
            "hbhhhbbbbbbhhh"
        );
    }

    #[test]
    fn errors_propagate() {
        let mut bad = || -> Result<(), Error> { Err("boom".into()) };
        assert!(best_ms(3, 1, &mut bad, None).is_err());
    }

    #[test]
    fn a_noise_failure_is_cleared_by_remeasuring_but_a_real_one_stays() {
        let mut r = Report {
            schema: SCHEMA,
            commit: None,
            base_commit: None,
            runner: "t".into(),
            measurements: vec![m(1.2, 1.0)],
        };
        confirm(&mut r, 2, || Ok(vec![m(1.0, 1.0)])).unwrap();
        assert!(failures(&r).is_empty());
        assert_eq!(r.measurements[0].samples, 6);

        let mut r = Report {
            measurements: vec![m(1.2, 1.0)],
            ..r
        };
        let mut calls = 0;
        confirm(&mut r, 2, || {
            calls += 1;
            Ok(vec![m(1.3, 1.01)])
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(failures(&r).len(), 1);
    }

    #[test]
    fn unknown_and_dangling_options_are_rejected() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_opts(&args(&["--base-bp", "x"]), &["base-bp"]).is_ok());
        assert!(parse_opts(&args(&["--base-bpp", "x"]), &["base-bp"]).is_err());
        assert!(parse_opts(&args(&["--base-bp"]), &["base-bp"]).is_err());
        assert!(parse_opts(&args(&["base-bp"]), &["base-bp"]).is_err());
    }
}
