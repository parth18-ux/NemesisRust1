# RuseTect — Cakewalk 2026 Competition Edition

This is a **separate competition workspace** for the existing RuseTect fraud detection system. It does not change the Rust inference engine or claim a Kaggle score.

## Competition facts from registration screenshots
- Online data-science competition; up to 3 students per team.
- Final private leaderboard rank: 80%; notebook quality/documentation: 20%.
- Limited submissions per hour; one account per team.
- Pretrained models permitted only when declared in advance.
- Screenshot shows November 21, 2026 6–9 PM, but registration deadline November 24: **confirm dates with organizers**.
- The official dataset, metric, submission format, and whether independent project demos are accepted are **not yet verified**.

## RuseTect project
Existing source: https://github.com/parth18-ux/NemesisRust1
Demo: https://nemesis-rust1.vercel.app
RuseTect detects payment scams and money mule activity, with XGBoost and LightGBM scoring, 31 historical features, message and session signals, explainable risk, and analyst review. See the root README and docs/MODEL_CARD.md for caveats.

## Workflow
1. Confirm event rules, dataset and metric.
2. Put train.csv, test.csv and sample_submission.csv under competition/data/ (do not commit private data).
3. Inspect target, ID, task type, leakage and train/test distribution.
4. Run `python competition/train.py --train ... --test ... --sample ... --target TARGET --metric roc_auc` for a binary classification task. Set `--id-column` when needed.
5. Check validation score, class balance, leakage, and output CSV schema before uploading.
6. Submit only within organizers' rate limit. Do not optimize repeatedly on the public leaderboard.
7. Fill the evidence fields in PRESENTATION.md with actual validated numbers.

## Important boundaries
The baseline training script is for tabular **binary classification** only. It does not replace RuseTect's existing payment engine, and cannot directly handle multiclass, regression, text or images. Competition-specific adaptation requires actual files. Do not represent the existing IBM AMLworld benchmarks as competition results.

## Deliverables
- train.py — working baseline, validation, final model fit and submission CSV
- PRESENTATION.md — talk track, demo flow and judge questions
- CHECKLIST.md — readiness, compliance, and hand-in requirements
