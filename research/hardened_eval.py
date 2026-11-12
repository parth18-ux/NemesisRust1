"""Format-hardened score: the served cascade plus the format-free XGBoost as a second opinion.

    python3 -I research/hardened_eval.py <hismall research_dump csv.gz>

H = max(iso_served(served), iso_free(format_free)), both isotonic maps fitted on HI-Small validation. The served
score keeps its accuracy on honest traffic; the format-free model keeps catching laundering re-sent in another
format, which drops the served PR-AUC to under 0.01 (research/out/full_hi_eval.json). Band thresholds keep the
served validation alert rates, so analyst workload does not change.

ADOPTION RULE (fixed here before any test number is computed; same thresholds as research/full_hi_eval.py):
  H1  H validation PR-AUC >= served validation PR-AUC - 0.03
  H2  H worst validation PR-AUC under the format attack >= 0.8 x H clean validation PR-AUC
  H3  served worst validation PR-AUC under the attack < 0.5 x served clean (the attack is real)
The format-free model and its isotonic map come from full_hi_eval.py (trained on train rows, calibrated on
validation). Results: research/out/hardened_eval.json; if adopted, the maps and thresholds the Rust engine serves
are written to service/models/hardening.json.
SERVED=1 evaluates service/models/hardening.json as served (model, maps and thresholds read, nothing fitted or written
to service/) and writes research/out/hardened_eval_served.json, leaving the adoption record above untouched; used after
research/pooled_hardening.py replaced the second opinion (adopted under its own rules G1-G3, not H1-H3).
"""
import json, os, sys, time
import numpy as np, pandas as pd, xgboost as xgb, lightgbm as lgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from full_hi_eval import CFG, F, FNOFMT, L1T, VER, HOLD, BLK, VAL0, TEST0, RULE, pr, band_counts, thr_for_rate  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "research", "out")
TAIL = [f"2022/09/{d}" for d in range(11, 19)]


def interp(m, s):
    return np.interp(s, m["x"], m["y"])


def main():
    t0 = time.time()
    d = pd.read_csv(sys.argv[1], usecols=["row", "y", "l1", "l2x", "l2l"] + F, dtype=np.float32)
    assert len(d) == 5_078_345 and (np.diff(d.row.values) == 1).all()
    y = d.y.values.astype(np.int8); X = d[F].values.astype(np.float32)
    live = np.where(d.l1 < L1T, d.l1, (d.l2x + d.l2l) / 2).astype(np.float64)
    fi = F.index("fmt_code")
    b1 = xgb.Booster(); b1.load_model(os.path.join(ROOT, "service/models/l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(ROOT, "service/models/l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(ROOT, "ml/exported/l2_lgbm.txt"))
    SV = os.environ.get("SERVED") == "1"
    HJ = json.load(open(os.path.join(ROOT, "service", "models", "hardening.json"))) if SV else None
    FF = HJ["features"] if SV else FNOFMT
    cols = [F.index(f) for f in FF]
    bf = xgb.Booster(); bf.load_model(os.path.join(ROOT, "service", "models", "hardening_xgb.json") if SV else os.path.join(OUT, "candidate_no_format_xgb.json"))
    iso_f = HJ["iso_free"] if SV else json.load(open(os.path.join(OUT, "candidate_isotonic.json")))

    def served(Xm):
        dm = xgb.DMatrix(Xm, missing=np.nan, feature_names=F)
        s1 = b1.predict(dm)
        return np.where(s1 < L1T, s1, (bx.predict(dm) + bl.predict(Xm.astype(np.float64))) / 2)

    def free(Xm):
        return interp(iso_f, bf.predict(xgb.DMatrix(Xm[:, cols], missing=np.nan, feature_names=FF)))

    va, te = slice(VAL0, TEST0), slice(TEST0, None)
    yv, Xv = y[va], X[va]
    # the dump's served score must equal the Python reproduction (train/serve parity)
    assert np.abs(served(Xv[:200_000]) - live[va][:200_000]).max() < 1e-5
    iso = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(live[va], yv)
    iso_s = HJ["iso_served"] if SV else {"x": [float(v) for v in iso.X_thresholds_], "y": [float(v) for v in iso.y_thresholds_]}

    def hard(Xm, s_live=None):
        s = served(Xm) if s_live is None else s_live
        return np.maximum(interp(iso_s, s), free(Xm))

    formats = {k: v for k, v in CFG["fmt_codes"].items() if k not in ("ACH", "Reinvestment")}

    def attack(Xm, yy, fn):
        res = {}
        for name, code in formats.items():
            Xa = Xm.copy(); Xa[yy == 1, fi] = code
            res[name] = round(float(AP(yy, fn(Xa))), 4)
        return res

    R = {"rule": {"H1_max_drop": RULE["R1_max_drop"], "H2_min_ratio": RULE["R2_min_ratio"], "H3_served_max_ratio": RULE["R3_served_max_ratio"]}}
    hv = hard(Xv, live[va])
    R["validation"] = {"served": pr(yv, live[va], 100), "hardened": pr(yv, hv, 100)}
    R["validation_attack"] = {"served": attack(Xv, yv, served), "hardened": attack(Xv, yv, hard)}
    s_clean, h_clean = R["validation"]["served"]["pr_auc"], R["validation"]["hardened"]["pr_auc"]
    s_worst, h_worst = min(R["validation_attack"]["served"].values()), min(R["validation_attack"]["hardened"].values())
    checks = {"H1": h_clean >= s_clean - RULE["R1_max_drop"], "H2": h_worst >= RULE["R2_min_ratio"] * h_clean,
              "H3": s_worst < RULE["R3_served_max_ratio"] * s_clean}
    rates = {b: float((live[va] >= t).mean()) for b, t in (("verify", VER), ("hold", HOLD), ("block", BLK))}
    hthr = HJ["thresholds"] if SV else {b: thr_for_rate(hv, r) for b, r in rates.items()}
    R["thresholds"] = {"served_validation_alert_rates": rates, "hardened": hthr}
    R["decision"] = {"checks": checks, "adopt": all(checks.values()), "decided_at_seconds": round(time.time() - t0, 1),
                     "note": "decided on validation only; test scored after this line"}
    OUTF = os.path.join(OUT, "hardened_eval_served.json" if SV else "hardened_eval.json")
    json.dump(R, open(OUTF, "w"), indent=1)

    # ---------------------------------------------------------------- test, after the decision
    meta_day = pd.read_csv(sys.argv[2], usecols=["ts"], dtype=str).ts.str.slice(0, 10).values[TEST0:] if len(sys.argv) > 2 else None
    yt, Xt = y[te], X[te]
    ht = hard(Xt, live[te])
    T = {"served": pr(yt, live[te]), "hardened": pr(yt, ht)}
    if meta_day is not None:
        keep = ~np.isin(meta_day, TAIL)
        T["without_sep11_18_tail"] = {"served": pr(yt[keep], live[te][keep]), "hardened": pr(yt[keep], ht[keep])}
    T["attack"] = {"served": attack(Xt, yt, served), "hardened": attack(Xt, yt, hard)}
    T["bands"] = {"served": {b: band_counts(yt, live[te], t) for b, t in (("verify", VER), ("hold", HOLD), ("block", BLK))},
                  "hardened": {b: band_counts(yt, ht, t) for b, t in hthr.items()}}
    Xa = Xt.copy(); Xa[yt == 1, fi] = CFG["fmt_codes"]["Cheque"]
    sa = served(Xa); ha = hard(Xa, sa)
    T["bands_under_cheque_attack"] = {"served": {b: band_counts(yt, sa, t) for b, t in (("verify", VER), ("hold", HOLD), ("block", BLK))},
                                      "hardened": {b: band_counts(yt, ha, t) for b, t in hthr.items()}}
    R["test"] = T
    json.dump(R, open(OUTF, "w"), indent=1)
    if SV:
        R["source"] = "service/models/hardening.json as served: " + HJ["note"]
        R["decision"]["note"] = "H1-H3 re-checked for the served pooled model; it was adopted under G1-G3 (pooled_hardening.json)"
        json.dump(R, open(OUTF, "w"), indent=1)
    elif R["decision"]["adopt"]:
        json.dump({"note": "written by research/hardened_eval.py; risk = max(iso_served(cascade), iso_free(format-free XGBoost))",
                   "model": "hardening_xgb.json", "features": FNOFMT, "iso_served": iso_s, "iso_free": iso_f, "thresholds": hthr},
                  open(os.path.join(ROOT, "service", "models", "hardening.json"), "w"), indent=1)
        bf.save_model(os.path.join(ROOT, "service", "models", "hardening_xgb.json"))
    print(json.dumps({"decision": R["decision"], "validation": R["validation"], "test": {k: T[k] for k in ("served", "hardened")}}, indent=1))


if __name__ == "__main__":
    main()
