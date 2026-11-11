//! Tree ensembles for classification: Extremely Randomized Trees and Random Forest, plus a single CART tree.
//!
//! Replaces scikit-learn's ExtraTreesClassifier / RandomForestClassifier / DecisionTreeClassifier (used by the mouse
//! dynamics profile and the offline model comparisons). Same algorithms and hyper-parameters (gini impurity, sqrt
//! feature sampling, min_samples_leaf, balanced class weights, bootstrap for the random forest), but scikit-learn's
//! random number stream is not reproduced, so individual trees differ from a Python run with the same seed.
use rand::seq::index::sample;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;

#[derive(Clone, Copy, Debug)]
pub enum MaxFeatures {
    Sqrt,
    All,
    Fraction(f64),
}

#[derive(Clone, Debug)]
pub struct ForestParams {
    pub n_estimators: usize,
    pub max_features: MaxFeatures,
    pub min_samples_leaf: usize,
    pub min_samples_split: usize,
    pub max_depth: Option<usize>,
    pub bootstrap: bool,
    /// true: Extra-Trees random thresholds; false: best split (CART)
    pub random_splits: bool,
    pub class_weight_balanced: bool,
    pub seed: u64,
    pub n_jobs: usize,
}

impl ForestParams {
    pub fn extra_trees(n_estimators: usize, min_samples_leaf: usize, seed: u64) -> Self {
        ForestParams { n_estimators, max_features: MaxFeatures::Sqrt, min_samples_leaf, min_samples_split: 2, max_depth: None, bootstrap: false,
                       random_splits: true, class_weight_balanced: false, seed, n_jobs: 2 }
    }
    pub fn random_forest(n_estimators: usize, seed: u64) -> Self {
        ForestParams { n_estimators, max_features: MaxFeatures::Sqrt, min_samples_leaf: 1, min_samples_split: 2, max_depth: None, bootstrap: true,
                       random_splits: false, class_weight_balanced: false, seed, n_jobs: 2 }
    }
    pub fn decision_tree(max_depth: Option<usize>, seed: u64) -> Self {
        ForestParams { n_estimators: 1, max_features: MaxFeatures::All, min_samples_leaf: 1, min_samples_split: 2, max_depth, bootstrap: false,
                       random_splits: false, class_weight_balanced: false, seed, n_jobs: 1 }
    }
}

#[derive(Clone, Debug)]
struct Node {
    feat: i32,
    thr: f32,
    left: u32,
    right: u32,
    p1: f64,
}

#[derive(Clone, Debug)]
pub struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    pub fn predict(&self, x: &[f32]) -> f64 {
        let mut i = 0usize;
        loop {
            let n = &self.nodes[i];
            if n.feat < 0 {
                return n.p1;
            }
            // sklearn sends NaN-free input (callers nan_to_num); x <= thr goes left
            i = if x[n.feat as usize] <= n.thr { n.left as usize } else { n.right as usize };
        }
    }
}

#[derive(Clone, Debug)]
pub struct Forest {
    pub trees: Vec<Tree>,
    pub n_features: usize,
}

struct Data<'a> {
    x: &'a [Vec<f32>],
    y: &'a [u8],
    w: Vec<f64>, // sample weight (class weight x bootstrap count)
}

fn gini(w0: f64, w1: f64) -> f64 {
    let t = w0 + w1;
    if t <= 0.0 {
        return 0.0;
    }
    let (p0, p1) = (w0 / t, w1 / t);
    1.0 - p0 * p0 - p1 * p1
}

impl Forest {
    /// Fit on rows `x` (all the same length) and binary labels `y`.
    pub fn fit(x: &[Vec<f32>], y: &[u8], p: &ForestParams) -> Forest {
        let n_features = x.first().map(|r| r.len()).unwrap_or(0);
        let (n0, n1) = y.iter().fold((0usize, 0usize), |(a, b), &v| if v == 1 { (a, b + 1) } else { (a + 1, b) });
        let n = y.len() as f64;
        let cw = if p.class_weight_balanced && n0 > 0 && n1 > 0 { [n / (2.0 * n0 as f64), n / (2.0 * n1 as f64)] } else { [1.0, 1.0] };
        let jobs = p.n_jobs.max(1).min(p.n_estimators.max(1));
        let seeds: Vec<u64> = {
            let mut r = StdRng::seed_from_u64(p.seed);
            (0..p.n_estimators).map(|_| r.gen()).collect()
        };
        let mut trees: Vec<Option<Tree>> = vec![None; p.n_estimators];
        std::thread::scope(|s| {
            let chunks: Vec<(usize, &mut [Option<Tree>])> = {
                let size = p.n_estimators.div_ceil(jobs);
                trees.chunks_mut(size.max(1)).enumerate().map(|(i, c)| (i * size.max(1), c)).collect()
            };
            for (start, chunk) in chunks {
                let seeds = &seeds;
                s.spawn(move || {
                    for (k, slot) in chunk.iter_mut().enumerate() {
                        let mut rng = StdRng::seed_from_u64(seeds[start + k]);
                        let mut w: Vec<f64> = y.iter().map(|&c| cw[c as usize]).collect();
                        if p.bootstrap {
                            let mut cnt = vec![0u32; y.len()];
                            for _ in 0..y.len() {
                                cnt[rng.gen_range(0..y.len())] += 1;
                            }
                            for (wi, c) in w.iter_mut().zip(cnt) {
                                *wi *= c as f64;
                            }
                        }
                        let d = Data { x, y, w };
                        *slot = Some(build_tree(&d, p, &mut rng, n_features));
                    }
                });
            }
        });
        Forest { trees: trees.into_iter().map(|t| t.unwrap()).collect(), n_features }
    }

    /// Mean probability of class 1 over the trees.
    pub fn predict_proba(&self, x: &[f32]) -> f64 {
        self.trees.iter().map(|t| t.predict(x)).sum::<f64>() / self.trees.len() as f64
    }
}

fn build_tree(d: &Data, p: &ForestParams, rng: &mut StdRng, nf: usize) -> Tree {
    let idx: Vec<u32> = (0..d.y.len() as u32).filter(|&i| d.w[i as usize] > 0.0).collect();
    let mut nodes = Vec::new();
    let k = match p.max_features {
        MaxFeatures::Sqrt => ((nf as f64).sqrt() as usize).max(1),
        MaxFeatures::All => nf,
        MaxFeatures::Fraction(f) => ((nf as f64 * f) as usize).max(1),
    }
    .min(nf.max(1));
    grow(d, p, rng, nf, k, idx, 0, &mut nodes);
    Tree { nodes }
}

#[allow(clippy::too_many_arguments)]
fn grow(d: &Data, p: &ForestParams, rng: &mut StdRng, nf: usize, k: usize, idx: Vec<u32>, depth: usize, nodes: &mut Vec<Node>) -> u32 {
    let (w0, w1) = idx.iter().fold((0.0, 0.0), |(a, b), &i| {
        let w = d.w[i as usize];
        if d.y[i as usize] == 1 { (a, b + w) } else { (a + w, b) }
    });
    let me = nodes.len() as u32;
    let p1 = if w0 + w1 > 0.0 { w1 / (w0 + w1) } else { 0.0 };
    nodes.push(Node { feat: -1, thr: 0.0, left: 0, right: 0, p1 });
    let pure = w0 == 0.0 || w1 == 0.0;
    if pure || idx.len() < p.min_samples_split.max(2 * p.min_samples_leaf) || p.max_depth.map(|m| depth >= m).unwrap_or(false) {
        return me;
    }
    let parent_imp = gini(w0, w1);
    let mut best: Option<(f64, usize, f32)> = None; // (weighted child impurity, feature, threshold)
    // visit features in random order; keep drawing past constant features until k informative ones were tried
    let order = sample(rng, nf, nf).into_vec();
    let mut tried = 0;
    for f in order {
        if tried >= k {
            break;
        }
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for &i in &idx {
            let v = d.x[i as usize][f];
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if !(hi > lo) {
            continue;
        }
        tried += 1;
        if p.random_splits {
            let thr = lo + (hi - lo) * rng.gen::<f32>();
            let thr = if thr >= hi { lo } else { thr };
            let (mut l0, mut l1, mut nl) = (0.0, 0.0, 0usize);
            for &i in &idx {
                if d.x[i as usize][f] <= thr {
                    nl += 1;
                    let w = d.w[i as usize];
                    if d.y[i as usize] == 1 { l1 += w } else { l0 += w }
                }
            }
            let nr = idx.len() - nl;
            if nl < p.min_samples_leaf || nr < p.min_samples_leaf {
                continue;
            }
            let (r0, r1) = (w0 - l0, w1 - l1);
            let imp = ((l0 + l1) * gini(l0, l1) + (r0 + r1) * gini(r0, r1)) / (w0 + w1);
            if best.map(|b| imp < b.0).unwrap_or(true) {
                best = Some((imp, f, thr));
            }
        } else {
            let mut s: Vec<(f32, u32)> = idx.iter().map(|&i| (d.x[i as usize][f], i)).collect();
            s.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let (mut l0, mut l1) = (0.0, 0.0);
            for j in 0..s.len() - 1 {
                let i = s[j].1 as usize;
                if d.y[i] == 1 { l1 += d.w[i] } else { l0 += d.w[i] }
                if s[j].0 == s[j + 1].0 {
                    continue;
                }
                let nl = j + 1;
                if nl < p.min_samples_leaf || s.len() - nl < p.min_samples_leaf {
                    continue;
                }
                let (r0, r1) = (w0 - l0, w1 - l1);
                let imp = ((l0 + l1) * gini(l0, l1) + (r0 + r1) * gini(r0, r1)) / (w0 + w1);
                if best.map(|b| imp < b.0).unwrap_or(true) {
                    best = Some((imp, f, s[j].0 + (s[j + 1].0 - s[j].0) / 2.0));
                }
            }
        }
    }
    let Some((imp, f, thr)) = best else { return me };
    if imp >= parent_imp && !p.random_splits {
        return me;
    }
    let (li, ri): (Vec<u32>, Vec<u32>) = idx.into_iter().partition(|&i| d.x[i as usize][f] <= thr);
    let l = grow(d, p, rng, nf, k, li, depth + 1, nodes);
    let r = grow(d, p, rng, nf, k, ri, depth + 1, nodes);
    let n = &mut nodes[me as usize];
    n.feat = f as i32;
    n.thr = thr;
    n.left = l;
    n.right = r;
    me
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_two_clusters() {
        let mut rng = StdRng::seed_from_u64(0);
        let mut x = vec![];
        let mut y = vec![];
        for i in 0..400 {
            let c = (i % 2) as u8;
            x.push(vec![c as f32 * 3.0 + rng.gen::<f32>(), rng.gen::<f32>()]);
            y.push(c);
        }
        for p in [ForestParams::extra_trees(50, 2, 0), ForestParams::random_forest(50, 0), ForestParams::decision_tree(Some(4), 0)] {
            let f = Forest::fit(&x, &y, &p);
            assert!(f.predict_proba(&[3.5, 0.5]) > 0.9 && f.predict_proba(&[0.5, 0.5]) < 0.1);
        }
    }
}
