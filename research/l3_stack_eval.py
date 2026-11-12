"""L3 combiner: a learned last layer over the layer scores, in place of max(iso(cascade), iso(format-free)).

    python3 -I research/l3_stack_eval.py <v2 folder> <hi_small_sorted.csv.gz>

research/l3_eval.py found a third L2 model that ranks better on test, but the block band did not move: the isotonic maps
are step functions, so many payments share a value at the block threshold and the top of the ranking loses resolution.
Here a small XGBoost with every input constrained to raise the risk (monotone) combines the layer scores:
  inputs   l1, cascade (L1 below the gate, else the L2 mean), the pooled format-free score, and the L2 members
           (XGBoost, LightGBM and, in the "with_l3" arm, the third model of research/l3_eval.py; missing below the gate)
  fit      first half of HI-Small validation (in time); nothing from test
  judge    second half of validation, at exactly the served model's alert counts (the k highest scores per band)
Arms: current (served), stack (two L2 members), stack_with_l3 (three).
ADOPTION RULE (fixed here before any number is computed; second half of validation only):
  pick the arm with the higher block recall at the served block count k; adopt it over current only if
  S1  block recall at k >= current + 0.02 (same k, so precision rises by the same share)
  S2  PR-AUC >= current - 0.005
  S3  worst PR-AUC when every laundering payment switches format >= current worst - 0.02
Test after the decision. Results: research/out/l3_stack_eval.json; if adopted, service/models/l3_stack.json (and
l3_xgb.json for the with_l3 arm), hardening.json gains "stack" with its inputs and band thresholds.
"""
import json, os, sys, time
import numpy as np, pandas as pd, xgboost as xgb, lightgbm as lgb
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from full_hi_eval import CFG, F, L1T, VAL0, TEST0, pr  # noqa: E402
from l3_eval import train_part, PARAMS as L3P, RNG  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT, MD = os.path.join(ROOT, "research", "out"), os.path.join(ROOT, "service", "models")
H = json.load(open(os.path.join(MD, "hardening.json")))
TAIL = [f"2022/09/{d}" for d in range(11, 19)]
BANDS = ("verify", "hold", "block")
SP = {"objective": "binary:logistic", "max_depth": 3, "eta": 0.05, "subsample": 0.8, "min_child_weight": 5,
      "tree_method": "hist", "nthread": 4}
COLS = {"stack": ["l1", "cascade", "free", "l2x", "l2l"], "stack_with_l3": ["l1", "cascade", "free", "l2x", "l2l", "l3"]}


def at_k(y, s, k):
    """recall and precision of the k highest scores (ties broken by order, so exactly k alerts)"""
    top = np.argsort(-s, kind="stable")[:k]
    tp = int(y[top].sum())
    return {"flags": int(k), "tp": tp, "precision": round(tp / max(1, k), 4), "recall": round(tp / max(1, int(y.sum())), 4)}


def main():
    t0 = time.time()
    v2, sorted_src = sys.argv[1], sys.argv[2]
    d = pd.read_csv(os.path.join(v2, "hismall.csv.gz"), usecols=["row", "y"] + F, dtype=np.float32)
    assert len(d) == 5_078_345
    y = d.y.values.astype(np.int8); X = d[F].values.astype(np.float32)
    del d
    # the third L2 model, trained exactly as in research/l3_eval.py (same seed, data and parameters)
    es0 = int(VAL0 * 0.9)
    k = (y[:es0] == 1) | (RNG.random(es0) < 0.25)
    parts = [(X[:es0][k], y[:es0][k], np.where(y[:es0][k] == 1, 1.0, 4.0).astype(np.float32))]
    for name in ("himed", "hi_large"):
        parts.append(train_part(os.path.join(v2, f"{name}.csv.gz"), 0.5))
    dtr = xgb.DMatrix(np.vstack([p[0] for p in parts]), label=np.concatenate([p[1] for p in parts]), weight=np.concatenate([p[2] for p in parts]), missing=np.nan, feature_names=F)
    l3 = xgb.train(L3P, dtr, 3000, evals=[(xgb.DMatrix(X[es0:VAL0], label=y[es0:VAL0], missing=np.nan, feature_names=F), "es")], early_stopping_rounds=100, verbose_eval=False)
    l3 = l3[: l3.best_iteration + 1]
    del dtr, parts
    print("l3 rounds", l3.num_boosted_rounds(), round(time.time() - t0), "s", flush=True)

    b1 = xgb.Booster(); b1.load_model(os.path.join(MD, "l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(MD, "l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(ROOT, "ml", "exported", "l2_lgbm.txt"))
    bf = xgb.Booster(); bf.load_model(os.path.join(MD, H["model"]))
    fcols, fi = [F.index(f) for f in H["features"]], F.index("fmt_code")

    def inputs(Xm):
        dm = xgb.DMatrix(Xm, missing=np.nan, feature_names=F)
        s1, sx, sl, s3 = b1.predict(dm), bx.predict(dm), bl.predict(Xm.astype(np.float64)), l3.predict(dm)
        esc = s1 >= L1T
        free = bf.predict(xgb.DMatrix(Xm[:, fcols], missing=np.nan, feature_names=H["features"]))
        z = {"l1": s1, "cascade": np.where(esc, (sx + sl) / 2, s1), "free": free, "l2x": np.where(esc, sx, np.nan),
             "l2l": np.where(esc, sl, np.nan), "l3": np.where(esc, s3, np.nan)}
        return {c: v.astype(np.float32) for c, v in z.items()}

    def served(z):
        return np.maximum(np.interp(z["cascade"].astype(np.float64), H["iso_served"]["x"], H["iso_served"]["y"]),
                          np.interp(z["free"].astype(np.float64), H["iso_free"]["x"], H["iso_free"]["y"]))

    def matrix(z, arm):
        return xgb.DMatrix(np.column_stack([z[c] for c in COLS[arm]]), missing=np.nan, feature_names=COLS[arm])

    def attacked(Xm, yy, z, code):
        Xa = Xm[yy == 1].copy(); Xa[:, fi] = code
        za = inputs(Xa); out = {c: v.copy() for c, v in z.items()}
        for c in out:
            out[c][yy == 1] = za[c]
        return out

    mid = VAL0 + (TEST0 - VAL0) // 2
    v1, v2s = slice(VAL0, mid), slice(mid, TEST0)
    z1, z2, y1, y2 = inputs(X[v1]), inputs(X[v2s]), y[v1], y[v2s]
    stk = {}
    for arm, cols in COLS.items():
        stk[arm] = xgb.train({**SP, "monotone_constraints": "(" + ",".join("1" for _ in cols) + ")"},
                             xgb.DMatrix(np.column_stack([z1[c] for c in cols]), label=y1, missing=np.nan, feature_names=cols), 300)
    score = {"current": served, **{a: (lambda z, a=a: stk[a].predict(matrix(z, a)).astype(np.float64)) for a in COLS}}
    ks = {b: int((served(z2) >= H["thresholds"][b]).sum()) for b in BANDS}
    codes = {n: c for n, c in CFG["fmt_codes"].items() if n not in ("ACH", "Reinvestment")}
    R = {"rule": {"S1_min_block_recall_gain": 0.02, "S2_max_pr_auc_drop": 0.005, "S3_max_attack_drop": 0.02},
         "served_alert_counts_second_half": ks, "validation_second_half": {}}
    for arm, fn in score.items():
        s = fn(z2)
        att = {n: round(float(AP(y2, fn(attacked(X[v2s], y2, z2, c)))), 4) for n, c in codes.items()}
        R["validation_second_half"][arm] = {"pr_auc": round(float(AP(y2, s)), 4), "attack_worst": min(att.values()),
                                            "bands": {b: at_k(y2, s, k) for b, k in ks.items()}}
        print(arm, R["validation_second_half"][arm]["pr_auc"], R["validation_second_half"][arm]["attack_worst"], R["validation_second_half"][arm]["bands"]["block"], flush=True)
    V = R["validation_second_half"]; c = V["current"]
    best = max(COLS, key=lambda a: V[a]["bands"]["block"]["recall"])
    checks = {"S1": V[best]["bands"]["block"]["recall"] >= c["bands"]["block"]["recall"] + 0.02,
              "S2": V[best]["pr_auc"] >= c["pr_auc"] - 0.005, "S3": V[best]["attack_worst"] >= c["attack_worst"] - 0.02}
    R["decision"] = {"candidate": best, "checks": checks, "adopt": all(checks.values()), "decided_at_seconds": round(time.time() - t0),
                     "note": "decided on the second half of validation only; test scored after this line"}
    json.dump(R, open(os.path.join(OUT, "l3_stack_eval.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)

    # ------------------------------------------------------------- test, after the decision
    # band thresholds for every arm: the served model's alert rates on the whole validation part
    zv = inputs(X[VAL0:TEST0])
    rates = {b: float((served(zv) >= H["thresholds"][b]).mean()) for b in BANDS}
    thr = {a: {b: float(np.quantile(fn(zv), 1 - r)) for b, r in rates.items()} for a, fn in score.items()}
    thr["current"] = dict(H["thresholds"])
    day = pd.read_csv(sorted_src, usecols=["ts"], dtype=str).ts.str.slice(0, 10).values[TEST0:]
    keep = ~np.isin(day, TAIL)
    zt, yt = inputs(X[TEST0:]), y[TEST0:]
    zc = attacked(X[TEST0:], yt, zt, CFG["fmt_codes"]["Cheque"])

    def bands(s, t):
        out = {}
        for b, v in t.items():
            f = s >= v
            tp = int((f & (yt == 1)).sum())
            out[b] = {"flags": int(f.sum()), "tp": tp, "precision": round(tp / max(1, int(f.sum())), 4), "recall": round(tp / int(yt.sum()), 4)}
        return out
    T = {}
    for arm, fn in score.items():
        s, sc = fn(zt), fn(zc)
        T[arm] = {"all": pr(yt, s), "without_sep11_18_tail": pr(yt[keep], s[keep]), "bands": bands(s, thr[arm]),
                  "bands_under_cheque_attack": bands(sc, thr[arm]), "thresholds": thr[arm]}
        print("test", arm, T[arm]["all"]["pr_auc"], T[arm]["bands"]["block"], T[arm]["bands_under_cheque_attack"]["block"]["recall"], flush=True)
    R["test"] = T
    json.dump(R, open(os.path.join(OUT, "l3_stack_eval.json"), "w"), indent=1)
    if R["decision"]["adopt"]:
        stk[best].save_model(os.path.join(MD, "l3_stack.json"))
        if best == "stack_with_l3":
            l3.save_model(os.path.join(MD, "l3_xgb.json"))
        H["stack"] = {"model": "l3_stack.json", "inputs": COLS[best], "thresholds": thr[best]}
        H["note"] += f"; L3 combiner {best} (research/l3_stack_eval.py)"
        json.dump(H, open(os.path.join(MD, "hardening.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
