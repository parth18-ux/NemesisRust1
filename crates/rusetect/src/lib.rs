//! RuseTect core library: everything the API, the scoring workers and the ML tools share.
//!
//! Module map (Python original in brackets):
//!   engine          streaming features, L1/L2 cascade, ring linker, L0 onboarding   [service/rusetect_engine.py]
//!   compact_state   compact account state with exact or Bloom pair index             [service/compact_state.py]
//!   models          XGBoost JSON + TreeSHAP, LightGBM dump inference                  [xgboost, service/lgbm_lite.py]
//!   context_rules   consent, redaction, cue families, risk-point fusion               [service/context_rules.py]
//!   scam_text       L-1 lure detector (hashed TF-IDF logistic model + explanations)   [service/scam_text.py]
//!   lure_fusion     message -> payment escalation and alert-fatigue policy            [service/lure_fusion.py]
//!   session_signals session and identity domain, attack chains                        [service/session_signals.py]
//!   review          alerts, four-eyes review, hash-chained audit log, evidence packs  [service/review.py]
//!   security        analyst keys, nonces, rate limit, Ed25519 signing                 [service/security.py]
//!   consortium      signed hash-chained cross-bank ledger                              [service/consortium.py]
//!   intervention    pause screen and duress PIN                                        [service/intervention.py]
//!   behaviour       typing rhythm profile and verifier                                 [service/behaviour.py]
//!   mouse_dynamics  mouse action features and per-customer model                       [service/mouse_dynamics.py]
//!   campaigns, coordination, str_report, risk_score, branding                          [same names]
//!   pyfmt, pyre     Python-compatible numerics, formatting, JSON and regex semantics
// Deliberate: comparisons are written NaN-aware (`!(a > b)`), branches mirror the Python they port line by line,
// and min/max chains keep Python's NaN behaviour where clamp() would panic or differ.
#![allow(clippy::neg_cmp_op_on_partial_ord, clippy::if_same_then_else, clippy::manual_clamp, clippy::type_complexity)]
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]
pub mod behaviour;
pub mod branding;
pub mod campaigns;
pub mod compact_state;
pub mod consortium;
pub mod context_rules;
pub mod coordination;
pub mod engine;
pub mod extra_trees;
pub mod intervention;
pub mod lure_fusion;
pub mod models;
pub mod mouse_dynamics;
pub mod paths;
pub mod payment;
pub mod pyfmt;
pub mod pyre;
pub mod review;
pub mod risk_score;
pub mod scam_text;
pub mod security;
pub mod session_signals;
pub mod str_report;

pub use engine::Engine;
pub use payment::Payment;
