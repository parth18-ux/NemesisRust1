"""Account-level aggregation: widen the 24 h window from a payer->payee pair to all of a payer's payments.

    python3 -I research/account_aggregation.py <hismall research_dump csv.gz> <hi_small_sorted.csv.gz>

Splitting one sum over new mule accounts defeats the pair window (research/out/evasion_suite.json, split4_new_payees),
while a payer window catches it but raises genuine alerts a hundredfold at the pair thresholds. So the payer-window score
gets its own band thresholds, fitted on HI-Small validation at the cascade's validation alert rates (same budget).
  A = hardened(max(cascade, payer noisy-OR of cascade risks over 24 h))
  P = hardened(max(cascade, pair noisy-OR)), the served score
ADOPTION RULE (fixed here before any number is computed):
  A1  HI-Small validation PR-AUC of A >= P - 0.03
  A2  mean block recall over the evasion-suite attacks (demo window, any part caught) of A >= 1.5 x that of P,
      with A's validation-fitted thresholds
  A3  clean demo-window block alerts of A <= 1.5 x those of P
Results: research/out/account_aggregation.json; if adopted, hardening.json gains "window": "payer" and A's thresholds.
"""
import json, os, sys
import numpy as np, pandas as pd

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from structuring_defence import pair_noisy_or, hardened, H, L1T  # noqa: E402
from full_hi_eval import VAL0, TEST0, CFG  # noqa: E402
from sklearn.metrics import average_precision_score as AP

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ST = os.path.join(ROOT, "research", "out", "streams")
ATTACKS = ["cheque", "amount_minus10", "amount_jitter", "delay_6h", "split4_spread", "split4_new_payees", "cheque_split4"]


def keyed(raw):
    t = (pd.to_datetime(raw.ts, format="%Y/%m/%d %H:%M").astype("int64") // 60_000_000_000).values
    return t, (raw.fb + "|" + raw.fa + ">" + raw.tb + "|" + raw.ta).values, (raw.fb + "|" + raw.fa).values


def thresholds(score, ref, va):
    """band thresholds giving score the same validation alert rates as the cascade (ref)"""
    out = {}
    for b in ("verify", "hold", "block"):
        rate = float((ref[va] >= CFG[f"{b}_threshold"]).mean())
        out[b] = float(np.quantile(score[va], 1 - rate))
    return out


def main():
    d = pd.read_csv(sys.argv[1], usecols=["y", "l1", "l2x", "l2l"] + H["features"], dtype=np.float32)
    raw = pd.read_csv(sys.argv[2], usecols=["ts", "fb", "fa", "tb", "ta"], dtype=str)
    assert len(d) == len(raw)
    t, pair, payer = keyed(raw)
    s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2).astype(np.float64)
    y = d.y.values.astype(int)
    P = hardened(d, np.maximum(s, pair_noisy_or(t, pair, s)))
    A = hardened(d, np.maximum(s, pair_noisy_or(t, payer, s)))
    va = np.zeros(len(y), bool); va[VAL0:TEST0] = True
    thr_a = thresholds(A, s, va)
    R = {"rule": {"A1_max_drop": 0.03, "A2_min_ratio": 1.5, "A3_max_alert_ratio": 1.5},
         "validation_pr_auc": {"pair_served": round(float(AP(y[va], P[va])), 4), "payer": round(float(AP(y[va], A[va])), 4)},
         "payer_thresholds": thr_a}
    # demo-window attacks with each score's own thresholds
    rec = {"pair_served": [], "payer": []}; alerts = {}
    for name in ["base"] + ATTACKS:
        stem = "base" if name == "base" else f"ev_{name}"
        r2 = pd.read_csv(os.path.join(ST, f"{stem}.csv.gz"), usecols=["ts", "fb", "fa", "tb", "ta"], dtype=str)
        d2 = pd.read_csv(os.path.join(ST, f"{stem}_dump.csv"), usecols=["y", "l1", "l2x", "l2l"] + H["features"])
        o = np.loadtxt(os.path.join(ST, f"{stem}.origin"), dtype=np.int64)
        t2, k2, p2 = keyed(r2)
        s2 = np.where(d2.l1.values < L1T, d2.l1.values, (d2.l2x.values + d2.l2l.values) / 2)
        sc = {"pair_served": (hardened(d2, np.maximum(s2, pair_noisy_or(t2, k2, s2))), H["thresholds"]["block"]),
              "payer": (hardened(d2, np.maximum(s2, pair_noisy_or(t2, p2, s2))), thr_a["block"])}
        ev, yy = o >= 100_000, d2.y.values
        row = {}
        for lab, (v, b) in sc.items():
            r = float(pd.Series((v >= b)[ev & (yy == 1)]).groupby(o[ev & (yy == 1)]).any().mean())
            row[lab] = {"block_recall": round(r, 4), "block_alerts": int((v[ev] >= b).sum())}
            if name != "base":
                rec[lab].append(r)
        R.setdefault("demo_window", {})[name] = row
        if name == "base":
            alerts = {k: v["block_alerts"] for k, v in row.items()}
        print(name, row, flush=True)
    mp, ma = float(np.mean(rec["pair_served"])), float(np.mean(rec["payer"]))
    checks = {"A1": R["validation_pr_auc"]["payer"] >= R["validation_pr_auc"]["pair_served"] - 0.03,
              "A2": ma >= 1.5 * mp, "A3": alerts["payer"] <= 1.5 * max(1, alerts["pair_served"])}
    R["decision"] = {"checks": checks, "adopt": all(checks.values()), "mean_attack_block_recall": {"pair_served": round(mp, 4), "payer": round(ma, 4)},
                     "clean_block_alerts": alerts}
    json.dump(R, open(os.path.join(ROOT, "research", "out", "account_aggregation.json"), "w"), indent=1)
    print(json.dumps(R["decision"]))
    if R["decision"]["adopt"]:
        h = json.load(open(os.path.join(ROOT, "service", "models", "hardening.json")))
        h["window"] = "payer"; h["thresholds"] = thr_a
        json.dump(h, open(os.path.join(ROOT, "service", "models", "hardening.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
