# RuseTect — Competition Presentation and Demo

## Opening (30 seconds)
"RuseTect is an explainable scam-payment and money-mule detection system. It connects suspicious messages, account history, transaction patterns and review workflows to recommend proportional interventions."

## Suggested five-minute presentation
**0:00–0:40 — Problem:** Scam payments and mule networks are hard to catch with one transaction rule.
**0:40–1:30 — Architecture:** Explain the fast gatekeeper, XGBoost/LightGBM scoring, 31 historical features, message/session evidence, and analyst controls.
**1:30–2:20 — Machine learning:** Explain temporal train/test separation, PR-AUC for rare fraud, feature attribution, and why accuracy alone can mislead.
**2:20–3:20 — Live demo:** Open https://nemesis-rust1.vercel.app, show a labelled demo payment, its risk reasons and investigator workflow. Have screenshots ready if the site is unavailable.
**3:20–4:10 — Evaluation:** Distinguish the existing benchmark from the new contest. The README reports HI-Small ROC-AUC 0.985 and PR-AUC 0.711; cite the exact evaluation and mention synthetic/public data limitations. Never claim these are competition scores.
**4:10–5:00 — Limitations and close:** Mention distribution shift, payment-format shortcuts, calibration, and shadow testing before bank deployment. Close with how the system supports analysts rather than making unsupported autonomous blocking claims.

## If this is a dataset leaderboard contest
Do NOT substitute a product demo for the requested submission. Show the notebook, metric, validation protocol, exact submission format and actual competition result. Only reuse RuseTect models when the data schema/task matches.

## Evidence to fill before presentation
- Official competition name and rules URL: [PENDING]
- Competition dataset and license: [PENDING]
- Task type and evaluation metric: [PENDING]
- Validation design: [PENDING]
- Actual validation result: [PENDING]
- Actual public/private leaderboard result: [PENDING]
- Model/pretraining declaration accepted by organizers: [PENDING]

## Likely judge questions
**Why PR-AUC?** Fraud is rare, so precision-recall summarizes the tradeoff more meaningfully than accuracy.
**Why Rust?** The serving layer can evaluate models without the Python runtime; repository documents parity tests.
**Is this real banking performance?** No. Published evaluations use synthetic or nonbank public datasets.
**What is the failure mode?** Domain shift and payment-format reliance; the project documents robustness tests and mitigation.
**How is a block decided?** The project requires two-analyst approval for blocks; show the actual review path.
**Did you train during the competition?** Answer truthfully and disclose existing models if used.
