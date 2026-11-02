//! A scan of a module's source for what an inline plugin may not use.

/// What the source reaches for that the runtime does not give, if anything:
/// a static or dynamic `import`, or `require`. Comments and string and
/// template literals are skipped, so the words may appear in them. QuickJS
/// has no module loader here either, so a form this scan misses still fails
/// to load.
pub(crate) fn forbidden(source: &str) -> Option<&'static str> {
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b'"' | b'\'' | b'`' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            _ if is_ident(b) => {
                let start = i;
                while i < bytes.len() && is_ident(bytes[i]) {
                    i += 1;
                }
                let after_dot = source[..start].trim_end().ends_with('.');
                if after_dot {
                    continue;
                }
                match &source[start..i] {
                    "import" => return Some("import"),
                    "require" => return Some("require"),
                    _ => {}
                }
            }
            _ => i += 1,
        }
    }
    None
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}
