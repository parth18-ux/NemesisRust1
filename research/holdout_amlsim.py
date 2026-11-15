"""Fresh holdout on IBM AMLSim: the served score, scored once. Pre-registered in docs/HOLDOUT_PREREG.md.

    python3 -I research/holdout_amlsim.py <AMLSim tx_log.csv> <scratch dir> <provenance.json>
    (after cargo build --release -p rusetect --example research_dump)

This file is committed with the pre-registration, before any AMLSim data exists. Nothing is fitted here: models, maps,
thresholds and features are the served ones (service/models/). Mapping and rule: docs/HOLDOUT_PREREG.md.
Results: research/out/holdout_amlsim.json
"""
import json, os, subprocess, sys
import numpy as np, pandas as pd
from sklearn.metrics import average_precision_score as AP, roc_auc_score as AUC

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from structuring_defence import pair_noisy_or, hardened, H, L1T, BANDS  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "examples", "research_dump")
STEPS, TEST_FROM = 720, 576  # total_steps of the AMLSim config; test = the last 20% of steps
BASE = pd.Timestamp("2017-01-01")  # AMLSim base_date
BANK = "AMLSIM"  # AMLSim has one bank in this config
B = 500


def convert(src, dst):
    d = pd.read_csv(src, usecols=["step", "type", "amount", "nameOrig", "nameDest", "isSAR"], dtype={"nameOrig": str, "nameDest": str})
    d = d.sort_values("step", kind="stable").reset_index(drop=True)
    amt = d.amount.map(lambda x: f"{x:.2f}")
    ts = (BASE + pd.to_timedelta(d.step, unit="D")).dt.strftime("%Y/%m/%d %H:%M")
    out = pd.DataFrame({"ts": ts, "fb": BANK, "fa": d.nameOrig, "tb": BANK, "ta": d.nameDest, "amt_recv": amt, "cur_recv": "US Dollar",
                        "amt_paid": amt, "cur_paid": "US Dollar", "fmt": np.where(d.type.str.startswith("CASH"), "Cash", "ACH"),
                        "y": d.isSAR.astype(int)})
    out.to_csv(dst, index=False)
    return d, out


def at_thr(y, s, t):
    f = s >= t
    tp = int((f & (y == 1)).sum())
    return {"flags": int(f.sum()), "alerts_per_1000": round(1000 * float(f.mean()), 3),
            "precision": round(tp / max(1, int(f.sum())), 4), "recall": round(tp / max(1, int(y.sum())), 4)}


def bands(y, s, thr):
    r = {b: at_thr(y, s, thr[b]) for b in ("verify", "hold", "block")}
    r["hold_or_block"] = at_thr(y, s, thr["hold"])
    return r


def boot(y, served, cas, amount, hold_thr, day, rng):
    """95% intervals from whole-day (step) resampling of the test window"""
    days = np.unique(day)
    idx = {d: np.flatnonzero(day == d) for d in days}
    R = {k: [] for k in ("served_pr_auc", "served_roc_auc", "cascade_pr_auc", "cascade_roc_auc", "amount_pr_auc",
                         "served_minus_best_baseline", "served_hold_or_block_precision")}
    for _ in range(B):
        p = np.concatenate([idx[d] for d in rng.choice(days, len(days))])
        yp = y[p]
        if yp.sum() == 0 or yp.sum() == len(yp):
            continue
        sa, aa = AP(yp, served[p]), AP(yp, amount[p])
        R["served_pr_auc"].append(sa); R["amount_pr_auc"].append(aa)
        R["served_roc_auc"].append(AUC(yp, served[p]))
        R["cascade_pr_auc"].append(AP(yp, cas[p])); R["cascade_roc_auc"].append(AUC(yp, cas[p]))
        R["served_minus_best_baseline"].append(sa - max(aa, float(yp.mean())))
        f = served[p] >= hold_thr
        R["served_hold_or_block_precision"].append(float(yp[f].sum()) / max(1, int(f.sum())))
    q = {k: [round(float(np.quantile(v, 0.025)), 4), round(float(np.quantile(v, 0.975)), 4)] for k, v in R.items()}
    q["resamples"], q["days"] = len(R["served_pr_auc"]), int(len(days))
    return q


def main():
    src, work, prov = sys.argv[1:4]
    os.makedirs(work, exist_ok=True)
    raw, conv = convert(src, os.path.join(work, "amlsim_aml.csv"))
    dump = os.path.join(work, "amlsim_dump.csv")
    subprocess.run([BIN, os.path.join(work, "amlsim_aml.csv"), dump], check=True)
    d = pd.read_csv(dump, usecols=["row", "y", "usd", "l1", "l2x", "l2l"] + H["features"])
    assert len(d) == len(raw) and (d.y.values.astype(int) == raw.isSAR.values.astype(int)).all()
    t = raw.step.values.astype(np.int64) * 1440
    key = (conv.fb + "|" + conv.fa + ">" + conv.tb + "|" + conv.ta).values
    cas = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2).astype(np.float64)
    served = hardened(d, np.maximum(cas, pair_noisy_or(t, key, cas)))
    te = raw.step.values >= TEST_FROM
    y, s, c, a, day = d.y.values[te].astype(int), served[te], cas[te], d.usd.values[te].astype(np.float64), raw.step.values[te]
    prev = float(y.mean())
    R = {"script": "research/holdout_amlsim.py", "prereg": "docs/HOLDOUT_PREREG.md", "provenance": json.load(open(prov)),
         "rows_total": int(len(raw)), "positives_total": int(raw.isSAR.sum()),
         "rows_by_type": {k: int(v) for k, v in raw.type.value_counts().items()},
         "positives_by_type": {k: int(v) for k, v in raw[raw.isSAR == 1].type.value_counts().items()},
         "test": {"steps": [TEST_FROM, STEPS - 1], "rows": int(te.sum()), "positives": int(y.sum()), "prevalence": round(prev, 6)},
         "served": {"pr_auc": round(float(AP(y, s)), 4), "roc_auc": round(float(AUC(y, s)), 4), "thresholds": H["thresholds"], "bands": bands(y, s, H["thresholds"])},
         "cascade": {"pr_auc": round(float(AP(y, c)), 4), "roc_auc": round(float(AUC(y, c)), 4), "thresholds": BANDS, "bands": bands(y, c, BANDS)},
         "baseline_amount": {"pr_auc": round(float(AP(y, a)), 4), "roc_auc": round(float(AUC(y, a)), 4)},
         "ci95_day_bootstrap": boot(y, s, c, a, H["thresholds"]["hold"], day, np.random.default_rng(41))}
    ci = R["ci95_day_bootstrap"]
    h1 = ci["served_minus_best_baseline"][0] > 0
    h2 = R["served"]["bands"]["hold_or_block"]["flags"] > 0 and ci["served_hold_or_block_precision"][0] > prev
    R["rule"] = {"H1_adds_value": bool(h1), "H2_bands_mean_something": bool(h2),
                 "verdict": "transfers" if h1 and h2 else ("partial transfer" if h1 or h2 else "does not transfer")}
    json.dump(R, open(os.path.join(ROOT, "research", "out", "holdout_amlsim.json"), "w"), indent=1)
    print(json.dumps(R, indent=1))


if __name__ == "__main__":
    main()
