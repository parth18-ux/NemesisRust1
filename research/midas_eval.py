"""Does a MIDAS-R streaming anomaly score help? Controlled test on the full HI-Small stream.

    python3 -I research/midas_eval.py <hismall research_dump csv.gz> <hi_small_sorted.csv.gz>

MIDAS-R (Bhatia et al., AAAI 2020 / TKDD 2022; github.com/Stream-AD/MIDAS, Apache-2.0) scores each edge of a time-evolving
graph against decayed count-min sketches of the edge, its source and its destination: a sudden burst (a microcluster)
scores high. Here an edge is a payment (payer account -> payee account) and a tick is one hour. It is computed causally,
before the payment updates nothing else, over every HI-Small payment in time order (the Medium and Large dumps were
pre-sampled, so their streams are incomplete and cannot be used).
Two models, identical except for the features, trained on HI-Small train rows (early stop on its last 10%):
  ctrl   the 31 served features
  midas  the 31 served features + 3 MIDAS-R scores (edge, source, destination; log1p)
Arms (L2 = mean of the served XGBoost, LightGBM and the model; hardening as served, isotonic map refitted):
  current, mean3_ctrl, mean3_midas. Calibration on the first half of HI-Small validation; judged on the second half at
  the served alert counts (k highest scores per band), as research/new_features_eval.py.
ADOPTION RULE (fixed here before any number is computed; second half of validation only):
  M1  mean3_midas block recall at k >= current + 0.02
  M2  mean3_midas block recall at k >= mean3_ctrl + 0.01
  M3  mean3_midas PR-AUC >= current - 0.005
  M4  mean3_midas worst PR-AUC under the format attack >= current worst - 0.02
Also reported, not judged: the MIDAS-R score alone (ROC-AUC, PR-AUC on validation). If adopted, MIDAS-R is ported to
Rust (engine and Redis worker) before anything is served. Results: research/out/midas_eval.json
"""
import json, math, os, sys, time
import numpy as np, pandas as pd, xgboost as xgb, lightgbm as lgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP, roc_auc_score as AUC

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from full_hi_eval import CFG, F, L1T, VAL0, TEST0  # noqa: E402
from l3_eval import PARAMS  # noqa: E402
from l3_stack_eval import at_k  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT, MD = os.path.join(ROOT, "research", "out"), os.path.join(ROOT, "service", "models")
H = json.load(open(os.path.join(MD, "hardening.json")))
BANDS = ("verify", "hold", "block")
MIDAS = ["midas_edge", "midas_src", "midas_dst"]
R_ROWS, C_COLS, FACTOR, PRIME = 2, 4096, 0.5, 104729


def midas_r(src, dst, tick):
    """MIDAS-R, as Stream-AD/MIDAS src/RelationalCore.hpp: three (current, total) sketch pairs, current decayed by FACTOR
    whenever the tick advances; score = max over the three of ((a - s/t) * t)^2 / (s * (t - 1)). Returns the three parts."""
    rng = np.random.default_rng(1)
    p1 = rng.integers(1, 2**31 - 1, R_ROWS); p2 = rng.integers(0, 2**31 - 1, R_ROWS)
    cur = np.zeros((3, R_ROWS * C_COLS), np.float64); tot = np.zeros((3, R_ROWS * C_COLS), np.float64)
    base = np.arange(R_ROWS) * C_COLS
    out = np.zeros((len(src), 3), np.float32)
    now, t = None, 0
    for i in range(len(src)):
        if tick[i] != now:
            if now is not None:
                cur *= FACTOR ** (tick[i] - now)
                t += 1
            now = tick[i]
        tt = t + 1
        keys = ((src[i] + PRIME * dst[i]), src[i], dst[i])
        for j, k in enumerate(keys):
            idx = base + (k * p1 + p2) % C_COLS
            cur[j, idx] += 1; tot[j, idx] += 1
            a, s = cur[j, idx].min(), tot[j, idx].min()
            out[i, j] = 0.0 if s == 0 or tt - 1 == 0 else ((a - s / tt) * tt) ** 2 / (s * (tt - 1))
    return out


def main():
    t0 = time.time()
    raw = pd.read_csv(sys.argv[2], usecols=["ts", "fb", "fa", "tb", "ta"], dtype=str)
    tick = (pd.to_datetime(raw.ts, format="%Y/%m/%d %H:%M").astype("int64") // 3_600_000_000_000).values
    ids = {}
    node = lambda a: ids.setdefault(a, len(ids))
    src = np.array([node(x) for x in (raw.fb + "|" + raw.fa).values], np.int64)
    dst = np.array([node(x) for x in (raw.tb + "|" + raw.ta).values], np.int64)
    del raw
    m = np.log1p(midas_r(src, dst, tick))
    print("midas", round(time.time() - t0), "s", flush=True)
    d = pd.read_csv(sys.argv[1], usecols=["row", "y"] + F, dtype=np.float32)
    assert len(d) == len(m) == 5_078_345
    y = d.y.values.astype(np.int8)
    X = np.hstack([d[F].values.astype(np.float32), m])
    FM = F + MIDAS
    del d
    rng = np.random.default_rng(43)
    es0 = int(VAL0 * 0.9)
    k = (y[:es0] == 1) | (rng.random(es0) < 0.25)
    w = np.where(y[:es0][k] == 1, 1.0, 4.0).astype(np.float32)
    models = {}
    for arm, cols in (("ctrl", F), ("midas", FM)):
        ix = [FM.index(c) for c in cols]
        b = xgb.train(PARAMS, xgb.DMatrix(X[:es0][k][:, ix], label=y[:es0][k], weight=w, missing=np.nan, feature_names=cols), 3000,
                      evals=[(xgb.DMatrix(X[es0:VAL0][:, ix], label=y[es0:VAL0], missing=np.nan, feature_names=cols), "es")],
                      early_stopping_rounds=100, verbose_eval=False)
        models[arm] = (b[: b.best_iteration + 1], ix, cols)
        print(arm, "rounds", b.best_iteration + 1, flush=True)
    b1 = xgb.Booster(); b1.load_model(os.path.join(MD, "l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(MD, "l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(ROOT, "ml", "exported", "l2_lgbm.txt"))
    bf = xgb.Booster(); bf.load_model(os.path.join(MD, H["model"]))
    fcols, fi, base = [FM.index(f) for f in H["features"]], FM.index("fmt_code"), [FM.index(f) for f in F]

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
    mv = np.max(X[VAL0:TEST0][:, [FM.index(c) for c in MIDAS]], 1)
    R = {"rule": {"M1_min_gain_vs_current": 0.02, "M2_min_gain_vs_ctrl": 0.01, "M3_max_pr_auc_drop": 0.005, "M4_max_attack_drop": 0.02},
         "midas": {"rows": R_ROWS, "cols": C_COLS, "factor": FACTOR, "tick": "hour"},
         "midas_alone_validation": {"roc_auc": round(float(AUC(y[VAL0:TEST0], mv)), 4), "pr_auc": round(float(AP(y[VAL0:TEST0], mv)), 4)},
         "rounds": {a: int(v[0].num_boosted_rounds()) for a, v in models.items()}, "alert_counts_second_half": ks, "validation_second_half": {}}
    for arm in ("current", "mean3_ctrl", "mean3_midas"):
        iso = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(cascade(z1, arm), y1)
        score = lambda z: np.maximum(iso.predict(cascade(z, arm)), z["free"])
        s = score(z2)
        att = {n: round(float(AP(y2, score(attacked(X[mid:TEST0], y2, z2, c)))), 4) for n, c in codes.items()}
        R["validation_second_half"][arm] = {"pr_auc": round(float(AP(y2, s)), 4), "attack_worst": min(att.values()),
                                            "bands": {b: at_k(y2, s, kk) for b, kk in ks.items()}}
        print(arm, R["validation_second_half"][arm]["pr_auc"], R["validation_second_half"][arm]["bands"]["block"], flush=True)
    V = R["validation_second_half"]; c, n, ct = V["current"], V["mean3_midas"], V["mean3_ctrl"]
    rb = lambda v: v["bands"]["block"]["recall"]
    checks = {"M1": rb(n) >= rb(c) + 0.02, "M2": rb(n) >= rb(ct) + 0.01, "M3": n["pr_auc"] >= c["pr_auc"] - 0.005,
              "M4": n["attack_worst"] >= c["attack_worst"] - 0.02}
    R["decision"] = {"checks": checks, "adopt": all(checks.values()), "seconds": round(time.time() - t0),
                     "note": "decided on the second half of validation only; the test part is not read by this script"}
    R["midas_model_gain"] = {k2: round(float(v), 2) for k2, v in sorted(models["midas"][0].get_score(importance_type="gain").items(), key=lambda kv: -kv[1])[:12]}
    json.dump(R, open(os.path.join(OUT, "midas_eval.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)


if __name__ == "__main__":
    main()
