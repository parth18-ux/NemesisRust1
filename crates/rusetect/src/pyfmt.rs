//! Python-compatible numerics and formatting.
//!
//! The Rust port must give the same numbers and the same bytes as the Python service it replaces, so the few
//! places where CPython has its own rules are reproduced here exactly:
//!   * `round(x, n)`            correctly rounded, ties to even (Rust's `{:.n}` formatting already does this)
//!   * `repr(float)`            shortest round trip digits, exponent form outside 1e-4 <= |x| < 1e16
//!   * `sum()` of floats        CPython 3.12+ uses Neumaier compensated summation
//!   * `math.fsum`              Shewchuk's exact partials (CPython's msum)
//!   * `statistics.*`           fmean = fsum / n, median, pstdev computed exactly then correctly rounded
//!   * `json.dumps`             key order, separators, `ensure_ascii`, float repr
//!   * numpy pairwise sum       for array means and standard deviations
use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
use num_traits::{One, Signed, ToPrimitive, Zero};
use serde_json::Value;

// ------------------------------------------------------------------ rounding and repr

/// Python's `round(x, ndigits)` for floats.
pub fn py_round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let s = format!("{:.*}", ndigits, x);
    s.parse::<f64>().unwrap_or(x)
}

/// Python's `round(x)` (no ndigits): nearest integer, ties to even.
pub fn py_round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `repr(float)` (also what `json.dumps` writes).
pub fn py_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // shortest round-trip digits from Rust's LowerExp: "d.ddde±x"
    let e = format!("{:e}", x.abs());
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1; // value = 0.DIGITS x 10^decpt
    let sign = if x < 0.0 { "-" } else { "" };
    let nd = digits.len() as i32;
    let body = if decpt <= -4 || decpt > 16 {
        let mut m = digits[..1].to_string();
        if nd > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let ee = decpt - 1;
        format!("{}e{}{:02}", m, if ee < 0 { '-' } else { '+' }, ee.abs())
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if decpt >= nd {
        format!("{}{}.0", digits, "0".repeat((decpt - nd) as usize))
    } else {
        format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
    };
    format!("{sign}{body}")
}

/// Integer digits with thousands separators (`f"{n:,}"`).
pub fn group_thousands(int_digits: &str) -> String {
    let (sign, d) = match int_digits.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", int_digits),
    };
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}

/// `f"{x:,.{n}f}"`
pub fn fmt_comma(x: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, x);
    match s.split_once('.') {
        Some((i, f)) => format!("{}.{}", group_thousands(i), f),
        None => group_thousands(&s),
    }
}

// ------------------------------------------------------------------ summation

/// CPython 3.12+ built-in `sum()` over floats (Neumaier compensated summation, start 0).
pub fn py_sum<I: IntoIterator<Item = f64>>(items: I) -> f64 {
    let mut f = 0.0f64;
    let mut c = 0.0f64;
    for x in items {
        let t = f + x;
        if f.abs() >= x.abs() {
            c += (f - t) + x;
        } else {
            c += (x - t) + f;
        }
        f = t;
    }
    if c != 0.0 && c.is_finite() {
        f += c;
    }
    f
}

/// CPython `math.fsum` (exact partials, correctly rounded result).
pub fn fsum<I: IntoIterator<Item = f64>>(items: I) -> f64 {
    let mut p: Vec<f64> = Vec::new();
    let mut special_sum = 0.0f64;
    let mut inf_sum = 0.0f64;
    let mut lo = 0.0f64;
    for x0 in items {
        let mut x = x0;
        let xsave = x;
        let mut i = 0usize;
        for j in 0..p.len() {
            let mut y = p[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let yr = hi - x;
            lo = y - yr;
            if lo != 0.0 {
                p[i] = lo;
                i += 1;
            }
            x = hi;
        }
        p.truncate(i);
        if x != 0.0 {
            if !x.is_finite() {
                if xsave.is_infinite() {
                    inf_sum += xsave;
                }
                special_sum += xsave;
                p.clear();
            } else {
                p.push(x);
            }
        }
    }
    if special_sum != 0.0 {
        return if inf_sum.is_nan() { f64::NAN } else { special_sum };
    }
    let mut hi = 0.0f64;
    let mut n = p.len();
    if n > 0 {
        n -= 1;
        hi = p[n];
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = p[n];
            hi = x + y;
            let yr = hi - x;
            lo = y - yr;
            if lo != 0.0 {
                break;
            }
        }
        if n > 0 && ((lo < 0.0 && p[n - 1] < 0.0) || (lo > 0.0 && p[n - 1] > 0.0)) {
            let y = lo * 2.0;
            let x = hi + y;
            let yr = x - hi;
            if y == yr {
                hi = x;
            }
        }
    }
    hi
}

/// numpy's pairwise summation for a contiguous float64 array (`np.add.reduce`).
pub fn np_sum(a: &[f64]) -> f64 {
    pairwise(a)
}

fn pairwise(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut res = 0.0;
        for &x in a {
            res += x;
        }
        res
    } else if n <= 128 {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for k in 0..8 {
                r[k] += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise(&a[..n2]) + pairwise(&a[n2..])
    }
}

/// `np.mean`
pub fn np_mean(a: &[f64]) -> f64 {
    np_sum(a) / a.len() as f64
}

/// `np.std` (population, ddof=0)
pub fn np_std(a: &[f64]) -> f64 {
    let m = np_mean(a);
    let d: Vec<f64> = a.iter().map(|x| (x - m) * (x - m)).collect();
    (np_sum(&d) / a.len() as f64).sqrt()
}

// ------------------------------------------------------------------ statistics module

/// `statistics.fmean`
pub fn fmean(v: &[f64]) -> f64 {
    fsum(v.iter().copied()) / v.len() as f64
}

/// `statistics.median`
pub fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Exact `(mantissa, exponent)` of a finite float: x = m * 2^e.
fn exact_parts(x: f64) -> (BigInt, i64) {
    if x == 0.0 {
        return (BigInt::zero(), 0);
    }
    let bits = x.to_bits();
    let sign = if bits >> 63 == 1 { -1i64 } else { 1 };
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if exp == 0 { (frac, -1074) } else { (frac | (1u64 << 52), exp - 1075) };
    (BigInt::from(sign) * BigInt::from(m), e)
}

/// Correctly rounded (ties to even) conversion of a non-negative big integer to f64.
fn biguint_to_f64(n: &BigUint) -> f64 {
    let bits = n.bits();
    if bits <= 53 {
        return n.to_u64().unwrap() as f64;
    }
    let shift = bits - 54;
    let top = (n >> shift).to_u64().unwrap();
    let sticky = !(n & ((BigUint::one() << shift) - BigUint::one())).is_zero();
    let mut mant = top >> 1;
    let round = top & 1;
    if round == 1 && (sticky || mant & 1 == 1) {
        mant += 1;
    }
    (mant as f64) * 2f64.powi((shift + 1) as i32)
}

/// Square root of n/m rounded to an integer with round-to-odd (CPython `_integer_sqrt_of_frac_rto`).
fn isqrt_frac_rto(n: &BigUint, m: &BigUint) -> BigUint {
    let a = (n / m).sqrt();
    if &(&a * &a) * m != *n {
        a | BigUint::one()
    } else {
        a
    }
}

/// Correctly rounded sqrt(n/m) (CPython `statistics._float_sqrt_of_frac`).
fn float_sqrt_of_frac(n: &BigUint, m: &BigUint) -> f64 {
    let width: i64 = 2 * 53 + 3;
    let q = Integer::div_floor(&(n.bits() as i64 - m.bits() as i64 - width), &2);
    if q >= 0 {
        let num = isqrt_frac_rto(n, &(m << (2 * q as u64))) << (q as u64);
        biguint_to_f64(&num)
    } else {
        let num = isqrt_frac_rto(&(n << ((-2 * q) as u64)), m);
        biguint_to_f64(&num) * 2f64.powi(q as i32)
    }
}

/// `statistics.pstdev` (exact rational variance, correctly rounded square root).
pub fn pstdev(v: &[f64]) -> f64 {
    let n = v.len();
    let parts: Vec<(BigInt, i64)> = v.iter().map(|&x| exact_parts(x)).collect();
    let emin = parts.iter().filter(|(m, _)| !m.is_zero()).map(|(_, e)| *e).min().unwrap_or(0);
    let ms: Vec<BigInt> = parts.iter().map(|(m, e)| if m.is_zero() { BigInt::zero() } else { m << ((e - emin) as u64) }).collect();
    let sx: BigInt = ms.iter().sum();
    let sxx: BigInt = ms.iter().map(|m| m * m).sum();
    let nn = BigInt::from(n);
    // variance = (n*sxx - sx^2) / n^2 * 2^(2*emin)
    let mut num = (&nn * &sxx - &sx * &sx).abs();
    let mut den = &nn * &nn;
    if emin < 0 {
        den <<= (-2 * emin) as u64;
    } else {
        num <<= (2 * emin) as u64;
    }
    let g = num.gcd(&den);
    let (num, den) = if g.is_zero() { (num, den) } else { (&num / &g, &den / &g) };
    let (_, nu) = num.into_parts();
    let (_, de) = den.into_parts();
    let _ = Sign::Plus;
    if nu.is_zero() {
        return 0.0;
    }
    float_sqrt_of_frac(&nu, &de)
}

// ------------------------------------------------------------------ strings

/// `str.isspace()` for one character.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.split()` (no argument).
pub fn py_split(s: &str) -> Vec<&str> {
    s.split(is_py_space).filter(|w| !w.is_empty()).collect()
}

/// `str.strip()` (no argument).
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// First `n` characters (`s[:n]`).
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's `float(str)`.
pub fn py_float_str(s: &str) -> Option<f64> {
    let t = py_strip(s);
    let low = t.to_ascii_lowercase();
    let unsigned = low.trim_start_matches(['+', '-']);
    if low.len() - unsigned.len() <= 1 && matches!(unsigned, "inf" | "infinity" | "nan") {
        let neg = low.starts_with('-');
        let v = if unsigned == "nan" { f64::NAN } else { f64::INFINITY };
        return Some(if neg { -v } else { v });
    }
    let b: Vec<char> = t.chars().collect();
    for (i, c) in b.iter().enumerate() {
        if *c == '_' && !(i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit()) {
            return None;
        }
    }
    let clean: String = b.iter().filter(|c| **c != '_').collect();
    if clean.is_empty() || clean.chars().any(|c| !(c.is_ascii_digit() || "+-.eE".contains(c))) {
        return None;
    }
    clean.parse::<f64>().ok()
}

/// Python's `float(v)` for a JSON value (bool, int, float, numeric string); None for anything else.
pub fn py_float_value(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_float_str(s),
        _ => None,
    }
}

// ------------------------------------------------------------------ json.dumps

#[derive(Clone, Copy)]
pub struct JsonStyle {
    pub sort_keys: bool,
    pub item_sep: &'static str,
    pub key_sep: &'static str,
    pub ensure_ascii: bool,
}

/// `json.dumps(obj, sort_keys=True, default=str)` (the audit log and evidence pack hash)
pub const HASH_STYLE: JsonStyle = JsonStyle { sort_keys: true, item_sep: ", ", key_sep: ": ", ensure_ascii: true };
/// `json.dumps(obj, sort_keys=True, separators=(",", ":"))` (what gets signed)
pub const CANON_STYLE: JsonStyle = JsonStyle { sort_keys: true, item_sep: ",", key_sep: ":", ensure_ascii: true };
/// FastAPI's JSONResponse: `json.dumps(obj, ensure_ascii=False, separators=(",", ":"))`
pub const RESPONSE_STYLE: JsonStyle = JsonStyle { sort_keys: false, item_sep: ",", key_sep: ":", ensure_ascii: false };
/// Default `json.dumps(obj)`
pub const DEFAULT_STYLE: JsonStyle = JsonStyle { sort_keys: false, item_sep: ", ", key_sep: ": ", ensure_ascii: true };
/// `json.dumps(obj, indent=1)` as used for the committed result files
pub fn dumps_indent(v: &Value, indent: usize) -> String {
    let mut out = String::new();
    write_indent(&mut out, v, indent, 0);
    out
}

fn write_indent(out: &mut String, v: &Value, ind: usize, level: usize) {
    match v {
        Value::Array(a) if !a.is_empty() => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(ind * (level + 1)));
                write_indent(out, x, ind, level + 1);
            }
            out.push('\n');
            out.push_str(&" ".repeat(ind * level));
            out.push(']');
        }
        Value::Object(m) if !m.is_empty() => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(ind * (level + 1)));
                write_str(out, k, true);
                out.push_str(": ");
                write_indent(out, x, ind, level + 1);
            }
            out.push('\n');
            out.push_str(&" ".repeat(ind * level));
            out.push('}');
        }
        other => write_value(out, other, DEFAULT_STYLE),
    }
}

pub fn dumps(v: &Value, style: JsonStyle) -> String {
    let mut out = String::new();
    write_value(&mut out, v, style);
    out
}

fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, v: &Value, st: JsonStyle) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&py_repr(n.as_f64().unwrap()));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => write_str(out, s, st.ensure_ascii),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(st.item_sep);
                }
                write_value(out, x, st);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            let mut keys: Vec<&String> = m.keys().collect();
            if st.sort_keys {
                keys.sort();
            }
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(st.item_sep);
                }
                write_str(out, k, st.ensure_ascii);
                out.push_str(st.key_sep);
                write_value(out, &m[k], st);
            }
            out.push('}');
        }
    }
}

/// A float as a JSON value; NaN becomes null (the API maps NaN features to None).
pub fn jf(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

// ------------------------------------------------------------------ time

/// `time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())`
pub fn utc_now_z() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Seconds since the epoch as a float (`time.time()`).
pub fn time_time() -> f64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    d.as_secs_f64()
}

static TS_DASH: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
    fancy_regex::Regex::new(r"^([0-9]{4})-(1[0-2]|0[1-9]|[1-9])-(3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])[\s\x1c-\x1f]+(2[0-3]|[0-1][0-9]|[0-9]):([0-5][0-9]|[0-9])$").unwrap()
});

/// `datetime.strptime(s, "%Y-%m-%d %H:%M")` read as UTC, in minutes since the epoch.
pub fn parse_minutes(s: &str) -> Option<i64> {
    let c = TS_DASH.captures(s).ok()??;
    // ASCII digits only ([0-9], not \d, which also matches other scripts' digits); never panics on input
    let g = |i: usize| c.get(i).and_then(|m| m.as_str().trim().parse::<u32>().ok());
    let date = chrono::NaiveDate::from_ymd_opt(g(1)? as i32, g(2)?, g(3)?)?;
    let dt = date.and_hms_opt(g(4)?, g(5)?, 0)?;
    Some(dt.and_utc().timestamp().div_euclid(60))
}

/// `calendar.timegm(time.strptime(s, "%Y/%m/%d %H:%M")) // 60`
pub fn parse_minutes_slash(s: &str) -> Option<i64> {
    parse_minutes(&s.replace('/', "-"))
}

/// (hour, ISO weekday 1..7) of a minute timestamp (`time.gmtime(t * 60)`).
pub fn hour_dow(tmin: i64) -> (i64, i64) {
    let secs = tmin * 60;
    let days = secs.div_euclid(86_400);
    let hour = secs.rem_euclid(86_400) / 3600;
    let wday = (days + 3).rem_euclid(7); // 1970-01-01 was a Thursday (tm_wday 3)
    (hour, wday + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_with_non_ascii_digits_are_rejected_not_panicked() {
        assert_eq!(parse_minutes("٢٠٢٤-01-01 00:00"), None);
        assert_eq!(parse_minutes("２０２２-09-01 00:10"), None);
        assert!(parse_minutes("2022-09-01 00:10").is_some());
    }

    #[test]
    fn repr_matches_python() {
        for (x, s) in [(1e-5, "1e-05"), (1e16, "1e+16"), (1e15, "1000000000000000.0"), (0.1, "0.1"), (1.5e-7, "1.5e-07"),
                       (123456789.0, "123456789.0"), (0.0001, "0.0001"), (-2.5, "-2.5"), (100.0, "100.0"), (1.2345e20, "1.2345e+20")] {
            assert_eq!(py_repr(x), s);
        }
    }

    #[test]
    fn round_ties_to_even() {
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(2.5, 0), 2.0);
    }

    #[test]
    fn compensated_sum() {
        assert_eq!(py_sum(vec![0.1; 10]), 1.0);
        assert_eq!(py_sum(vec![1e16, 1.0, -1e16]), 1.0);
        assert_eq!(fsum(vec![1e-16, 1.0, 1e16]), 10000000000000002.0);
    }

    #[test]
    fn pstdev_exact() {
        assert_eq!(pstdev(&[1.0, 2.0, 3.0, 4.0]), 1.118033988749895);
        assert_eq!(pstdev(&[2.0, 2.0]), 0.0);
    }

    #[test]
    fn json_styles() {
        let v: Value = serde_json::from_str(r#"{"b": 1.0, "a": "é", "c": [1, 1e-5]}"#).unwrap();
        assert_eq!(dumps(&v, HASH_STYLE), "{\"a\": \"\\u00e9\", \"b\": 1.0, \"c\": [1, 1e-05]}");
        assert_eq!(dumps(&v, RESPONSE_STYLE), r#"{"b":1.0,"a":"é","c":[1,1e-05]}"#);
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_minutes("2022-09-09 10:00"), Some(27_711_960));
        assert_eq!(parse_minutes("2022-9-9 10:00"), Some(27_711_960));
        assert_eq!(parse_minutes("2022-02-30 10:00"), None);
        assert_eq!(hour_dow(27_711_960), (10, 5));
        assert_eq!(fmt_comma(1234567.5, 0), "1,234,568");
    }
}
