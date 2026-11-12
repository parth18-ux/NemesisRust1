"""Generalisation fix: replace the HI-Small-only second opinion in the hardened risk by the pooled model trained on
every payment dataset (research/unified_train.py, 29 engine features, no format and no currency code).

    python3 -I research/pooled_hardening.py <v2 folder>

Current:   risk = max(iso_served(cascade), iso_free(HI-Small format-free XGBoost))      service/models/hardening.json
Candidate: risk = max(iso_served(cascade), iso_pool(pooled 29-feature XGBoost))         isotonic map fitted on HI-Small validation
Validation parts only (rows 60% to 80% of each file in time); the pooled model never saw them.

ADOPTION RULE (fixed here before any number is computed; test parts are scored only after the decision):
  G1  HI-Small validation PR-AUC of the candidate >= current - 0.03                       (keeps home accuracy)
  G2  candidate worst HI-Small validation PR-AUC under the format attack >= 0.8 x its clean value
  G3  mean validation ROC-AUC over the other datasets: candidate - current, paired bootstrap 95% lower bound > 0
If adopted, service/models/hardening.json and the model file are replaced; band thresholds keep the cascade's
HI-Small validation alert rates. Results: research/out/pooled_hardening.json
"""
import json, os, sys
import numpy as np, pandas as pd, xgboost as xgb, lightgbm as lgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP, roc_auc_score as AUC

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "research", "out")
MD = os.path.join(ROOT, "service", "models")
CFG = json.load(open(os.path.join(MD, "cascade_config.json")))
H = json.load(open(os.path.join(MD, "hardening.json")))
F, L1T = CFG["features"], CFG["l1_threshold"]
BASE = [f for f in F if f not in ("fmt_code", "cur_code")]
RNG = np.random.default_rng(53)
NEG = 600_000


def part(path, lo, hi):
    """rows [lo*n, hi*n) of a dump; genuine rows sampled to about NEG with weights restoring the true count"""
    side = json.load(open(path + ".json")) if os.path.exists(path + ".json") else None
    cols = ["row", "y", "l1", "l2x", "l2l"] + F
    n = side["rows"] if side else max(int(c.row.max()) + 1 for c in pd.read_csv(path, usecols=["row"], chunksize=2_000_000))
    pre = side["neg_rate"] if side else 1.0
    a, b = int(n * lo), int(n * hi)
    rate = min(pre, NEG / max(1, b - a))
    keep = []
    for c in pd.read_csv(path, usecols=cols, dtype={k: (np.int64 if k == "row" else np.float32) for k in cols}, chunksize=1_000_000):
        m = (c.row.values >= a) & (c.row.values < b)
        m &= (c.y.values == 1) | (RNG.random(len(c)) < rate / pre)
        keep.append(c[m])
    d = pd.concat(keep, ignore_index=True)
    y = d.y.values.astype(int)
    w = np.where(y == 1, 1.0, 1.0 / rate)
    s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2).astype(np.float64)
    return d[F].values.astype(np.float32), y, w, s


def boot_roc_diff(sets, n=200):
    out = []
    for _ in range(n):
        diffs = []
        for y, w, a, b in sets:
            ip, ineg = np.where(y == 1)[0], np.where(y == 0)[0]
            ix = np.r_[RNG.choice(ip, len(ip)), RNG.choice(ineg, len(ineg))]
            diffs.append(AUC(y[ix], b[ix], sample_weight=w[ix]) - AUC(y[ix], a[ix], sample_weight=w[ix]))
        out.append(np.mean(diffs))
    return [round(float(v), 4) for v in np.percentile(out, [2.5, 97.5])]


def main():
    folder = sys.argv[1]
    cur = xgb.Booster(); cur.load_model(os.path.join(MD, H["model"]))
    pool = xgb.Booster(); pool.load_model(os.path.join(OUT, "unified_pooled_base_xgb.json"))
    fi = [F.index(f) for f in H["features"]]; bi = [F.index(f) for f in BASE]
    iso_s = (H["iso_served"]["x"], H["iso_served"]["y"]); iso_c = (H["iso_free"]["x"], H["iso_free"]["y"])
    raw_cur = lambda X: cur.predict(xgb.DMatrix(X[:, fi], missing=np.nan, feature_names=H["features"]))
    raw_pool = lambda X: pool.predict(xgb.DMatrix(X[:, bi], missing=np.nan))
    names = [n for n in ("hismall", "himed", "hi_large", "li_small", "li_medium", "li_large", "samld", "amlgentex", "banksim", "sparkov", "paysim", "tabformer", "ieee")
             if os.path.exists(os.path.join(folder, f"{n}.csv.gz"))]
    V = {n: part(os.path.join(folder, f"{n}.csv.gz"), 0.6, 0.8) for n in names}
    Xh, yh, wh, sh = V["hismall"]
    iso = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(raw_pool(Xh), yh, sample_weight=wh)
    iso_p = (list(map(float, iso.X_thresholds_)), list(map(float, iso.y_thresholds_)))
    cur_h = lambda X, s: np.maximum(np.interp(s, *iso_s), np.interp(raw_cur(X), *iso_c))
    cand_h = lambda X, s: np.maximum(np.interp(s, *iso_s), np.interp(raw_pool(X), *iso_p))
    # served cascade for the attack (format changes the cascade input)
    b1 = xgb.Booster(); b1.load_model(os.path.join(MD, "l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(MD, "l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(ROOT, "ml", "exported", "l2_lgbm.txt"))

    def cascade(X):
        dm = xgb.DMatrix(X, missing=np.nan, feature_names=F)
        s1 = b1.predict(dm)
        return np.where(s1 < L1T, s1, (bx.predict(dm) + bl.predict(X.astype(np.float64))) / 2)

    fmt = F.index("fmt_code")
    codes = {k: v for k, v in CFG["fmt_codes"].items() if k not in ("ACH", "Reinvestment")}

    def worst_attack(fn):
        res = {}
        for k, c in codes.items():
            Xa = Xh.copy(); Xa[yh == 1, fmt] = c
            res[k] = round(float(AP(yh, fn(Xa, cascade(Xa)), sample_weight=wh)), 4)
        return res

    R = {"rule": {"G1_max_drop": 0.03, "G2_min_ratio": 0.8, "G3": "paired bootstrap lower bound of the mean ROC gain > 0"}, "validation": {}}
    hc, hp = cur_h(Xh, sh), cand_h(Xh, sh)
    R["validation"]["hismall"] = {"current": round(float(AP(yh, hc, sample_weight=wh)), 4), "candidate": round(float(AP(yh, hp, sample_weight=wh)), 4)}
    R["validation"]["hismall_attack"] = {"current": worst_attack(cur_h), "candidate": worst_attack(cand_h)}
    sets = []
    for n in names[1:]:
        X, y, w, s = V[n]
        if y.sum() == 0:
            continue
        a, b = cur_h(X, s), cand_h(X, s)
        R["validation"][n] = {"positives": int(y.sum()), "current_roc": round(float(AUC(y, a, sample_weight=w)), 4), "candidate_roc": round(float(AUC(y, b, sample_weight=w)), 4),
                              "current_pr": round(float(AP(y, a, sample_weight=w)), 4), "candidate_pr": round(float(AP(y, b, sample_weight=w)), 4)}
        sets.append((y, w, a, b))
    gain = float(np.mean([R["validation"][n]["candidate_roc"] - R["validation"][n]["current_roc"] for n in names[1:] if n in R["validation"]]))
    lo, hi = boot_roc_diff(sets)
    c_clean = R["validation"]["hismall"]["candidate"]
    checks = {"G1": c_clean >= R["validation"]["hismall"]["current"] - 0.03,
              "G2": min(R["validation"]["hismall_attack"]["candidate"].values()) >= 0.8 * c_clean,
              "G3": lo > 0}
    R["decision"] = {"checks": checks, "adopt": all(checks.values()), "mean_other_roc_gain": round(gain, 4), "ci95": [lo, hi],
                     "note": "decided on validation parts only; test parts scored after this line"}
    json.dump(R, open(os.path.join(OUT, "pooled_hardening.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)

    # ------------------------------------------------ test parts, after the decision
    T = {}
    for n in names:
        X, y, w, s = part(os.path.join(folder, f"{n}.csv.gz"), 0.8, 1.0)
        if y.sum() == 0:
            continue
        sc = {"cascade": s, "current_hardened": cur_h(X, s), "pooled_hardened": cand_h(X, s)}
        T[n] = {"positives": int(y.sum()), **{k: {"roc_auc": round(float(AUC(y, v, sample_weight=w)), 4), "pr_auc": round(float(AP(y, v, sample_weight=w)), 4)} for k, v in sc.items()}}
        print(n, T[n], flush=True)
    R["test"] = T
    if R["decision"]["adopt"]:
        rates = {b: float((sh >= CFG[f"{b}_threshold"]).mean()) for b in ("verify", "hold", "block")}
        # thresholds at the cascade's validation alert rates, on the weighted HI-Small validation sample
        order = np.argsort(-hp); cw = np.cumsum(wh[order]) / wh.sum()
        thr = {b: float(hp[order][min(len(hp) - 1, np.searchsorted(cw, r))]) for b, r in
               {b: float(np.sum(wh[sh >= CFG[f"{b}_threshold"]]) / wh.sum()) for b in rates}.items()}
        json.dump({"note": "written by research/pooled_hardening.py; risk = max(iso_served(cascade), iso_free(pooled 29-feature XGBoost))",
                   "model": "hardening_xgb.json", "features": BASE, "iso_served": H["iso_served"], "iso_free": {"x": iso_p[0], "y": iso_p[1]},
                   "thresholds": thr, "previous": "HI-Small-only format-free model (research/hardened_eval.py)"}, open(os.path.join(MD, "hardening.json"), "w"), indent=1)
        pool.save_model(os.path.join(MD, "hardening_xgb.json"))
        R["served_thresholds"] = thr
    json.dump(R, open(os.path.join(OUT, "pooled_hardening.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
