//! Python `re` semantics on top of fancy-regex.
//!
//! The rule tables (context cues, scam workflows, redaction patterns) are written in Python regex syntax and must
//! match exactly what Python matched. Two classes differ between Python's `re` and Rust's regex engine, so patterns
//! are rewritten before compiling:
//!   * `\w` / `\b`: Python's word characters are letters, numbers and `_` (`str.isalnum()`), while Rust also counts
//!     combining marks (Devanagari vowel signs). `\b` becomes an explicit lookaround on `[\p{L}\p{N}_]`.
//!   * `\s`: Python's whitespace also includes the separators U+001C..U+001F.
//! Everything else used here (lookbehind, `\d` = Unicode Nd, `.` without newline, simple case folding) agrees.
use fancy_regex::{Captures, Regex};

const W: &str = r"[\p{L}\p{N}_]";
const W_IN: &str = r"\p{L}\p{N}_";
const S_IN: &str = r"\s\x1c-\x1f";

/// Rewrite a Python pattern into an equivalent fancy-regex pattern.
pub fn translate(py: &str) -> String {
    let mut out = String::with_capacity(py.len() * 2);
    let chars: Vec<char> = py.chars().collect();
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            let rep: Option<String> = if in_class {
                match n {
                    'w' => Some(W_IN.into()),
                    's' => Some(S_IN.into()),
                    _ => None,
                }
            } else {
                match n {
                    'b' => Some(format!("(?:(?<={W})(?!{W})|(?<!{W})(?={W}))")),
                    'B' => Some(format!("(?:(?<={W})(?={W})|(?<!{W})(?!{W}))")),
                    'w' => Some(W.into()),
                    'W' => Some(format!("[^{W_IN}]")),
                    's' => Some(format!("[{S_IN}]")),
                    'S' => Some(format!("[^{S_IN}]")),
                    'Z' => Some(r"\z".into()),
                    _ => None,
                }
            };
            match rep {
                Some(r) => out.push_str(&r),
                None => {
                    out.push(c);
                    out.push(n);
                }
            }
            i += 2;
            continue;
        }
        if c == '[' && !in_class {
            in_class = true;
            out.push(c);
            // a ']' right after '[' or '[^' is a literal
            if i + 1 < chars.len() && chars[i + 1] == '^' {
                out.push('^');
                i += 1;
            }
            if i + 1 < chars.len() && chars[i + 1] == ']' {
                out.push_str("\\]");
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == ']' && in_class {
            in_class = false;
        } else if c == '[' && in_class {
            // Python treats '[' inside a class as a literal; Rust would read a nested class
            out.push_str("\\[");
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// A compiled Python-style regex.
#[derive(Clone, Debug)]
pub struct PyRegex {
    re: Regex,
    pub pattern: String,
}

impl PyRegex {
    pub fn new(py_pattern: &str, ignore_case: bool) -> Self {
        let t = translate(py_pattern);
        let full = if ignore_case { format!("(?i){t}") } else { t };
        let re = fancy_regex::RegexBuilder::new(&full)
            .backtrack_limit(10_000_000)
            .build()
            .unwrap_or_else(|e| panic!("bad pattern {py_pattern:?}: {e}"));
        PyRegex { re, pattern: py_pattern.to_string() }
    }

    /// `re.search(...) is not None`
    pub fn is_match(&self, text: &str) -> bool {
        self.re.is_match(text).unwrap_or(false)
    }

    /// `[m.start() for m in re.finditer(...)]` as byte offsets
    pub fn find_starts(&self, text: &str) -> Vec<usize> {
        self.re.find_iter(text).filter_map(|m| m.ok()).map(|m| m.start()).collect()
    }

    /// `re.findall` for a pattern without groups
    pub fn find_all(&self, text: &str) -> Vec<String> {
        self.re.find_iter(text).filter_map(|m| m.ok()).map(|m| m.as_str().to_string()).collect()
    }

    /// `re.findall` for a pattern with groups: one Vec of group texts per match ("" for a group that did not take part)
    pub fn find_groups(&self, text: &str) -> Vec<Vec<String>> {
        self.re
            .captures_iter(text)
            .filter_map(|c| c.ok())
            .map(|c| (1..c.len()).map(|i| c.get(i).map(|m| m.as_str().to_string()).unwrap_or_default()).collect())
            .collect()
    }

    /// `re.sub(pattern, repl, text)` with a literal replacement
    pub fn sub(&self, text: &str, repl: &str) -> String {
        self.sub_with(text, |_| repl.to_string())
    }

    /// `re.sub(pattern, fn, text)`
    pub fn sub_with<F: FnMut(&Captures) -> String>(&self, text: &str, mut f: F) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for c in self.re.captures_iter(text).filter_map(|c| c.ok()) {
            let m = c.get(0).unwrap();
            out.push_str(&text[last..m.start()]);
            out.push_str(&f(&c));
            last = m.end();
        }
        out.push_str(&text[last..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundary_ignores_combining_marks_like_python() {
        // 'ा' (U+093E) is a mark: not a word character in Python, so "\bimmediately" matches after it
        assert!(PyRegex::new(r"\bimmediately\b", true).is_match("बताimmediately now"));
        assert!(!PyRegex::new(r"\bimmediately\b", true).is_match("xximmediately"));
    }

    #[test]
    fn lookbehind_digits() {
        let r = PyRegex::new(r"(?<!\d)\d{4,8}(?!\d)", false);
        assert_eq!(r.find_all("otp 4821 and 123456789 x 99"), vec!["4821"]);
    }

    #[test]
    fn whitespace_includes_separators() {
        assert_eq!(PyRegex::new(r"\s+", false).sub("a\u{1c}b  c", " "), "a b c");
    }
}
