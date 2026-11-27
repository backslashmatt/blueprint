//! The platform shell (REQUIREMENTS §5.3): windows, menus and packaging.

pub mod updater;

use std::time::Instant;

use gpui::{App, Window};

/// After the window's first frame is painted, writes the time since `start`
/// to `path` as `{"cold_start_ms": …}` and quits.
pub fn first_paint(window: &mut Window, cx: &mut App, start: Instant, path: String) {
    window.on_next_frame(move |_window, cx| {
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let json = format!("{{\"cold_start_ms\": {ms:.2}}}\n");
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("blueprint: cannot write {path}: {e}");
            std::process::exit(1);
        }
        cx.quit();
    });
    let _ = cx;
}

/// Once `samples` frames have answered input, writes GPUI's input-to-present
/// latency histogram (#32) to `path` as JSON and quits: `{"samples",
/// "p50_ms", "p95_ms", "p99_ms", "max_ms", "mid_draw_events_dropped",
/// "draw_p50_ms", "draw_p95_ms", "dirty_to_present_p50_ms",
/// "dirty_to_present_p95_ms"}`, the last four from GPUI's frame timings. Each
/// sample runs from the dispatch of the first input event a frame answers to
/// the end of presenting that frame.
#[cfg(feature = "latency")]
pub fn latency_report(window: gpui::AnyWindowHandle, path: String, samples: u64, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            let Ok((snapshot, frames)) = window.update(cx, |_, window, _| {
                (
                    window.input_latency_snapshot(),
                    window.frame_duration_snapshot(),
                )
            }) else {
                return;
            };
            let h = &snapshot.latency_histogram;
            if h.len() < samples {
                continue;
            }
            let ms = |q: f64| h.value_at_quantile(q) as f64 / 1e6;
            let (draw, dirty) = (
                &frames.draw_duration_histogram,
                &frames.dirty_to_present_histogram,
            );
            let json = format!(
                "{{\"samples\": {}, \"p50_ms\": {:.2}, \"p95_ms\": {:.2}, \"p99_ms\": {:.2}, \"max_ms\": {:.2}, \"mid_draw_events_dropped\": {}, \"draw_p50_ms\": {:.2}, \"draw_p95_ms\": {:.2}, \"dirty_to_present_p50_ms\": {:.2}, \"dirty_to_present_p95_ms\": {:.2}}}\n",
                h.len(),
                ms(0.5),
                ms(0.95),
                ms(0.99),
                h.max() as f64 / 1e6,
                snapshot.mid_draw_events_dropped,
                draw.value_at_quantile(0.5) as f64 / 1e6,
                draw.value_at_quantile(0.95) as f64 / 1e6,
                dirty.value_at_quantile(0.5) as f64 / 1e6,
                dirty.value_at_quantile(0.95) as f64 / 1e6
            );
            if let Err(e) = std::fs::write(&path, json) {
                eprintln!("blueprint: cannot write {path}: {e}");
                std::process::exit(1);
            }
            cx.update(|cx| cx.quit());
            return;
        }
    })
    .detach();
}

/// The process's peak resident memory in bytes (#163): `VmHWM` from
/// `/proc/self/status` on Linux. `None` elsewhere, or if it cannot be read.
/// Windows would need `GetProcessMemoryInfo`, which is `unsafe` to call
/// (forbidden in this workspace) and not in the bench job, which runs on
/// Linux.
#[must_use]
pub fn peak_resident_bytes() -> Option<u64> {
    if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| vm_hwm(&s))
    } else {
        None
    }
}

/// The `VmHWM` line of a `/proc/<pid>/status` text, in bytes.
fn vm_hwm(status: &str) -> Option<u64> {
    let rest = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))?
        .trim();
    let kb = rest.strip_suffix("kB")?.trim().parse::<u64>().ok()?;
    kb.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_peak_from_proc_status() {
        let status =
            "Name:\tblueprint\nVmPeak:\t 2000000 kB\nVmHWM:\t  312456 kB\nVmRSS:\t  300000 kB\n";
        assert_eq!(vm_hwm(status), Some(312_456 * 1024));
        assert_eq!(vm_hwm("VmRSS:\t1 kB\n"), None);
        assert_eq!(vm_hwm("VmHWM:\tlots kB\n"), None);
        assert_eq!(vm_hwm("VmHWM:\t12 MB\n"), None);
    }

    #[test]
    fn this_process_has_a_peak_on_linux() {
        let peak = peak_resident_bytes();
        if cfg!(target_os = "linux") {
            assert!(peak.is_some_and(|b| b > 0), "{peak:?}");
        } else {
            assert_eq!(peak, None);
        }
    }
}
