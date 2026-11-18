//! The performance budgets of REQUIREMENTS §4, as data. Every row is listed so
//! the report shows which budgets are measured and which are not yet.

/// One row of the §4 budget table.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Stable identifier; benchmark results refer to it.
    pub id: &'static str,
    /// The operation, as worded in §4.
    pub operation: &'static str,
    /// The limit, in `unit`. Lower is better for every row.
    pub limit: f64,
    /// Unit of `limit`.
    pub unit: &'static str,
}

/// Every §4 row. Frame-rate rows are expressed as frame-time limits: 60 fps
/// everywhere, and 120 fps where the display supports it.
pub const BUDGETS: &[Budget] = &[
    Budget {
        id: "cold_start",
        operation: "Cold start to interactive window",
        limit: 300.0,
        unit: "ms",
    },
    Budget {
        id: "open_indexed",
        operation: "Open a previously indexed repository, to first model on screen",
        limit: 200.0,
        unit: "ms",
    },
    Budget {
        id: "index_medium",
        operation: "First index, medium repository",
        limit: 10_000.0,
        unit: "ms",
    },
    Budget {
        id: "index_medium_l2",
        operation: "First index, medium repository: L1-L2 on screen",
        limit: 1_000.0,
        unit: "ms",
    },
    Budget {
        id: "index_large",
        operation: "First index, large repository",
        limit: 120_000.0,
        unit: "ms",
    },
    Budget {
        id: "index_large_l2",
        operation: "First index, large repository: L1-L2 on screen",
        limit: 3_000.0,
        unit: "ms",
    },
    Budget {
        id: "reindex_one_file",
        operation: "Re-index after a one-file change",
        limit: 50.0,
        unit: "ms",
    },
    Budget {
        id: "diff_medium",
        operation: "Architectural diff of two indexed commits, medium repository",
        limit: 150.0,
        unit: "ms",
    },
    Budget {
        id: "drill",
        operation: "Drill down or up one level (input to painted frame)",
        limit: 50.0,
        unit: "ms",
    },
    Budget {
        id: "input_latency",
        operation: "Any keystroke or pointer input to painted frame",
        limit: 16.0,
        unit: "ms",
    },
    Budget {
        id: "frame_time",
        operation: "Scrolling and pan/zoom (60 fps minimum)",
        limit: 16.7,
        unit: "ms",
    },
    Budget {
        id: "frame_time_120hz",
        operation: "Scrolling and pan/zoom on a 120 Hz display (120 fps)",
        limit: 8.3,
        unit: "ms",
    },
    Budget {
        id: "open_50k_file",
        operation: "Open a 50,000-line file at L4",
        limit: 100.0,
        unit: "ms",
    },
    Budget {
        id: "memory_medium",
        operation: "Resident memory, medium repository open",
        limit: 500.0,
        unit: "MB",
    },
    Budget {
        id: "installed_size",
        operation: "Installed size",
        limit: 50.0,
        unit: "MB",
    },
];

/// Looks up a budget row by id.
pub fn budget(id: &str) -> Option<&'static Budget> {
    BUDGETS.iter().find(|b| b.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_limits_positive() {
        for (i, b) in BUDGETS.iter().enumerate() {
            assert!(b.limit > 0.0, "{}", b.id);
            assert!(
                BUDGETS[i + 1..].iter().all(|o| o.id != b.id),
                "duplicate {}",
                b.id
            );
        }
        assert_eq!(BUDGETS.len(), 15);
    }
}
