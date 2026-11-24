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
            .backtrack_limit(1_000_000)
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
        self.re.find_iter(text).map_while(Result::ok).map(|m| m.start()).collect()
    }

    /// `re.findall` for a pattern without groups
    pub fn find_all(&self, text: &str) -> Vec<String> {
        self.re.find_iter(text).map_while(Result::ok).map(|m| m.as_str().to_string()).collect()
    }

    /// `re.findall` for a pattern with groups: one Vec of group texts per match ("" for a group that did not take part)
    pub fn find_groups(&self, text: &str) -> Vec<Vec<String>> {
        self.re
            .captures_iter(text)
            .map_while(Result::ok)
            .map(|c| (1..c.len()).map(|i| c.get(i).map(|m| m.as_str().to_string()).unwrap_or_default()).collect())
            .collect()
    }

    /// `re.sub(pattern, repl, text)` with a literal replacement
    pub fn sub(&self, text: &str, repl: &str) -> String {
        self.sub_with(text, |_| repl.to_string())
    }

    /// `re.sub(pattern, fn, text)`
    pub fn sub_with<F: FnMut(&Captures) -> String>(&self, text: &str, f: F) -> String {
        let (mut out, last, _) = self.sub_parts(text, f);
        out.push_str(&text[last..]);
        out
    }

    /// `re.sub` up to where the search ended: the replaced text before byte `last`, and whether the search reached the
    /// end of `text`. It stops early only when a match attempt exceeds the backtracking limit; the text from `last` on
    /// was then not searched. (fancy-regex's iterators repeat that error forever instead of ending, so every loop here
    /// stops at the first error: `map_while`, never `filter_map`.)
    pub fn sub_parts<F: FnMut(&Captures) -> String>(&self, text: &str, mut f: F) -> (String, usize, bool) {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for c in self.re.captures_iter(text) {
            let Ok(c) = c else { return (out, last, false) };
            let m = c.get(0).unwrap();
            out.push_str(&text[last..m.start()]);
            out.push_str(&f(&c));
            last = m.end();
        }
        (out, last, true)
    }
}

/// Test guard: abort the whole test process if it is still alive after `secs`, so a regression to an endless regex
/// loop fails the run instead of hanging it (the looping thread cannot be stopped from outside).
#[cfg(test)]
pub(crate) fn deadline(secs: u64) -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if rx.recv_timeout(std::time::Duration::from_secs(secs)) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            eprintln!("test deadline of {secs} s passed: aborting");
            std::process::abort();
        }
    });
    tx
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
    fn a_search_over_the_backtracking_limit_ends_instead_of_looping() {
        let _deadline = deadline(60);
        // quadratic for this pattern: the search from every start position runs to the end of the text, so 4,600
        // characters pass the 1M-step limit; fancy-regex then yields the same error forever, which hung the server
        let r = PyRegex::new(r"\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}\b", true);
        let text = "a.".repeat(2_300);
        assert!(r.find_all(&text).is_empty() && r.find_starts(&text).is_empty() && r.find_groups(&text).is_empty());
        assert_eq!(r.sub(&text, "[EMAIL]"), text, "the rest of the text is kept as it is");
        assert_eq!(r.sub_parts(&text, |_| String::new()), (String::new(), 0, false));
    }

    #[test]
    fn whitespace_includes_separators() {
        assert_eq!(PyRegex::new(r"\s+", false).sub("a\u{1c}b  c", " "), "a b c");
    }
}
