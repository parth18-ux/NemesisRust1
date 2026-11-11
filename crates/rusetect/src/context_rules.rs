//! RuseTect context layer (port of service/context_rules.py): consent, redaction, a keyword baseline and the
//! interpretable context + transaction + graph risk-point fusion.
//!
//! Fusion contract:
//!   * signal domains, each capped: context 0-40, transaction 0-30, graph 0-30 (+ session 0-40, ctx-1.4)
//!   * bands: CRITICAL >= 85, HIGH >= 60, MEDIUM >= 40, else LOW
//!   * HIGH and CRITICAL need at least two domains with points (one domain alone caps at MEDIUM)
//!   * MEDIUM means customer verification; HIGH or CRITICAL means analyst escalation
//!   * risk points are deterministic rule outputs, NOT calibrated fraud probabilities
//!   * AI authorship adds zero points: RuseTect scores fraud intent and behaviour, not who wrote the text
//!
//! Consent and privacy:
//!   * message context is processed only when the customer opted in; otherwise only payment and graph are scored
//!   * text is redacted before anything is stored; entities are kept only as salted hashes so a later payment to the
//!     same UPI ID / account can still be linked
//!   * every stored record carries an expiry (RETENTION_DAYS)
//!
//! Rules are frozen per release: RULESET_VERSION changes whenever a phrase or weight changes.
use crate::engine::{fx, Features};
use crate::pyre::PyRegex;
use crate::session_signals::SessionResult;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

/// 1.1: negation-aware cues, sender mule signature; 1.2: API fuses the intent model by default;
/// 1.3: tighter mule signature, tuned on the validation weeks of the full IBM HI-Small file (results/context_tuning_HI.json)
/// 1.4: session and identity domain, flagged sender network rule, Hindi and Hinglish cues
pub const RULESET_VERSION: &str = "ctx-1.4";
pub const RETENTION_DAYS: i64 = 30;
pub const CONTEXT_CAP: i64 = 40;
pub const TXN_CAP: i64 = 30;
pub const GRAPH_CAP: i64 = 30;

static SALT: LazyLock<String> = LazyLock::new(|| std::env::var("RUSETECT_HASH_SALT").unwrap_or_else(|_| "rusetect-demo-salt".into()));

pub fn action_for(band: &str) -> &'static str {
    match band {
        "LOW" => "allow",
        "MEDIUM" => "verify customer",
        "HIGH" => "escalate to analyst",
        _ => "escalate to analyst (priority)",
    }
}

pub fn nfkc(s: &str) -> String {
    s.nfkc().collect()
}

// ------------------------------------------------------------------ redaction
struct Patterns {
    upi: PyRegex,
    email: PyRegex,
    link: PyRegex,
    phone: PyRegex,
    account: PyRegex,
    code: PyRegex,
}

static PAT: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    upi: PyRegex::new(r"\b[a-z0-9._-]{2,64}@[a-z]{2,20}\b", true),
    email: PyRegex::new(r"\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}\b", true),
    link: PyRegex::new(r"(?:https?://|www\.)\S+", true),
    phone: PyRegex::new(r"(?<!\d)(?:\+?91[\s-]?)?[6-9]\d{4}[\s-]?\d{5}(?!\d)", false),
    account: PyRegex::new(r"(?<!\d)\d{9,18}(?!\d)", false),
    code: PyRegex::new(r"(?<!\d)\d{4,8}(?!\d)", false),
});

fn entity_hash(kind: &str, value: &str) -> String {
    let h = Sha256::digest(format!("{}|{}|{}", *SALT, kind, value.to_lowercase()).as_bytes());
    format!("{}:{}", kind, &hex::encode(h)[..16])
}

/// (redacted text, salted entity hashes). Links before accounts so digits inside URLs are not double counted.
pub fn redact(text: &str) -> (String, Vec<String>) {
    let mut out = nfkc(text);
    let mut ents = Vec::new();
    let order: [(&str, &PyRegex); 6] = [("link", &PAT.link), ("email", &PAT.email), ("upi", &PAT.upi), ("phone", &PAT.phone),
                                         ("account", &PAT.account), ("code", &PAT.code)];
    for (kind, rx) in order {
        out = rx.sub_with(&out, |c| {
            if kind != "code" {
                ents.push(entity_hash(kind, c.get(0).unwrap().as_str()));
            }
            format!("[{}]", kind.to_uppercase())
        });
    }
    (out, ents)
}

// ------------------------------------------------------------------ keyword baseline
pub const BASELINE_TERMS: [&str; 5] = ["urgent", "otp", "password", "verify", "account blocked"];
static BASELINE: LazyLock<Vec<(&'static str, PyRegex)>> = LazyLock::new(|| {
    BASELINE_TERMS.iter().map(|w| (*w, PyRegex::new(&format!(r"\b{}\b", fancy_escape(w)), false))).collect()
});

/// `re.escape` for the plain words used here (letters and spaces only need no escaping)
fn fancy_escape(w: &str) -> String {
    w.chars().map(|c| if c.is_alphanumeric() || c == '_' { c.to_string() } else if c == ' ' { "\\ ".into() } else { format!("\\{c}") }).collect()
}

pub fn keyword_baseline(text: &str) -> Value {
    let t = nfkc(text).to_lowercase();
    let hits: Vec<&str> = BASELINE.iter().filter(|(_, r)| r.is_match(&t)).map(|(w, _)| *w).collect();
    json!({"flag": !hits.is_empty(), "hits": hits})
}

// ------------------------------------------------------------------ context cue families (phrase proxy, frozen)
pub const CUES: [(&str, i64, &[&str]); 9] = [
    ("authority", 15, &[r"\b(?:finance|accounts|payments?) (?:office|team|department|desk)\b", r"\bceo\b", r"\bmanaging director\b",
        r"\b(?:bank|rbi|police|cbi|customs|income tax|cyber cell|trai|court) (?:officer|official|department|notice)\b",
        r"\bcustomer (?:care|support) (?:team|executive|agent)\b", r"\bsupport (?:team|desk|executive)\b",
        r"\b(?:on behalf of|calling from) (?:the )?(?:bank|rbi|police|government)\b", r"\b(?:police|cbi|crime branch|cyber cell|bank) (?:se|wala|wale) (?:bol|baat)",
        "पुलिस", "सीबीआई", "साइबर सेल", "बैंक से बोल"]),
    ("redirection", 15, &[r"\b(?:revised|updated|new|changed|replacement) (?:vendor |bank |account |payment )?(?:details|beneficiary|account|bank details)\b",
        r"\bremit (?:the )?(?:balance|amount|payment)\b", r"\btransfer (?:it |the amount |the money )?to (?:this|the following|a safe|the new)\b",
        r"\bsafe account\b", r"\bpay (?:to|into) (?:this|the new|a different)\b", r"\bsafe (?:khate|account) (?:me|mein)\b", "सुरक्षित खाते"]),
    ("pressure", 10, &[r"\b(?:before|ahead of) (?:the )?(?:afternoon |evening |end of day )?(?:cutoff|cut-off|deadline)\b", r"\bwithin (?:\d+|one|two|24) (?:minutes?|hours?)\b",
        r"\bimmediately\b", r"\bright now\b", r"\blast chance\b", r"\bor (?:else )?(?:your|the) account will\b", r"\bdigital arrest\b", r"\btoday itself\b",
        r"\b(?:giraftar|giraftaar|arrest ho)", r"\babhi (?:ke abhi|turant)\b", r"\bturant\b", "गिरफ्तार", "तुरंत", "अभी के अभी", "डिजिटल अरेस्ट"]),
    ("credential", 15, &[r"\b(?:share|send|tell|give|read out) (?:me |us )?(?:the |your )?(?:otp|pin|password|cvv|upi pin)\b", r"\benter (?:your )?upi pin\b",
        r"\b(?:otp|pin|password) (?:batao|bataiye|bata do|bhejo|bhejiye)\b", "ओटीपी बताइए", "ओटीपी बताओ", "ओटीपी भेज", "पिन बताइए"]),
    ("remote_access", 15, &[r"\b(?:anydesk|teamviewer|quick ?support|rustdesk)\b", r"\bscreen ?shar(?:e|ing)\b", "स्क्रीन शेयर", "एनीडेस्क"]),
    ("refund", 15, &[r"\brefund\b.*\b(?:scan|approve|collect request|enter)\b", r"\bcashback\b.*\bscan\b"]),
    ("investment", 20, &[r"\bguaranteed (?:returns?|profit)\b", r"\bdouble (?:your )?money\b", r"\b\d+% (?:daily|weekly|monthly) (?:returns?|profit)\b",
        r"\btrading (?:group|tips) (?:vip|premium)\b", r"\bpaisa double\b", r"\bpaise double\b", "पैसे डबल", "गारंटीड रिटर्न"]),
    ("identity", 15, &[r"\b(?:kyc|pan|aadhaar) (?:update|verification|expired|expiry|will expire)\b", r"\bre-?kyc\b", r"\bkyc (?:band|block|update kar)",
        "केवाईसी", "आधार अपडेट", "पैन अपडेट"]),
    ("secrecy", 10, &[r"\bdo not (?:tell|inform|share this with) (?:anyone|your family|the bank)\b", r"\bkeep (?:this|it) confidential\b",
        r"\bkisi ko (?:mat|na) (?:batana|bataiye|bolna)\b", "किसी को मत बताना", "किसी को न बताएं", "किसी को मत बताइए"]),
];

static CUE_RX: LazyLock<Vec<(&'static str, i64, Vec<PyRegex>)>> =
    LazyLock::new(|| CUES.iter().map(|(n, w, ps)| (*n, *w, ps.iter().map(|p| PyRegex::new(p, false)).collect())).collect());

static NEG: LazyLock<PyRegex> = LazyLock::new(|| {
    PyRegex::new(r"\b(?:never|not|no one|nobody|don't|dont|do not|won't|will not|cannot|can't|if (?:someone|anyone|anybody) (?:asks|calls|tells))\b", false)
});
/// "do not tell anyone" IS the cue
pub const NO_NEGATION: [&str; 1] = ["secrecy"];

/// True if the sentence holding the match has a negation before the match ("we will NOT ask you to ...").
fn negated(t: &str, start: usize) -> bool {
    let head = &t[..start];
    let s0 = ['.', '!', '?', '\n'].iter().filter_map(|c| head.rfind(*c)).max().map(|i| i + 1).unwrap_or(0);
    NEG.is_match(&t[s0..start])
}

#[derive(Debug, Clone, Default)]
pub struct Cues {
    pub points: i64,
    pub families: Vec<String>,
    pub negated: Vec<String>,
}

pub fn context_cues(text: &str, negation: bool) -> Cues {
    let t = nfkc(text).to_lowercase();
    let mut fam: Vec<(&str, i64)> = Vec::new();
    let mut neg: Vec<String> = Vec::new();
    for (name, w, pats) in CUE_RX.iter() {
        let hits: Vec<usize> = pats.iter().flat_map(|p| p.find_starts(&t)).collect();
        if hits.is_empty() {
            continue;
        }
        if negation && !NO_NEGATION.contains(name) && hits.iter().all(|&s| negated(&t, s)) {
            neg.push(name.to_string());
            continue;
        }
        fam.push((name, *w));
    }
    let points = CONTEXT_CAP.min(fam.iter().map(|f| f.1).sum());
    let mut families: Vec<String> = fam.iter().map(|f| f.0.to_string()).collect();
    families.sort();
    neg.sort();
    Cues { points, families, negated: neg }
}

/// Map the trained L-1 message model's band to context points (used only when a model result is supplied).
pub fn intent_points(model_band: Option<&str>) -> i64 {
    match model_band {
        Some("high") => 30,
        Some("caution") => 15,
        _ => 0,
    }
}

// ------------------------------------------------------------------ transaction and graph checks
#[derive(Debug, Clone, Default)]
pub struct PaymentContext {
    pub new_beneficiary: bool,
    /// amount outside the payer's usual range (> 3x their mean)
    pub amount_deviation: bool,
    /// payee forwards most of what it received within minutes
    pub rapid_pass_through: bool,
    /// distinct senders paying the payee in 24 h
    pub fan_in_24h: i64,
    /// payee or device shared by several flagged accounts
    pub coordinated: bool,
    /// distinct accounts that paid the SENDER in the last 24 h (mule signature)
    pub sender_fan_in_24h: i64,
    /// forwards >= 95% of the latest deposit within 2 minutes (None = unknown)
    pub tight_pass_through: Option<bool>,
    /// how many different accounts the sender has paid so far (None = unknown)
    pub sender_distinct_payees: Option<i64>,
    /// payee UPI / account matches an entity hash from the message
    pub payee_in_message: bool,
}

pub struct Points {
    pub points: i64,
    pub checks: Vec<String>,
}

pub fn transaction_points(c: &PaymentContext) -> Points {
    let mut parts: Vec<(&str, i64)> = Vec::new();
    if c.new_beneficiary {
        parts.push(("new_beneficiary", 15));
    }
    if c.amount_deviation {
        parts.push(("amount_deviation", 15));
    }
    if c.payee_in_message {
        parts.push(("payee_named_in_message", 15));
    }
    let mut checks: Vec<String> = parts.iter().map(|p| p.0.to_string()).collect();
    checks.sort();
    Points { points: TXN_CAP.min(parts.iter().map(|p| p.1).sum()), checks }
}

pub fn graph_points(c: &PaymentContext) -> Points {
    let mut parts: Vec<(&str, i64)> = Vec::new();
    if c.fan_in_24h >= 4 {
        parts.push(("fan_in_4plus", 15));
    }
    if c.rapid_pass_through {
        parts.push(("rapid_pass_through", 15));
    }
    if c.coordinated {
        parts.push(("coordinated_accounts", 15));
    }
    if c.sender_fan_in_24h >= 4 && c.rapid_pass_through && c.tight_pass_through != Some(false)
        && c.sender_distinct_payees.map(|n| n <= 10).unwrap_or(true)
    {
        // ctx-1.3: collects from 4+ payers, forwards >= 95% within 2 min, pays out to few accounts
        parts.push(("sender_mule_signature", 15));
    }
    let mut checks: Vec<String> = parts.iter().map(|p| p.0.to_string()).collect();
    checks.sort();
    Points { points: GRAPH_CAP.min(parts.iter().map(|p| p.1).sum()), checks }
}

// ------------------------------------------------------------------ fusion
pub fn band_for(total: i64, domains_with_points: usize) -> &'static str {
    let b = if total >= 85 {
        "CRITICAL"
    } else if total >= 60 {
        "HIGH"
    } else if total >= 40 {
        "MEDIUM"
    } else {
        "LOW"
    };
    if domains_with_points < 2 && (b == "HIGH" || b == "CRITICAL") {
        "MEDIUM"
    } else {
        b
    }
}

#[derive(Debug, Clone)]
pub struct ContextResult {
    pub context: i64,
    pub transaction: i64,
    pub graph: i64,
    pub total: i64,
    pub band: String,
    pub action: String,
    pub reasons: Vec<String>,
    pub consent: bool,
    pub redacted_text: Option<String>,
    pub entity_hashes: Vec<String>,
    pub keyword_baseline: Value,
    pub ruleset: String,
    pub expires_at: Option<String>,
    /// ctx-1.4: session and identity domain
    pub session: i64,
    pub session_detail: Option<Value>,
}

impl ContextResult {
    pub fn to_json(&self) -> Value {
        json!({"context": self.context, "transaction": self.transaction, "graph": self.graph, "total": self.total,
               "band": self.band, "action": self.action, "reasons": self.reasons, "consent": self.consent,
               "redacted_text": self.redacted_text, "entity_hashes": self.entity_hashes, "keyword_baseline": self.keyword_baseline,
               "ruleset": self.ruleset, "expires_at": self.expires_at, "session": self.session, "session_detail": self.session_detail})
    }
}

/// Score one payment with optional message context. Text is used only if consent is true. `negation=false`
/// reproduces ruleset ctx-1.0; `model_band` from the L-1 message model can lift the context score.
pub fn assess(text: Option<&str>, payment: &PaymentContext, consent: bool, now: Option<DateTime<Utc>>, negation: bool,
              model_band: Option<&str>, session: Option<&SessionResult>) -> ContextResult {
    let now = now.unwrap_or_else(Utc::now);
    let mut ctx = Cues::default();
    let (mut red, mut ents, mut base) = (None, Vec::new(), json!({}));
    let text = text.filter(|t| !t.is_empty());
    if let (true, Some(t)) = (consent, text) {
        let (r, e) = redact(t);
        red = Some(r);
        ents = e;
        ctx = context_cues(t, negation);
        base = keyword_baseline(t);
        let mp = intent_points(model_band);
        if mp > ctx.points {
            ctx.points = CONTEXT_CAP.min(mp);
            ctx.families.push(format!("intent model ({})", model_band.unwrap_or("None")));
        }
    }
    let tx = transaction_points(payment);
    let gr = graph_points(payment);
    let sp = session.map(|s| s.points).unwrap_or(0);
    let total = ctx.points + tx.points + gr.points + sp;
    let n_dom = [ctx.points, tx.points, gr.points, sp].iter().filter(|p| **p > 0).count();
    let mut band = band_for(total, n_dom);
    if gr.checks.iter().any(|c| c == "sender_mule_signature") && band == "LOW" {
        band = "MEDIUM"; // ctx-1.1: a collector that forwards within minutes is reviewed even without other domains
    }
    if session.map(|s| s.force_verify).unwrap_or(false) && band == "LOW" {
        band = "MEDIUM"; // ctx-1.4: likely coercion, or possibly not the customer: always ask
    }
    let mut reasons: Vec<String> = ctx.families.iter().map(|f| format!("message cue: {f}")).collect();
    reasons.extend(tx.checks.iter().map(|c| format!("payment check: {c}")));
    reasons.extend(gr.checks.iter().map(|c| format!("account graph: {c}")));
    if let Some(s) = session {
        reasons.extend(s.cues.iter().map(|c| format!("session: {c}")));
    }
    reasons.extend(ctx.negated.iter().map(|f| format!("negated, not counted: {f}")));
    if !consent && text.is_some() {
        reasons.push("message context not used: no consent".into());
    }
    let expires = (now + Duration::days(RETENTION_DAYS)).format("%Y-%m-%dT%H:%M:%S+00:00").to_string();
    ContextResult {
        context: ctx.points,
        transaction: tx.points,
        graph: gr.points,
        total,
        band: band.into(),
        action: action_for(band).into(),
        reasons,
        consent,
        redacted_text: red,
        entity_hashes: ents,
        keyword_baseline: base,
        ruleset: RULESET_VERSION.into(),
        expires_at: Some(expires),
        session: sp,
        session_detail: session.map(|s| s.to_json()),
    }
}

// ------------------------------------------------------------------ combining with the payment cascade
pub const PAYMENT_RANK: [(&str, usize); 4] = [("Approve", 0), ("Step-up verification", 1), ("Hold for analyst", 2), ("Block + STR draft", 3)];

pub fn payment_rank(d: &str) -> usize {
    PAYMENT_RANK.iter().find(|(n, _)| *n == d).map(|(_, r)| *r).unwrap_or(0)
}

/// Context never auto-blocks; the analyst decides.
pub fn context_to_rank(band: &str) -> usize {
    match band {
        "MEDIUM" => 1,
        "HIGH" | "CRITICAL" => 2,
        _ => 0,
    }
}

/// Context veto: the final action is the stricter of the two; context alone never goes beyond an analyst hold.
pub fn combine(payment_decision: &str, ctx: &ContextResult) -> Map<String, Value> {
    let p = payment_rank(payment_decision);
    let c = context_to_rank(&ctx.band);
    let final_ = PAYMENT_RANK[p.max(c)].0;
    let mut m = Map::new();
    m.insert("payment_decision".into(), json!(payment_decision));
    m.insert("context_band".into(), json!(ctx.band));
    m.insert("final_decision".into(), json!(final_));
    m.insert("raised_by_context".into(), json!(c > p));
    m
}

// ------------------------------------------------------------------ bridge from the live payment engine (ctx-1.1)
/// Transaction and graph checks from the engine's own live (past-only) features.
pub fn from_engine_features(f: &Features, sender_fan_in: usize) -> PaymentContext {
    let g = |i: usize| f.0[i];
    let ok = |v: f64| !v.is_nan();
    let hist = g(fx::S_OUT_N) >= 3.0;
    let usd = g(fx::LOG_USD).exp_m1();
    let usual = g(fx::S_OUT_MEAN_USD).exp_m1();
    let (msi, ratio) = (g(fx::S_MINS_SINCE_IN), g(fx::S_OUT_OVER_LAST_IN));
    let rapid = ok(msi) && msi <= 10.0 && ok(ratio) && ratio >= 0.8;
    let tight = ok(msi) && msi <= 2.0 && ok(ratio) && ratio >= 0.95;
    PaymentContext {
        new_beneficiary: g(fx::NEW_PAIR) != 0.0,
        amount_deviation: hist && usd > 3.0 * usual,
        rapid_pass_through: rapid,
        fan_in_24h: g(fx::D_FANIN_24H) as i64,
        coordinated: false,
        sender_fan_in_24h: sender_fan_in as i64,
        tight_pass_through: Some(tight),
        sender_distinct_payees: Some(g(fx::S_UNIQ_DST) as i64),
        payee_in_message: false,
    }
}

/// Same, from the API's `features` object (null = unknown).
pub fn features_from_json(m: &Map<String, Value>) -> Features {
    let mut f = [f64::NAN; 31];
    for (i, k) in crate::engine::FEATURES.iter().enumerate() {
        f[i] = m.get(*k).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
    }
    Features(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_consent_means_text_is_ignored_and_not_stored() {
        let s9 = "The finance office has revised the vendor details. Please remit the balance to the replacement beneficiary ahead of the afternoon cutoff.";
        let r = assess(Some(s9), &PaymentContext { new_beneficiary: true, ..Default::default() }, false, None, true, None, None);
        assert_eq!(r.context, 0);
        assert!(r.redacted_text.is_none() && r.entity_hashes.is_empty());
        assert!(r.reasons.contains(&"message context not used: no consent".to_string()));
    }

    #[test]
    fn redaction_removes_identifiers_and_keeps_hashes() {
        let (txt, ents) = redact("Pay 50000 to fraudster99@ybl or call 98765 43210, acct 123456789012, OTP 4821, see https://x.in/a");
        for secret in ["fraudster99@ybl", "98765", "123456789012", "4821", "https://x.in"] {
            assert!(!txt.contains(secret), "{secret} in {txt}");
        }
        assert!(ents.iter().any(|e| e.starts_with("upi:")) && ents.iter().any(|e| e.starts_with("account:")));
        assert_eq!(redact("a@ybl").1, redact("A@YBL").1);
    }

    #[test]
    fn single_domain_never_escalates() {
        assert_eq!(band_for(70, 1), "MEDIUM");
        assert_eq!(band_for(70, 2), "HIGH");
        assert_eq!(band_for(85, 2), "CRITICAL");
    }

    #[test]
    fn keyword_baseline_misses_s09() {
        assert_eq!(keyword_baseline("The finance office has revised the vendor details.")["flag"], json!(false));
        assert_eq!(keyword_baseline("Share your OTP urgently")["flag"], json!(true));
    }

    #[test]
    fn context_veto_raises_but_never_blocks() {
        let hi = assess(Some("This is the CEO. Pay into the new account details immediately."),
                        &PaymentContext { new_beneficiary: true, amount_deviation: true, ..Default::default() }, true, None, true, None, None);
        let out = combine("Approve", &hi);
        assert_eq!(out["final_decision"], json!("Hold for analyst"));
        assert_eq!(out["raised_by_context"], json!(true));
        assert_eq!(combine("Block + STR draft", &hi)["final_decision"], json!("Block + STR draft"));
        let low = assess(None, &PaymentContext::default(), true, None, true, None, None);
        assert_eq!(combine("Approve", &low)["final_decision"], json!("Approve"));
    }

    #[test]
    fn negation_is_respected() {
        let c = context_cues("We will never ask you to share your OTP.", true);
        assert!(c.families.is_empty() && c.negated == vec!["credential".to_string()]);
    }
}
