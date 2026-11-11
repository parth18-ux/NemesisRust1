//! Request bodies (the pydantic models of service/server.py): parsing with pydantic coercions and `model_dump()`.
use crate::validate::{fval, Errors, Obj};
use rusetect::payment::Payment;
use rusetect::session_signals::{Event, SessionSignals};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone)]
pub struct PaymentIn {
    pub timestamp: String,
    pub from_bank: String,
    pub from_account: String,
    pub to_bank: String,
    pub to_account: String,
    pub amount_paid: f64,
    pub payment_currency: String,
    pub receiving_currency: Option<String>,
    pub payment_format: String,
}

impl PaymentIn {
    pub fn parse(o: &Obj, e: &mut Errors) -> Self {
        let p = PaymentIn {
            timestamp: o.req_str(e, "timestamp"),
            from_bank: o.req_str(e, "from_bank"),
            from_account: o.req_str(e, "from_account"),
            to_bank: o.req_str(e, "to_bank"),
            to_account: o.req_str(e, "to_account"),
            amount_paid: o.req_float(e, "amount_paid"),
            payment_currency: o.str_or(e, "payment_currency", "US Dollar"),
            receiving_currency: o.opt_str(e, "receiving_currency"),
            payment_format: o.str_or(e, "payment_format", "ACH"),
        };
        if o.get("amount_paid").is_some() {
            o.gt(e, "amount_paid", p.amount_paid, 0.0);
        }
        p
    }

    pub fn dump(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("timestamp".into(), json!(self.timestamp));
        m.insert("from_bank".into(), json!(self.from_bank));
        m.insert("from_account".into(), json!(self.from_account));
        m.insert("to_bank".into(), json!(self.to_bank));
        m.insert("to_account".into(), json!(self.to_account));
        m.insert("amount_paid".into(), fval(self.amount_paid));
        m.insert("payment_currency".into(), json!(self.payment_currency));
        m.insert("receiving_currency".into(), json!(self.receiving_currency));
        m.insert("payment_format".into(), json!(self.payment_format));
        m
    }

    /// `to_payload`: minutes since the epoch from "YYYY-MM-DD HH:MM" (UTC); "/" separators accepted.
    pub fn to_payment(&self) -> Option<Payment> {
        let tmin = rusetect::pyfmt::parse_minutes(&self.timestamp.replace('/', "-"))?;
        Some(Payment {
            from_bank: self.from_bank.clone(),
            from_account: self.from_account.clone(),
            to_bank: self.to_bank.clone(),
            to_account: self.to_account.clone(),
            amount_paid: self.amount_paid,
            payment_currency: self.payment_currency.clone(),
            receiving_currency: self.receiving_currency.clone().unwrap_or_else(|| self.payment_currency.clone()),
            payment_format: self.payment_format.clone(),
            tmin,
            eid: None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionIn {
    pub session_id: Option<String>,
    pub on_call: bool,
    pub call_minutes: f64,
    pub video_call: bool,
    pub screen_share: bool,
    pub remote_access_app: bool,
    pub payee_pasted: bool,
    pub hesitation_s: f64,
    pub app_hidden_s: f64,
    pub typing_anomaly: bool,
    pub device_hash: Option<String>,
    pub enrolled_device_hashes: Vec<String>,
    pub sim_swap_hours: Option<f64>,
    pub events: Vec<Event>,
}

impl SessionIn {
    pub fn parse(o: &Obj, e: &mut Errors) -> Self {
        let mut events = vec![];
        match o.get("events") {
            None => {}
            Some(Value::Array(a)) => {
                for (i, ev) in a.iter().enumerate() {
                    let mut loc = o.loc.clone();
                    loc.push("events".into());
                    loc.push(i.to_string());
                    if let Some(eo) = Obj::new(ev, loc, e) {
                        events.push(Event { kind: eo.req_str(e, "type"), minute: eo.req_int(e, "minute") });
                    }
                }
            }
            Some(v) => {
                let mut loc: Vec<&str> = o.loc.iter().map(|s| s.as_str()).collect();
                loc.push("events");
                e.push("list_type", &loc, "Input should be a valid list", v);
            }
        }
        SessionIn {
            session_id: o.opt_str(e, "session_id"),
            on_call: o.bool_or(e, "on_call", false),
            call_minutes: o.float_or(e, "call_minutes", 0.0),
            video_call: o.bool_or(e, "video_call", false),
            screen_share: o.bool_or(e, "screen_share", false),
            remote_access_app: o.bool_or(e, "remote_access_app", false),
            payee_pasted: o.bool_or(e, "payee_pasted", false),
            hesitation_s: o.float_or(e, "hesitation_s", 0.0),
            app_hidden_s: o.float_or(e, "app_hidden_s", 0.0),
            typing_anomaly: o.bool_or(e, "typing_anomaly", false),
            device_hash: o.opt_str(e, "device_hash"),
            enrolled_device_hashes: o.str_list(e, "enrolled_device_hashes"),
            sim_swap_hours: o.opt_float(e, "sim_swap_hours"),
            events,
        }
    }

    pub fn dump(&self) -> Value {
        json!({"session_id": self.session_id, "on_call": self.on_call, "call_minutes": fval(self.call_minutes), "video_call": self.video_call,
               "screen_share": self.screen_share, "remote_access_app": self.remote_access_app, "payee_pasted": self.payee_pasted,
               "hesitation_s": fval(self.hesitation_s), "app_hidden_s": fval(self.app_hidden_s), "typing_anomaly": self.typing_anomaly,
               "device_hash": self.device_hash, "enrolled_device_hashes": self.enrolled_device_hashes,
               "sim_swap_hours": self.sim_swap_hours.map(fval), "events": self.events.iter().map(|x| json!({"type": x.kind, "minute": x.minute})).collect::<Vec<_>>()})
    }

    pub fn to_signals(&self) -> SessionSignals {
        SessionSignals {
            session_id: self.session_id.clone(),
            on_call: self.on_call,
            call_minutes: self.call_minutes,
            video_call: self.video_call,
            screen_share: self.screen_share,
            remote_access_app: self.remote_access_app,
            payee_pasted: self.payee_pasted,
            hesitation_s: self.hesitation_s,
            app_hidden_s: self.app_hidden_s,
            typing_anomaly: self.typing_anomaly,
            device_hash: self.device_hash.clone(),
            enrolled_device_hashes: self.enrolled_device_hashes.clone(),
            sim_swap_hours: self.sim_swap_hours,
            events: self.events.clone(),
            network_flag: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AssessIn {
    pub payment: PaymentIn,
    pub message: Option<String>,
    pub consent: bool,
    pub use_intent_model: bool,
    pub session: Option<SessionIn>,
}

impl AssessIn {
    pub fn parse(o: &Obj, e: &mut Errors) -> Self {
        let payment = PaymentIn::parse(o, e);
        let message = o.opt_str(e, "message");
        if let Some(m) = &message {
            o.check_len(e, "message", m, None, Some(5000));
        }
        let session = match o.get("session") {
            None | Some(Value::Null) => None,
            Some(v) => Obj::new(v, vec!["session".into()], e).map(|so| SessionIn::parse(&so, e)),
        };
        AssessIn { payment, message, consent: o.bool_or(e, "consent", false), use_intent_model: o.bool_or(e, "use_intent_model", true), session }
    }

    /// `model_dump(exclude={"message"})`
    pub fn dump_without_message(&self) -> Map<String, Value> {
        let mut m = self.payment.dump();
        m.insert("consent".into(), json!(self.consent));
        m.insert("use_intent_model".into(), json!(self.use_intent_model));
        m.insert("session".into(), self.session.as_ref().map(|s| s.dump()).unwrap_or(Value::Null));
        m
    }
}
