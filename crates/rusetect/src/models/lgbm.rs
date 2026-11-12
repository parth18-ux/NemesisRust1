//! LightGBM inference from a `Booster.dump_model()` JSON file (port of service/lgbm_lite.py).
//!
//! Supports what the RuseTect models use: numerical `<=` and categorical `==` splits, missing_type None / Zero / NaN,
//! and the binary sigmoid objective. Leaf values are summed in float64 with CPython's compensated `sum()`, so scores
//! equal the Python LiteBooster (which itself matches native LightGBM to 1e-9, see the parity tests).
use crate::pyfmt::py_sum;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone)]
enum Split {
    Num { thr: f64 },
    Cat { cats: HashSet<i64> },
}

#[derive(Debug, Clone)]
struct Node {
    feat: i32, // -1 = leaf
    split: Option<Split>,
    default_left: bool,
    missing: u8, // 0 None, 1 Zero, 2 NaN
    left: usize,
    right: usize,
    leaf: f64,
}

#[derive(Debug, Clone)]
pub struct LiteBooster {
    trees: Vec<Vec<Node>>,
    pub sigmoid: f64,
    pub binary: bool,
    pub feature_names: Vec<String>,
}

fn num(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| anyhow!("bad number")),
        Value::String(s) => s.parse::<f64>().map_err(|e| anyhow!("{e}")),
        _ => Err(anyhow!("expected a number, got {v}")),
    }
}

impl LiteBooster {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_json(&serde_json::from_str(&text)?)
    }

    pub fn from_json(m: &Value) -> Result<Self> {
        let obj = m["objective"].as_str().unwrap_or("");
        let mut sigmoid = 1.0;
        if let Some((_, rest)) = obj.split_once("sigmoid:") {
            sigmoid = rest.split_whitespace().next().unwrap_or("1").parse()?;
        }
        let feature_names = m["feature_names"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let trees = m["tree_info"].as_array().ok_or_else(|| anyhow!("no tree_info"))?.iter().map(|t| {
            let mut nodes = Vec::new();
            flatten(&t["tree_structure"], &mut nodes)?;
            Ok(nodes)
        }).collect::<Result<Vec<_>>>()?;
        Ok(LiteBooster { trees, sigmoid, binary: obj.starts_with("binary"), feature_names })
    }

    fn walk(t: &[Node], x: &[f64]) -> f64 {
        let mut i = 0usize;
        loop {
            let n = &t[i];
            if n.feat < 0 {
                return n.leaf;
            }
            let mut v = x[n.feat as usize];
            let isnan = v.is_nan();
            let go_left = match n.split.as_ref().unwrap() {
                Split::Num { thr } => {
                    if n.missing == 2 && isnan {
                        n.default_left
                    } else if n.missing == 1 && (isnan || v == 0.0) {
                        n.default_left
                    } else {
                        if isnan {
                            v = 0.0;
                        }
                        v <= *thr
                    }
                }
                Split::Cat { cats } => !isnan && v >= 0.0 && cats.contains(&(v.trunc() as i64)),
            };
            i = if go_left { n.left } else { n.right };
        }
    }

    /// Sum of leaf values for one row.
    pub fn raw(&self, x: &[f64]) -> f64 {
        py_sum(self.trees.iter().map(|t| Self::walk(t, x)))
    }

    /// `predict([x])[0]`
    pub fn predict(&self, x: &[f64]) -> f64 {
        let z = self.raw(x);
        if self.binary {
            1.0 / (1.0 + (-self.sigmoid * z).exp())
        } else {
            z
        }
    }

    /// Prediction from a float32 feature vector (the API casts the live features to float32 first).
    pub fn predict_f32(&self, x: &[f32]) -> f64 {
        let v: Vec<f64> = x.iter().map(|&a| a as f64).collect();
        self.predict(&v)
    }
}

fn flatten(n: &Value, out: &mut Vec<Node>) -> Result<usize> {
    let i = out.len();
    out.push(Node { feat: -1, split: None, default_left: false, missing: 0, left: 0, right: 0, leaf: 0.0 });
    if n.get("split_index").is_none() {
        out[i].leaf = num(&n["leaf_value"])?;
        return Ok(i);
    }
    let feat = n["split_feature"].as_i64().ok_or_else(|| anyhow!("split_feature"))? as i32;
    let missing = match n["missing_type"].as_str() {
        Some("None") => 0,
        Some("Zero") => 1,
        Some("NaN") => 2,
        other => return Err(anyhow!("missing_type {other:?}")),
    };
    let split = if n["decision_type"].as_str() == Some("==") {
        let s = match &n["threshold"] {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        Split::Cat { cats: s.split("||").map(|c| c.trim().parse::<i64>()).collect::<std::result::Result<_, _>>()? }
    } else {
        Split::Num { thr: num(&n["threshold"])? }
    };
    out[i].feat = feat;
    out[i].split = Some(split);
    out[i].default_left = n["default_left"].as_bool().unwrap_or(false);
    out[i].missing = missing;
    let l = flatten(&n["left_child"], out)?;
    let r = flatten(&n["right_child"], out)?;
    out[i].left = l;
    out[i].right = r;
    Ok(i)
}
