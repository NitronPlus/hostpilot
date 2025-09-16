use hostpilot::parse;

#[test]
fn test_parse_remote_host_with_port() {
    let (u, h, p) = parse::parse_remote_host("root@example.com:2222").unwrap();
    assert_eq!(u, "root");
    assert_eq!(h, "example.com");
    assert_eq!(p, 2222);
}

#[test]
fn test_parse_remote_host_default_port() {
    let (u, h, p) = parse::parse_remote_host("user@host").unwrap();
    assert_eq!(u, "user");
    assert_eq!(h, "host");
    assert_eq!(p, 22);
}

#[test]
fn test_parse_alias_and_path_ok() {
    let (a, p) = parse::parse_alias_and_path("alias:~/path").unwrap();
    assert_eq!(a, "alias");
    assert_eq!(p, "~/path");
}
