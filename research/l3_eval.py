"""A third L2 model trained on far more laundering from the same generator, to raise the block band.

    python3 -I research/l3_eval.py <v2 folder> <hi_small_sorted.csv.gz>

The served L2 (mean of XGBoost and LightGBM) learned from HI-Small's training part only. HI-Medium and HI-Large come from
the same AMLworld generator with tens of times more laundering. Candidate model: XGBoost on the 31 served features,
trained on the training part (first 60% in time) of HI-Small, HI-Medium and HI-Large; pre-sampled genuine rows are
re-weighted; early stopping on the last 10% of HI-Small's training part (never validation). Arms, all through the served
hardening (max of the calibrated cascade and the calibrated pooled format-free model, service/models/hardening.json):
  current   L2 = mean(XGBoost, LightGBM)                 (served now)
  mean3     L2 = mean(XGBoost, LightGBM, candidate)
  l3_only   L2 = candidate
Each arm's isotonic map and band thresholds are fitted on HI-Small validation at the current validation alert rates
(same analyst workload). Payments below the L1 gate keep the L1 score in every arm.
ADOPTION RULE (fixed here before any number is computed; HI-Small validation only):
  pick the arm with the highest validation block-band recall; adopt it over current only if
  L1  its block recall >= current + 0.02 (same block alert count, so precision rises with it)
  L2  its hardened validation PR-AUC >= current - 0.005
  L3  its worst hardened validation PR-AUC under the format attack >= current worst - 0.02
Test numbers are computed after the decision is written. Results: research/out/l3_eval.json; if adopted the model goes
to service/models/l3_xgb.json, cascade_config.json gains "l2_mode", hardening.json gets the arm's map and thresholds.
"""
import json, os, sys, time
import numpy as np, pandas as pd, xgboost as xgb, lightgbm as lgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from full_hi_eval import CFG, F, L1T, VAL0, TEST0, pr, band_counts, thr_for_rate  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT, MD = os.path.join(ROOT, "research", "out"), os.path.join(ROOT, "service", "models")
H = json.load(open(os.path.join(MD, "hardening.json")))
TAIL = [f"2022/09/{d}" for d in range(11, 19)]
RNG = np.random.default_rng(31)
PARAMS = {"objective": "binary:logistic", "eval_metric": "aucpr", "max_depth": 8, "eta": 0.05, "subsample": 0.8,
          "colsample_bytree": 0.8, "min_child_weight": 2, "tree_method": "hist", "nthread": 4}
BANDS = ("verify", "hold", "block")


def train_part(path, keep_neg):
    """training rows (first 60% in time) of one dump: all positives, a share of genuine rows, weights for the true mix"""
    side = json.load(open(path + ".json")) if os.path.exists(path + ".json") else None
    n, pre = (side["rows"], side["neg_rate"]) if side else (VAL0 / 0.6, 1.0)
    cols = ["row", "y"] + F
    X, y, w = [], [], []
    for ch in pd.read_csv(path, usecols=cols, chunksize=1_000_000, dtype=np.float32):
        ch = ch[ch.row < int(n * 0.6)]
        if ch.empty:
            break
        yy = ch.y.values.astype(np.int8)
        k = (yy == 1) | (RNG.random(len(ch)) < keep_neg)
        X.append(ch[F].values[k]); y.append(yy[k]); w.append(np.where(yy[k] == 1, 1.0, 1 / (pre * keep_neg)).astype(np.float32))
    return np.vstack(X), np.concatenate(y), np.concatenate(w)


def main():
    t0 = time.time()
    v2, sorted_src = sys.argv[1], sys.argv[2]
    d = pd.read_csv(os.path.join(v2, "hismall.csv.gz"), usecols=["row", "y", "l1", "l2x", "l2l"] + F, dtype=np.float32)
    assert len(d) == 5_078_345
    y = d.y.values.astype(np.int8); X = d[F].values.astype(np.float32)
    # training data: HI-Small train rows (early-stop on its last 10%), HI-Medium and HI-Large train parts
    es0 = int(VAL0 * 0.9)
    k = (y[:es0] == 1) | (RNG.random(es0) < 0.25)
    parts = [(X[:es0][k], y[:es0][k], np.where(y[:es0][k] == 1, 1.0, 4.0).astype(np.float32))]
    for name in ("himed", "hi_large"):
        parts.append(train_part(os.path.join(v2, f"{name}.csv.gz"), 0.5))
        print(name, "train rows", len(parts[-1][1]), "positives", int(parts[-1][1].sum()), flush=True)
    Xt = np.vstack([p[0] for p in parts]); yt = np.concatenate([p[1] for p in parts]); wt = np.concatenate([p[2] for p in parts])
    dtr = xgb.DMatrix(Xt, label=yt, weight=wt, missing=np.nan, feature_names=F)
    des = xgb.DMatrix(X[es0:VAL0], label=y[es0:VAL0], missing=np.nan, feature_names=F)
    l3 = xgb.train(PARAMS, dtr, 3000, evals=[(des, "es")], early_stopping_rounds=100, verbose_eval=200)
    l3 = l3[: l3.best_iteration + 1]  # serve exactly the evaluated trees
    print("trained", l3.num_boosted_rounds(), "rounds", round(time.time() - t0), "s", flush=True)
    del dtr, Xt, yt, wt, parts

    b1 = xgb.Booster(); b1.load_model(os.path.join(MD, "l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(MD, "l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(ROOT, "ml", "exported", "l2_lgbm.txt"))
    bf = xgb.Booster(); bf.load_model(os.path.join(MD, H["model"]))
    fcols = [F.index(f) for f in H["features"]]
    fi = F.index("fmt_code")

    def members(Xm):
        dm = xgb.DMatrix(Xm, missing=np.nan, feature_names=F)
        return b1.predict(dm), bx.predict(dm), bl.predict(Xm.astype(np.float64)), l3.predict(dm)

    def free(Xm):
        return np.interp(bf.predict(xgb.DMatrix(Xm[:, fcols], missing=np.nan, feature_names=H["features"])), H["iso_free"]["x"], H["iso_free"]["y"])

    arm_l2 = {"current": lambda x, l, z: (x + l) / 2, "mean3": lambda x, l, z: (x + l + z) / 3, "l3_only": lambda x, l, z: z}

    def cascade(m, arm):
        s1, sx, sl, s3 = m
        return np.where(s1 < L1T, s1, arm_l2[arm](sx, sl, s3)).astype(np.float64)

    va, te = slice(VAL0, TEST0), slice(TEST0, None)
    mv, fv, yv = members(X[va]), free(X[va]), y[va]
    # parity: the reproduced served cascade equals the dump
    live = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2)
    assert np.abs(cascade(mv, "current") - live[va]).max() < 1e-5
    served_rates = {b: float((np.maximum(np.interp(live[va], H["iso_served"]["x"], H["iso_served"]["y"]), fv) >= H["thresholds"][b]).mean()) for b in BANDS}
    codes = {k: v for k, v in CFG["fmt_codes"].items() if k not in ("ACH", "Reinvestment")}

    def attacked(Xm, yy, m, code):
        """members with every laundering payment's format switched; genuine rows unchanged"""
        Xa = Xm[yy == 1].copy(); Xa[:, fi] = code
        out = [a.copy() for a in m]
        for a, sub in zip(out, members(Xa)):
            a[yy == 1] = sub
        return out

    R = {"rule": {"L1_min_block_recall_gain": 0.02, "L2_max_pr_auc_drop": 0.005, "L3_max_attack_drop": 0.02},
         "train": {"rounds": int(l3.num_boosted_rounds())}, "validation_alert_rates": served_rates, "validation": {}}
    fit = {}
    for arm in arm_l2:
        s = cascade(mv, arm)
        iso = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(s, yv)
        iso_s = {"x": [float(v) for v in iso.X_thresholds_], "y": [float(v) for v in iso.y_thresholds_]}
        h = np.maximum(np.interp(s, iso_s["x"], iso_s["y"]), fv)
        thr = {b: thr_for_rate(h, r) for b, r in served_rates.items()}
        att = {}
        for name, code in codes.items():
            ma = attacked(X[va], yv, mv, code)
            att[name] = round(float(AP(yv, np.maximum(np.interp(cascade(ma, arm), iso_s["x"], iso_s["y"]), fv))), 4)
        fit[arm] = (iso_s, thr)
        R["validation"][arm] = {"pr_auc": round(float(AP(yv, h)), 4), "attack_worst": min(att.values()),
                                "bands": {b: band_counts(yv, h, t) for b, t in thr.items()}}
        print(arm, R["validation"][arm]["pr_auc"], R["validation"][arm]["attack_worst"], R["validation"][arm]["bands"]["block"], flush=True)
    V = R["validation"]
    best = max(("mean3", "l3_only"), key=lambda a: V[a]["bands"]["block"]["recall"])
    c = V["current"]
    checks = {"L1": V[best]["bands"]["block"]["recall"] >= c["bands"]["block"]["recall"] + 0.02,
              "L2": V[best]["pr_auc"] >= c["pr_auc"] - 0.005, "L3": V[best]["attack_worst"] >= c["attack_worst"] - 0.02}
    R["decision"] = {"candidate": best, "checks": checks, "adopt": all(checks.values()), "decided_at_seconds": round(time.time() - t0),
                     "note": "decided on validation only; test scored after this line"}
    json.dump(R, open(os.path.join(OUT, "l3_eval.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)

    # ------------------------------------------------------------- test, after the decision
    day = pd.read_csv(sorted_src, usecols=["ts"], dtype=str).ts.str.slice(0, 10).values[TEST0:]
    keep = ~np.isin(day, TAIL)
    mt, ft, yt = members(X[te]), free(X[te]), y[te]
    T = {}
    for arm, (iso_s, thr) in fit.items():
        h = np.maximum(np.interp(cascade(mt, arm), iso_s["x"], iso_s["y"]), ft)
        ma = attacked(X[te], yt, mt, CFG["fmt_codes"]["Cheque"])
        hc = np.maximum(np.interp(cascade(ma, arm), iso_s["x"], iso_s["y"]), ft)
        T[arm] = {"all": pr(yt, h), "without_sep11_18_tail": pr(yt[keep], h[keep]),
                  "bands": {b: band_counts(yt, h, t) for b, t in thr.items()},
                  "bands_under_cheque_attack": {b: band_counts(yt, hc, t) for b, t in thr.items()}}
        print("test", arm, T[arm]["all"]["pr_auc"], T[arm]["bands"]["block"], T[arm]["bands_under_cheque_attack"]["block"]["recall"], flush=True)
    R["test"] = T
    json.dump(R, open(os.path.join(OUT, "l3_eval.json"), "w"), indent=1)
    if R["decision"]["adopt"]:
        l3.save_model(os.path.join(MD, "l3_xgb.json"))
        cfg = json.load(open(os.path.join(MD, "cascade_config.json"))); cfg["l2_mode"] = best
        json.dump(cfg, open(os.path.join(MD, "cascade_config.json"), "w"), indent=1)
        H["iso_served"], H["thresholds"] = fit[best]
        H["note"] += f"; cascade L2 = {best} (research/l3_eval.py)"
        json.dump(H, open(os.path.join(MD, "hardening.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
