//! Score texts from a JSON list with the message model and context rules (for comparing with the Python service).
use rusetect::{context_rules as ctx, paths, scam_text};
use serde_json::{json, Value};
fn main() -> anyhow::Result<()> {
    let texts: Vec<String> = serde_json::from_str(&std::fs::read_to_string(std::env::args().nth(1).unwrap())?)?;
    let m = scam_text::MessageModel::load(&paths::models_dir().join("message_model.json"))?;
    let mut out = vec![];
    for t in texts.iter().filter(|t| !t.is_empty()) {
        let (r, e) = ctx::redact(t);
        let c = ctx::context_cues(t, true);
        out.push(json!({"score": m.score(t), "redact": [r, e], "cues": {"points": c.points, "families": c.families, "negated": c.negated},
                        "base": ctx::keyword_baseline(t), "norm": scam_text::normalize(t)}));
    }
    println!("{}", rusetect::pyfmt::dumps(&Value::Array(out), rusetect::pyfmt::RESPONSE_STYLE));
    Ok(())
}
