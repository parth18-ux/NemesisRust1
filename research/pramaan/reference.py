"""Reference decisions from PRAMAAN's own Python engine, for checking the Rust port (crates/rusetect/src/pramaan.rs).

    python3 -I research/pramaan/reference.py <PRAMAAN checkout> <out.jsonl> [n_users]

Generates simulated traffic with PRAMAAN's simulator (ml/auth/sim.py), runs every request through PRAMAAN's AuthEngine
(model, features, policy, state) and writes one line per step: the request as an API client would send it, what the
engine answered, and, for a challenged request, whether the challenge passed (seeded, as in ml/auth/evaluate.py) and
the verify result. Touch and typing deviations are drawn per request (the Rust port takes them as inputs); attackers'
deviations are drawn far from the owner's, which flatters behaviour, so NO_BEHAVIOUR=1 leaves them out entirely.
Also writes service/models/auth_xgb.json (the trees PRAMAAN scores with) and auth_config.json.
"""
import json, os, sys
import numpy as np

src, out = sys.argv[1], sys.argv[2]
n_users = int(sys.argv[3]) if len(sys.argv) > 3 else 300
NO_BEHAVIOUR = os.environ.get("NO_BEHAVIOUR") == "1"  # conservative run: no touch or typing deviations at all
sys.path.insert(0, src)
os.chdir(src)
from ml.auth.engine import AuthEngine  # noqa: E402
from ml.auth.model import AuthModel  # noqa: E402
from ml.auth.policy import AuthPolicy, EFFICACY, GENUINE_PASS  # noqa: E402
from ml.auth.sim import SimConfig, generate  # noqa: E402
from ml.auth.state import AuthState, MemoryKV  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
model = AuthModel.load()
served = model.booster[: model.best_iteration + 1]
served.save_model(os.path.join(ROOT, "service", "models", "auth_xgb.json"))
json.dump({"source": "PRAMAAN (MIT, Team CaffeineEaters) ml/auth/artifacts/auth_xgb.json, trees 0..best_iteration",
           "features": model.features, "fit_prior": model.fit_prior, "real_prior": model.real_prior,
           "version": model.meta.get("version"), "trained_on": model.meta.get("trained_on"),
           "test_roc_auc": model.meta.get("test_roc_auc")}, open(os.path.join(ROOT, "service", "models", "auth_config.json"), "w"), indent=1)

cfg = SimConfig(n_users=n_users) if "n_users" in SimConfig.__dataclass_fields__ else SimConfig()
events = generate(cfg)
rng = np.random.default_rng(3)
eng = AuthEngine(model, AuthPolicy(), AuthState(MemoryKV()))
n = 0
with open(out, "w") as fh:
    for e in events:
        lab = e["label"]
        req = {k: e[k] for k in ("user_id", "ts", "channel", "event_type", "device", "network", "transaction")}
        req["request_id"] = e["event_id"]
        beh = dict(e["behaviour"])
        if not NO_BEHAVIOUR and e["channel"] == "mobile" and rng.random() < 0.8:
            beh["bio_dev"] = round(float(rng.normal(1.0, 0.7) if lab["is_attack"] and lab["scenario"] != "coerced" else rng.normal(-0.7, 0.5)), 4)
        if not NO_BEHAVIOUR and rng.random() < 0.6:
            beh["typing_dev"] = round(float(rng.normal(0.9, 0.6) if lab["is_attack"] and lab["scenario"] != "coerced" else rng.normal(-0.55, 0.45)), 4)
        req["behaviour"] = beh
        sent = json.loads(json.dumps(req))
        d = eng.authenticate(req)
        line = {"request": sent, "label": lab, "expected": {k: d[k] for k in ("decision", "risk_score", "challenge_type", "rule", "features")},
                "expected_reasons": [r["signal"] for r in d["reasons"]]}
        if d["decision"] == "challenge":
            u, c = rng.random(), d["challenge_type"]
            passed = bool(u >= EFFICACY[c][lab["scenario"]]) if lab["is_attack"] else bool(u < GENUINE_PASS[c])
            v = eng.verify_challenge(d["decision_id"], passed)
            line["challenge"] = {"passed": passed, "decision": v["decision"], "device_trusted": v["device_trusted"]}
        fh.write(json.dumps(line, separators=(",", ":")) + "\n")
        n += 1
print("wrote", n, "steps;", sum(1 for e in events if e["label"]["is_attack"]), "attack requests")
