"""Do the 13 extra account-behaviour indicators raise the block band? Controlled test with a third L2 model.

    python3 -I research/new_features_eval.py <v2 folder>

research/l3_eval.py showed that a third L2 model trained on HI-Small, HI-Medium and HI-Large (31 served features) ranks
better but leaves block-band recall flat. This asks whether new information does what more data did not. Two third
models, identical except for the features, both trained on the training parts of the three HI sets (as l3_eval.py):
  ctrl   the 31 served features
  new    the 31 served features + the 13 indicators of research_dump (bursts, new-payee velocity, amount anomaly,
         structuring pairs, payee age; all causal)
Arms (L2 = mean of the served XGBoost, LightGBM and the third model; hardening as served, iso map refitted):
  current, mean3_ctrl, mean3_new
Calibration: each arm's isotonic map on the first half of HI-Small validation (in time); judged on the second half at
exactly the served model's alert counts (k highest scores per band).
ADOPTION RULE (fixed here before any number is computed; second half of validation only):
  N1  mean3_new block recall at k >= current + 0.02
  N2  mean3_new block recall at k >= mean3_ctrl + 0.01   (the gain comes from the indicators, not the extra model)
  N3  mean3_new PR-AUC >= current - 0.005
  N4  mean3_new worst PR-AUC under the format attack >= current worst - 0.02
If adopted the indicators must first be ported to the Rust engine and the Redis state; nothing is served from here.
Results: research/out/new_features_eval.json
"""
import json, os, sys, time
import numpy as np, pandas as pd, xgboost as xgb, lightgbm as lgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from full_hi_eval import CFG, F, L1T, VAL0, TEST0  # noqa: E402
from unified_train import NEW  # noqa: E402
from l3_eval import PARAMS  # noqa: E402
from l3_stack_eval import at_k  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT, MD = os.path.join(ROOT, "research", "out"), os.path.join(ROOT, "service", "models")
H = json.load(open(os.path.join(MD, "hardening.json")))
BANDS = ("verify", "hold", "block")
FN = F + NEW


def train_part(path, keep_neg, rng):
    side = json.load(open(path + ".json"))
    n, pre = side["rows"], side["neg_rate"]
    X, y, w = [], [], []
    for ch in pd.read_csv(path, usecols=["row", "y"] + FN, chunksize=1_000_000, dtype=np.float32):
        ch = ch[ch.row < int(n * 0.6)]
        if ch.empty:
            break
        yy = ch.y.values.astype(np.int8)
        k = (yy == 1) | (rng.random(len(ch)) < keep_neg)
        X.append(ch[FN].values[k]); y.append(yy[k]); w.append(np.where(yy[k] == 1, 1.0, 1 / (pre * keep_neg)).astype(np.float32))
    return np.vstack(X), np.concatenate(y), np.concatenate(w)


def main():
    t0 = time.time()
    v2 = sys.argv[1]
    rng = np.random.default_rng(37)
    d = pd.read_csv(os.path.join(v2, "hismall.csv.gz"), usecols=["row", "y"] + FN, dtype=np.float32)
    assert len(d) == 5_078_345
    y = d.y.values.astype(np.int8); X = d[FN].values.astype(np.float32)
    del d
    es0 = int(VAL0 * 0.9)
    k = (y[:es0] == 1) | (rng.random(es0) < 0.25)
    parts = [(X[:es0][k], y[:es0][k], np.where(y[:es0][k] == 1, 1.0, 4.0).astype(np.float32))]
    for name in ("himed", "hi_large"):
        parts.append(train_part(os.path.join(v2, f"{name}.csv.gz"), 0.5, rng))
    Xt = np.vstack([p[0] for p in parts]); yt = np.concatenate([p[1] for p in parts]); wt = np.concatenate([p[2] for p in parts])
    del parts
    models = {}
    for arm, cols in (("ctrl", F), ("new", FN)):
        ix = [FN.index(c) for c in cols]
        b = xgb.train(PARAMS, xgb.DMatrix(Xt[:, ix], label=yt, weight=wt, missing=np.nan, feature_names=cols), 3000,
                      evals=[(xgb.DMatrix(X[es0:VAL0][:, ix], label=y[es0:VAL0], missing=np.nan, feature_names=cols), "es")],
                      early_stopping_rounds=100, verbose_eval=False)
        models[arm] = (b[: b.best_iteration + 1], ix, cols)
        print(arm, "rounds", b.best_iteration + 1, round(time.time() - t0), "s", flush=True)
    del Xt, yt, wt

    b1 = xgb.Booster(); b1.load_model(os.path.join(MD, "l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(MD, "l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(ROOT, "ml", "exported", "l2_lgbm.txt"))
    bf = xgb.Booster(); bf.load_model(os.path.join(MD, H["model"]))
    fcols, fi, base = [FN.index(f) for f in H["features"]], FN.index("fmt_code"), [FN.index(f) for f in F]

    def members(Xm):
        dm = xgb.DMatrix(Xm[:, base], missing=np.nan, feature_names=F)
        z = {"l1": b1.predict(dm), "sx": bx.predict(dm), "sl": bl.predict(Xm[:, base].astype(np.float64)),
             "free": np.interp(bf.predict(xgb.DMatrix(Xm[:, fcols], missing=np.nan, feature_names=H["features"])), H["iso_free"]["x"], H["iso_free"]["y"])}
        for arm, (b, ix, cols) in models.items():
            z[arm] = b.predict(xgb.DMatrix(Xm[:, ix], missing=np.nan, feature_names=cols))
        return z

    def cascade(z, arm):
        l2 = (z["sx"] + z["sl"]) / 2 if arm == "current" else (z["sx"] + z["sl"] + z[arm.split("_")[1]]) / 3
        return np.where(z["l1"] < L1T, z["l1"], l2).astype(np.float64)

    def attacked(Xm, yy, z, code):
        Xa = Xm[yy == 1].copy(); Xa[:, fi] = code
        za = members(Xa); out = {c: v.copy() for c, v in z.items()}
        for c in out:
            out[c][yy == 1] = za[c]
        return out

    mid = VAL0 + (TEST0 - VAL0) // 2
    z1, z2, y1, y2 = members(X[VAL0:mid]), members(X[mid:TEST0]), y[VAL0:mid], y[mid:TEST0]
    served2 = np.maximum(np.interp(cascade(z2, "current"), H["iso_served"]["x"], H["iso_served"]["y"]), z2["free"])
    ks = {b: int((served2 >= H["thresholds"][b]).sum()) for b in BANDS}
    codes = {n: c for n, c in CFG["fmt_codes"].items() if n not in ("ACH", "Reinvestment")}
    R = {"rule": {"N1_min_gain_vs_current": 0.02, "N2_min_gain_vs_ctrl": 0.01, "N3_max_pr_auc_drop": 0.005, "N4_max_attack_drop": 0.02},
         "indicators": NEW, "rounds": {a: int(m[0].num_boosted_rounds()) for a, m in models.items()}, "alert_counts_second_half": ks, "validation_second_half": {}}
    for arm in ("current", "mean3_ctrl", "mean3_new"):
        iso = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(cascade(z1, arm), y1)
        score = lambda z: np.maximum(iso.predict(cascade(z, arm)), z["free"])
        s = score(z2)
        att = {n: round(float(AP(y2, score(attacked(X[mid:TEST0], y2, z2, c)))), 4) for n, c in codes.items()}
        R["validation_second_half"][arm] = {"pr_auc": round(float(AP(y2, s)), 4), "attack_worst": min(att.values()),
                                            "bands": {b: at_k(y2, s, k) for b, k in ks.items()}}
        print(arm, R["validation_second_half"][arm]["pr_auc"], R["validation_second_half"][arm]["attack_worst"], R["validation_second_half"][arm]["bands"]["block"], flush=True)
    V = R["validation_second_half"]; c, n, ct = V["current"], V["mean3_new"], V["mean3_ctrl"]
    rb = lambda v: v["bands"]["block"]["recall"]
    checks = {"N1": rb(n) >= rb(c) + 0.02, "N2": rb(n) >= rb(ct) + 0.01, "N3": n["pr_auc"] >= c["pr_auc"] - 0.005,
              "N4": n["attack_worst"] >= c["attack_worst"] - 0.02}
    R["decision"] = {"checks": checks, "adopt": all(checks.values()), "seconds": round(time.time() - t0),
                     "note": "decided on the second half of validation only; the test part is not read by this script"}
    imp = models["new"][0].get_score(importance_type="gain")
    R["new_model_top_gain"] = sorted(imp.items(), key=lambda kv: -kv[1])[:15]
    json.dump(R, open(os.path.join(OUT, "new_features_eval.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)


if __name__ == "__main__":
    main()
