/// 选择自动并发度的启发式实现
/// 简单的启发式：基于文件数和总字节计算一个合理的 worker 数。
pub fn choose_auto_concurrency(total_files: usize, total_bytes: u64) -> usize {
    if total_files == 0 {
        return 1;
    }

    if total_files > 50000 {
        return 32;
    }
    // 基本启发式：以文件数的平方根为基础
    let mut workers = (total_files as f64).sqrt().ceil() as usize;
    // 如果平均文件很大，减少并发
    if total_bytes > 0 {
        let avg = total_bytes / total_files as u64;
        if avg > 10 * 1024 * 1024 {
            // 大于 10 MiB 的平均文件，降低并发
            workers = workers.min(4);
        }
    }
    workers.clamp(1, 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_choose_zero_files() {
        assert_eq!(choose_auto_concurrency(0, 0), 1);
    }

    #[test]
    fn test_choose_small_files() {
        let w = choose_auto_concurrency(16, 16 * 1024);
        assert!((3..=16).contains(&w));
    }

    #[test]
    fn test_choose_large_avg() {
        let w = choose_auto_concurrency(9, 9 * 20 * 1024 * 1024);
        assert!(w <= 4);
    }
}
