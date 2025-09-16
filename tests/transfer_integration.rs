use hostpilot::transfer;

#[test]
fn test_wildcard_match_simple() {
    assert!(transfer::wildcard_match("*.txt", "file.txt"));
    assert!(transfer::wildcard_match("data-??.bin", "data-01.bin"));
    assert!(!transfer::wildcard_match("a*b", "ac"));
}
