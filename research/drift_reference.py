"""Drift reference profile: what the served score and a few inputs look like on HI-Small validation.

    python3 -I research/drift_reference.py <hi_small_sorted.csv.gz> <scratch dir>
    (after cargo build --release -p rusetect --example research_dump)

Only rows before TEST0 are read (the test part is never opened). Rows before VAL0 build engine state; the validation
rows (VAL0 to TEST0) are scored by the served Rust engine (research_dump) and the served risk is rebuilt exactly as
research/evasion_validation.py does (cascade, 24 h pair noisy-OR, max(iso_served, iso_free)). Nothing is fitted.

The online monitor (crates/rusetect/src/drift.rs) bins live payments with these edges and compares shares (PSI) and
band rates. Binning: bin = number of edges <= x (numpy searchsorted side='right'); a feature's missing values (null in
the API) have their own share. Risk is rounded to 5 decimals first, as the API reports it.
  score     edges = validation deciles (data values, duplicates and an edge at the minimum dropped)
  features  <= 20 distinct values: one bin per value; else deciles as for the score
  bands     share of validation payments in each served band (verify, hold, block are exclusive)
Output: service/models/drift_reference.json (served by the API; read by GET /api/v2/drift).
"""
import json, os, subprocess, sys
import numpy as np, pandas as pd

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from structuring_defence import pair_noisy_or, hardened, H, L1T  # noqa: E402
from evasion_validation import VAL0, TEST0, COLS, BIN, minutes  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FEATURES = ["log_usd", "fmt_code", "cur_code", "cross_bank", "round_100"]  # properties of the payment itself, not of
# account state, so the profile does not depend on how much history an instance has loaded


def edges_for(x, categorical):
    x = x[~np.isnan(x)]
    if categorical:
        e = np.unique(x)
    else:
        e = np.unique(np.quantile(x, np.arange(1, 10) / 10, method="inverted_cdf"))
    return [float(v) for v in e if v > x.min()]


def shares(x, edges):
    nan = np.isnan(x)
    c = np.bincount(np.searchsorted(np.asarray(edges), x[~nan], side="right"), minlength=len(edges) + 1)
    return [round(float(v) / len(x), 6) for v in c], round(float(nan.mean()), 6)


def main():
    src, work = sys.argv[1:3]
    os.makedirs(work, exist_ok=True)
    raw = pd.read_csv(src, nrows=TEST0, dtype=str, keep_default_na=False)[COLS]  # stops before the test part
    assert len(raw) == TEST0
    t = minutes(raw.ts)
    first = int(np.searchsorted(t, t[VAL0] - 1440, side="right"))  # the 24 h pair window before validation
    out = os.path.join(work, "drift_val_dump.csv")
    p = subprocess.Popen([BIN, "/dev/stdin", out, "1.0", str(first)], stdin=subprocess.PIPE)
    raw.to_csv(p.stdin, index=False)
    p.stdin.close()
    assert p.wait() == 0
    d = pd.read_csv(out, usecols=["row", "y", "l1", "l2x", "l2l"] + sorted(set(H["features"]) | set(FEATURES)))
    r = raw.iloc[first:]
    assert len(d) == len(r) and (d.row.values == np.arange(first, TEST0)).all()
    key = (r.fb + "|" + r.fa + ">" + r.tb + "|" + r.ta).values
    s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2)
    risk = np.round(hardened(d, np.maximum(s, pair_noisy_or(t[first:], key, s))), 5)
    ev = d.row.values >= VAL0
    risk, d = risk[ev], d[ev]
    thr = H["thresholds"]
    se = edges_for(risk, False)
    R = {"script": "research/drift_reference.py", "split": "HI-Small validation (rows 3,047,007 to 4,062,675); test not read",
         "rows": int(len(risk)), "rounding": 5, "binning": "bin = number of edges <= x; missing values share apart",
         "thresholds": thr,
         "bands": {"verify": round(float(((risk >= thr["verify"]) & (risk < thr["hold"])).mean()), 6),
                   "hold": round(float(((risk >= thr["hold"]) & (risk < thr["block"])).mean()), 6),
                   "block": round(float((risk >= thr["block"]).mean()), 6)},
         "score": {"edges": se, "shares": shares(risk, se)[0]}, "features": {}}
    for f in FEATURES:
        x = d[f].values.astype(np.float64)
        e = edges_for(x, len(np.unique(x[~np.isnan(x)])) <= 20)
        sh, miss = shares(x, e)
        R["features"][f] = {"edges": e, "shares": sh, "missing": miss}
    os.remove(out)
    json.dump(R, open(os.path.join(ROOT, "service", "models", "drift_reference.json"), "w"), indent=1)
    print(json.dumps({k: v for k, v in R.items() if k != "features"}, indent=1))


if __name__ == "__main__":
    main()
