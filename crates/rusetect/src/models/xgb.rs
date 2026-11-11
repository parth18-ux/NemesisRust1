//! XGBoost inference from a saved JSON model (XGBoost 2.x / 3.x `save_model("*.json")`), no native library.
//!
//! Reproduces the CPU predictor bit for bit: float32 features, `x < split_condition` goes left, missing values follow
//! `default_left`, leaf values summed in float32 onto the base margin, then the logistic transform in float32.
//! `contributions` is XGBoost's exact TreeSHAP (`pred_contribs=True`), ported from src/predictor/treeshap.cc with the
//! same float32 operation order, so the reason chips and their impacts match the Python service.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct XgbTree {
    left: Vec<i32>,
    right: Vec<i32>,
    feat: Vec<u32>,
    cond: Vec<f32>, // split condition, or the leaf value for leaves
    default_left: Vec<bool>,
    hess: Vec<f32>,
    mean: Vec<f32>, // expected value under each node (TreeSHAP)
    max_depth: usize,
}

#[derive(Debug, Clone)]
pub struct XgbBooster {
    pub trees: Vec<XgbTree>,
    pub base_margin: f32,
    pub logistic: bool,
    pub feature_names: Vec<String>,
    pub num_feature: usize,
}

fn as_vec_f32(v: &Value, k: &str) -> Result<Vec<f32>> {
    v[k].as_array().ok_or_else(|| anyhow!("missing {k}"))?.iter().map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| anyhow!("bad {k}"))).collect()
}

fn as_vec_i64(v: &Value, k: &str) -> Result<Vec<i64>> {
    v[k].as_array().ok_or_else(|| anyhow!("missing {k}"))?.iter().map(|x| x.as_i64().ok_or_else(|| anyhow!("bad {k}"))).collect()
}

fn parse_base_score(s: &str) -> Result<f32> {
    let t = s.trim().trim_start_matches('[').trim_end_matches(']');
    t.parse::<f32>().with_context(|| format!("base_score {s}"))
}

impl XgbTree {
    fn from_json(t: &Value) -> Result<Self> {
        let left: Vec<i32> = as_vec_i64(t, "left_children")?.into_iter().map(|x| x as i32).collect();
        let right: Vec<i32> = as_vec_i64(t, "right_children")?.into_iter().map(|x| x as i32).collect();
        let feat: Vec<u32> = as_vec_i64(t, "split_indices")?.into_iter().map(|x| x as u32).collect();
        let cond = as_vec_f32(t, "split_conditions")?;
        let default_left: Vec<bool> = as_vec_i64(t, "default_left")?.into_iter().map(|x| x != 0).collect();
        let hess = as_vec_f32(t, "sum_hessian")?;
        if let Some(st) = t.get("split_type").and_then(|v| v.as_array()) {
            if st.iter().any(|x| x.as_i64() != Some(0)) {
                bail!("categorical XGBoost splits are not supported");
            }
        }
        let n = left.len();
        let mut tree = XgbTree { left, right, feat, cond, default_left, hess, mean: vec![0.0; n], max_depth: 0 };
        tree.fill_mean(0);
        tree.max_depth = tree.depth(0);
        Ok(tree)
    }

    #[inline]
    fn is_leaf(&self, n: usize) -> bool {
        self.left[n] == -1
    }

    fn fill_mean(&mut self, n: usize) -> f32 {
        let r = if self.is_leaf(n) {
            self.cond[n]
        } else {
            let (l, rr) = (self.left[n] as usize, self.right[n] as usize);
            let mut r = self.fill_mean(l) * self.hess[l];
            r += self.fill_mean(rr) * self.hess[rr];
            r / self.hess[n]
        };
        self.mean[n] = r;
        r
    }

    fn depth(&self, n: usize) -> usize {
        if self.is_leaf(n) {
            0
        } else {
            1 + self.depth(self.left[n] as usize).max(self.depth(self.right[n] as usize))
        }
    }

    #[inline]
    fn next(&self, n: usize, x: f32) -> usize {
        if x.is_nan() {
            if self.default_left[n] { self.left[n] as usize } else { self.right[n] as usize }
        } else if x < self.cond[n] {
            self.left[n] as usize
        } else {
            self.right[n] as usize
        }
    }

    #[inline]
    pub fn leaf(&self, x: &[f32]) -> f32 {
        let mut n = 0usize;
        while !self.is_leaf(n) {
            n = self.next(n, x[self.feat[n] as usize]);
        }
        self.cond[n]
    }
}

impl XgbBooster {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let v: Value = serde_json::from_str(&text)?;
        Self::from_json(&v)
    }

    pub fn from_json(v: &Value) -> Result<Self> {
        let l = &v["learner"];
        let obj = l["objective"]["name"].as_str().unwrap_or("");
        let logistic = matches!(obj, "binary:logistic" | "reg:logistic");
        let p = &l["learner_model_param"];
        let base = parse_base_score(p["base_score"].as_str().unwrap_or("0.5"))?;
        // ObjFunction::ProbToMargin for the logistic objectives, in float32
        let base_margin = if logistic { -(1.0f32 / base - 1.0f32).ln() } else { base };
        let num_feature: usize = p["num_feature"].as_str().unwrap_or("0").parse().unwrap_or(0);
        let gb = &l["gradient_booster"];
        if gb["name"].as_str() != Some("gbtree") {
            bail!("only gbtree boosters are supported");
        }
        let trees = gb["model"]["trees"].as_array().ok_or_else(|| anyhow!("no trees"))?.iter().map(XgbTree::from_json).collect::<Result<Vec<_>>>()?;
        let feature_names = l["feature_names"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        Ok(XgbBooster { trees, base_margin, logistic, feature_names, num_feature })
    }

    /// Raw margin (float32 accumulation in tree order, as the CPU predictor does).
    pub fn margin(&self, x: &[f32]) -> f32 {
        let mut s = self.base_margin;
        for t in &self.trees {
            s += t.leaf(x);
        }
        s
    }

    /// `Booster.predict(DMatrix([x]))[0]`
    pub fn predict(&self, x: &[f32]) -> f32 {
        let m = self.margin(x);
        if self.logistic {
            1.0f32 / (1.0f32 + (-m).exp())
        } else {
            m
        }
    }

    /// `Booster.predict(dm, pred_contribs=True)[0]`: one SHAP value per feature, then the bias.
    ///
    /// Trees are split across threads (each tree's contribution is computed on its own, as XGBoost does), then added
    /// in tree order, so the float32 result is identical to a single-threaded run.
    pub fn contributions(&self, x: &[f32]) -> Vec<f32> {
        let nf = x.len();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8);
        let per_tree: Vec<Vec<f32>> = if threads > 1 && self.trees.len() >= 64 {
            let chunk = self.trees.len().div_ceil(threads);
            std::thread::scope(|s| {
                let hs: Vec<_> = self.trees.chunks(chunk).map(|ts| s.spawn(move || tree_contribs(ts, x))).collect();
                hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
            })
        } else {
            tree_contribs(&self.trees, x)
        };
        let mut out = vec![0.0f32; nf + 1];
        for this in &per_tree {
            for (o, v) in out.iter_mut().zip(this) {
                *o += *v;
            }
        }
        out[nf] += self.base_margin;
        out
    }
}

/// Per-tree SHAP vectors (features + bias) for a run of trees, reusing one path buffer.
fn tree_contribs(trees: &[XgbTree], x: &[f32]) -> Vec<Vec<f32>> {
    let nf = x.len();
    let maxd = trees.iter().map(|t| t.max_depth).max().unwrap_or(0) + 2;
    let mut path = vec![PathElement::default(); maxd * (maxd + 1) / 2];
    trees.iter().map(|t| {
        let mut this = vec![0.0f32; nf + 1];
        this[nf] += t.mean[0];
        tree_shap(t, x, &mut this, 0, 0, &mut path, 0, 1.0, 1.0, -1);
        this
    }).collect()
}

#[derive(Clone, Copy, Default, Debug)]
struct PathElement {
    feature_index: i32,
    zero_fraction: f32,
    one_fraction: f32,
    pweight: f32,
}

fn extend_path(p: &mut [PathElement], unique_depth: usize, zero_fraction: f32, one_fraction: f32, feature_index: i32) {
    p[unique_depth] = PathElement { feature_index, zero_fraction, one_fraction, pweight: if unique_depth == 0 { 1.0 } else { 0.0 } };
    let ud1 = (unique_depth + 1) as f32;
    for i in (0..unique_depth).rev() {
        p[i + 1].pweight += one_fraction * p[i].pweight * (i + 1) as f32 / ud1;
        p[i].pweight = zero_fraction * p[i].pweight * (unique_depth - i) as f32 / ud1;
    }
}

fn unwind_path(p: &mut [PathElement], unique_depth: usize, path_index: usize) {
    let one_fraction = p[path_index].one_fraction;
    let zero_fraction = p[path_index].zero_fraction;
    let mut next_one_portion = p[unique_depth].pweight;
    let ud1 = (unique_depth + 1) as f32;
    for i in (0..unique_depth).rev() {
        if one_fraction != 0.0 {
            let tmp = p[i].pweight;
            p[i].pweight = next_one_portion * ud1 / ((i + 1) as f32 * one_fraction);
            next_one_portion = tmp - p[i].pweight * zero_fraction * (unique_depth - i) as f32 / ud1;
        } else {
            p[i].pweight = (p[i].pweight * ud1) / (zero_fraction * (unique_depth - i) as f32);
        }
    }
    for i in path_index..unique_depth {
        p[i].feature_index = p[i + 1].feature_index;
        p[i].zero_fraction = p[i + 1].zero_fraction;
        p[i].one_fraction = p[i + 1].one_fraction;
    }
}

fn unwound_path_sum(p: &[PathElement], unique_depth: usize, path_index: usize) -> f32 {
    let one_fraction = p[path_index].one_fraction;
    let zero_fraction = p[path_index].zero_fraction;
    let mut next_one_portion = p[unique_depth].pweight;
    let mut total = 0.0f32;
    let ud1 = (unique_depth + 1) as f32;
    for i in (0..unique_depth).rev() {
        if one_fraction != 0.0 {
            let tmp = next_one_portion * ud1 / ((i + 1) as f32 * one_fraction);
            total += tmp;
            next_one_portion = p[i].pweight - tmp * zero_fraction * ((unique_depth - i) as f32 / ud1);
        } else if zero_fraction != 0.0 {
            total += (p[i].pweight / zero_fraction) / ((unique_depth - i) as f32 / ud1);
        }
    }
    total
}

/// TreeShap with condition = 0 (no interaction conditioning). `base` is the offset of this call's path segment.
#[allow(clippy::too_many_arguments)]
fn tree_shap(t: &XgbTree, x: &[f32], phi: &mut [f32], node: usize, mut unique_depth: usize, buf: &mut [PathElement], base: usize,
             parent_zero_fraction: f32, parent_one_fraction: f32, parent_feature_index: i32) {
    // extend the unique path: copy the parent's segment to the next segment
    let seg = base + unique_depth + 1;
    for k in 0..=unique_depth {
        buf[seg + k] = buf[base + k];
    }
    {
        let up = &mut buf[seg..];
        extend_path(up, unique_depth, parent_zero_fraction, parent_one_fraction, parent_feature_index);
    }
    if t.is_leaf(node) {
        let up = &buf[seg..];
        let leaf = t.cond[node];
        for i in 1..=unique_depth {
            let w = unwound_path_sum(up, unique_depth, i);
            let el = up[i];
            phi[el.feature_index as usize] += w * (el.one_fraction - el.zero_fraction) * leaf * 1.0f32;
        }
        return;
    }
    let split = t.feat[node];
    let hot = t.next(node, x[split as usize]);
    let cold = if hot == t.left[node] as usize { t.right[node] as usize } else { t.left[node] as usize };
    let w = t.hess[node];
    let hot_zero = t.hess[hot] / w;
    let cold_zero = t.hess[cold] / w;
    let mut incoming_zero = 1.0f32;
    let mut incoming_one = 1.0f32;
    let mut path_index = 0usize;
    while path_index <= unique_depth {
        if buf[seg + path_index].feature_index == split as i32 {
            break;
        }
        path_index += 1;
    }
    if path_index != unique_depth + 1 {
        incoming_zero = buf[seg + path_index].zero_fraction;
        incoming_one = buf[seg + path_index].one_fraction;
        unwind_path(&mut buf[seg..], unique_depth, path_index);
        unique_depth -= 1;
    }
    tree_shap(t, x, phi, hot, unique_depth + 1, buf, seg, hot_zero * incoming_zero, incoming_one, split as i32);
    tree_shap(t, x, phi, cold, unique_depth + 1, buf, seg, cold_zero * incoming_zero, 0.0, split as i32);
}
