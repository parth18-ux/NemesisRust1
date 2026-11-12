"""Real against synthetic training data, card payments: controlled comparison through the same engine features.

    python3 -I research/real_vs_synthetic.py <v2 folder> <ieee_aml.csv.gz>

Real:      IEEE-CIS (Vesta, real card-not-present payments, real fraud labels; research/card_convert.py ieee)
Synthetic: Sparkov, BankSim, TabFormer (simulated card payments, the closest synthetic equivalents)
Every set is streamed through the served Rust feature code (research_dump) and split by time 60/20/20. Features: the 29
engine features without payment format and currency code (BASE in research/unified_train.py). Same learner and
hyper-parameters for every arm (unified_train.PARAMS, 500 rounds), each dataset given equal total training weight.
Arms:  real_only, synthetic_only, hybrid (real + synthetic)
Leakage controls: chronological splits; label-adjacent columns never enter; an unseen-payer test on IEEE-CIS (test
payments whose payer appeared in a training fraud are removed), because cards recur over time.

SELECTION RULE (fixed here before any number is computed; validation only):
  primary target is the real data. Pick the arm with the highest IEEE-CIS validation PR-AUC; prefer hybrid over
  real_only if its IEEE-CIS validation PR-AUC is within 0.01 of real_only AND its mean synthetic validation ROC-AUC
  is higher (better generalisation at no real-data cost).
Per arm and test set: PR-AUC, ROC-AUC, and precision, recall, F1 and false-positive rate at the threshold that
maximises F1 on that set's validation part. Results: research/out/real_vs_synthetic.json
"""
import json, os, sys
import numpy as np
from sklearn.metrics import average_precision_score as AP, roc_auc_score as AUC, precision_recall_curve

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import unified_train as U  # noqa: E402
import xgboost as xgb  # noqa: E402

REAL, SYN = ["ieee"], ["sparkov", "banksim", "tabformer"]


def f1_threshold(y, s, w):
    p, r, t = precision_recall_curve(y, s, sample_weight=w)
    f = 2 * p * r / np.maximum(p + r, 1e-12)
    i = int(np.nanargmax(f[:-1])) if len(t) else 0
    return float(t[i]) if len(t) else 0.5


def at(y, s, w, thr):
    k = s >= thr
    tp = float(w[k & (y == 1)].sum()); fp = float(w[k & (y == 0)].sum()); fn = float(w[~k & (y == 1)].sum()); tn = float(w[~k & (y == 0)].sum())
    prec = tp / max(tp + fp, 1e-12); rec = tp / max(tp + fn, 1e-12)
    return {"precision": round(prec, 4), "recall": round(rec, 4), "f1": round(2 * prec * rec / max(prec + rec, 1e-12), 4), "fpr": round(fp / max(fp + tn, 1e-12), 5)}


def main():
    folder, ieee_src = sys.argv[1], sys.argv[2]
    names = [n for n in REAL + SYN if os.path.exists(os.path.join(folder, f"{n}.csv.gz"))]
    D = {n: U.load(n, os.path.join(folder, f"{n}.csv.gz"), ieee_src if n == "ieee" else None) for n in names}
    ib = list(range(len(U.BASE)))
    arms = {"real_only": [n for n in REAL if n in D], "synthetic_only": [n for n in SYN if n in D], "hybrid": names}
    M = {a: U.train(D, ns, ib) for a, ns in arms.items()}
    score = lambda a, X: M[a].predict(xgb.DMatrix(X[:, ib], missing=np.nan))
    R = {"datasets": {n: {"rows": D[n]["n"], "train_positives": int(D[n]["tr"][1].sum()), "test_positives": int(D[n]["te"][1].sum())} for n in names},
         "arms": arms, "validation": {}}
    for a in arms:
        R["validation"][a] = {}
        for n in names:
            X, y, w, _, _ = D[n]["va"]
            s = score(a, X)
            R["validation"][a][n] = {"pr_auc": round(float(AP(y, s, sample_weight=w)), 4), "roc_auc": round(float(AUC(y, s, sample_weight=w)), 4), "f1_threshold": f1_threshold(y, s, w)}
    v = R["validation"]
    syn_roc = lambda a: float(np.mean([v[a][n]["roc_auc"] for n in SYN if n in D]))
    best = max(arms, key=lambda a: v[a]["ieee"]["pr_auc"])
    if best == "real_only" and v["hybrid"]["ieee"]["pr_auc"] >= v["real_only"]["ieee"]["pr_auc"] - 0.01 and syn_roc("hybrid") > syn_roc("real_only"):
        best = "hybrid"
    R["decision"] = {"selected": best, "ieee_validation_pr_auc": {a: v[a]["ieee"]["pr_auc"] for a in arms},
                     "mean_synthetic_validation_roc": {a: round(syn_roc(a), 4) for a in arms}, "note": "decided on validation only; test scored after this line"}
    json.dump(R, open(os.path.join(U.OUT, "real_vs_synthetic.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)
    T = {}
    for a in arms:
        T[a] = {}
        for n in names:
            X, y, w, served, _ = D[n]["te"]
            s = score(a, X)
            T[a][n] = {"pr_auc": round(float(AP(y, s, sample_weight=w)), 4), "roc_auc": round(float(AUC(y, s, sample_weight=w)), 4), **at(y, s, w, v[a][n]["f1_threshold"])}
            if n == "ieee" and "te_unseen" in D[n]:
                u = D[n]["te_unseen"]
                T[a]["ieee_unseen_payers"] = {"positives": int(y[u].sum()), "pr_auc": round(float(AP(y[u], s[u], sample_weight=w[u])), 4),
                                              "roc_auc": round(float(AUC(y[u], s[u], sample_weight=w[u])), 4), **at(y[u], s[u], w[u], v[a][n]["f1_threshold"])}
        print(a, {n: (T[a][n]["pr_auc"], T[a][n]["f1"]) for n in T[a]}, flush=True)
    X, y, w, served, _ = D["ieee"]["te"]
    T["served_cascade_zero_shot_on_ieee"] = {"pr_auc": round(float(AP(y, served, sample_weight=w)), 4), "roc_auc": round(float(AUC(y, served, sample_weight=w)), 4)}
    R["test"] = T
    json.dump(R, open(os.path.join(U.OUT, "real_vs_synthetic.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
