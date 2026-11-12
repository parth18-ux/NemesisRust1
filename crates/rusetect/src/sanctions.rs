//! Sanctions / watchlist name screening.
//!
//! Names are normalised (accents and punctuation stripped, lower case, honorifics dropped) and split into tokens. A
//! query scores against a list name by aligning tokens in any order: each token takes its best Jaro-Winkler match on the
//! other side, and the score is the mean of both directions, in [0, 1]. A Soundex key per token breaks ties between
//! equal scores. The hit threshold is fitted on validation pairs in research/sanctions_eval.py.
//! Jaro-Winkler and Soundex are written from their published definitions (Winkler 1990; Russell and Odell's Soundex).
use std::path::Path;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

const HONORIFICS: [&str; 12] = ["mr", "mrs", "ms", "miss", "mx", "dr", "prof", "sir", "madam", "sheikh", "haji", "hon"];

/// Lower case, accents and punctuation removed, honorifics dropped. "Dr. José O'Neil-Ruiz" -> ["jose", "oneil", "ruiz"].
pub fn normalise(name: &str) -> Vec<String> {
    let s: String = name.nfd().filter(|c| !is_combining_mark(*c) && !matches!(c, '\'' | '’' | '`'))
        .flat_map(char::to_lowercase).map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    s.split_whitespace().filter(|t| !HONORIFICS.contains(t)).map(String::from).collect()
}

pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let window = (a.len().max(b.len()) / 2).saturating_sub(1);
    let (mut am, mut bm) = (vec![false; a.len()], vec![false; b.len()]);
    let mut m = 0usize;
    for i in 0..a.len() {
        for j in i.saturating_sub(window)..(i + window + 1).min(b.len()) {
            if !bm[j] && a[i] == b[j] {
                (am[i], bm[j], m) = (true, true, m + 1);
                break;
            }
        }
    }
    if m == 0 {
        return 0.0;
    }
    let bs = b.iter().zip(&bm).filter(|p| *p.1).map(|p| p.0);
    let t = a.iter().zip(&am).filter(|p| *p.1).map(|p| p.0).zip(bs).filter(|(x, y)| x != y).count() as f64 / 2.0;
    let m = m as f64;
    let jaro = (m / a.len() as f64 + m / b.len() as f64 + (m - t) / m) / 3.0;
    let prefix = a.iter().zip(&b).take(4).take_while(|(x, y)| x == y).count() as f64;
    jaro + prefix * 0.1 * (1.0 - jaro)
}

/// American Soundex of the ASCII letters of `s` ("robert" -> "R163"); empty when there are none.
pub fn soundex(s: &str) -> String {
    let code = |c: char| match c {
        'b' | 'f' | 'p' | 'v' => '1',
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2',
        'd' | 't' => '3',
        'l' => '4',
        'm' | 'n' => '5',
        'r' => '6',
        _ => '0',
    };
    let mut cs = s.chars().filter(char::is_ascii_alphabetic).map(|c| c.to_ascii_lowercase());
    let Some(first) = cs.next() else { return String::new() };
    let (mut out, mut last) = (first.to_ascii_uppercase().to_string(), code(first));
    for c in cs {
        let d = code(c);
        if d != '0' && d != last {
            out.push(d);
            if out.len() == 4 {
                break;
            }
        }
        if c != 'h' && c != 'w' {
            last = d;
        }
    }
    format!("{out:0<4}")
}

/// Mean over `a`'s tokens of the best Jaro-Winkler against `b`'s tokens, and how many of those best pairs share a Soundex key.
fn align(a: &[String], b: &[String]) -> (f64, usize) {
    let mut sum = 0.0;
    let mut phon = 0;
    for x in a {
        let (s, y) = b.iter().map(|y| (jaro_winkler(x, y), y)).fold((0.0, None), |acc, (s, y)| if s > acc.0 { (s, Some(y)) } else { acc });
        sum += s;
        phon += y.is_some_and(|y| soundex(x) == soundex(y)) as usize;
    }
    (sum / a.len().max(1) as f64, phon)
}

/// Token-aligned, order-insensitive similarity of two normalised names in [0, 1], plus the Soundex tie-break count.
pub fn score(q: &[String], c: &[String]) -> (f64, usize) {
    if q.is_empty() || c.is_empty() {
        return (0.0, 0);
    }
    let ((s1, p1), (s2, p2)) = (align(q, c), align(c, q));
    ((s1 + s2) / 2.0, p1 + p2)
}

pub struct Entry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub program: String,
    tokens: Vec<String>,
}

pub struct List {
    pub entries: Vec<Entry>,
}

impl List {
    /// CSV with columns id, name, type, program.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut rd = csv::Reader::from_path(path)?;
        let entries = rd.records().map(|r| {
            let r = r?;
            let g = |i: usize| r.get(i).unwrap_or("").to_string();
            Ok(Entry { tokens: normalise(&g(1)), id: g(0), name: g(1), kind: g(2), program: g(3) })
        }).collect::<anyhow::Result<Vec<_>>>()?;
        Ok(List { entries })
    }

    /// Scores of `name` against every entry, in list order.
    pub fn scores(&self, name: &str) -> Vec<(f64, usize)> {
        let q = normalise(name);
        self.entries.iter().map(|e| score(&q, &e.tokens)).collect()
    }

    /// The `top` best entries (of `kind` when given), highest score first, Soundex agreement breaking ties.
    pub fn screen(&self, name: &str, kind: Option<&str>, top: usize) -> Vec<(&Entry, f64)> {
        let mut m: Vec<_> = self.entries.iter().zip(self.scores(name)).filter(|(e, _)| kind.is_none_or(|k| e.kind == k)).collect();
        m.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(b.1.1.cmp(&a.1.1)));
        m.into_iter().take(top).map(|(e, (s, _))| (e, s)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &str, b: &str) -> f64 {
        score(&normalise(a), &normalise(b)).0
    }

    #[test]
    fn building_blocks() {
        assert_eq!(normalise("Dr. José O'Neil-Ruiz"), ["jose", "oneil", "ruiz"]);
        assert!((jaro_winkler("martha", "marhta") - 0.9611).abs() < 1e-4);
        assert!((jaro_winkler("dixon", "dicksonx") - 0.8133).abs() < 1e-4);
        assert_eq!((soundex("Robert"), soundex("Rupert"), soundex("Ashcraft"), soundex("Pfister")), ("R163".into(), "R163".into(), "A261".into(), "P236".into()));
    }

    #[test]
    fn exact_reordered_typo_unrelated() {
        assert_eq!(s("Aldric Venthorpe", "Aldric Venthorpe"), 1.0);
        assert_eq!(s("VENTHORPE, Aldric", "Aldric Venthorpe"), 1.0, "order and case do not matter");
        assert_eq!(s("Mr. Aldric Venthorpe", "Aldric Venthorpe"), 1.0, "honorifics are dropped");
        assert!(s("Aldrik Venthrope", "Aldric Venthorpe") > 0.9, "small typos still match");
        assert!(s("Muhammad Khalil Al Farzani", "Mohammed Khaleel Al-Farzani") > 0.85, "transliteration");
        assert!(s("Maria Garcia", "Aldric Venthorpe") < 0.6, "unrelated names stay low");
    }

    #[test]
    fn screen_ranks_and_filters_by_kind() {
        let path = std::env::temp_dir().join(format!("sdn_test_{}.csv", std::process::id()));
        std::fs::write(&path, "id,name,type,program\n1,Ann Smith,individual,X\n2,Ann Smyth,individual,X\n3,Smith Shipping,entity,X\n").unwrap();
        let l = List::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let top = l.screen("Ann Smith", None, 2);
        assert_eq!((top[0].0.id.as_str(), top[0].1), ("1", 1.0));
        assert_eq!(l.screen("Smith", Some("entity"), 5).len(), 1);
    }
}
