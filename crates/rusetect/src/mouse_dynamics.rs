//! Mouse dynamics, the "person" level (port of service/mouse_dynamics.py): is the person at the mouse the owner?
//!
//! Raw pointer events (time in seconds, x, y, button, state) are cut into ACTIONS (Antal and Egyed 2019, on the
//! Balabit data): a mouse move, a point and click, or a drag and drop. Each action becomes 24 numbers about its shape
//! and speed. A per customer model (that customer's actions against everyone else's) scores each action; a session
//! score is the mean over its actions. A SUPPORTING signal only: it never blocks on its own.
use crate::extra_trees::{Forest, ForestParams};
use crate::pyfmt::{np_mean, np_std, np_sum, py_round};
use serde_json::{json, Value};

pub const MAX_GAP_S: f64 = 3.0;
pub const MIN_POINTS: usize = 4;
pub const FEATURES: [&str; 24] = ["kind_move", "kind_click", "kind_drag", "duration", "travel", "straight", "straightness", "n_points",
    "v_mean", "v_std", "v_max", "vx_mean", "vy_mean", "a_mean", "a_std", "a_max", "jerk_mean",
    "curv_mean", "curv_std", "angle_change_mean", "largest_deviation", "dir_sin", "dir_cos", "pause_frac"];

#[derive(Debug, Clone)]
pub struct MouseEvent {
    pub t: f64,
    pub x: f64,
    pub y: f64,
    pub button: String,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct Action {
    pub pts: Vec<(f64, f64, f64)>,
    pub kind: &'static str,
}

/// Events in time order -> actions with their points and kind.
pub fn actions(events: &[MouseEvent]) -> Vec<Action> {
    let mut out = Vec::new();
    let mut cur: Vec<(f64, f64, f64)> = Vec::new();
    let mut dragging = false;
    for e in events {
        if e.button == "Scroll" || e.x > 10000.0 || e.y > 10000.0 {
            continue; // wheel events and sentinel coordinates carry no shape
        }
        if let Some(last) = cur.last() {
            if e.t - last.0 > MAX_GAP_S {
                if cur.len() >= MIN_POINTS {
                    out.push(Action { pts: std::mem::take(&mut cur), kind: "move" });
                }
                cur.clear();
                dragging = false;
            }
        }
        cur.push((e.t, e.x, e.y));
        if e.state == "Drag" {
            dragging = true;
        } else if e.state == "Released" {
            if cur.len() >= MIN_POINTS {
                out.push(Action { pts: std::mem::take(&mut cur), kind: if dragging { "drag" } else { "click" } });
            }
            cur.clear();
            dragging = false;
        }
    }
    if cur.len() >= MIN_POINTS {
        out.push(Action { pts: cur, kind: "move" });
    }
    out
}

fn diff(a: &[f64]) -> Vec<f64> {
    a.windows(2).map(|w| w[1] - w[0]).collect()
}

fn max(a: &[f64]) -> f64 {
    a.iter().copied().fold(f64::NEG_INFINITY, |m, x| if x > m || x.is_nan() { x } else { m })
}

fn abs_v(a: &[f64]) -> Vec<f64> {
    a.iter().map(|x| x.abs()).collect()
}

pub fn features(action: &Action) -> Vec<f64> {
    let t: Vec<f64> = action.pts.iter().map(|p| p.0).collect();
    let x: Vec<f64> = action.pts.iter().map(|p| p.1).collect();
    let y: Vec<f64> = action.pts.iter().map(|p| p.2).collect();
    let mut dt = diff(&t);
    for d in dt.iter_mut() {
        if *d <= 0.0 {
            *d = 1e-3;
        }
    }
    let (dx, dy) = (diff(&x), diff(&y));
    let ds: Vec<f64> = dx.iter().zip(&dy).map(|(a, b)| a.hypot(*b)).collect();
    let v: Vec<f64> = ds.iter().zip(&dt).map(|(a, b)| a / b).collect();
    let vx: Vec<f64> = dx.iter().zip(&dt).map(|(a, b)| a / b).collect();
    let vy: Vec<f64> = dy.iter().zip(&dt).map(|(a, b)| a / b).collect();
    let a: Vec<f64> = if v.len() > 1 { diff(&v).iter().zip(&dt[1..]).map(|(a, b)| a / b).collect() } else { vec![0.0] };
    let j: Vec<f64> = if a.len() > 1 { diff(&a).iter().zip(&dt[2..]).map(|(a, b)| a / b).collect() } else { vec![0.0] };
    let ang: Vec<f64> = dy.iter().zip(&dx).map(|(b, a)| b.atan2(*a)).collect();
    let dang: Vec<f64> = if ang.len() > 1 { diff(&ang).iter().map(|d| d.sin().atan2(d.cos()).abs()).collect() } else { vec![0.0] };
    let curv: Vec<f64> = if ds.len() > 1 { dang.iter().zip(&ds[1..]).map(|(a, b)| a / b.max(1e-3)).collect() } else { vec![0.0] };
    let travel = np_sum(&ds);
    let n = t.len();
    let straight = (x[n - 1] - x[0]).hypot(y[n - 1] - y[0]);
    let dev = if straight > 0.0 {
        let (nx, ny) = ((y[n - 1] - y[0]) / straight, -(x[n - 1] - x[0]) / straight);
        max(&x.iter().zip(&y).map(|(xi, yi)| ((xi - x[0]) * nx + (yi - y[0]) * ny).abs()).collect::<Vec<_>>())
    } else {
        0.0
    };
    let theta = (y[n - 1] - y[0]).atan2(x[n - 1] - x[0]);
    let k = action.kind;
    let b = |c: bool| if c { 1.0 } else { 0.0 };
    vec![b(k == "move"), b(k == "click"), b(k == "drag"), t[n - 1] - t[0], travel, straight, if travel != 0.0 { straight / travel } else { 0.0 }, n as f64,
         np_mean(&v), np_std(&v), max(&v), np_mean(&abs_v(&vx)), np_mean(&abs_v(&vy)),
         np_mean(&abs_v(&a)), np_std(&a), max(&abs_v(&a)), np_mean(&abs_v(&j)),
         np_mean(&curv), np_std(&curv), np_mean(&dang), dev, theta.sin(), theta.cos(),
         dt.iter().filter(|d| **d > 0.1).count() as f64 / dt.len() as f64]
}

/// One float32 row per action.
pub fn session_matrix(events: &[MouseEvent]) -> Vec<Vec<f32>> {
    actions(events).iter().map(|a| features(a).into_iter().map(|v| v as f32).collect()).collect()
}

/// `np.nan_to_num` for float32 rows
pub fn nan_to_num(rows: &[Vec<f32>]) -> Vec<Vec<f32>> {
    rows.iter().map(|r| r.iter().map(|v| if v.is_nan() { 0.0 } else if *v == f32::INFINITY { f32::MAX } else if *v == f32::NEG_INFINITY { f32::MIN } else { *v }).collect()).collect()
}

/// Per customer model. fit(own, others) then score(session events) -> mean probability that it is the owner.
pub struct MouseProfile {
    pub params: ForestParams,
    model: Option<Forest>,
}

impl MouseProfile {
    pub fn new(n_estimators: usize, seed: u64) -> Self {
        let mut p = ForestParams::extra_trees(n_estimators, 2, seed);
        p.class_weight_balanced = true;
        MouseProfile { params: p, model: None }
    }

    pub fn fit(&mut self, own: &[Vec<f32>], others: &[Vec<f32>]) -> &mut Self {
        let x: Vec<Vec<f32>> = nan_to_num(&[own, others].concat());
        let y: Vec<u8> = std::iter::repeat_n(1u8, own.len()).chain(std::iter::repeat_n(0u8, others.len())).collect();
        self.model = Some(Forest::fit(&x, &y, &self.params));
        self
    }

    pub fn action_scores(&self, m: &[Vec<f32>]) -> Vec<f64> {
        let f = self.model.as_ref().expect("fit first");
        nan_to_num(m).iter().map(|r| f.predict_proba(r)).collect()
    }

    pub fn score(&self, events: &[MouseEvent], max_actions: Option<usize>) -> Value {
        let mut m = session_matrix(events);
        if let Some(k) = max_actions {
            m.truncate(k);
        }
        let s = self.action_scores(&m);
        json!({"actions": s.len(), "owner_probability": if s.is_empty() { Value::Null } else { json!(py_round(np_mean(&s), 3)) }})
    }
}
