# RuseTect

RuseTect detects scam payments and money mule activity in digital banking. It follows a scam from the first message a
victim receives to the accounts that launder the money, and it answers with the lightest intervention that fits the
evidence: a question to the customer, a short pause, an analyst review, or a block approved by two analysts.

Live demo: [nemesis-rust1.vercel.app](https://nemesis-rust1.vercel.app) · Author: Parth Sharma

> RuseTect is a research project. Its models are trained and evaluated on public datasets that are synthetic or
> collected outside banking (IBM AMLworld, Feedzai BAF, PaySim, Aalto keystrokes, Balabit mouse data). The results below show
> how the methods behave on that data, not performance on real bank traffic.

## How it works

A payment passes through a cascade. A small gatekeeper model approves most payments quickly; the rest are
scored by two gradient boosted models built on 31 account history features, each computed only from earlier payments.
Around that core, independent evidence domains raise or lower the response, and no single domain can hold money on its
own.

| Layer | What it does | Code |
|---|---|---|
| Message (L-1) | Scores an SMS, WhatsApp message or call transcript, names the scam workflow (digital arrest, KYC block, task job, voice clone emergency) and extracts UPI IDs, phone numbers and links | `crates/rusetect/src/scam_text.rs` |
| Message to payment | Asks for confirmation on a payment to a new payee within 72 hours of a risky message, and holds payments to an identifier named in one | `crates/rusetect/src/lure_fusion.rs` |
| Account opening (L0) | Scores new account applications for mule risk, with separate thresholds for applicants under and over 50 | `service/models/l0_onboarding_lgbm.json` |
| Payment cascade (L1, L2) | XGBoost gatekeeper, then XGBoost and LightGBM averaged into a risk score with per feature reasons | `crates/rusetect/src/engine.rs` |
| Ring linking (L4) | Merges blocked payments that share accounts into a single case | `crates/rusetect/src/engine.rs` |
| Context rules | Combines message, payment, account graph and session evidence; escalation needs two domains | `crates/rusetect/src/context_rules.rs` |
| Session and identity | Live call, screen sharing, remote access apps, pasted payee details, hesitation, SIM swap and device checks before the OTP, account takeover sequences | `crates/rusetect/src/session_signals.rs` |
| Person | Typing rhythm and mouse dynamics compared with the customer's own profile | `crates/rusetect/src/behaviour.rs`, `crates/rusetect/src/mouse_dynamics.rs` |
| Intervention | Pause screen in English and Hindi with a question chosen from the evidence, cool off period, duress PIN | `crates/rusetect/src/intervention.rs` |
| Network | Links victims of the same scam campaign; a signed, hash chained ledger lets banks share mule flags without sharing personal data | `crates/rusetect/src/campaigns.rs`, `crates/rusetect/src/consortium.rs` |
| Review desk | Analyst keys and roles, two analyst approval for blocks, signed verdicts and evidence packs, STR drafts | `crates/rusetect/src/review.rs`, `crates/rusetect/src/security.rs`, `crates/rusetect/src/str_report.rs` |

This is the Rust edition of RuseTect. The original Python implementation lives at
[parth18-ux/Nemesis1](https://github.com/parth18-ux/Nemesis1); this repository serves the same models with the same
decisions, without Python, NumPy, XGBoost or LightGBM installed. XGBoost and LightGBM models are read from their JSON
files and evaluated in Rust, including XGBoost's exact TreeSHAP for the per-feature reasons.

### Same answers as the Python service

The port was checked against the Python service it replaces:

* the live engine on 20,000 test-period payments after 100,000 history payments: every feature, L1 and L2 score,
  decision and SHAP reason identical (`cargo run --release --example parity_dump`)
* the message model, redaction and context cues on 809 messages: every probability, band, workflow, key phrase,
  entity and salted hash identical (`cargo run --release --example text_dump`)
* 321 API calls replayed against both servers (payments, assessments with messages and session signals, alerts,
  reviews, STR drafts, campaigns, onboarding, typing checks, validation errors, the console replay): identical
  responses apart from timings, random keys and timestamps
* the original Python test suite run against the Rust server through an HTTP proxy: 44 of 44 API tests pass
* evidence pack hashes, the audit chain and Ed25519 signatures from the Rust server verify with the Python code

To get there the Python details that change numbers are reproduced exactly: CPython's compensated `sum()`,
`round()`, `repr(float)` and `json.dumps` byte for byte, exact `statistics.pstdev`, Python regex word boundaries and
whitespace, float32 XGBoost accumulation and TreeSHAP operation order. Case ids are the one deliberate change: Python
derived them from its per-process random `hash()`, the Rust engine uses a stable hash, so a case keeps its id across
restarts.

## Results

Every number comes from an evaluation script and a file in `results/`. They were measured with the original Python
pipeline; the Rust port serves the same models, so the live path reproduces them.

| Area | Data | Result |
|---|---|---|
| Payment models | IBM AMLworld HI-Small, every payment streamed through the live engine, last 20% of time as test | PR-AUC 0.711, ROC-AUC 0.985 |
| Payment models, served hardened risk (pooled format-free second opinion) | HI-Small test, same protocol ([research/out/served_eval.json](research/out/served_eval.json)) | PR-AUC 0.720 (cascade 0.710; the difference is within noise); block band at the design budget: 63.5% recall, 83.8% precision; PR-AUC 0.687 when every laundering payment switches format (cascade alone: 0.004) |
| Payment models, lower laundering rate | IBM AMLworld LI-Small, same protocol | PR-AUC 0.345, ROC-AUC 0.967 |
| Account opening | Feedzai BAF Base and Variants I to V, months 6 to 7 | mean ROC-AUC 0.888 over six populations; false alarm parity between age groups 0.93 on Base |
| Messages | Synthetic Indian scam messages from templates held out of training | 0.95 precision at 68% recall for push warnings; no false alarms on genuine OTP, bank, bill and delivery messages |
| Typing rhythm | Aalto 136M Keystrokes, 2,868 people never seen in training | ROC-AUC 0.890 from one sentence, 0.936 from three; 64% of impostors caught at 5% false alarms, equal across age groups |
| Mouse dynamics | Balabit Mouse Dynamics Challenge, labelled impostor sessions | median per user ROC-AUC 0.915 from 20 mouse actions |
| End to end scenarios | 480 authored, randomised runs through the API | 280 of 280 scam payments stopped before money moved; 1 of 200 genuine payments interrupted |
| Mobile money | PaySim, 6.36M transactions, last 20% of time as test | PR-AUC 0.52 from account history alone; near perfect once balances are used, because every simulated fraud empties the account ([note](docs/datasets.md)) |
| Throughput | Full HI-Small file, 5.08M payments, one CPU core | 41,459 payments per second with compact account state |

Details: [model selection](docs/model-selection.md), [retraining with the extra data](docs/RETRAINING.md),
[person, session and network layer](docs/SESSION_AND_NETWORK.md), [context layer](docs/CONTEXT_LAYER.md),
[scaling](docs/SCALING.md), [datasets](docs/datasets.md), [project audit](docs/project-audit.md).

### Research review (November 2026)

An independent check of [the audit and roadmap](docs/research/) against the served models ([PDF](docs/research/RuseTect%20research%20review%202026-11.pdf)).
It runs robustness, evasion, calibration and cross generator experiments through the live Rust feature code
(`research/`, results in `research/out/`). Main findings: the served model leans on payment format (92% of laundering in
the bundled HI-Small slice is ACH, and re-sending laundering as Cheque takes block recall from 37% to 0%); splitting
payments in four removes the block band; the LightGBM half adds no measurable PR-AUC; scores are uncalibrated; and the
31 features transfer to AMLgentex when retrained while the HI-Small model does not. The API no longer fills a missing
`payment_format` with ACH. Two fixes are now served: a format-free second opinion
(block recall under the Cheque attack 65.8%, was 2.0%)
and 24 hour payer-to-payee aggregation against split payments (four-part block recall
26%, was 5%, an exploratory variant chosen on the same window;
[research/out/structuring_defence.json](research/out/structuring_defence.json)).

The [model card](docs/MODEL_CARD.md) lists intended use, measured performance, robustness and known limits; the
[pilot protocol](docs/PILOT.md) is how a bank can measure it on its own data in shadow mode.

## Quick start

Install Rust from [rustup.rs](https://rustup.rs), then:

```bash
RUSETECT_DEMO=1 cargo run --release -p rusetect-server -- --port 8001
```

Open `http://localhost:8001`. The site has four pages: the overview (`/`), the investigator (`/investigate`) where you
can replay scam scenarios and review alerts, a live console (`/console`) that replays test period payments through the
engine, and the benchmark report (`/report`). The interactive API reference is at `/docs`. On Windows you can also run
`.\run_local.ps1`.

Score a payment:

```bash
curl -X POST localhost:8001/api/v2/payment -H 'Content-Type: application/json' -d '{
  "timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "A1",
  "to_bank": "011", "to_account": "B2", "amount_paid": 9300, "payment_format": "ACH"}'
```

Score a message and link it to the payer, so that their next payment to a new payee is escalated:

```bash
curl -X POST localhost:8001/api/v2/message -H 'Content-Type: application/json' -d '{
  "text": "Your SBI account will be blocked today. Update PAN at http://sbi-kyc-update.in/verify",
  "timestamp": "2022-09-09 09:00", "bank": "070", "account": "A1"}'
```

The full assessment with session signals is `POST /api/v2/assess`. Other routes include `POST /api/v2/onboard`,
`GET /api/v2/cases`, `GET /api/v2/campaigns`, `GET /api/v2/model-card` and `GET /api/health`.

To run the tests:

```bash
cargo test --release --workspace
```

The Redis scale tests start their own Redis, so they need the `redis-server` program on your PATH (Linux:
`apt install redis-server`, macOS: `brew install redis`, Windows: run the tests in WSL). Without it, they are skipped.

For the console to use full account history, point `RUSETECT_HISTORY_GLOB` at the IBM HI-Small CSV files; otherwise it
uses a bundled sample of 100,000 payments.

## Reproducing the results

The datasets are not stored in this repository. Download them, then point `RUSETECT_DATA` at the folder that holds them.

| Dataset | Source |
|---|---|
| IBM AMLworld HI-Small and LI-Small | [Kaggle: ealtman2019/ibm-transactions-for-anti-money-laundering-aml](https://www.kaggle.com/datasets/ealtman2019/ibm-transactions-for-anti-money-laundering-aml) |
| Feedzai Bank Account Fraud (Base, Variants I to V) | [Kaggle: sgpjesus/bank-account-fraud-dataset-neurips-2022](https://www.kaggle.com/datasets/sgpjesus/bank-account-fraud-dataset-neurips-2022) |
| Aalto 136M Keystrokes | [userinterfaces.aalto.fi/136Mkeystrokes](https://userinterfaces.aalto.fi/136Mkeystrokes/) |
| Balabit Mouse Dynamics Challenge | [github.com/balabit/Mouse-Dynamics-Challenge](https://github.com/balabit/Mouse-Dynamics-Challenge) |
| PaySim | [Kaggle: ealaxi/paysim1](https://www.kaggle.com/datasets/ealaxi/paysim1) |

The training and evaluation pipeline is being ported to Rust in `crates/rusetect-ml` (next commits). Until it lands,
the scripts that produced `results/` are in the Python edition, [Nemesis1](https://github.com/parth18-ux/Nemesis1) `ml/`.


## Deployment

`rusetect-server` is a single binary; it reads the models and pages from the repository folder (`RUSETECT_ROOT`).
`deploy/Dockerfile` builds an image with the server and the streaming workers, and `deploy/docker-compose.yml` runs
the API with Redis and two scoring workers (`rusetect-worker`, `rusetect-replay`); see [scaling](docs/SCALING.md).
**Vercel.** `api/rusetect.rs` wraps the same server for Vercel's Rust runtime (beta), and `vercel.json` routes every
path to it. Import the repository in Vercel (or run `npx vercel deploy --prod`) and set `RUSETECT_DEMO=1` for the public demo. The models and
pages are embedded in the function binary. Serverless instances do not share memory, so demo state such as the console
replay can reset between requests.

All requests are applied one at a time behind a single lock, the same order the Python event loop gave them, so demo
state behaves as before; scoring takes microseconds, so this is not the bottleneck.

The server refuses to start unless it is told which mode it is in. `RUSETECT_DEMO=1` is the public demo, which
publishes its analyst keys so the review desk can be tried. For anything real set `RUSETECT_ANALYST_KEYS`; the server
then also requires `RUSETECT_SIGNING_KEY`, `RUSETECT_CONSORTIUM_SECRET` and `RUSETECT_HASH_SALT`.

## Project layout

```
crates/rusetect/          core library: live engine, model inference, risk layers, review desk, signing
crates/rusetect-server/   HTTP API and pages (rusetect-server), Redis state and stream workers (rusetect-worker, rusetect-replay)
crates/rusetect-ml/       feature building, training, benchmarks and evaluations
service/models/           served models and thresholds
service/static/           web pages
results/                  measured results, one JSON file per evaluation
report/                   source of the benchmark report page
docs/                     design notes and results
```

## Limitations

The payment and account opening data are synthetic, and the message corpus was written from public advisories, so
models and thresholds must be validated again on a bank's own data before use. Call audio is not modelled; the message
layer needs a transcript. Session signals have no public benchmark, and the typing and mouse checks were validated on
desktop and typing test data rather than inside a banking app. The demo keeps state in memory; production needs the
Redis state layer or a feature store.

## Acknowledgements

Datasets by IBM Research (AMLworld, Altman et al., NeurIPS 2023), Feedzai (Bank Account Fraud, Jesus et al., NeurIPS
2022), Aalto University (Dhakal, Feit, Kristensson and Oulasvirta, "Observations on Typing from 136 Million
Keystrokes", CHI 2018), Balabit (Mouse Dynamics Challenge) and Lopez-Rojas, Elmir and Axelsson (PaySim, 2016).
