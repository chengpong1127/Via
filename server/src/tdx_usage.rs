//! Metric labels and summaries for TDX usage.

/// Fixed set of metric labels, so a typo cannot create a new series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dataset {
    Bus,
    Bike,
    Metro,
    Tra,
    Static,
    Token,
}

impl Dataset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bus => "bus",
            Self::Bike => "bike",
            Self::Metro => "metro",
            Self::Tra => "tra",
            Self::Static => "static",
            Self::Token => "token",
        }
    }
}

/// Keeps only the TDX usage counters from a rendered Prometheus exposition, for the one-off
/// `via-data` job, which has no scrape endpoint: it prints these lines at job end.
pub fn usage_summary(rendered: &str) -> String {
    rendered
        .lines()
        .filter(|line| line.starts_with("via_tdx_"))
        .collect::<Vec<_>>()
        .join("\n")
}
