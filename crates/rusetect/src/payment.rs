//! One payment as the engine sees it (the Python services pass the same fields as a dict).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Payment {
    pub from_bank: String,
    pub from_account: String,
    pub to_bank: String,
    pub to_account: String,
    pub amount_paid: f64,
    pub payment_currency: String,
    /// defaults to the payment currency
    pub receiving_currency: String,
    pub payment_format: String,
    /// minutes since the epoch (UTC)
    pub tmin: i64,
    /// event id, used by the streaming workers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eid: Option<i64>,
}

impl Payment {
    pub fn sender_key(&self) -> String {
        format!("{}:{}", self.from_bank, self.from_account)
    }
    pub fn payee_key(&self) -> String {
        format!("{}:{}", self.to_bank, self.to_account)
    }

    /// One row of an IBM AMLworld style demo CSV (columns ts, fb, fa, tb, ta, amt_recv, cur_recv, amt_paid, cur_paid, fmt, y).
    pub fn from_demo_row(r: &DemoRow) -> Option<Payment> {
        Some(Payment {
            from_bank: r.fb.clone(),
            from_account: r.fa.clone(),
            to_bank: r.tb.clone(),
            to_account: r.ta.clone(),
            amount_paid: crate::pyfmt::py_float_str(&r.amt_paid)?,
            payment_currency: r.cur_paid.clone(),
            receiving_currency: r.cur_recv.clone(),
            payment_format: r.fmt.clone(),
            tmin: crate::pyfmt::parse_minutes_slash(&r.ts)?,
            eid: None,
        })
    }
}

/// A row of service/models/demo_stream.csv.gz.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DemoRow {
    pub ts: String,
    pub fb: String,
    pub fa: String,
    pub tb: String,
    pub ta: String,
    #[serde(default)]
    pub amt_recv: String,
    pub cur_recv: String,
    pub amt_paid: String,
    pub cur_paid: String,
    pub fmt: String,
    #[serde(default)]
    pub y: String,
}

/// Read a (possibly gzip-compressed) CSV with a header into rows.
pub fn read_demo_rows(path: &std::path::Path) -> anyhow::Result<Vec<DemoRow>> {
    let f = std::fs::File::open(path)?;
    let reader: Box<dyn std::io::Read> = if path.extension().map(|e| e == "gz").unwrap_or(false) {
        Box::new(flate2::read::MultiGzDecoder::new(f))
    } else {
        Box::new(f)
    };
    let mut rd = csv::Reader::from_reader(std::io::BufReader::new(reader));
    let mut out = Vec::new();
    for r in rd.deserialize() {
        out.push(r?);
    }
    Ok(out)
}
