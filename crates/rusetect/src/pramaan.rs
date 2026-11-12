//! PRAMAAN account-takeover authentication: allow / challenge / block for a login or payment, and which challenge.
//!
//! Ported from PRAMAAN (github.com/abhinavtop1G/PayPal_AI_Hackathon_Pramaan, MIT, Team CaffeineEaters: Abhinav Saini,
//! Vyom Khanna, Sanyam Ahuja, Parth Sharma): ml/auth/{features,state,geo,model,policy,engine}.py. The decisions match
//! the Python engine request by request (research/pramaan/reference.py, examples/pramaan_parity.rs).
//!
//! request + live state -> 27 causal features -> monotone XGBoost (risk can only rise with each signal; exact
//! tree-SHAP signals) -> prior shift to real-world attack prevalence -> cost-based policy that picks the step-up the
//! likely attack cannot pass (never an SMS code after a SIM swap) -> decision, reasons (English and Hindi), signals.
//! The touch and typing deviations (`behaviour.bio_dev`, `behaviour.typing_dev`) are inputs here: PRAMAAN computes them
//! with its touch BiLSTM + LDA and typing 1D-CNN; RuseTect's own typing check can supply `typing_dev`.
use crate::models::XgbBooster;
use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

const DAY: f64 = 86400.0;
const TRUST_AFTER_DAYS: f64 = 7.0;
const TRUST_AFTER_USES: u32 = 3;
const MAX_PAYEES: usize = 300;
const PRIOR_LOG_AMT: f64 = 6.908754779315221; // ln(1 + 1000)
const PRIOR_SD: f64 = 1.2;
/// Bounds on remembered state (users, devices, IP addresses, pending challenges); new keys beyond them are not stored.
const MAX_KEYS: usize = 2_000_000;

/// (name, family, English reason, Hindi reason). Hindi to be reviewed by a native speaker before user-facing use.
pub const SPEC: [(&str, &str, &str, &str); 27] = [
    ("bio_dev", "behaviour", "touch pattern does not match the account owner", "टच करने का तरीका खाताधारक से मेल नहीं खाता"),
    ("typing_dev", "behaviour", "typing rhythm does not match the account owner", "टाइप करने की लय खाताधारक से मेल नहीं खाती"),
    ("fast_submit", "behaviour", "submitted faster than a person can type", "इतनी तेज़ी से भरा गया जितना कोई व्यक्ति टाइप नहीं कर सकता"),
    ("low_input", "behaviour", "almost no screen interaction before paying", "भुगतान से पहले लगभग कोई स्क्रीन गतिविधि नहीं"),
    ("unknown_call_active", "behaviour", "on a call with an unknown number while paying", "भुगतान के दौरान अनजान नंबर से कॉल चल रही है"),
    ("screen_share_active", "behaviour", "screen is being shared", "स्क्रीन शेयर की जा रही है"),
    ("device_new", "device", "first time this device is used on the account", "इस खाते पर यह डिवाइस पहली बार इस्तेमाल हुआ"),
    ("device_untrusted", "device", "device is not yet trusted for this account", "यह डिवाइस अभी इस खाते के लिए भरोसेमंद नहीं है"),
    ("n_devices_30d", "device", "many devices used on this account recently", "हाल में इस खाते पर कई डिवाइस इस्तेमाल हुए"),
    ("accounts_on_device", "device", "this device was used for several other accounts", "इस डिवाइस से कई अन्य खाते इस्तेमाल हुए"),
    ("emulator", "device", "request comes from an emulator", "अनुरोध एमुलेटर से आया है"),
    ("rooted", "device", "device is rooted / jailbroken", "डिवाइस रूटेड / जेलब्रोकन है"),
    ("remote_access_app", "device", "a remote-access app is running", "रिमोट-एक्सेस ऐप चल रहा है"),
    ("sim_fresh", "device", "SIM card was changed recently", "सिम कार्ड हाल ही में बदला गया"),
    ("ip_datacenter", "device", "connection comes from a hosting provider, not a home or mobile network", "कनेक्शन घर/मोबाइल नेटवर्क से नहीं, होस्टिंग सर्वर से है"),
    ("ip_anonymiser", "device", "connection goes through a VPN / Tor", "कनेक्शन VPN / Tor से है"),
    ("ip_accounts_1h", "device", "many accounts used from this IP address in the last hour", "पिछले एक घंटे में इस IP से कई खाते इस्तेमाल हुए"),
    ("geo_speed", "device", "location changed sharply since the last payment", "पिछले भुगतान के बाद स्थान में बड़ा बदलाव"),
    ("new_city", "device", "payment from a city this account has never used", "ऐसे शहर से भुगतान जहाँ से यह खाता कभी इस्तेमाल नहीं हुआ"),
    ("amt_z", "transaction", "amount is much larger than usual for this account", "राशि इस खाते के सामान्य भुगतान से बहुत अधिक है"),
    ("payee_new", "transaction", "first payment to this payee", "इस प्राप्तकर्ता को पहला भुगतान"),
    ("payee_risk", "transaction", "receiving account is linked to suspected fraud", "प्राप्तकर्ता खाता संदिग्ध धोखाधड़ी से जुड़ा है"),
    ("txn_count_1h", "transaction", "many payments in the last hour", "पिछले एक घंटे में कई भुगतान"),
    ("spend_24h_z", "transaction", "unusually high spending in the last 24 hours", "पिछले 24 घंटों में असामान्य रूप से अधिक खर्च"),
    ("hour_rarity", "transaction", "unusual time of day for this account", "इस खाते के लिए असामान्य समय"),
    ("short_history", "transaction", "account has little payment history", "खाते का भुगतान इतिहास बहुत कम है"),
    ("fails_24h", "transaction", "failed verification attempts in the last 24 hours", "पिछले 24 घंटों में असफल सत्यापन प्रयास"),
];

fn fi(name: &str) -> usize {
    SPEC.iter().position(|s| s.0 == name).expect("known feature")
}

// ------------------------------------------------------------------------------------------------ geo
/// Demo subsets of public hosting-provider and anonymiser ranges, as in PRAMAAN (production loads maintained feeds).
const DATACENTER: [&str; 9] = ["3.0.0.0/9", "13.64.0.0/11", "34.64.0.0/10", "35.184.0.0/13", "104.131.0.0/16", "139.59.0.0/16",
    "159.89.0.0/16", "172.104.0.0/15", "51.68.0.0/16"];
const ANONYMISER: [&str; 4] = ["185.220.100.0/22", "199.249.230.0/24", "45.83.64.0/22", "103.216.220.0/22"];
/// Python's `ipaddress.IPv4Address.is_private` networks (plus loopback), which PRAMAAN reads as "unknown".
const PRIVATE: [&str; 13] = ["0.0.0.0/8", "10.0.0.0/8", "127.0.0.0/8", "169.254.0.0/16", "172.16.0.0/12", "192.0.0.0/24",
    "192.0.2.0/24", "192.168.0.0/16", "198.18.0.0/15", "198.51.100.0/24", "203.0.113.0/24", "240.0.0.0/4", "255.255.255.255/32"];

fn in_cidr(a: u32, cidr: &str) -> bool {
    let (net, bits) = cidr.split_once('/').unwrap();
    let net: std::net::Ipv4Addr = net.parse().unwrap();
    let bits: u32 = bits.parse().unwrap();
    let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
    a & mask == u32::from(net) & mask
}

/// "datacenter" | "anonymiser" | "residential" | "unknown"
pub fn ip_kind(ip: Option<&str>) -> &'static str {
    let Some(ip) = ip.filter(|s| !s.is_empty()) else { return "unknown" };
    match ip.parse::<std::net::IpAddr>() {
        Err(_) => "unknown",
        Ok(std::net::IpAddr::V4(v4)) => {
            let a = u32::from(v4);
            if ANONYMISER.iter().any(|c| in_cidr(a, c)) {
                "anonymiser"
            } else if DATACENTER.iter().any(|c| in_cidr(a, c)) {
                "datacenter"
            } else if PRIVATE.iter().any(|c| in_cidr(a, c)) {
                "unknown"
            } else {
                "residential"
            }
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            // loopback, link-local, unique-local, documentation: private as in Python
            if v6.is_loopback() || v6.is_unspecified() || (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xfe00) == 0xfc00 || (s[0] == 0x2001 && s[1] == 0x0db8) {
                "unknown"
            } else {
                "residential"
            }
        }
    }
}

pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = (p2 - p1, (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().min(1.0).asin()
}

/// Hour of day in India Standard Time (UTC+5:30).
fn hour(ts: f64) -> usize {
    (((ts + 19800.0).rem_euclid(DAY)) / 3600.0).floor() as usize % 24
}

// ------------------------------------------------------------------------------------------------ request access
fn part<'a>(req: &'a Value, k: &str) -> Option<&'a Map<String, Value>> {
    req.get(k).and_then(|v| v.as_object())
}

/// Python truthiness of a JSON value
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|x| x != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn num(m: Option<&Map<String, Value>>, k: &str) -> Option<f64> {
    m.and_then(|m| m.get(k)).and_then(|v| v.as_f64()).filter(|x| x.is_finite())
}

fn text<'a>(m: Option<&'a Map<String, Value>>, k: &str) -> Option<&'a str> {
    m.and_then(|m| m.get(k)).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

// ------------------------------------------------------------------------------------------------ state
#[derive(Clone, Debug)]
struct Device {
    first: f64,
    last: f64,
    n: u32,
    trusted: bool,
}

impl Device {
    fn trusted_at(&self, now: f64) -> bool {
        self.trusted || (now - self.first >= TRUST_AFTER_DAYS * DAY && self.n >= TRUST_AFTER_USES)
    }
}

#[derive(Clone, Debug, Default)]
struct User {
    devices: IndexMap<String, Device>,
    geo: Option<(f64, f64, f64)>,
    cities: HashMap<String, u32>,
    amt: (u64, f64, f64),
    payees: IndexMap<String, u32>,
    hours: [u32; 24],
    recent: Vec<(f64, f64)>,
    fails: Vec<f64>,
    n: u64,
}

struct Pending {
    req: Value,
    challenge_type: &'static str,
}

/// What the service remembers about users, devices and IP addresses (PRAMAAN ml/auth/state.py), in memory.
#[derive(Default)]
pub struct AuthState {
    users: HashMap<String, User>,
    device_users: HashMap<String, HashMap<String, f64>>,
    ip_users: HashMap<String, HashMap<String, f64>>,
    pending: IndexMap<String, Pending>,
}

impl AuthState {
    fn user(&self, uid: &str) -> User {
        self.users.get(uid).cloned().unwrap_or_default()
    }

    fn put_user(&mut self, uid: &str, u: User) {
        if self.users.len() < MAX_KEYS || self.users.contains_key(uid) {
            self.users.insert(uid.to_string(), u);
        }
    }

    fn recent(map: &HashMap<String, HashMap<String, f64>>, key: &str, now: f64, window: f64) -> HashMap<String, f64> {
        map.get(key).map(|m| m.iter().filter(|(_, t)| now - **t <= window).map(|(u, t)| (u.clone(), *t)).collect()).unwrap_or_default()
    }

    /// Every attempt, completed or not: which accounts each device and IP address touches.
    fn record_attempt(&mut self, uid: &str, device: Option<&str>, ip: Option<&str>, now: f64) {
        for (map, key, window) in [(&mut self.device_users, device, 30.0 * DAY), (&mut self.ip_users, ip, 3600.0)] {
            let Some(key) = key else { continue };
            let mut users = Self::recent(map, key, now, window);
            users.insert(uid.to_string(), now);
            if map.len() < MAX_KEYS || map.contains_key(key) {
                map.insert(key.to_string(), users);
            }
        }
    }

    /// A completed transaction becomes part of the user's history.
    fn commit(&mut self, uid: &str, req: &Value, now: f64, trust_device: bool) {
        let mut u = self.user(uid);
        let (dev, net, tx) = (part(req, "device"), part(req, "network"), part(req, "transaction"));
        if let Some(id) = text(dev, "device_id") {
            let d = u.devices.entry(id.to_string()).or_insert(Device { first: now, last: now, n: 0, trusted: false });
            d.last = now;
            d.n += 1;
            d.trusted |= trust_device;
        }
        if let (Some(lat), Some(lon)) = (num(net, "lat"), num(net, "lon")) {
            u.geo = Some((lat, lon, now));
        }
        if let Some(city) = text(net, "city") {
            *u.cities.entry(city.to_string()).or_insert(0) += 1;
        }
        let amount = num(tx, "amount").unwrap_or(0.0);
        if amount > 0.0 {
            let x = amount.ln_1p();
            let (mut n, mut mean, mut m2) = u.amt;
            n += 1;
            let delta = x - mean;
            mean += delta / n as f64;
            m2 += delta * (x - mean);
            u.amt = (n, mean, m2);
            u.recent.retain(|r| now - r.0 <= DAY);
            u.recent.push((now, amount));
            if let Some(pid) = text(tx, "payee_id") {
                *u.payees.entry(pid.to_string()).or_insert(0) += 1;
                if u.payees.len() > MAX_PAYEES {
                    // forget the rarest payees (stable: earliest seen first among equals, as Python's sorted)
                    let mut order: Vec<(String, u32)> = u.payees.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    order.sort_by_key(|kv| kv.1);
                    for (k, _) in order.into_iter().take(u.payees.len() - MAX_PAYEES) {
                        u.payees.shift_remove(&k);
                    }
                }
            }
        }
        u.hours[hour(now)] += 1;
        u.n += 1;
        self.put_user(uid, u);
    }

    fn record_failure(&mut self, uid: &str, now: f64) {
        let mut u = self.user(uid);
        u.fails.retain(|t| now - t <= DAY);
        u.fails.push(now);
        self.put_user(uid, u);
    }

    pub fn clear(&mut self) {
        *self = AuthState::default();
    }
}

// ------------------------------------------------------------------------------------------------ features
pub struct Ctx {
    device_trusted: bool,
    has_other_trusted_device: bool,
    last_fail_age_s: Option<f64>,
}

/// Request + live state -> the 27 features (NaN = missing), read before any state is written (causal).
pub fn build(req: &Value, st: &AuthState) -> ([f64; 27], Ctx) {
    let now = req["ts"].as_f64().unwrap_or(0.0);
    let uid = req["user_id"].as_str().unwrap_or("");
    let u = st.user(uid);
    let (dev, net, beh, tx) = (part(req, "device"), part(req, "network"), part(req, "behaviour"), part(req, "transaction"));
    let mut f = [f64::NAN; 27];
    let b = |x: bool| if x { 1.0 } else { 0.0 };
    f[fi("bio_dev")] = num(beh, "bio_dev").unwrap_or(f64::NAN);
    f[fi("typing_dev")] = num(beh, "typing_dev").unwrap_or(f64::NAN);
    f[fi("fast_submit")] = b(num(beh, "time_to_submit_s").is_some_and(|t| t < 3.0));
    f[fi("low_input")] = b(num(beh, "input_events").is_some_and(|n| n < 4.0));
    f[fi("unknown_call_active")] = b(truthy(beh.and_then(|m| m.get("unknown_call_active"))));
    f[fi("screen_share_active")] = b(truthy(dev.and_then(|m| m.get("screen_share_active"))));

    let dev_id = text(dev, "device_id");
    let known = dev_id.and_then(|id| u.devices.get(id));
    let trusted_here = known.is_some_and(|d| d.trusted_at(now));
    f[fi("device_new")] = b(known.is_none());
    f[fi("device_untrusted")] = b(known.is_some() && !trusted_here);
    f[fi("n_devices_30d")] = (u.devices.values().filter(|d| now - d.last <= 30.0 * DAY).count() + known.is_none() as usize) as f64;
    let others = dev_id.map(|id| AuthState::recent(&st.device_users, id, now, 30.0 * DAY)).unwrap_or_default();
    f[fi("accounts_on_device")] = others.keys().filter(|k| *k != uid).count() as f64;
    f[fi("emulator")] = b(truthy(dev.and_then(|m| m.get("is_emulator"))));
    f[fi("rooted")] = b(truthy(dev.and_then(|m| m.get("is_rooted"))));
    f[fi("remote_access_app")] = b(truthy(dev.and_then(|m| m.get("remote_access_app"))));
    f[fi("sim_fresh")] = num(dev, "sim_change_hours").map(|h| (1.0 - h / 168.0).max(0.0)).unwrap_or(0.0);
    let ip = text(net, "ip");
    let kind = ip_kind(ip);
    f[fi("ip_datacenter")] = b(kind == "datacenter");
    f[fi("ip_anonymiser")] = b(kind == "anonymiser");
    let ip_users = ip.map(|a| AuthState::recent(&st.ip_users, a, now, 3600.0)).unwrap_or_default();
    f[fi("ip_accounts_1h")] = ip_users.keys().filter(|k| *k != uid).count() as f64;
    f[fi("geo_speed")] = match (u.geo, num(net, "lat"), num(net, "lon")) {
        (Some((lat0, lon0, t0)), Some(lat), Some(lon)) => {
            let km = haversine_km(lat0, lon0, lat, lon);
            (km / ((now - t0) / 3600.0).max(0.25)).min(20000.0).ln_1p()
        }
        _ => f64::NAN,
    };
    f[fi("new_city")] = b(u.n >= 3 && text(net, "city").is_some_and(|c| !u.cities.contains_key(c)));

    let amount = num(tx, "amount").unwrap_or(0.0);
    let (n, mean, m2) = u.amt;
    let (mu, sd) = if n >= 3 { (mean, (m2 / (n - 1) as f64).sqrt().max(0.35)) } else { (PRIOR_LOG_AMT, PRIOR_SD) };
    let recent: Vec<&(f64, f64)> = u.recent.iter().filter(|r| now - r.0 <= DAY).collect();
    if amount > 0.0 {
        f[fi("amt_z")] = (amount.ln_1p() - mu) / sd;
        f[fi("spend_24h_z")] = ((amount + recent.iter().map(|r| r.1).sum::<f64>()).ln_1p() - mu) / sd;
    }
    let pid = text(tx, "payee_id");
    f[fi("payee_new")] = b(pid.is_some_and(|p| !u.payees.contains_key(p)));
    f[fi("payee_risk")] = num(tx, "payee_risk").unwrap_or(f64::NAN);
    f[fi("txn_count_1h")] = recent.iter().filter(|r| now - r.0 <= 3600.0).count() as f64;
    f[fi("hour_rarity")] = 1.0 - (u.hours[hour(now)] as f64 + 1.0) / (u.n as f64 + 24.0);
    f[fi("short_history")] = b(u.n < 5);
    f[fi("fails_24h")] = u.fails.iter().filter(|t| now - **t <= DAY).count() as f64;
    let ctx = Ctx {
        device_trusted: trusted_here,
        has_other_trusted_device: u.devices.iter().any(|(k, d)| d.trusted_at(now) && Some(k.as_str()) != dev_id),
        last_fail_age_s: u.fails.iter().cloned().fold(None, |m: Option<f64>, t| Some(m.map_or(t, |x| x.max(t)))).map(|t| now - t),
    };
    (f, ctx)
}

// ------------------------------------------------------------------------------------------------ policy
pub const CHALLENGES: [&str; 4] = ["sms_otp", "passkey", "bank_callback", "cool_off"];
const STRONG: [&str; 2] = ["passkey", "bank_callback"];

/// Cost-based policy (PRAMAAN ml/auth/policy.py). Efficacies and frictions are assumptions to tune with the bank.
pub struct AuthPolicy {
    pub efficacy: [f64; 4],
    pub friction: [f64; 4],
    pub block_cost: f64,
    pub login_exposure: f64,
    pub fail_block_window_s: f64,
    pub block_min_p: f64,
}

impl Default for AuthPolicy {
    fn default() -> Self {
        AuthPolicy { efficacy: [0.55, 0.90, 0.85, 0.55], friction: [8.0, 6.0, 60.0, 40.0], block_cost: 400.0, login_exposure: 20000.0,
                     fail_block_window_s: 1800.0, block_min_p: 0.5 }
    }
}

fn ci(c: &str) -> usize {
    CHALLENGES.iter().position(|x| *x == c).unwrap()
}

impl AuthPolicy {
    /// The step-up that fits the signals that fired: one the likely attack cannot pass.
    pub fn choose_challenge(&self, f: &[f64; 27], ctx: &Ctx) -> &'static str {
        let bio = f[fi("bio_dev")];
        let own_hands = !(bio.is_finite() && bio > 0.5);
        let set = |k: &str| f[fi(k)] != 0.0 && !f[fi(k)].is_nan();
        if set("remote_access_app") || (set("screen_share_active") && !own_hands) {
            return "bank_callback";
        }
        if ctx.device_trusted && own_hands && (set("unknown_call_active") || set("screen_share_active")) {
            return "cool_off";
        }
        if f[fi("sim_fresh")] > 0.0 {
            return "passkey"; // never an SMS code: after a SIM swap it goes to the attacker
        }
        if (set("device_new") || set("device_untrusted")) && ctx.has_other_trusted_device {
            return "passkey";
        }
        "sms_otp"
    }

    /// (action, challenge type, hard rule, expected cost of allow / challenge / block)
    pub fn decide(&self, p: f64, amount: f64, f: &[f64; 27], ctx: &Ctx) -> (&'static str, Option<&'static str>, Option<&'static str>, [Option<f64>; 3]) {
        let a = if amount > 0.0 { amount } else { self.login_exposure };
        let first = self.choose_challenge(f, ctx);
        let options: Vec<&'static str> = if first == "bank_callback" || f[fi("sim_fresh")] > 0.0 { vec![first] } else { vec![first, "bank_callback"] };
        let ch_cost = |o: &str| p * a * (1.0 - self.efficacy[ci(o)]) + (1.0 - p) * self.friction[ci(o)];
        let mut c = options[0];
        for o in &options[1..] {
            if ch_cost(o) < ch_cost(c) {
                c = o;
            }
        }
        let cost = [p * a, ch_cost(c), if p < self.block_min_p { f64::INFINITY } else { (1.0 - p) * self.block_cost }];
        let mut action = ["allow", "challenge", "block"][(0..3).fold(0, |b, i| if cost[i] < cost[b] { i } else { b })];
        let set = |k: &str| f[fi(k)] != 0.0 && !f[fi(k)].is_nan();
        let mut rule = None;
        if ctx.last_fail_age_s.is_some_and(|s| s <= self.fail_block_window_s) {
            (action, rule) = ("block", Some("recent_failed_challenge"));
        } else if action == "allow" && set("emulator") && (set("ip_datacenter") || set("ip_anonymiser")) {
            (action, rule) = ("challenge", Some("emulator_on_hosting_ip"));
        } else if action == "allow" && set("device_new") && amount > 0.0 {
            // device binding: the first payment from a device never seen on this account is always stepped up
            (action, rule, c) = ("challenge", Some("new_device_binding"), self.choose_challenge(f, ctx));
        }
        let r2 = |v: f64| if v.is_finite() { Some((v * 100.0).round() / 100.0) } else { None };
        (action, if action == "challenge" { Some(c) } else { None }, rule, [r2(cost[0]), r2(cost[1]), r2(cost[2])])
    }
}

fn message(decision: &str, challenge: Option<&str>, coached: bool) -> (&'static str, &'static str) {
    match (decision, challenge, coached) {
        ("block", _, true) => ("This payment was stopped. If someone on a call or screen share is telling you to pay, it is a scam: banks, police and courts never ask for payments like this. Hang up and report it at 1930.",
                               "यह भुगतान रोका गया है। अगर कॉल या स्क्रीन शेयर पर कोई आपसे भुगतान करवा रहा है, तो यह ठगी है: बैंक, पुलिस और अदालतें कभी ऐसे भुगतान नहीं माँगतीं। कॉल काटें और 1930 पर शिकायत करें।"),
        ("challenge", Some("bank_callback"), true) => ("Your bank will call you on your registered number before this payment goes through. If someone on a call is telling you to pay, hang up: banks and police never do this. Report fraud at 1930.",
                                                       "यह भुगतान होने से पहले आपका बैंक आपके रजिस्टर्ड नंबर पर कॉल करेगा। अगर कॉल पर कोई आपसे भुगतान करवा रहा है तो कॉल काट दें: बैंक और पुलिस ऐसा कभी नहीं करते। धोखाधड़ी की शिकायत 1930 पर करें।"),
        ("challenge", Some("sms_otp"), _) => ("Enter the one-time password sent to your registered mobile number.", "अपने रजिस्टर्ड मोबाइल नंबर पर भेजा गया OTP दर्ज करें।"),
        ("challenge", Some("passkey"), _) => ("Approve this payment on your registered device with your fingerprint or face.", "अपने रजिस्टर्ड डिवाइस पर फ़िंगरप्रिंट या चेहरे से इस भुगतान को स्वीकृत करें।"),
        ("challenge", Some("bank_callback"), _) => ("Your bank will call you on your registered number to confirm this payment.", "इस भुगतान की पुष्टि के लिए आपका बैंक आपके रजिस्टर्ड नंबर पर कॉल करेगा।"),
        ("challenge", _, _) => ("We've paused this payment for 30 minutes. If someone on a call is asking you to pay, hang up: banks and police never do this. Report fraud at 1930.",
                                "हमने यह भुगतान 30 मिनट के लिए रोका है। अगर कॉल पर कोई आपसे भुगतान करवाना चाहता है तो कॉल काट दें: बैंक और पुलिस ऐसा कभी नहीं करते। धोखाधड़ी की शिकायत 1930 पर करें।"),
        ("block", _, _) => ("This payment was blocked to protect your account. Call your bank's helpline if this was you.", "आपके खाते की सुरक्षा के लिए यह भुगतान रोका गया है। अगर यह आप थे, तो अपने बैंक की हेल्पलाइन पर कॉल करें।"),
        _ => ("Payment approved.", "भुगतान स्वीकृत।"),
    }
}

// ------------------------------------------------------------------------------------------------ engine
/// Monotone XGBoost risk model with the prior shift to real-world attack prevalence.
pub struct AuthModel {
    booster: XgbBooster,
    shift: f64,
    pub version: String,
}

fn logit(p: f64) -> f64 {
    let p = p.clamp(1e-9, 1.0 - 1e-9);
    (p / (1.0 - p)).ln()
}

impl AuthModel {
    /// service/models/auth_xgb.json and auth_config.json (written by research/pramaan/reference.py); None if absent.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let cfg = dir.join("auth_config.json");
        if !cfg.exists() {
            return Ok(None);
        }
        let c: Value = serde_json::from_str(&std::fs::read_to_string(&cfg)?).context("auth_config.json")?;
        let names: Vec<&str> = c["features"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
        anyhow::ensure!(names == SPEC.iter().map(|s| s.0).collect::<Vec<_>>(), "auth_config.json: unexpected feature order");
        let shift = logit(c["real_prior"].as_f64().unwrap_or(0.002)) - logit(c["fit_prior"].as_f64().context("fit_prior")?);
        Ok(Some(AuthModel { booster: XgbBooster::load(&dir.join("auth_xgb.json"))?, shift, version: c["version"].as_str().unwrap_or("dev").into() }))
    }
}

pub struct AuthEngine {
    pub model: AuthModel,
    pub policy: AuthPolicy,
    pub state: AuthState,
    reason_min: f64,
}

impl AuthEngine {
    pub fn new(model: AuthModel) -> Self {
        AuthEngine { model, policy: AuthPolicy::default(), state: AuthState::default(), reason_min: 0.15 }
    }

    /// Decide one login or payment. `req` has user_id, ts (epoch seconds) and optional device, network, behaviour
    /// and transaction objects (PRAMAAN's AuthRequest).
    pub fn authenticate(&mut self, req: &Value, decision_id: String) -> Value {
        let t0 = std::time::Instant::now();
        let now = req["ts"].as_f64().unwrap_or(0.0);
        let uid = req["user_id"].as_str().unwrap_or("").to_string();
        let (f, ctx) = build(req, &self.state);
        let amount = num(part(req, "transaction"), "amount").unwrap_or(0.0);
        let x: Vec<f32> = f.iter().map(|v| *v as f32).collect();
        let c = self.model.booster.contributions(&x);
        let total: f32 = c.iter().sum();
        let p = 1.0 / (1.0 + (-(total as f64 + self.model.shift)).exp());
        let mut signals: Vec<(usize, f64)> = (0..27).map(|i| (i, (c[i] as f64 * 1e4).round() / 1e4)).collect();
        signals.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (decision, challenge, rule, cost) = self.policy.decide(p, amount, &f, &ctx);
        let (dev, net) = (part(req, "device"), part(req, "network"));
        self.state.record_attempt(&uid, text(dev, "device_id"), text(net, "ip"), now);
        if decision == "allow" {
            self.state.commit(&uid, req, now, false);
        } else if decision == "challenge" && (self.state.pending.len() < 100_000 || self.state.pending.contains_key(&decision_id)) {
            let slim = json!({"user_id": uid, "ts": now, "device": req.get("device").cloned().unwrap_or(json!({})),
                              "network": req.get("network").cloned().unwrap_or(json!({})), "transaction": req.get("transaction").cloned().unwrap_or(json!({}))});
            self.state.pending.insert(decision_id.clone(), Pending { req: slim, challenge_type: challenge.unwrap() });
        }
        let coached = (f[fi("unknown_call_active")] != 0.0 || f[fi("screen_share_active")] != 0.0) && ctx.device_trusted;
        let (en, hi) = message(decision, challenge, coached);
        let reasons: Vec<Value> = if decision == "allow" { vec![] } else {
            signals.iter().filter(|s| s.1 >= self.reason_min).take(3)
                .map(|(i, v)| json!({"signal": SPEC[*i].0, "family": SPEC[*i].1, "text_en": SPEC[*i].2, "text_hi": SPEC[*i].3, "contribution": v})).collect()
        };
        let mut fam = IndexMap::new();
        for k in ["behaviour", "device", "transaction"] {
            let s: f64 = signals.iter().filter(|(i, _)| SPEC[*i].1 == k).map(|s| s.1).sum();
            fam.insert(k, (s * 1e4).round() / 1e4);
        }
        let fv = |v: f64| if v.is_finite() { json!((v * 1e4).round() / 1e4) } else { Value::Null };
        json!({
            "decision_id": decision_id, "user_id": uid, "decision": decision, "risk_score": (p * 1e5).round() / 1e5,
            "challenge_type": challenge, "rule": rule, "message_en": en, "message_hi": hi, "reasons": reasons,
            "family_contribution": fam,
            "signals": signals.iter().map(|(i, v)| json!({"name": SPEC[*i].0, "family": SPEC[*i].1, "value": fv(f[*i]), "contribution": v})).collect::<Vec<_>>(),
            "features": SPEC.iter().enumerate().map(|(i, s)| (s.0.to_string(), fv(f[i]))).collect::<Map<String, Value>>(),
            "expected_cost": {"allow": cost[0], "challenge": cost[1], "block": cost[2]}, "amount": amount,
            "model_version": self.model.version, "latency_ms": (t0.elapsed().as_secs_f64() * 1e5).round() / 100.0,
        })
    }

    /// The outcome of a challenge: passed -> allow (a strong challenge also trusts the device); failed -> block, and
    /// the account's next attempts are blocked for 30 minutes.
    pub fn verify_challenge(&mut self, decision_id: &str, passed: bool) -> Option<Value> {
        let doc = self.state.pending.shift_remove(decision_id)?;
        let now = doc.req["ts"].as_f64().unwrap_or(0.0);
        let uid = doc.req["user_id"].as_str().unwrap_or("").to_string();
        let strong = STRONG.contains(&doc.challenge_type);
        let final_ = if passed {
            self.state.commit(&uid, &doc.req, now, strong);
            "allow"
        } else {
            self.state.record_failure(&uid, now);
            "block"
        };
        let (en, hi) = message(final_, None, false);
        let has_dev = text(part(&doc.req, "device"), "device_id").is_some();
        Some(json!({"decision_id": decision_id, "user_id": uid, "challenge_type": doc.challenge_type, "passed": passed, "decision": final_,
                    "device_trusted": passed && strong && has_dev, "message_en": en, "message_hi": hi}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_kinds_and_geo() {
        assert_eq!(ip_kind(Some("185.220.101.5")), "anonymiser");
        assert_eq!(ip_kind(Some("34.80.1.1")), "datacenter");
        assert_eq!(ip_kind(Some("192.168.1.4")), "unknown");
        assert_eq!(ip_kind(Some("49.229.31.106")), "residential");
        assert_eq!(ip_kind(Some("not an ip")), "unknown");
        assert!((haversine_km(19.076, 72.878, 28.614, 77.209) - 1153.0).abs() < 5.0, "Mumbai to Delhi");
        assert_eq!(hour(0.0), 5, "midnight UTC is 05:30 IST");
    }

    #[test]
    fn sim_swap_never_gets_an_sms_code() {
        let p = AuthPolicy::default();
        let mut f = [0.0; 27];
        f[fi("sim_fresh")] = 0.9;
        f[fi("bio_dev")] = f64::NAN;
        let ctx = Ctx { device_trusted: false, has_other_trusted_device: false, last_fail_age_s: None };
        assert_eq!(p.choose_challenge(&f, &ctx), "passkey");
        let (a, c, _, _) = p.decide(0.3, 50_000.0, &f, &ctx);
        assert_eq!((a, c), ("challenge", Some("passkey")));
    }
}
