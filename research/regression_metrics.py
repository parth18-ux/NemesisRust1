"""MAE, RMSE and R² of the served risk scores against the 0/1 laundering label (HI-Small test).

    python3 -I research/regression_metrics.py <hismall research_dump csv.gz>

For a yes/no target these are calibration measures: RMSE² is the Brier score, and R² = 1 - Brier / var(y) is the Brier
skill score (0 = no better than always predicting the base rate). Results: research/out/regression_metrics.json
"""
import json, os, sys
import numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from structuring_defence import hardened, H, L1T  # noqa: E402
from full_hi_eval import TEST0  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
d = pd.read_csv(sys.argv[1], usecols=["y", "l1", "l2x", "l2l"] + H["features"], dtype=np.float32, skiprows=range(1, TEST0 + 1))
y = d.y.values.astype(float)
s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2).astype(float)
out = {"rows": len(y), "positives": int(y.sum())}
for name, p in (("cascade_uncalibrated", s), ("served_hardened_calibrated", hardened(d, s)), ("base_rate_constant", np.full(len(y), y.mean()))):
    e = p - y
    mse = float(np.mean(e ** 2))
    out[name] = {"MAE": round(float(np.mean(np.abs(e))), 6), "RMSE": round(mse ** 0.5, 6), "R2": round(1 - mse / float(np.var(y)), 4)}
json.dump(out, open(os.path.join(ROOT, "research", "out", "regression_metrics.json"), "w"), indent=1)
print(json.dumps(out, indent=1))
