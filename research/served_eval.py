"""The served score on HI-Small test, measured the way a bank would check it. Evaluation only: nothing is fitted.

    python3 -I research/served_eval.py <hismall research_dump csv.gz> <hi_small_sorted.csv.gz>

Answers four objections to research/out/hardened_eval_served.json:
  workload    band thresholds are quantiles of step functions (isotonic maps), so whole plateaus cross them and the
              served bands raise more alerts than the cascade's budget. Reported: alerts per band at the served
              thresholds against the design budget (the cascade's validation alert rates), and recall / precision at
              exactly that budget (the k highest scores, ties broken by payment order) for cascade and served score.
  tail        the last eight test days (Sep 11 to 18) hold a third of test laundering; every number is also given
              without them.
  intervals   laundering comes in linked chains, so payments are not independent: intervals resample whole days
              (block bootstrap), and the served-minus-cascade difference is paired.
  calibration expected calibration error of the served risk (20 equal-count bins) and mean risk against the observed rate.
Results: research/out/served_eval.json
"""
import json, os, sys
import numpy as np, pandas as pd, xgboost as xgb
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from full_hi_eval import CFG, F, L1T, VAL0, TEST0  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
H = json.load(open(os.path.join(ROOT, "service", "models", "hardening.json")))
TAIL = [f"2022/09/{d}" for d in range(11, 19)]
BANDS = ("verify", "hold", "block")
DESIGN = {"verify": CFG["verify_threshold"], "hold": CFG["hold_threshold"], "block": CFG["block_threshold"]}
B = 500


def at_thr(y, s, t):
    f = s >= t
    tp = int((f & (y == 1)).sum())
    return {"flags": int(f.sum()), "rate": round(float(f.mean()), 6), "precision": round(tp / max(1, int(f.sum())), 4), "recall": round(tp / max(1, int(y.sum())), 4)}


def at_k(y, s, k):
    top = np.argsort(-s, kind="stable")[:k]
    tp = int(y[top].sum())
    return {"flags": int(k), "precision": round(tp / max(1, k), 4), "recall": round(tp / max(1, int(y.sum())), 4)}


def ece(y, p, bins=20):
    order = np.argsort(p, kind="stable")
    e = 0.0
    for part in np.array_split(order, bins):
        e += len(part) / len(p) * abs(float(p[part].mean()) - float(y[part].mean()))
    return round(e, 6)


def day_bootstrap(y, a, b, day, rng):
    """95% intervals of PR-AUC for a, b and the paired difference b - a, resampling whole days"""
    days = np.unique(day)
    idx = {d: np.flatnonzero(day == d) for d in days}
    ra, rb, rd = [], [], []
    for _ in range(B):
        pick = np.concatenate([idx[d] for d in rng.choice(days, len(days))])
        if y[pick].sum() == 0:
            continue
        x, z = AP(y[pick], a[pick]), AP(y[pick], b[pick])
        ra.append(x); rb.append(z); rd.append(z - x)
    q = lambda v: [round(float(np.quantile(v, 0.025)), 4), round(float(np.quantile(v, 0.975)), 4)]
    return {"cascade": q(ra), "served": q(rb), "served_minus_cascade": q(rd), "resamples": len(rd), "days": int(len(days))}


def main():
    d = pd.read_csv(sys.argv[1], usecols=["y", "l1", "l2x", "l2l"] + F, dtype=np.float32, skiprows=range(1, VAL0 + 1))
    day = pd.read_csv(sys.argv[2], usecols=["ts"], dtype=str).ts.str.slice(0, 10).values[VAL0:]
    assert len(d) == len(day) == 5_078_345 - VAL0
    y = d.y.values.astype(np.int8)
    cas = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2).astype(np.float64)
    bf = xgb.Booster(); bf.load_model(os.path.join(ROOT, "service", "models", H["model"]))
    free = np.interp(bf.predict(xgb.DMatrix(d[H["features"]].values, missing=np.nan, feature_names=H["features"])), H["iso_free"]["x"], H["iso_free"]["y"])
    served = np.maximum(np.interp(cas, H["iso_served"]["x"], H["iso_served"]["y"]), free)
    va, te = slice(0, TEST0 - VAL0), slice(TEST0 - VAL0, None)
    budget = {b: float((cas[va] >= t).mean()) for b, t in DESIGN.items()}  # the cascade's validation alert rates
    R = {"design_budget_validation_rates": budget,
         "validation_rates_at_served_thresholds": {b: round(float((served[va] >= H["thresholds"][b]).mean()), 6) for b in BANDS}}
    keep = ~np.isin(day[te], TAIL)
    rng = np.random.default_rng(41)
    for name, m in (("all", np.ones(int(keep.size), bool)), ("without_sep11_18_tail", keep)):
        yt, ct, st, dt = y[te][m], cas[te][m], served[te][m], day[te][m]
        k = {b: int(round(r * len(yt))) for b, r in budget.items()}
        R[name] = {
            "rows": int(len(yt)), "positives": int(yt.sum()),
            "pr_auc": {"cascade": round(float(AP(yt, ct)), 4), "served": round(float(AP(yt, st)), 4)},
            "pr_auc_ci95_day_bootstrap": day_bootstrap(yt, ct, st, dt, rng),
            "bands_at_thresholds": {"cascade": {b: at_thr(yt, ct, t) for b, t in DESIGN.items()}, "served": {b: at_thr(yt, st, H["thresholds"][b]) for b in BANDS}},
            "bands_at_design_budget": {"k": k, "cascade": {b: at_k(yt, ct, k[b]) for b in BANDS}, "served": {b: at_k(yt, st, k[b]) for b in BANDS}},
            "calibration": {"ece_20_equal_count_bins": ece(yt, st), "mean_risk": round(float(st.mean()), 6), "observed_rate": round(float(yt.mean()), 6)},
        }
        print(name, json.dumps({k2: R[name][k2] for k2 in ("pr_auc", "pr_auc_ci95_day_bootstrap")}), flush=True)
        print("  block at thresholds", R[name]["bands_at_thresholds"]["served"]["block"], "at budget", R[name]["bands_at_design_budget"]["served"]["block"], "cascade at budget", R[name]["bands_at_design_budget"]["cascade"]["block"], flush=True)
    json.dump(R, open(os.path.join(ROOT, "research", "out", "served_eval.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
