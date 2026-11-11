//! RuseTect L-1 lure detector (port of service/scam_text.py): scores the message side of a scam (SMS, WhatsApp,
//! e-mail, call transcript).
//!
//!   featurize()    hashed word 1-2 grams + character 3-5 grams, sublinear TF x IDF, L2-normalised. Used by the
//!                  trainer (rusetect-ml train-message-model) and the API, so offline and online scores match exactly.
//!   tag()          rule-based explanation: which scam workflow the text resembles and which pressure tactics it uses.
//!   entities()     UPI IDs, phone numbers, links, account numbers and amounts mentioned in the text.
//!   MessageModel   loads service/models/message_model.json (logistic regression weights) and scores a text.
use crate::context_rules::nfkc;
use crate::pyfmt::{py_round, py_split, py_sum};
use crate::pyre::PyRegex;
use anyhow::Result;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

pub const N_BUCKETS: u32 = 1 << 18;

pub const URL_PATTERN: &str = r"(?:https?://|www\.)\S+|\b[a-z0-9-]+\.(?:com|in|net|org|co|xyz|top|info|link|ly|me|site|online|app)(?:/\S*)?\b";

struct Rx {
    url: PyRegex,
    upi: PyRegex,
    phone: PyRegex,
    acct: PyRegex,
    amount: PyRegex,
    money: PyRegex,
    digits: PyRegex,
    spaces: PyRegex,
    words: PyRegex,
    non_digit: PyRegex,
    safe_otp: PyRegex,
    delivery_otp: PyRegex,
}

static RX: LazyLock<Rx> = LazyLock::new(|| Rx {
    url: PyRegex::new(URL_PATTERN, true),
    upi: PyRegex::new(r"\b[a-z0-9._-]{2,64}@(?:ok[a-z]+|ybl|ibl|axl|paytm|upi|apl|ptyes|ptsbi|pthdfc|ptaxis|sbi|icici|hdfcbank|axisbank|kotak|yesbank|fbl|idfcbank|jupiteraxis|slc|waaxis|freecharge|airtel|jio)\b", true),
    phone: PyRegex::new(r"(?<!\d)(?:\+?91[\s-]?)?[6-9]\d{4}[\s-]?\d{5}(?!\d)|(?<!\d)(?:1800|1860)[\s-]?\d{3}[\s-]?\d{4}(?!\d)", false),
    acct: PyRegex::new(r"(?<!\d)\d{9,18}(?!\d)", false),
    amount: PyRegex::new(r"(?:rs\.?|inr|₹|£|\$)\s?([\d,]+(?:\.\d+)?)\s*(lakh|lac|crore|k)?", true),
    money: PyRegex::new(r"(?:rs\.?|inr|₹|£|\$)\s?[\d,]+(?:\.\d+)?", false),
    digits: PyRegex::new(r"\d+", false),
    spaces: PyRegex::new(r"\s+", false),
    words: PyRegex::new(r"[a-z_0]+|[ऀ-ॿ]+", false),
    non_digit: PyRegex::new(r"\D", false),
    safe_otp: PyRegex::new(r"(do not|don'?t|never) (share|disclose)|(bank|we) (will )?never (ask|call)", true),
    delivery_otp: PyRegex::new(r"share (the )?otp \d* ?only with the delivery", false),
});

pub fn normalize(text: &str) -> String {
    let t = nfkc(text).to_lowercase();
    let t = RX.url.sub(&t, " _url_ ");
    let t = RX.upi.sub(&t, " _upi_ ");
    let t = RX.phone.sub(&t, " _phone_ ");
    let t = RX.money.sub(&t, " _money_ ");
    let t = RX.digits.sub(&t, "0");
    RX.spaces.sub(&t, " ").trim_matches(crate::pyfmt::is_py_space).to_string()
}

/// Word, bigram and character n-grams of a normalised text, in Python's generation order.
pub fn grams(norm: &str) -> Vec<String> {
    let words = RX.words.find_all(norm);
    let mut out = Vec::new();
    for w in &words {
        out.push(format!("w:{w}"));
    }
    for p in words.windows(2) {
        out.push(format!("b:{} {}", p[0], p[1]));
    }
    let s: Vec<char> = format!(" {norm} ").chars().collect();
    for n in [3usize, 4, 5] {
        if s.len() >= n {
            for i in 0..=(s.len() - n) {
                let g: String = s[i..i + n].iter().collect();
                out.push(format!("c:{g}"));
            }
        }
    }
    out
}

pub fn bucket(g: &str) -> u32 {
    crc32fast::hash(g.as_bytes()) & (N_BUCKETS - 1)
}

/// Hashed term counts {bucket: count} for one text (insertion order kept, as in Python).
pub fn counts(text: &str) -> IndexMap<u32, u32> {
    let mut out: IndexMap<u32, u32> = IndexMap::new();
    for g in grams(&normalize(text)) {
        *out.entry(bucket(&g)).or_insert(0) += 1;
    }
    out
}

/// Sparse TF-IDF vector {bucket: weight}, L2-normalised. `idf` indexed by bucket.
pub fn featurize(text: &str, idf: &[f32]) -> IndexMap<u32, f64> {
    let v: IndexMap<u32, f64> = counts(text).into_iter().map(|(h, c)| (h, (1.0 + (c as f64).ln()) * idf[h as usize] as f64)).collect();
    let mut n = py_sum(v.values().map(|x| x * x)).sqrt();
    if n == 0.0 {
        n = 1.0;
    }
    v.into_iter().map(|(h, x)| (h, x / n)).collect()
}

// ---------------------------------------------------------------- explanations
pub const WORKFLOWS: [(&str, &str, &str, &[&str]); 15] = [
    ("digital_arrest", "Digital arrest / fake police", "Caller poses as CBI, ED, customs, police or TRAI and threatens arrest unless money is moved to a 'verification' account.",
     &[r"digital arrest", r"\b(cbi|ed|ncb|crime branch|cyber cell|trai|customs officer|mumbai police|delhi police)\b", r"arrest warrant|money laundering case|fir (has been|is) (registered|filed)", r"stay on (the )?video call|do not disconnect", r"(rbi|safe|secure|verification) account"]),
    ("parcel_customs", "Parcel / courier seizure", "Claims a parcel in your name holds drugs or fake passports, then hands you to a fake officer.",
     &[r"\b(parcel|courier|package|consignment)\b.*\b(seized|drugs|mdma|illegal|passports?|customs)\b", r"\b(fedex|dhl|blue ?dart)\b.*\b(seized|held|returned|illegal)\b"]),
    ("kyc_block", "KYC / account block", "Says your bank account, card, wallet or SIM will be blocked today unless you 'update KYC' through a link or call.",
     &[r"\b(kyc|pan|aadhaa?r)\b.*\b(update|expire|expired|pending|suspend|block)", r"(account|a/c|card|wallet|yono|sim).{0,40}(will be|has been|is) (blocked|suspended|deactivated|closed)", r"(block|suspend)ed (today|within)"]),
    ("upi_collect_refund", "Collect-request / refund trick", "Asks you to 'approve' a request or enter your UPI PIN to receive a refund, prize or payment. A PIN is never needed to receive money.",
     &[r"(enter|put|type).{0,20}(upi )?pin.{0,30}(receive|get|credit|refund)", r"(approve|accept).{0,30}(request|collect).{0,30}(receive|refund|cashback|credit)", r"scan.{0,20}qr.{0,30}(receive|get)"]),
    ("remote_access", "OTP / screen-share / app install", "Asks for an OTP or to install AnyDesk, TeamViewer, QuickSupport or an .apk, which hands control of your phone to the scammer.",
     &[r"\b(anydesk|teamviewer|quick ?support|rustdesk|airdroid)\b", r"\.apk\b", r"(share|send|tell|read out|forward).{0,25}\b(otp|code|pin)\b", r"screen ?shar"]),
    ("task_job", "Task / part-time job", "Offers easy daily earnings for liking videos, rating hotels or 'tasks', then asks for deposits to unlock withdrawals.",
     &[r"part[ -]?time", r"(like|rate|review|subscribe).{0,30}(videos?|hotels?|products?|channels?)", r"(earn|income|salary).{0,30}(daily|per day|/day|from home)", r"\b(prepaid|merchant) task", r"\btelegram\b.*\b(task|job|hr)\b"]),
    ("investment", "Fake investment / trading group", "Promises guaranteed or very high returns via a WhatsApp/Telegram 'stock tips' group, IPO quota or crypto app.",
     &[r"guaranteed (returns?|profit)", r"\d{2,4}\s?% (returns?|profit)", r"(stock|share|ipo|crypto|trading|forex).{0,40}(tips|group|vip|institutional|quota|allotment|profit)", r"(sebi|nse|bse).{0,20}(registered|approved)"]),
    ("utility_disconnect", "Bill / power disconnection", "Says electricity, gas or mobile service will be cut tonight unless you call a number or pay through a link.",
     &[r"(electricity|power|light|bijli|gas|connection).{0,40}(disconnect|cut|kaat|band)", r"(previous|last) month bill (was )?not (updated|paid)", r"tonight.{0,20}(9|10)[:.]?\d{0,2}\s?(pm)?"]),
    ("fake_loan", "Instant loan fee", "Pre-approved loan with no documents, but a 'processing', 'insurance' or 'GST' fee must be paid first.",
     &[r"(loan|credit).{0,40}(approved|sanctioned|pre-?approved)", r"(processing|file|insurance|gst|registration) (fee|charge)", r"(without|no) (cibil|documents?|income proof)"]),
    ("lottery_prize", "Lottery / prize / KBC", "You have 'won' a prize, lottery, car or lucky draw and must pay tax or fees to claim it.",
     &[r"\b(kbc|lottery|lucky draw|jackpot|lucky winner)\b", r"\b(you|u) (have )?(won|win)\b", r"(claim|collect).{0,20}(prize|reward|gift)"]),
    ("family_emergency", "Family emergency / voice clone", "A relative or friend (possibly an AI-cloned voice) is 'in trouble' and needs money urgently and secretly.",
     &[r"(papa|mummy|mom|dad|beta|bhai|uncle|mama|son|daughter).{0,60}(trouble|accident|hospital|police|arrest|caught|pakad)", r"(don'?t|do not|mat) (tell|batana|bata)", r"new number|naya number|this is my new"]),
    ("sextortion", "Sextortion threat", "Threatens to leak a recorded video call or photos unless paid.",
     &[r"(video|photos?|recording|clip).{0,40}(viral|leak|upload|send to your (family|contacts|friends))", r"(nude|obscene|objectionable|private) (video|photos?)"]),
    ("govt_scheme", "Fake government scheme / tax refund", "Fake PM Kisan, subsidy, income-tax refund or free-laptop scheme asking for details or a fee.",
     &[r"(pm kisan|subsidy|yojana|free laptop|income tax refund|it refund|tax refund|epfo|pension)", r"(government|govt|sarkari).{0,30}(scheme|benefit|grant)"]),
    ("reward_points", "Reward points / cashback expiry", "Card reward points or cashback 'expiring today', redeem through a link.",
     &[r"(reward|redeem|loyalty) points?", r"cashback.{0,30}(expir|claim|credited)"]),
    ("fake_support", "Fake customer care", "Fake helpline or refund agent who then asks for OTP, card details or remote access.",
     &[r"customer (care|support|executive)", r"(refund|complaint|order).{0,40}(call|contact|whatsapp).{0,20}(_phone_|\d{10})"]),
];

pub const TACTICS: [(&str, &str, &[&str]); 10] = [
    ("urgency", "Urgency", &[r"\b(urgent|immediately|within \d+ (hours?|hrs|minutes?|mins)|today|tonight|last (date|day|chance)|expir|abhi|turant|jaldi|asap)\b"]),
    ("authority", "Impersonates authority", &[r"\b(rbi|cbi|police|court|income tax|customs|trai|sebi|npci|government|govt|officer|ministry|uidai)\b"]),
    ("threat", "Threat or fear", &[r"\b(arrest|blocked?|suspend|legal action|penalty|jail|fir|warrant|disconnect|seized|viral|leak)\b"]),
    ("secrecy", "Asks for secrecy", &[r"(don'?t|do not|mat) (tell|share with|inform)|keep (it|this) (secret|confidential)|kisi ko (mat|na)"]),
    ("credential", "Asks for OTP / PIN / card", &[r"(share|send|tell|enter|provide|confirm|read out|forward).{0,25}\b(otp|upi pin|pin|cvv|password|card (number|details))\b"]),
    ("remote_app", "Remote-access app", &[r"\b(anydesk|teamviewer|quick ?support|rustdesk|airdroid)\b|\.apk\b"]),
    ("payment_ask", "Asks you to pay", &[r"\b(pay|transfer|send|deposit)\b.{0,30}(rs\.?|₹|inr|amount|fee|money|charges?|\d)", r"(processing|registration|gst|tax) (fee|charge)"]),
    ("too_good", "Too good to be true", &[r"guaranteed|\bwon\b|\bwinner\b|free gift|\d{2,4}\s?% (profit|returns?)|double (your )?money|per day"]),
    ("link", "Link to click", &[URL_PATTERN]),
    ("off_platform", "Moves you to WhatsApp / Telegram", &[r"\b(whatsapp|telegram|signal)\b"]),
];

/// Asks that RBI / NPCI / banks state are never legitimate. Any one of them puts a message at least in the caution band.
pub const HARD_SIGNALS: [(&str, &str, &str); 6] = [
    ("remote_app", r"\b(anydesk|teamviewer|quick ?support|rustdesk|airdroid)\b", "Asks you to install a remote-access app"),
    ("apk", r"\.apk\b", "Sends an .apk app file"),
    ("pin_to_receive", r"\b(upi pin|pin|approve|accept)\b.{0,40}\b(to receive|receive|get it|credit)", "Asks for a UPI PIN or approval to RECEIVE money"),
    ("digital_arrest", r"digital arrest|stay on (the )?video call|do not disconnect", "'Digital arrest': no police or agency arrests over video call"),
    ("safe_account", r"(rbi|safe|secure|verification) account", "Asks you to move money to a 'safe' or 'RBI' account"),
    ("share_otp", r"(share|send|tell|read out|forward)( me| us)?( the| your)? .{0,15}\b(otp|code)\b", "Asks you to share an OTP or code"),
];

static W_RX: LazyLock<Vec<Vec<PyRegex>>> = LazyLock::new(|| WORKFLOWS.iter().map(|w| w.3.iter().map(|p| PyRegex::new(p, true)).collect()).collect());
static T_RX: LazyLock<Vec<Vec<PyRegex>>> = LazyLock::new(|| TACTICS.iter().map(|t| t.2.iter().map(|p| PyRegex::new(p, true)).collect()).collect());
static H_RX: LazyLock<Vec<PyRegex>> = LazyLock::new(|| HARD_SIGNALS.iter().map(|h| PyRegex::new(h.1, true)).collect());

pub fn hard_signals(text: &str) -> Vec<Value> {
    let low = text.to_lowercase();
    let low = RX.safe_otp.sub(&low, " "); // genuine OTP SMS: "do not share this OTP"
    let low = RX.delivery_otp.sub(&low, " "); // genuine delivery-OTP instruction
    HARD_SIGNALS.iter().zip(H_RX.iter()).filter(|(_, r)| r.is_match(&low)).map(|(h, _)| json!({"id": h.0, "label": h.2})).collect()
}

pub struct Tagged {
    pub workflows: Vec<Value>,
    pub tactics: Vec<Value>,
}

/// Workflows the text resembles (best first) and the pressure tactics it uses.
pub fn tag(text: &str) -> Tagged {
    let low = text.to_lowercase();
    let mut hits: Vec<(usize, usize)> = Vec::new(); // (matches, workflow index)
    for (i, pats) in W_RX.iter().enumerate() {
        let n = pats.iter().filter(|p| p.is_match(&low)).count();
        if n > 0 {
            hits.push((n, i));
        }
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let mut tactics: Vec<Value> = TACTICS.iter().zip(T_RX.iter()).filter(|(_, ps)| ps.iter().any(|p| p.is_match(&low)))
        .map(|(t, _)| json!({"id": t.0, "label": t.1})).collect();
    if RX.safe_otp.is_match(&low) {
        tactics.retain(|t| t["id"] != "credential"); // "do not share your OTP" is a bank's warning, not a credential ask
    }
    let workflows = hits.iter().take(3).map(|(n, i)| {
        let w = &WORKFLOWS[*i];
        json!({"id": w.0, "label": w.1, "description": w.2, "matches": n})
    }).collect();
    Tagged { workflows, tactics }
}

pub fn entities(text: &str) -> Value {
    let mut amounts: Vec<Value> = Vec::new();
    for g in RX.amount.find_groups(text) {
        let Ok(mut v) = g[0].replace(',', "").parse::<f64>() else { continue };
        if g[0].replace(',', "").is_empty() {
            continue;
        }
        v *= match g[1].to_lowercase().as_str() {
            "lakh" | "lac" => 1e5,
            "crore" => 1e7,
            "k" => 1e3,
            _ => 1.0,
        };
        amounts.push(crate::pyfmt::jf(v));
    }
    let upi: BTreeSet<String> = RX.upi.find_all(text).into_iter().map(|m| m.to_lowercase()).collect();
    let phones: BTreeSet<String> = RX.phone.find_all(text).into_iter().map(|m| {
        let d: Vec<char> = RX.non_digit.sub(&m, "").chars().collect();
        d[d.len().saturating_sub(10)..].iter().collect()
    }).collect();
    let links: BTreeSet<String> = RX.url.find_all(text).into_iter().map(|m| m.trim_end_matches(['.', ',', ')']).to_string()).collect();
    let accounts: BTreeSet<String> = RX.acct.find_all(&RX.phone.sub(text, " ")).into_iter().collect();
    json!({"upi_ids": upi, "phones": phones, "links": links, "accounts": accounts, "amounts": amounts})
}

// ---------------------------------------------------------------- model
const STOP: &str = "a an the to of in on at by for from and or is are was be been your you yr ur u i me my we our us it this that with as \
will can has have had do not no so if but please pls sir madam dear hi hello 0 0th";

fn pretty(w: &str) -> &str {
    match w {
        "_money_" => "₹ amount",
        "_url_" => "link",
        "_upi_" => "UPI ID",
        "_phone_" => "phone number",
        _ => w,
    }
}

pub struct MessageModel {
    pub idf: Vec<f32>,
    pub w: Vec<f32>,
    pub b: f64,
    pub thr_high: f64,
    pub thr_caution: f64,
    pub meta: Value,
}

impl MessageModel {
    pub fn load(path: &Path) -> Result<Self> {
        let m: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        Self::from_json(&m)
    }

    pub fn from_json(m: &Value) -> Result<Self> {
        let unseen = m["idf_unseen"].as_f64().unwrap_or(1.0) as f32;
        let mut idf = vec![unseen; N_BUCKETS as usize];
        let mut w = vec![0.0f32; N_BUCKETS as usize];
        let arr = |k: &str| m[k].as_array().cloned().unwrap_or_default();
        let (buckets, idfs, coefs) = (arr("buckets"), arr("idf"), arr("coef"));
        for ((b, i), c) in buckets.iter().zip(&idfs).zip(&coefs) {
            let b = b.as_u64().unwrap_or(0) as usize;
            idf[b] = i.as_f64().unwrap_or(0.0) as f32;
            w[b] = c.as_f64().unwrap_or(0.0) as f32;
        }
        Ok(MessageModel {
            idf,
            w,
            b: m["intercept"].as_f64().unwrap_or(0.0),
            thr_high: m["thresholds"]["high"].as_f64().unwrap_or(0.75),
            thr_caution: m["thresholds"]["caution"].as_f64().unwrap_or(0.6),
            meta: m.get("meta").cloned().unwrap_or(json!({})),
        })
    }

    pub fn prob(&self, text: &str) -> f64 {
        let v = featurize(text, &self.idf);
        let z = self.b + py_sum(v.iter().map(|(h, x)| x * self.w[*h as usize] as f64));
        1.0 / (1.0 + (-z).exp())
    }

    /// Hashed n-grams with the largest positive contribution, mapped back to readable words.
    pub fn top_terms(&self, text: &str, k: usize) -> Vec<String> {
        let v = featurize(text, &self.idf);
        let norm = normalize(text);
        let stop: Vec<&str> = STOP.split_whitespace().collect();
        let mut seen: IndexMap<String, f64> = IndexMap::new();
        for g in grams(&norm) {
            if g.starts_with("c:") {
                continue;
            }
            let h = bucket(&g);
            let words = py_split(&g[2..]);
            if let Some(x) = v.get(&h) {
                if !words.iter().all(|w| stop.contains(w) || w.chars().count() < 3) {
                    let key = words.iter().map(|w| pretty(w)).collect::<Vec<_>>().join(" ");
                    seen.insert(key, x * self.w[h as usize] as f64);
                }
            }
        }
        let mut items: Vec<(String, f64)> = seen.into_iter().collect();
        items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        items.into_iter().take(k).filter(|(_, c)| *c > 0.0).map(|(t, _)| t).collect()
    }

    pub fn score(&self, text: &str) -> Value {
        let p = self.prob(text);
        let tg = tag(text);
        let hard = hard_signals(text);
        let (band, action) = if p >= self.thr_high || (!hard.is_empty() && p >= self.thr_caution) || hard.len() >= 2 {
            ("high", "Warn the user now and add a cool-off to payments to new payees for 24h")
        } else if p >= self.thr_caution || !hard.is_empty() {
            ("caution", "Show an inline caution; escalate only if a payment follows")
        } else {
            ("safe", "No action")
        };
        let risk = if hard.is_empty() { p } else { p.max(self.thr_caution) };
        let safe = band == "safe";
        json!({"risk": py_round(risk, 4), "band": band, "action": action,
               "workflow": if !safe { tg.workflows.first().cloned().unwrap_or(Value::Null) } else { Value::Null },
               "other_workflows": if !safe { tg.workflows.iter().skip(1).cloned().collect::<Vec<_>>() } else { vec![] },
               "tactics": tg.tactics, "key_phrases": if !safe { self.top_terms(text, 5) } else { vec![] },
               "hard_signals": hard, "model_risk": py_round(p, 4), "entities": entities(text)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const LEGIT_OTP: &str = "482913 is your OTP for transaction of Rs 2,300 at MYNTRA on HDFC Bank card XX1234. Do not share this OTP with anyone.";

    #[test]
    fn hard_signals_skip_genuine_warnings() {
        assert!(hard_signals(LEGIT_OTP).is_empty());
        assert!(hard_signals("Flipkart: Share OTP 123456 only with the delivery agent at your door.").is_empty());
        assert!(hard_signals("Install AnyDesk and share the code with me").iter().any(|h| h["id"] == "remote_app"));
        assert_eq!(entities("call 98765 43210 or pay to kyc.help@okaxis")["phones"], json!(["9876543210"]));
    }
}
