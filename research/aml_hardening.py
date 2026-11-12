"""Second opinion trained on bank-AML data only, to remove the pooled model's regression on the large AML sets.

    python3 -I research/aml_hardening.py <v2 folder>

The served second opinion (research/pooled_hardening.py) was trained on every payment dataset, cards and wallets
included. It gained on most sets but lost on the two largest bank-AML sets (HI-Large, LI-Large), and its rule had no
per-dataset guard. RuseTect scores bank transfers, so this candidate is the same learner and settings
(research/unified_train.py: 29 features without format and currency code, 500 rounds, each dataset equal weight)
trained on the training parts of the eight bank-AML sets only.
Arms: served (pooled, every dataset) and candidate (bank AML only); risk = max(iso_served(cascade), iso(second opinion)),
the candidate's map fitted on HI-Small validation as the served one was. Validation parts only for the decision.
ADOPTION RULE (fixed here before any number is computed):
  B1  on every bank-AML validation set, candidate ROC-AUC >= served - 0.005          (no AML set gets worse)
  B2  mean bank-AML validation ROC-AUC: candidate >= served + 0.005
  B3  HI-Small validation PR-AUC: candidate >= served - 0.01
  B4  candidate worst HI-Small validation PR-AUC under the format attack >= 0.8 x its clean value
Card and wallet sets are reported, not judged: they are outside the product's scope (docs/decision.md 8b).
If adopted, service/models/hardening.json and hardening_xgb.json are replaced (thresholds at the cascade's HI-Small
validation alert rates, as before). Results: research/out/aml_hardening.json
"""
import json, os, sys
import numpy as np, xgboost as xgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP, roc_auc_score as AUC

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import unified_train as U  # noqa: E402
from pooled_hardening import part, H, F, BASE, CFG, MD, OUT, L1T  # noqa: E402

AML = ["hismall", "himed", "hi_large", "li_small", "li_medium", "li_large", "samld", "amlgentex"]
OTHER = ["banksim", "sparkov", "paysim", "tabformer", "ieee"]


def main():
    folder = sys.argv[1]
    names = [n for n in AML if os.path.exists(os.path.join(folder, f"{n}.csv.gz"))]
    assert names == AML, f"missing bank-AML dumps: {set(AML) - set(names)}"
    D = {n: U.load(n, os.path.join(folder, f"{n}.csv.gz"), None) for n in names}
    cand = U.train(D, names, list(range(len(U.BASE))))
    del D
    assert U.BASE == BASE
    served = xgb.Booster(); served.load_model(os.path.join(MD, H["model"]))
    bi = [F.index(f) for f in BASE]
    raw = {"served": lambda X: served.predict(xgb.DMatrix(X[:, bi], missing=np.nan)), "candidate": lambda X: cand.predict(xgb.DMatrix(X[:, bi], missing=np.nan))}
    iso_s = (H["iso_served"]["x"], H["iso_served"]["y"])
    Xh, yh, wh, sh = part(os.path.join(folder, "hismall.csv.gz"), 0.6, 0.8)
    iso = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(raw["candidate"](Xh), yh, sample_weight=wh)
    maps = {"served": (H["iso_free"]["x"], H["iso_free"]["y"]), "candidate": (list(map(float, iso.X_thresholds_)), list(map(float, iso.y_thresholds_)))}
    risk = lambda arm, X, s: np.maximum(np.interp(s, *iso_s), np.interp(raw[arm](X), *maps[arm]))

    import lightgbm as lgb
    b1 = xgb.Booster(); b1.load_model(os.path.join(MD, "l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(MD, "l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(os.path.dirname(MD), "..", "ml", "exported", "l2_lgbm.txt"))

    def cascade(X):
        dm = xgb.DMatrix(X, missing=np.nan, feature_names=F)
        s1 = b1.predict(dm)
        return np.where(s1 < L1T, s1, (bx.predict(dm) + bl.predict(X.astype(np.float64))) / 2)
    fmt = F.index("fmt_code")
    codes = {k: v for k, v in CFG["fmt_codes"].items() if k not in ("ACH", "Reinvestment")}

    def worst_attack(arm):
        out = []
        for c in codes.values():
            Xa = Xh.copy(); Xa[yh == 1, fmt] = c
            out.append(float(AP(yh, risk(arm, Xa, cascade(Xa)), sample_weight=wh)))
        return round(min(out), 4)

    R = {"rule": {"B1_max_drop_any_aml_set": 0.005, "B2_min_mean_aml_gain": 0.005, "B3_max_hismall_pr_drop": 0.01, "B4_min_attack_ratio": 0.8},
         "trained_on": names, "validation": {}}
    for n in names + [o for o in OTHER if os.path.exists(os.path.join(folder, f"{o}.csv.gz"))]:
        X, y, w, s = (Xh, yh, wh, sh) if n == "hismall" else part(os.path.join(folder, f"{n}.csv.gz"), 0.6, 0.8)
        if y.sum() == 0:
            continue
        R["validation"][n] = {a: {"roc_auc": round(float(AUC(y, risk(a, X, s), sample_weight=w)), 4), "pr_auc": round(float(AP(y, risk(a, X, s), sample_weight=w)), 4)} for a in raw}
        print(n, R["validation"][n], flush=True)
    V = R["validation"]
    roc = lambda n, a: V[n][a]["roc_auc"]
    clean = V["hismall"]["candidate"]["pr_auc"]
    att = worst_attack("candidate")
    checks = {"B1": all(roc(n, "candidate") >= roc(n, "served") - 0.005 for n in names),
              "B2": float(np.mean([roc(n, "candidate") for n in names])) >= float(np.mean([roc(n, "served") for n in names])) + 0.005,
              "B3": clean >= V["hismall"]["served"]["pr_auc"] - 0.01, "B4": att >= 0.8 * clean}
    R["decision"] = {"checks": checks, "adopt": all(checks.values()), "candidate_worst_attack_pr_auc": att,
                     "mean_aml_roc": {a: round(float(np.mean([roc(n, a) for n in names])), 4) for a in raw},
                     "note": "decided on validation parts only; test parts scored after this line"}
    json.dump(R, open(os.path.join(OUT, "aml_hardening.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)

    T = {}
    for n in R["validation"]:
        X, y, w, s = part(os.path.join(folder, f"{n}.csv.gz"), 0.8, 1.0)
        if y.sum() == 0:
            continue
        T[n] = {a: {"roc_auc": round(float(AUC(y, risk(a, X, s), sample_weight=w)), 4), "pr_auc": round(float(AP(y, risk(a, X, s), sample_weight=w)), 4)} for a in raw}
        print("test", n, T[n], flush=True)
    R["test"] = T
    if R["decision"]["adopt"]:
        hp = risk("candidate", Xh, sh)
        order = np.argsort(-hp); cw = np.cumsum(wh[order]) / wh.sum()
        thr = {b: float(hp[order][min(len(hp) - 1, np.searchsorted(cw, float(np.sum(wh[sh >= CFG[f"{b}_threshold"]]) / wh.sum())))]) for b in ("verify", "hold", "block")}
        json.dump({"note": "written by research/aml_hardening.py; risk = max(iso_served(cascade), iso_free(bank-AML 29-feature XGBoost))",
                   "model": "hardening_xgb.json", "features": BASE, "iso_served": H["iso_served"], "iso_free": {"x": maps["candidate"][0], "y": maps["candidate"][1]},
                   "thresholds": thr, "previous": "pooled every-dataset model (research/pooled_hardening.py)"}, open(os.path.join(MD, "hardening.json"), "w"), indent=1)
        cand.save_model(os.path.join(MD, "hardening_xgb.json"))
        R["served_thresholds"] = thr
    json.dump(R, open(os.path.join(OUT, "aml_hardening.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
