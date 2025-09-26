// Simple glob-style matcher supporting '*' and '?'. Not full-featured but
// sufficient for our use (matching file/directory names).
pub fn wildcard_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    fn helper(p: &[char], t: &[char]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        if p[0] == '*' {
            // Try to match '*' with any number of chars
            if helper(&p[1..], t) {
                return true;
            }
            if !t.is_empty() && helper(p, &t[1..]) {
                return true;
            }
            return false;
        } else if !t.is_empty() && (p[0] == '?' || p[0] == t[0]) {
            return helper(&p[1..], &t[1..]);
        }
        false
    }
    helper(&p, &t)
}

fn is_windows_drive(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let c = bytes[0] as char;
        return c.is_ascii_uppercase() || c.is_ascii_lowercase();
    }
    false
}

pub fn is_remote_spec(s: &str) -> bool {
    if is_windows_drive(s) {
        return false;
    }
    // Consider something remote if it contains ':' before any '/'
    if let Some(pos) = s.find(':') {
        if let Some(slash_pos) = s.find('/') {
            return pos < slash_pos;
        }
        return true;
    }
    false
}

pub fn is_disallowed_glob(s: &str) -> bool {
    if s.contains("**") {
        return true;
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() <= 1 {
        return false;
    }
    for seg in &parts[..parts.len() - 1] {
        if seg.contains('*') || seg.contains('?') {
            return true;
        }
    }
    false
}

// Lightweight path display wrapper that renders with forward slashes.
// Avoids allocating strings until actually formatted for logs.
pub(crate) struct DisplayPath<'a>(pub(crate) &'a std::path::Path);

impl<'a> std::fmt::Display for DisplayPath<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.0.to_string_lossy().to_string();
        let out = normalize_path(&s, true);
        f.write_str(&out)
    }
}

pub(crate) fn display_path(p: &std::path::Path) -> DisplayPath<'_> {
    DisplayPath(p)
}

/// Normalize a path-like string for internal use:
/// - converts backslashes to forward slashes
/// - collapses repeated slashes
/// - optionally preserves a trailing slash (useful to keep explicit-dir-suffix semantics)
///
/// This is a lightweight helper intended for canonicalizing paths used for remote
/// SFTP operations and for cross-platform comparisons in tests.
pub fn normalize_path(p: &str, preserve_trailing_slash: bool) -> String {
    if p.is_empty() {
        return String::new();
    }
    // Convert backslashes to forward slashes first
    let mut s = p.replace('\\', "/");
    // Collapse repeated slashes ("//" -> "/") to avoid accidental differences
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    if !preserve_trailing_slash {
        // Strip trailing slashes, but keep root "/"
        while s.len() > 1 && s.ends_with('/') {
            s.pop();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_empty() {
        assert_eq!(normalize_path("", true), "");
        assert_eq!(normalize_path("", false), "");
    }

    #[test]
    fn normalize_windows_drive_keeps_drive() {
        // Drive letter paths should convert backslashes but keep drive prefix
        let in_s = "C:\\path\\to\\file";
        assert_eq!(normalize_path(in_s, false), "C:/path/to/file");
    }

    #[test]
    fn preserve_and_strip_trailing_slash() {
        assert_eq!(normalize_path("/a/b/", true), "/a/b/");
        assert_eq!(normalize_path("/a/b/", false), "/a/b");
        // root should remain "/"
        assert_eq!(normalize_path("/", true), "/");
        assert_eq!(normalize_path("/", false), "/");
    }

    #[test]
    fn collapse_repeated_slashes() {
        assert_eq!(normalize_path("//a///b//c", false), "/a/b/c");
    }

    #[test]
    fn glob_chars_preserved() {
        // ensure '*' and '?' are not altered by normalization
        assert_eq!(normalize_path("/some/dir/*.log", false), "/some/dir/*.log");
        assert_eq!(normalize_path("..\\foo\\?.txt", false), "../foo/?.txt");
    }

    #[test]
    fn normalize_unc_and_relative_dots() {
        // UNC path (Windows network share) - backslashes should convert to '/'
        // leading '//' will be collapsed to '/'
        assert_eq!(normalize_path("\\\\server\\share\\dir", false), "/server/share/dir");
        // relative path with dot segments should retain segment text (no resolution)
        assert_eq!(normalize_path(".\\a\\..\\b", false), "./a/../b");
    }

    #[test]
    fn is_remote_and_disallowed_glob_cases() {
        // remote spec detection
        assert!(is_remote_spec("user@host:/path"));
        assert!(is_remote_spec("host:folder/file"));
        assert!(!is_remote_spec("C:\\path\\to\\file"));
        assert!(!is_remote_spec("just-a-name"));

        // disallowed glob
        assert!(is_disallowed_glob("a/**/b"));
        assert!(is_disallowed_glob("a/*/b/c"));
        assert!(!is_disallowed_glob("a/b/*.txt"));
    }

    #[test]
    fn display_path_uses_normalize() {
        use std::path::Path;
        let p = Path::new("C:\\some\\path\\");
        let s = format!("{}", display_path(p));
        assert_eq!(s, "C:/some/path/");
    }
}
