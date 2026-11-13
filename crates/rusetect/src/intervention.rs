//! What the customer sees when RuseTect asks for verification, and the duress PIN (port of service/intervention.py).
//!
//! Pause screen: one question chosen from the evidence, a one line fact, two buttons: stop the payment, or continue
//! after a 30 minute cool off. English and Hindi. The analyst still sees the alert.
//! Duress PIN: a second PIN set in advance. Entered under pressure, the app shows "payment successful", the money is
//! held, trusted contacts and the fraud desk are alerted. PINs are stored only as salted PBKDF2 hashes.
use serde_json::{json, Value};
use std::collections::HashMap;
use subtle::ConstantTimeEq;

pub const COOL_OFF_MIN: i64 = 30;

/// question key -> [(en question, en fact), (hi question, hi fact)]
pub fn text(key: &str) -> [(&'static str, &'static str); 2] {
    match key {
        "call" => [("Is someone on the phone or sharing your screen asking you to make this payment?",
                    "Banks never ask you to move money or share your screen to \"verify\" anything."),
                   ("क्या कोई फ़ोन पर है या आपकी स्क्रीन देख रहा है और यह भुगतान करने को कह रहा है?",
                    "बैंक कभी भी पैसे भेजने या स्क्रीन शेयर करने को नहीं कहते।")],
        "arrest" => [("Has anyone said you are under investigation or \"digital arrest\" and must pay?",
                      "Police, CBI and courts never ask for money on a call. There is no such thing as digital arrest."),
                     ("क्या किसी ने कहा कि आप जाँच या \"डिजिटल अरेस्ट\" में हैं और पैसे देने होंगे?",
                      "पुलिस, सीबीआई या अदालत कभी फ़ोन पर पैसे नहीं माँगते। डिजिटल अरेस्ट जैसी कोई चीज़ नहीं होती।")],
        "kyc" => [("Did someone ask you to pay to unblock your account, update KYC or receive a refund?",
                   "You never pay or enter your PIN to receive money or to update KYC."),
                  ("क्या किसी ने खाता चालू करने, केवाईसी अपडेट करने या रिफ़ंड पाने के लिए पैसे भेजने को कहा?",
                   "पैसे पाने या केवाईसी अपडेट के लिए कभी भुगतान या पिन डालना नहीं पड़ता।")],
        "investment" => [("Were you promised guaranteed or very high returns for this payment?",
                          "Guaranteed returns are a common scam. Check the company on SEBI's website first."),
                         ("क्या इस भुगतान पर आपको पक्का या बहुत ज़्यादा मुनाफ़ा देने का वादा किया गया है?",
                          "पक्के मुनाफ़े का वादा अक्सर धोखा होता है। पहले सेबी की वेबसाइट पर कंपनी जाँचें।")],
        "family" => [("Has someone claiming to be family or a friend asked for money urgently?",
                      "Hang up and call them back on a number you already know before paying."),
                     ("क्या परिवार या दोस्त बनकर किसी ने तुरंत पैसे माँगे हैं?",
                      "पैसे भेजने से पहले फ़ोन काटें और उन्हें अपने पास पहले से मौजूद नंबर पर वापस कॉल करें।")],
        _ => [("Do you know and trust the person you are paying?",
               "If anyone is rushing you or telling you to keep this secret, stop and call your bank."),
              ("क्या आप उस व्यक्ति को जानते हैं और उस पर भरोसा करते हैं जिसे आप भुगतान कर रहे हैं?",
               "अगर कोई जल्दी करवा रहा है या इसे छिपाने को कह रहा है, तो रुकें और अपने बैंक को फ़ोन करें।")],
    }
}

fn buttons(lang: &str) -> (String, String) {
    if lang == "en" {
        ("Yes, stop this payment".into(), format!("No, continue in {COOL_OFF_MIN} min"))
    } else {
        ("हाँ, यह भुगतान रोकें".into(), format!("नहीं, {COOL_OFF_MIN} मिनट बाद जारी रखें"))
    }
}

fn title(lang: &str) -> &'static str {
    if lang == "en" { "Pause for a moment" } else { "एक पल रुकिए" }
}

/// message model workflow -> question
pub fn workflow_question(w: &str) -> Option<&'static str> {
    Some(match w {
        "digital_arrest" | "parcel_customs" | "sextortion" => "arrest",
        "remote_access" | "fake_support" => "call",
        "kyc_block" | "upi_collect_refund" | "reward_points" | "govt_scheme" | "utility_disconnect" | "lottery_prize" | "fake_loan" => "kyc",
        "investment" | "task_job" => "investment",
        "family_emergency" => "family",
        _ => return None,
    })
}

/// The most specific evidence wins: a named scam workflow, then live call or screen share cues, then message cues.
pub fn pick_question(reasons: &[String], workflow: Option<&str>) -> &'static str {
    if let Some(q) = workflow.and_then(workflow_question) {
        return q;
    }
    let r = reasons.join(" ").to_lowercase();
    if r.contains("pressure") && (r.contains("authority") || r.contains("arrest")) {
        return "arrest";
    }
    if ["on_call", "screen_share", "remote_access", "long_or_video_call"].iter().any(|k| r.contains(k)) {
        return "call";
    }
    if r.contains("investment") {
        return "investment";
    }
    if ["identity", "refund", "credential"].iter().any(|k| r.contains(k)) {
        return "kyc";
    }
    "default"
}

/// What the bank app should show for this decision.
pub fn plan(final_decision: &str, reasons: &[String], identity: Option<&Value>, workflow: Option<&str>) -> Value {
    if identity.and_then(|i| i.get("decision")).and_then(|d| d.as_str()) == Some("deny_otp_reverify") {
        return json!({"show": "reverify", "otp": "withheld",
            "message": {"en": "For your safety we could not send an OTP to this device. Please re-verify by video KYC or at your branch.",
                        "hi": "आपकी सुरक्षा के लिए इस डिवाइस पर ओटीपी नहीं भेजा गया। कृपया वीडियो केवाईसी या शाखा में दोबारा सत्यापन करें।"}});
    }
    if final_decision == "Approve" {
        return json!({"show": "none"});
    }
    let key = pick_question(reasons, workflow);
    let t = text(key);
    let mut screen = serde_json::Map::new();
    for (i, lang) in ["en", "hi"].iter().enumerate() {
        let (stop, cont) = buttons(lang);
        screen.insert(lang.to_string(), json!({"title": title(lang), "question": t[i].0, "fact": t[i].1, "stop": stop, "continue": cont}));
    }
    let held = final_decision != "Step-up verification";
    json!({"show": "pause", "question_type": key, "cool_off_minutes": COOL_OFF_MIN, "screen": screen,
           "step_up": if held { vec![] } else { vec!["voice or face check"] },
           "funds": if held { "held for analyst review" } else { "released after the cool off unless the customer stops it" }})
}

/// One registered customer: salted PBKDF2 hashes of both PINs. Opaque outside this module.
pub struct DuressUser {
    salt: [u8; 16],
    normal: [u8; 32],
    duress: [u8; 32],
    contacts: Vec<String>,
    failed: u32,
}

/// Registered duress users; new registrations are refused when full (a registered user is never dropped silently).
// shortcut: in-memory and capped, upgrade to a real store (encrypted database) before enrolling real customers
pub const MAX_DURESS_USERS: usize = if cfg!(test) { 2 } else { 100_000 };
/// Duress notifications kept; the oldest is dropped first.
pub const MAX_NOTIFICATIONS: usize = if cfg!(test) { 2 } else { 100_000 };

/// Wrong PINs in a row before confirmation is locked (a 4 digit PIN would otherwise fall to 10,000 guesses).
pub const MAX_FAILED_PINS: u32 = 5;

impl std::fmt::Debug for DuressUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DuressUser {{ salt: {}, normal: {}, duress: {}, contacts: {} }}", hex::encode(self.salt), hex::encode(self.normal),
               hex::encode(self.duress), self.contacts.len())
    }
}

#[derive(Default, Debug)]
pub struct DuressVault {
    users: HashMap<String, DuressUser>,
    pub notifications: Vec<Value>,
}

/// Deliberately slow (PBKDF2, 120,000 rounds): callers holding a shared lock should hash first, outside it.
pub fn pin_hash(pin: &str, salt: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(pin.as_bytes(), salt, 120_000, &mut out);
    out
}

/// Python `str.isdigit()` for a PIN character
fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || (c.is_numeric() && !matches!(c, '\u{2150}'..='\u{218f}' | '\u{00bc}'..='\u{00be}'))
}

impl DuressVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.users.clear();
        self.notifications.clear();
    }

    /// Validate and hash a registration. Slow; holds no state, so it can run outside any lock.
    pub fn prepare(normal_pin: &str, duress_pin: &str, trusted_contacts: &[String]) -> Result<DuressUser, String> {
        if normal_pin == duress_pin {
            return Err("the duress PIN must differ from the normal PIN".into());
        }
        let ok = |p: &str| (4..=6).contains(&p.chars().count()) && p.chars().all(is_digit);
        if !(ok(normal_pin) && ok(duress_pin)) {
            return Err("PINs are 4 to 6 digits".into());
        }
        let salt: [u8; 16] = rand::random();
        let contacts: Vec<String> = trusted_contacts.iter().take(5).cloned().collect();
        Ok(DuressUser { salt, normal: pin_hash(normal_pin, &salt), duress: pin_hash(duress_pin, &salt), contacts, failed: 0 })
    }

    /// Store a prepared registration. An account that is already registered is never overwritten: otherwise anyone
    /// could replace a customer's PINs and trusted contacts.
    pub fn insert(&mut self, account: &str, u: DuressUser) -> Result<Value, String> {
        if self.users.contains_key(account) {
            return Err("account already registered".into());
        }
        if self.users.len() >= MAX_DURESS_USERS {
            return Err(format!("duress registry is full ({MAX_DURESS_USERS} accounts)"));
        }
        let n = u.contacts.len();
        self.users.insert(account.to_string(), u);
        Ok(json!({"account": account, "trusted_contacts": n}))
    }

    pub fn register(&mut self, account: &str, normal_pin: &str, duress_pin: &str, trusted_contacts: &[String]) -> Result<Value, String> {
        self.insert(account, Self::prepare(normal_pin, duress_pin, trusted_contacts)?)
    }

    /// The account's salt, to hash a PIN outside the lock; None when not registered.
    pub fn salt(&self, account: &str) -> Option<[u8; 16]> {
        self.users.get(account).map(|u| u.salt)
    }

    /// What the customer sees and what really happens, given pin_hash(pin, salt). Constant time comparison; after
    /// MAX_FAILED_PINS wrong PINs in a row the account is locked until it is re-registered by the bank.
    pub fn confirm_hashed(&mut self, account: &str, h: &[u8; 32]) -> Value {
        let Some(u) = self.users.get_mut(account) else { return json!({"result": "not_registered"}) };
        if u.failed >= MAX_FAILED_PINS {
            return json!({"result": "locked"});
        }
        if bool::from(h.ct_eq(&u.duress)) {
            u.failed = 0;
            let contacts = u.contacts.clone();
            if self.notifications.len() >= MAX_NOTIFICATIONS {
                self.notifications.remove(0);
            }
            self.notifications.push(json!({"at": crate::pyfmt::utc_now_z(), "account": account, "to": contacts,
                "message": "Possible coercion: a payment was confirmed with the duress PIN. The money is on hold."}));
            return json!({"result": "duress", "shown_to_customer": "Payment successful", "funds": "held",
                          "trusted_contacts_alerted": contacts.len(), "fraud_desk": "priority alert"});
        }
        if bool::from(h.ct_eq(&u.normal)) {
            u.failed = 0;
            return json!({"result": "ok"});
        }
        u.failed += 1;
        json!({"result": if u.failed >= MAX_FAILED_PINS { "locked" } else { "wrong_pin" }})
    }

    pub fn confirm(&mut self, account: &str, pin: &str) -> Value {
        match self.salt(account) {
            Some(salt) => self.confirm_hashed(account, &pin_hash(pin, &salt)),
            None => json!({"result": "not_registered"}),
        }
    }

    pub fn debug_dump(&self) -> String {
        format!("{:?}", self.users)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_screen_question_follows_the_evidence() {
        assert_eq!(pick_question(&["message cue: authority".into(), "message cue: pressure".into()], None), "arrest");
        assert_eq!(pick_question(&["session: screen_share".into()], None), "call");
        let p = plan("Step-up verification", &["session: on_call_while_paying".into()], None, None);
        assert_eq!(p["show"], "pause");
        assert_eq!(p["cool_off_minutes"], 30);
        assert_eq!(p["screen"].as_object().unwrap().keys().cloned().collect::<Vec<_>>(), vec!["en", "hi"]);
        assert_eq!(plan("Approve", &[], None, None)["show"], "none");
        assert_eq!(plan("Hold for analyst", &[], None, None)["funds"], "held for analyst review");
    }

    #[test]
    fn duress_pin_looks_successful_but_holds() {
        let mut v = DuressVault::new();
        v.register("A1", "1234", "4321", &["+91-90000-00000".into()]).unwrap();
        assert_eq!(v.confirm("A1", "1234")["result"], "ok");
        let d = v.confirm("A1", "4321");
        assert_eq!(d["shown_to_customer"], "Payment successful");
        assert_eq!(d["funds"], "held");
        assert!(!v.notifications.is_empty());
        assert_eq!(v.confirm("A1", "0000")["result"], "wrong_pin");
        assert!(v.register("A1", "1111", "2222", &[]).is_err(), "a registration is never overwritten");
        for _ in 1..MAX_FAILED_PINS {
            v.confirm("A1", "0000");
        }
        assert_eq!(v.confirm("A1", "1234")["result"], "locked", "even the right PIN after too many wrong ones");
        assert!(v.register("A2", "1111", "1111", &[]).is_err());
        assert!(!v.debug_dump().contains("1234"));
    }

    #[test]
    fn duress_vault_is_capped() {
        let mut v = DuressVault::new();
        v.register("A1", "1234", "4321", &[]).unwrap();
        v.register("A2", "1234", "4321", &[]).unwrap();
        assert!(v.register("A3", "1234", "4321", &[]).unwrap_err().contains("full"), "refused, nobody evicted");
        assert_eq!(v.confirm("A1", "1234")["result"], "ok");
        for _ in 0..3 {
            v.confirm("A2", "4321");
        }
        assert_eq!(v.notifications.len(), MAX_NOTIFICATIONS);
    }
}
