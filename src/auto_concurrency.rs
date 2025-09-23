// Auto concurrency chooser extracted for testing.
// Heuristic tuned for practical defaults (50k files upper bound, avg size influence).
pub fn choose_auto_concurrency(total_files: usize, total_size_bytes: u64) -> usize {
    if total_files == 0 {
        return 1;
    }
    if total_files == 1 {
        return 1;
    }

    if total_size_bytes > 100 * 1024 * 1024 && total_files <= 4 {
        return 4;
    }

    if total_files >= 50_000 {
        return 16;
    }

    let mut base = (total_files as f64).sqrt().round() as usize;
    if base < 1 {
        base = 1;
    }

    let avg_size = if total_files > 0 { total_size_bytes / (total_files as u64) } else { 0 };
    if avg_size > 100 * 1024 * 1024 {
        base = ((base as f64) * 0.25).max(1.0) as usize;
    } else if avg_size > 1024 * 1024 {
        base = ((base as f64) * 0.5).max(1.0) as usize;
    }

    base.clamp(1, 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real remote paths provided by user for convenience in test names and docs.
    const HGS_DRIVER_REMOTE: &str = "hdev:~/hgs-driver-taro/hgs-driver";
    const LTCS_LOGS_REMOTE: &str = "hdev:~/ltcs/logs/*.log";

    #[test]
    fn zero_files() {
        assert_eq!(choose_auto_concurrency(0, 0), 1);
    }

    #[test]
    fn single_file() {
        assert_eq!(choose_auto_concurrency(1, 10), 1);
    }

    #[test]
    fn many_small_files_scaling() {
        // 10k small files -> sqrt(10000)=100 -> clamped to 16
        assert_eq!(choose_auto_concurrency(10_000, 10_000 * 1024), 16);
        // 100 files -> sqrt(100)=10 -> expect 10
        assert_eq!(choose_auto_concurrency(100, 100 * 512), 10);
    }

    #[test]
    fn avg_size_influence() {
        // 100 files with avg >1MiB should reduce concurrency
        let small_avg = choose_auto_concurrency(100, 100 * 512);
        let large_avg = choose_auto_concurrency(100, 100 * 2 * 1024 * 1024);
        assert!(large_avg < small_avg, "large_avg = {}, small_avg = {}", large_avg, small_avg);
    }

    #[test]
    fn very_large_single_files() {
        // few very large files -> conservative
        assert_eq!(choose_auto_concurrency(2, 300 * 1024 * 1024), 4);
    }

    #[test]
    fn saturate_for_50k() {
        assert_eq!(choose_auto_concurrency(50_000, 50_000 * 1024), 16);
    }

    #[test]
    fn real_world_hgs_driver_small_files() {
        // Simulate `HGS_DRIVER_REMOTE` — 10k+ small JS files
        // remote: {}
        let _ = HGS_DRIVER_REMOTE;
        let files = 10_000usize;
        let avg_kib = 10usize; // assume ~10 KiB per JS file
        let total = (files as u64) * (avg_kib as u64) * 1024u64;
        // Expect saturation to max (16) for many small files
        assert_eq!(choose_auto_concurrency(files, total), 16);
    }

    #[test]
    fn real_world_ltcs_logs_large_files() {
        // Simulate `LTCS_LOGS_REMOTE` — dozens of large logs (tens to hundreds MB)
        // remote: {}
        let _ = LTCS_LOGS_REMOTE;
        let files = 40usize; // dozens
        let avg_mb = 200u64; // assume ~200 MiB average
        let total = (files as u64) * avg_mb * 1024u64 * 1024u64;
        let c = choose_auto_concurrency(files, total);
        // For many large files we expect conservative concurrency (small number)
        assert!(c <= 4, "expected <=4 workers for large logs, got {}", c);
    }

    #[test]
    fn ltcs_logs_medium_size_expect_three() {
        // If logs are medium (e.g., ~50MiB average), expect modest concurrency (~3)
        let files = 40usize;
        let avg_mb = 50u64;
        let total = (files as u64) * avg_mb * 1024u64 * 1024u64;
        let c = choose_auto_concurrency(files, total);
        assert_eq!(c, 3);
    }
}
