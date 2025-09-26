use hostpilot::auto_concurrency::choose_auto_concurrency;

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
