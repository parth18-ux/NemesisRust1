"""Does the served hardened risk transfer better than the cascade alone? Evaluation only (no model is fitted here).

    python3 -I research/hardened_transfer.py <v2 folder>

For every research_dump file in the folder (bank AML, cards, wallets), the test part is the last 20% of rows in time.
Scores: the served cascade, and the hardened risk max(iso_served(cascade), iso_free(format-free XGBoost)) with the
maps fitted on HI-Small validation (service/models/hardening.json). Files pre-sampled while streaming carry a sidecar
<file>.json; genuine rows are weighted back by 1/neg_rate. Results: research/out/hardened_transfer.json
"""
import json, os, sys
import numpy as np, pandas as pd, xgboost as xgb
from sklearn.metrics import average_precision_score as AP, roc_auc_score as AUC

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CFG = json.load(open(os.path.join(ROOT, "service", "models", "cascade_config.json")))
H = json.load(open(os.path.join(ROOT, "service", "models", "hardening.json")))
L1T = CFG["l1_threshold"]
NAMES = {"hismall": "HI-Small (in domain)", "li_small": "LI-Small (in domain: the served models also trained on LI-Small)",
         "himed": "HI-Medium", "li_medium": "LI-Medium", "hi_large": "HI-Large", "li_large": "LI-Large", "samld": "SAML-D",
         "amlgentex": "AMLgentex Sweden difficult", "banksim": "BankSim (cards)", "sparkov": "Sparkov (cards)",
         "paysim": "PaySim (mobile wallets)", "tabformer": "TabFormer (cards)"}


def main():
    folder = sys.argv[1]
    bf = xgb.Booster(); bf.load_model(os.path.join(ROOT, "service", "models", H["model"]))
    out = {}
    for key, label in NAMES.items():
        path = os.path.join(folder, f"{key}.csv.gz")
        if not os.path.exists(path):
            continue
        side = json.load(open(path + ".json")) if os.path.exists(path + ".json") else None
        cols = ["row", "y", "l1", "l2x", "l2l"] + H["features"]
        if side:
            n = side["rows"]
        else:  # first pass for the row count, so only the test part is held in memory
            n = max(int(c.row.max()) + 1 for c in pd.read_csv(path, usecols=["row"], chunksize=2_000_000))
        te = pd.concat([c[c.row.values >= int(n * 0.8)] for c in pd.read_csv(path, usecols=cols, dtype={c: (np.int64 if c == "row" else np.float32) for c in cols}, chunksize=1_000_000)], ignore_index=True)
        y = te.y.values.astype(int)
        w = np.where(y == 1, 1.0, 1.0 / side["neg_rate"]) if side else np.ones(len(y))
        s = np.where(te.l1.values < L1T, te.l1.values, (te.l2x.values + te.l2l.values) / 2).astype(np.float64)
        free = np.interp(bf.predict(xgb.DMatrix(te[H["features"]].values, missing=np.nan, feature_names=H["features"])), H["iso_free"]["x"], H["iso_free"]["y"])
        h = np.maximum(np.interp(s, H["iso_served"]["x"], H["iso_served"]["y"]), free)
        if y.sum() == 0:
            continue
        m = lambda sc: {"roc_auc": round(float(AUC(y, sc, sample_weight=w)), 4), "pr_auc": round(float(AP(y, sc, sample_weight=w)), 4)}
        out[key] = {"label": label, "test_rows": int(round(w.sum())), "test_positives": int(y.sum()), "cascade": m(s), "hardened": m(h)}
        print(key, out[key], flush=True)
    json.dump(out, open(os.path.join(ROOT, "research", "out", "hardened_transfer.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
