//! RealityScan's JSON isn't always strict: error messages carry raw newlines
//! inside strings. [`sanitize`] escapes control characters that appear inside
//! string literals so `serde_json` accepts the text, leaving structure untouched.

/// Escape raw control characters and stray backslashes inside JSON string literals.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !in_string {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
            i += 1;
            continue;
        }
        match c {
            '\\' => {
                // Keep valid escapes; double a backslash that isn't one (Windows paths in
                // error messages arrive unescaped, e.g. `Z:\home`).
                let next = chars.get(i + 1).copied();
                let valid = match next {
                    Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => true,
                    Some('u') => {
                        chars.get(i + 2..i + 6).is_some_and(|h| h.iter().all(|c| c.is_ascii_hexdigit()))
                    }
                    _ => false,
                };
                if valid {
                    out.push('\\');
                    out.push(next.unwrap());
                    i += 2;
                } else {
                    out.push_str("\\\\");
                    i += 1;
                }
            }
            '"' => {
                in_string = false;
                out.push(c);
                i += 1;
            }
            '\n' => {
                out.push_str("\\n");
                i += 1;
            }
            '\r' => {
                out.push_str("\\r");
                i += 1;
            }
            '\t' => {
                out.push_str("\\t");
                i += 1;
            }
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_newlines_only_inside_strings() {
        let raw = "{\n  \"errorMessage\":\"Translation failed.\n\nSee console\",\n  \"n\":1\n}";
        let fixed = sanitize(raw);
        assert_eq!(fixed, "{\n  \"errorMessage\":\"Translation failed.\\n\\nSee console\",\n  \"n\":1\n}");
        assert!(serde_json::from_str::<serde_json::Value>(&fixed).is_ok());
    }

    #[test]
    fn doubles_stray_backslashes() {
        let raw = r#"{"errorMessage":"Loading 'Z:\home\x\probe\a.xml' failed","u":"\u00e9\uZZZZ"}"#;
        let fixed = sanitize(raw);
        let v: serde_json::Value = serde_json::from_str(&fixed).unwrap();
        assert_eq!(v["errorMessage"], r"Loading 'Z:\home\x\probe\a.xml' failed");
        assert_eq!(v["u"], "é\\uZZZZ");
    }

    #[test]
    fn keeps_existing_escapes_and_quotes() {
        let raw = r#"{"path":"Z:\\a\"b","t":"x\ty"}"#;
        let fixed = sanitize(&raw.replace("\\t", "\t"));
        assert_eq!(fixed, raw);
        assert_eq!(sanitize(raw), raw);
    }
}
