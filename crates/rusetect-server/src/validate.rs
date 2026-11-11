//! Request validation with pydantic v2 (lax mode) semantics, so the Rust API accepts and rejects the same bodies as
//! the FastAPI service and answers 422 with the same `{"detail": [{"type", "loc", "msg", "input"}]}` shape.
//!
//! Coercions reproduced: float fields take ints, floats, bools and numeric strings; int fields take ints, whole
//! floats and integer strings; bool fields take true/false, 0/1 and the usual strings; str fields take only strings.
use rusetect::pyfmt::{jf, py_float_str, py_strip};
use serde_json::{json, Map, Value};

#[derive(Default)]
pub struct Errors(pub Vec<Value>);

impl Errors {
    pub fn push(&mut self, kind: &str, loc: &[&str], msg: &str, input: &Value) {
        let mut l = vec![json!("body")];
        l.extend(loc.iter().map(|s| match s.parse::<i64>() {
            Ok(i) => json!(i),
            Err(_) => json!(s),
        }));
        self.0.push(json!({"type": kind, "loc": l, "msg": msg, "input": input}));
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Field reader over one JSON object.
pub struct Obj<'a> {
    pub m: &'a Map<String, Value>,
    pub loc: Vec<String>,
}

const MISSING: Value = Value::Null;

impl<'a> Obj<'a> {
    pub fn new(v: &'a Value, loc: Vec<String>, errs: &mut Errors) -> Option<Self> {
        match v.as_object() {
            Some(m) => Some(Obj { m, loc }),
            None => {
                let l: Vec<&str> = loc.iter().map(|s| s.as_str()).collect();
                errs.push("model_attributes_type", &l, "Input should be a valid dictionary or object to extract fields from", v);
                None
            }
        }
    }

    fn loc(&self, f: &str) -> Vec<String> {
        let mut l = self.loc.clone();
        l.push(f.to_string());
        l
    }

    fn err(&self, errs: &mut Errors, kind: &str, f: &str, msg: &str, input: &Value) {
        let l = self.loc(f);
        let r: Vec<&str> = l.iter().map(|s| s.as_str()).collect();
        errs.push(kind, &r, msg, input);
    }

    fn missing(&self, errs: &mut Errors, f: &str) {
        let input = Value::Object(self.m.clone());
        self.err(errs, "missing", f, "Field required", &input);
    }

    pub fn get(&self, f: &str) -> Option<&'a Value> {
        self.m.get(f)
    }

    // ---------------------------------------------------------------- strings
    fn as_str(&self, errs: &mut Errors, f: &str, v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            _ => {
                self.err(errs, "string_type", f, "Input should be a valid string", v);
                None
            }
        }
    }

    pub fn req_str(&self, errs: &mut Errors, f: &str) -> String {
        match self.m.get(f) {
            None => {
                self.missing(errs, f);
                String::new()
            }
            Some(v) => self.as_str(errs, f, v).unwrap_or_default(),
        }
    }

    pub fn str_or(&self, errs: &mut Errors, f: &str, default: &str) -> String {
        match self.m.get(f) {
            None => default.to_string(),
            Some(v) => self.as_str(errs, f, v).unwrap_or_default(),
        }
    }

    pub fn opt_str(&self, errs: &mut Errors, f: &str) -> Option<String> {
        match self.m.get(f) {
            None | Some(Value::Null) => None,
            Some(v) => self.as_str(errs, f, v),
        }
    }

    pub fn check_len(&self, errs: &mut Errors, f: &str, s: &str, min: Option<usize>, max: Option<usize>) {
        let n = s.chars().count();
        let input = self.m.get(f).unwrap_or(&MISSING);
        if let Some(lo) = min {
            if n < lo {
                self.err(errs, "string_too_short", f, &format!("String should have at least {lo} character{}", if lo == 1 { "" } else { "s" }), input);
            }
        }
        if let Some(hi) = max {
            if n > hi {
                self.err(errs, "string_too_long", f, &format!("String should have at most {hi} characters"), input);
            }
        }
    }

    // ---------------------------------------------------------------- numbers
    fn as_float(&self, errs: &mut Errors, f: &str, v: &Value) -> Option<f64> {
        let out = match v {
            Value::Number(n) => n.as_f64(),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::String(s) => py_float_str(s),
            _ => None,
        };
        if out.is_none() {
            let (kind, msg) = if v.is_string() {
                ("float_parsing", "Input should be a valid number, unable to parse string as a number")
            } else {
                ("float_type", "Input should be a valid number")
            };
            self.err(errs, kind, f, msg, v);
        }
        out
    }

    pub fn req_float(&self, errs: &mut Errors, f: &str) -> f64 {
        match self.m.get(f) {
            None => {
                self.missing(errs, f);
                0.0
            }
            Some(v) => self.as_float(errs, f, v).unwrap_or(0.0),
        }
    }

    pub fn float_or(&self, errs: &mut Errors, f: &str, default: f64) -> f64 {
        match self.m.get(f) {
            None => default,
            Some(v) => self.as_float(errs, f, v).unwrap_or(default),
        }
    }

    pub fn opt_float(&self, errs: &mut Errors, f: &str) -> Option<f64> {
        match self.m.get(f) {
            None | Some(Value::Null) => None,
            Some(v) => self.as_float(errs, f, v),
        }
    }

    pub fn gt(&self, errs: &mut Errors, f: &str, v: f64, lo: f64) {
        if !(v > lo) {
            let input = self.m.get(f).unwrap_or(&MISSING);
            self.err(errs, "greater_than", f, &format!("Input should be greater than {}", lo as i64), input);
        }
    }

    fn as_int(&self, errs: &mut Errors, f: &str, v: &Value) -> Option<i64> {
        let out = match v {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|x| x.fract() == 0.0 && x.is_finite()).map(|x| x as i64)),
            Value::Bool(b) => Some(*b as i64),
            Value::String(s) => py_strip(s).parse::<i64>().ok(),
            _ => None,
        };
        if out.is_none() {
            let (kind, msg) = match v {
                Value::Number(_) => ("int_from_float", "Input should be a valid integer, got a number with a fractional part"),
                Value::String(_) => ("int_parsing", "Input should be a valid integer, unable to parse string as an integer"),
                _ => ("int_type", "Input should be a valid integer"),
            };
            self.err(errs, kind, f, msg, v);
        }
        out
    }

    pub fn req_int(&self, errs: &mut Errors, f: &str) -> i64 {
        match self.m.get(f) {
            None => {
                self.missing(errs, f);
                0
            }
            Some(v) => self.as_int(errs, f, v).unwrap_or(0),
        }
    }

    // ---------------------------------------------------------------- bools, lists, dicts
    pub fn bool_or(&self, errs: &mut Errors, f: &str, default: bool) -> bool {
        match self.m.get(f) {
            None => default,
            Some(v) => match parse_bool(v) {
                Some(b) => b,
                None => {
                    self.err(errs, "bool_parsing", f, "Input should be a valid boolean", v);
                    default
                }
            },
        }
    }

    pub fn str_list(&self, errs: &mut Errors, f: &str) -> Vec<String> {
        match self.m.get(f) {
            None => vec![],
            Some(Value::Array(a)) => a.iter().enumerate().filter_map(|(i, x)| match x {
                Value::String(s) => Some(s.clone()),
                _ => {
                    let l = self.loc(f);
                    let mut r: Vec<String> = l;
                    r.push(i.to_string());
                    let rr: Vec<&str> = r.iter().map(|s| s.as_str()).collect();
                    errs.push("string_type", &rr, "Input should be a valid string", x);
                    None
                }
            }).collect(),
            Some(v) => {
                self.err(errs, "list_type", f, "Input should be a valid list", v);
                vec![]
            }
        }
    }

    pub fn req_dict(&self, errs: &mut Errors, f: &str) -> Map<String, Value> {
        match self.m.get(f) {
            None => {
                self.missing(errs, f);
                Map::new()
            }
            Some(Value::Object(m)) => m.clone(),
            Some(v) => {
                self.err(errs, "dict_type", f, "Input should be a valid dictionary", v);
                Map::new()
            }
        }
    }
}

pub fn parse_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => match n.as_f64() {
            Some(0.0) => Some(false),
            Some(1.0) => Some(true),
            _ => None,
        },
        Value::String(s) => parse_bool_str(s),
        _ => None,
    }
}

pub fn parse_bool_str(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
        "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
        _ => None,
    }
}

/// A float as stored by pydantic for a `float` field.
pub fn fval(x: f64) -> Value {
    jf(x)
}
