"""Pair-level aggregation against structuring (one laundering sum split into smaller payments).

    python3 -I research/structuring_defence.py

Each payment is scored alone, so four quarter-size parts each look less risky than the whole. The defence keeps, for
every payer-payee pair, a noisy-OR of the served scores of its payments in the last 24 hours:
    pair_risk = 1 - prod(1 - s_i)
and a payment's decision uses max(s, pair_risk). No parameter is fitted: the band thresholds of each scale are reused.
It is tested on two scales: the cascade alone, and the format-hardened risk the engine serves (research/hardened_eval.py,
service/models/hardening.json). The decision that counts is on the served (hardened) scale.
On the hardened scale the noisy-OR of calibrated risks barely moves (isotonic maps small cascade risks to near zero),
so a third variant was added AFTER seeing that result: aggregate the cascade risks, then apply the hardening map.
It uses no fitted parameter, but it was chosen after looking at this slice, so its numbers are exploratory.

Data: the HI-Small demo slice streams of research/make_streams.py (base, split2, split4, split4_spread) and their dumps
from the served Rust engine. Evaluation window: rows after the first 100,000 (the warm-up), as in the other attack tests.
ADOPTION RULE (fixed before running):
  S1  on the clean stream the number of block-band payments rises by at most 25%
  S2  split4_spread recall (a laundering payment counts as caught if any part is blocked) at least doubles over served
Results: research/out/structuring_defence.json
"""
import json, os
from collections import deque
import numpy as np, pandas as pd

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ST = os.path.join(ROOT, "research", "out", "streams")
CFG = json.load(open(os.path.join(ROOT, "service", "models", "cascade_config.json")))
L1T, BANDS = CFG["l1_threshold"], {"verify": CFG["verify_threshold"], "hold": CFG["hold_threshold"], "block": CFG["block_threshold"]}
WARM = 100_000


def pair_noisy_or(t, key, s, window=1440):
    """causal noisy-OR of s over the same key within the last `window` minutes, current payment included.
    Linear: a running sum of log(1 - s) per key, so busy payers do not make it quadratic."""
    out = np.empty(len(s)); q = {}
    lg = np.log(np.clip(1 - np.asarray(s, np.float64), 1e-300, None))
    for i in range(len(s)):
        e = q.get(key[i])
        if e is None:
            e = q[key[i]] = [deque(), 0.0]
        dq = e[0]
        while dq and dq[0][0] <= t[i] - window:
            e[1] -= dq.popleft()[1]
        if not dq:
            e[1] = 0.0
        dq.append((t[i], lg[i])); e[1] += lg[i]
        out[i] = 1 - np.exp(e[1])
    return out


H = json.load(open(os.path.join(ROOT, "service", "models", "hardening.json")))
SCALES = {"cascade": BANDS, "hardened": H["thresholds"], "hardened_pre_calibration": H["thresholds"]}


def hardened(d, s):
    import xgboost as xgb
    b = xgb.Booster(); b.load_model(os.path.join(ROOT, "service", "models", H["model"]))
    free = np.interp(b.predict(xgb.DMatrix(d[H["features"]].values.astype(np.float32), missing=np.nan, feature_names=H["features"])), H["iso_free"]["x"], H["iso_free"]["y"])
    return np.maximum(np.interp(s, H["iso_served"]["x"], H["iso_served"]["y"]), free)


def load(name):
    raw = pd.read_csv(os.path.join(ST, f"{name}.csv.gz"), usecols=["ts", "fb", "fa", "tb", "ta"], dtype=str)
    d = pd.read_csv(os.path.join(ST, f"{name}_dump.csv"), usecols=["y", "l1", "l2x", "l2l"] + H["features"])
    assert len(raw) == len(d)
    t = (pd.to_datetime(raw.ts, format="%Y/%m/%d %H:%M").astype("int64") // 60_000_000_000).values
    key = (raw.fb + "|" + raw.fa + ">" + raw.tb + "|" + raw.ta).values
    s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2)
    o = np.loadtxt(os.path.join(ST, f"{name}.origin"), dtype=np.int64)
    h, agg = hardened(d, s), np.maximum(s, pair_noisy_or(t, key, s))
    # pre_calibration: the pair noisy-OR of cascade risks goes through the hardening map (chosen after the first result)
    return {"cascade": (s, agg), "hardened": (h, np.maximum(h, pair_noisy_or(t, key, h))), "hardened_pre_calibration": (h, hardened(d, agg)),
            "y": d.y.values, "o": o, "ev": o >= WARM}


def main():
    R = {"rule": {"S1_max_block_increase": 0.25, "S2_min_recall_ratio": 2.0}, "scales": {}}
    V = {name: load(name) for name in ("base", "split2", "split4", "split4_spread")}
    for scale, bands in SCALES.items():
        out = {}
        for name, v in V.items():
            ev = v["ev"]; r = {}
            for lab, sc_all in zip(("per_payment", "pair_aggregated"), v[scale]):
                sc, y, o = sc_all[ev], v["y"][ev], v["o"][ev]
                row = {}
                for b, thr in bands.items():
                    caught = pd.Series(sc[y == 1] >= thr).groupby(o[y == 1]).any()
                    row[b] = {"payments_flagged": int((sc >= thr).sum()), "laundering_recall_any_part": round(float(caught.mean()), 4),
                              "flagged_genuine": int(((sc >= thr) & (y == 0)).sum())}
                r[lab] = row
            r["laundering_payments"] = int(pd.Series(v["o"][ev & (v["y"] == 1)]).nunique())
            out[name] = r
        b, sp = out["base"], out["split4_spread"]
        inc = b["pair_aggregated"]["block"]["payments_flagged"] / max(1, b["per_payment"]["block"]["payments_flagged"]) - 1
        s_rec, a_rec = sp["per_payment"]["block"]["laundering_recall_any_part"], sp["pair_aggregated"]["block"]["laundering_recall_any_part"]
        checks = {"S1": inc <= 0.25, "S2": a_rec >= 2.0 * s_rec and a_rec > 0}
        R["scales"][scale] = {"variants": out, "decision": {"checks": checks, "adopt": all(checks.values()), "values": {
            "clean_block_increase": round(inc, 4), "split4_spread_block_recall_per_payment": s_rec, "split4_spread_block_recall_aggregated": a_rec}}}
        print(scale, json.dumps(R["scales"][scale]["decision"]))
        for n, r in out.items():
            print(" ", n, {k: (r[k]["block"]["laundering_recall_any_part"], r[k]["block"]["payments_flagged"]) for k in ("per_payment", "pair_aggregated")})
    R["decision"] = dict(R["scales"]["hardened"]["decision"], note="the pre-registered hardened variant; see hardened_pre_calibration (post hoc)")
    json.dump(R, open(os.path.join(ROOT, "research", "out", "structuring_defence.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
