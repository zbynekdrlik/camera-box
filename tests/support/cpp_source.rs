//! Shared text helpers for the tests that anchor on vendored C/C++ source (issue 1367). Include
//! with `#[path = "support/cpp_source.rs"] mod cpp_source;` — files under `tests/support/` are not
//! test crates of their own.

/// Collapse every whitespace run to one space, so an anchor survives any reformatting.
pub fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop `/* ... */` and `// ...` comments (outside string literals) so prose in a comment never
/// satisfies or breaks an anchor. A block comment becomes one space; a line comment disappears up to
/// its newline.
pub fn strip_cpp_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut in_str = false;
    while let Some(c) = rest.chars().next() {
        if in_str {
            if c == '\\' {
                let esc: String = rest.chars().take(2).collect();
                out.push_str(&esc);
                rest = &rest[esc.len()..];
                continue;
            }
            in_str = c != '"';
            out.push(c);
            rest = &rest[c.len_utf8()..];
        } else if rest.starts_with("/*") {
            out.push(' ');
            rest = rest[2..].find("*/").map_or("", |e| &rest[2 + e + 2..]);
        } else if rest.starts_with("//") {
            rest = rest.find('\n').map_or("", |e| &rest[e..]);
        } else {
            in_str = c == '"';
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The balanced-brace body of the first function whose signature starts with `sig`.
pub fn body_of<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src.find(sig).unwrap_or_else(|| panic!("`{sig}` not found"));
    let open = start + src[start..].find('{').expect("function body opening brace");
    let mut depth = 0usize;
    for (off, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=open + off];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced body for `{sig}`");
}
