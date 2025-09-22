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
