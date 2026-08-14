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
