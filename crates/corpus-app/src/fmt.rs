//! Small shared display helpers (no corpus-core business logic here).

/// Human-readable byte size (`1.4mb`), matching the mock's casing.
/// Used by the sidebar's corpus summary and the project view's Corpus
/// panel.
pub fn fmt_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * KIB;
    const GIB: f64 = MIB * KIB;
    if bytes < 1024 {
        format!("{bytes}b")
    } else if bytes < (10.0 * MIB) as u64 {
        format!("{:.1}kb", bytes as f64 / KIB)
    } else if bytes < (10.0 * GIB) as u64 {
        format!("{:.1}mb", bytes as f64 / MIB)
    } else {
        format!("{:.1}gb", bytes as f64 / GIB)
    }
}

/// Compact token count (`950`, `12.3k`, `1.2M`) for the cost table.
pub fn fmt_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        format!("{tokens}")
    } else if tokens < 1_000_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    }
}

/// Compact wall-clock duration for project inference totals.
pub fn fmt_duration(milliseconds: u64) -> String {
    let seconds = milliseconds as f64 / 1_000.0;
    if milliseconds < 60_000 {
        format!("{seconds:.1}s")
    } else if milliseconds < 3_600_000 {
        format!("{:.1}m", seconds / 60.0)
    } else {
        format!("{:.1}h", seconds / 3_600.0)
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_duration;

    #[test]
    fn duration_uses_a_compact_project_metric_format() {
        assert_eq!(fmt_duration(3_250), "3.2s");
        assert_eq!(fmt_duration(90_000), "1.5m");
        assert_eq!(fmt_duration(7_200_000), "2.0h");
    }
}

/// USD cost, cents precision (4 decimals below a dime so small runs stay
/// distinguishable).
pub fn fmt_usd(cost: f64) -> String {
    if cost.abs() < 0.1 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}
