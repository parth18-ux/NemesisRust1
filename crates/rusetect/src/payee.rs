//! Payee name check (confirmation-of-payee style), aimed at authorised push payment scams: the name the payer typed for
//! the payee against the account holder name the bank holds. It reuses the sanctions matcher v2 (`score_v2`) unchanged;
//! only the verdict bands and the reason are new. Thresholds and the adoption decision come from the pre-registered
//! evaluation (research/payee_check_eval.py -> research/out/payee_check_eval.json).
use crate::sanctions::{normalise, score_v2, token_v2, FLOOR};

/// v2: common English diminutives -> the given name they stand for. Written for this project (no external list used).
/// shortcut: an ambiguous diminutive (Sam, Alex, Chris) maps to one name only; a per-name list would fix that.
const NICKNAMES: [(&str, &str); 56] = [
    ("bob", "robert"), ("bobby", "robert"), ("rob", "robert"), ("robbie", "robert"), ("bill", "william"),
    ("billy", "william"), ("will", "william"), ("willie", "william"), ("liz", "elizabeth"), ("lizzie", "elizabeth"),
    ("beth", "elizabeth"), ("betty", "elizabeth"), ("jim", "james"), ("jimmy", "james"), ("mike", "michael"),
    ("mick", "michael"), ("tom", "thomas"), ("tommy", "thomas"), ("dick", "richard"), ("rick", "richard"),
    ("ricky", "richard"), ("kate", "katherine"), ("katie", "katherine"), ("kathy", "katherine"),
    ("peggy", "margaret"), ("maggie", "margaret"), ("meg", "margaret"), ("tony", "anthony"), ("dave", "david"),
    ("chris", "christopher"), ("alex", "alexander"), ("sam", "samuel"), ("ben", "benjamin"), ("jenny", "jennifer"),
    ("jen", "jennifer"), ("sue", "susan"), ("andy", "andrew"), ("joe", "joseph"), ("steve", "stephen"),
    ("dan", "daniel"), ("danny", "daniel"), ("matt", "matthew"), ("nick", "nicholas"), ("vicky", "victoria"),
    ("becky", "rebecca"), ("ted", "edward"), ("ed", "edward"), ("charlie", "charles"), ("trish", "patricia"),
    ("pat", "patricia"), ("tim", "timothy"), ("pete", "peter"), ("fred", "frederick"), ("greg", "gregory"),
    ("larry", "lawrence"), ("debbie", "deborah"),
];

fn canonical(t: &str) -> &str {
    NICKNAMES.iter().find(|(n, _)| *n == t).map_or(t, |(_, c)| c)
}

/// Words that mark a business name (after `normalise`, so "& Co" is "co").
const BUSINESS: [&str; 22] = ["ltd", "limited", "plc", "llp", "lp", "llc", "inc", "incorporated", "corp", "corporation", "co",
    "company", "gmbh", "ag", "sa", "bv", "trading", "services", "holdings", "group", "partners", "partnership"];

#[derive(Debug, PartialEq)]
pub struct Check {
    /// "match", "close_match" or "no_match"
    pub verdict: &'static str,
    /// sanctions v2 score in [0, 1], rounded to 4 decimals (the value compared with the thresholds)
    pub score: f64,
    /// the account name, shown to the payer on a close match only
    pub suggested_name: Option<String>,
    pub reason: String,
}

fn is_business(t: &[String]) -> bool {
    t.iter().any(|x| BUSINESS.contains(&x.as_str()))
}

/// match: score >= t_hi and the entered name fits the account type; close_match: score >= t_lo (or >= t_hi with a type
/// mismatch); no_match otherwise. `account_type` is "individual", "business" or None (no type check).
/// `v2` (research/payee_check_eval_v2.py), individuals only: nicknames map to the canonical given name before scoring,
/// and close_match also needs an entered token matching the account surname (last token) at >= t_hi.
pub fn check(entered: &str, account: &str, account_type: Option<&str>, t_hi: f64, t_lo: f64, v2: bool) -> Check {
    let (mut e, mut a) = (normalise(entered), normalise(account));
    let business = account_type.map_or(is_business(&a), |t| t == "business");
    let v2 = v2 && !business;
    let mut nick = false;
    if v2 && !a.is_empty() {
        // the account's given names (all but the surname) map to canonical; an entered token maps only onto one of them
        let n = a.len() - 1;
        for t in a[..n].iter_mut() {
            (*t, nick) = (canonical(t).to_string(), nick || canonical(t) != t);
        }
        for t in e.iter_mut().filter(|t| canonical(t) != *t && a[..n].iter().any(|g| g == canonical(t))) {
            (*t, nick) = (canonical(t).to_string(), true);
        }
    }
    let score = (score_v2(&e, &a).0 * 1e4).round() / 1e4;
    let mismatch = account_type.is_some() && business != is_business(&e);
    let anchored = !v2 || a.last().is_some_and(|s| e.iter().any(|x| token_v2(x, s) >= t_hi));
    let verdict = if score >= t_hi && !mismatch { "match" } else if (score >= t_lo || score >= t_hi) && anchored { "close_match" } else { "no_match" };
    let (mut se, mut sa) = (e.clone(), a.clone());
    se.sort();
    sa.sort();
    let mut reason = if nick && se == sa { "same name, a nickname was used".into() } else { name_reason(&e, &a, business) };
    if mismatch {
        let why = if business { "the account is a business account but an individual's name was entered" } else { "the account is a personal account but a business name was entered" };
        reason = format!("{why}; {reason}");
    }
    Check { verdict, score, suggested_name: (verdict == "close_match").then(|| account.trim().to_string()), reason }
}

/// Short, token-level explanation: which parts of the name agree (a token pair agrees at the v2 subset FLOOR).
fn name_reason(e: &[String], a: &[String], business: bool) -> String {
    let same = |x: &String, y: &String| token_v2(x, y) >= FLOOR;
    if e == a {
        return "exact match".into();
    }
    let (mut se, mut sa) = (e.to_vec(), a.to_vec());
    se.sort();
    sa.sort();
    if se == sa {
        return "same name, words in a different order".into();
    }
    if business {
        let found = e.iter().filter(|x| a.iter().any(|y| same(x, y))).count();
        return match found {
            0 => "business name differs",
            n if n == e.len() && n == a.len() => "business name matches apart from spelling",
            n if n == e.len() => "business name matches but a word is missing (e.g. Ltd)",
            _ => "business name only partly matches",
        }.into();
    }
    let (Some(first), Some(last)) = (a.first(), a.last()) else { return "names differ".into() };
    let sur = e.iter().position(|x| same(x, last));
    let rest = || e.iter().enumerate().filter(move |(i, _)| Some(*i) != sur).map(|(_, x)| x);
    let fname = a.len() > 1 && rest().any(|x| same(x, first));
    let initial = a.len() > 1 && rest().any(|x| x.chars().count() == 1 && first.starts_with(x.as_str()));
    match (sur.is_some(), fname, initial) {
        (true, true, _) if e.len() < a.len() => "first name and surname match, a middle name is missing",
        (true, true, _) => "first name and surname match, spelling or format differs",
        (true, _, true) => "surname matches, first name given as an initial",
        (true, _, _) => "surname matches, first name differs",
        (false, true, _) => "first name matches, surname differs",
        _ => "names differ",
    }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(e: &str, a: &str, t: Option<&str>) -> Check {
        check(e, a, t, 0.95, 0.85, false)
    }

    fn c2(e: &str, a: &str) -> Check {
        check(e, a, Some("individual"), 0.95, 0.85, true)
    }

    #[test]
    fn v2_nicknames() {
        let r = c2("Bob Smith", "Robert Smith");
        assert_eq!((r.verdict, r.reason.as_str()), ("match", "same name, a nickname was used"));
        assert_eq!(c2("SMITH, Bill James", "William James Smith").verdict, "match");
        assert_eq!(c("Bob Smith", "Robert Smith", None).verdict, "no_match", "v1 is unchanged");
        // a surname that is also a diminutive is never mapped, and an entered token maps only onto an account given name
        assert_eq!(c2("John Will", "John Will").reason, "exact match");
        assert_eq!(c2("John Will", "John William").score, c("John Will", "John William", None).score);
        // business names are not touched
        assert_eq!(check("Bob Builders Ltd", "Robert Builders Ltd", Some("business"), 0.95, 0.85, true).verdict,
            check("Bob Builders Ltd", "Robert Builders Ltd", Some("business"), 0.95, 0.85, false).verdict);
    }

    #[test]
    fn v2_close_match_needs_the_surname() {
        let r = c2("J Smith", "John Smith");
        assert_eq!((r.verdict, r.suggested_name.as_deref()), ("close_match", Some("John Smith")));
        assert_eq!(c2("Mary Smith", "John Smith").verdict, c("Mary Smith", "John Smith", None).verdict, "given name may differ");
        let (v1, v2) = (c("John Smyth", "John Paul Brown", None), c2("John Smyth", "John Paul Brown"));
        assert_eq!(v2.verdict, "no_match", "no surname anchor, no close match (v1 gives {})", v1.verdict);
        assert_eq!((c2("John Smiht", "John Smith").verdict), c("John Smiht", "John Smith", None).verdict, "a surname typo still anchors");
    }

    #[test]
    fn verdicts_and_reasons() {
        let r = c("Mr John Smith", "John Smith", Some("individual"));
        assert_eq!((r.verdict, r.score, r.suggested_name, r.reason.as_str()), ("match", 1.0, None, "exact match"));
        assert_eq!(c("SMITH, John", "John Smith", None).reason, "same name, words in a different order");
        let r = c("J Smith", "John Smith", Some("individual"));
        assert_eq!((r.verdict, r.suggested_name.as_deref(), r.reason.as_str()), ("close_match", Some("John Smith"), "surname matches, first name given as an initial"));
        assert_eq!(c("John Smith", "John Paul Smith", None).reason, "first name and surname match, a middle name is missing");
        assert_eq!(c("Mary Smith", "John Smith", None).reason, "surname matches, first name differs");
        assert_eq!(c("John Brown", "John Smith", None).reason, "first name matches, surname differs");
        let r = c("Aldric Venthorpe", "John Smith", None);
        assert_eq!((r.verdict, r.suggested_name, r.reason.as_str()), ("no_match", None, "names differ"));
    }

    #[test]
    fn account_type_caps_a_match() {
        let r = c("John Smith", "John Smith", Some("business"));
        assert_eq!(r.verdict, "close_match", "a name match on the wrong account type is never a full match");
        assert!(r.reason.starts_with("the account is a business account but an individual's name was entered"));
        assert!(c("Smith Plumbing Ltd", "John Smith", Some("individual")).reason.starts_with("the account is a personal account"));
        let r = c("Acorn Plumbing Ltd", "Acorn Plumbing Ltd", Some("business"));
        assert_eq!((r.verdict, r.reason.as_str()), ("match", "exact match"));
        assert_eq!(c("Acorn Plumbng Ltd", "Acorn Plumbing Ltd", None).reason, "business name matches apart from spelling");
        assert_eq!(c("Acorn Plumbing", "Acorn Plumbing Ltd", None).reason, "business name matches but a word is missing (e.g. Ltd)");
    }
}
