# Dataset audit: synthetic against real

Every dataset used in the project, whether its labels come from real events, and the closest real equivalent that is
publicly available. Results are in `research/out/`; the comparison of real and synthetic training data is
`research/real_vs_synthetic.py` (results in `research/out/real_vs_synthetic.json`).

## The constraint

Banks do not publish labelled transfer data: suspicious activity reports are confidential by law, and payment records
are personal data. No public dataset has real bank-to-bank transfers with real laundering or mule labels. Every public
AML transfer set is simulated. Real labels exist for card payments (e-commerce and in-store), crypto transactions, loans
and logins. The project therefore uses real data wherever a real equivalent exists, and keeps synthetic data only for
bank-transfer laundering, where none exists.

## Synthetic datasets and their closest real equivalents

| Synthetic dataset | What it simulates | Closest real equivalent | Real labels? | Status |
|---|---|---|---|---|
| IBM AMLworld HI/LI Small, Medium, Large | Bank transfers with laundering typologies | None public. Berka (real Czech bank transfers, no laundering labels); Elliptic (real Bitcoin, illicit labels) | No real equivalent | Kept: the served models are trained on it; flagged as synthetic everywhere |
| SAML-D (Oztas et al.) | UK bank transfers, 28 typologies | As above | No | Kept as a second generator for transfer tests |
| AMLgentex (Ostman et al.) | Swedish instant payments | As above | No | Kept as a third generator |
| PaySim | Mobile money (an African operator's logs, simulated) | None public for mobile money fraud | No | Kept for wallet coverage; balances dropped (label shortcut) |
| BankSim | Spanish card payments (from real aggregates) | **IEEE-CIS** (real card-not-present payments, Vesta), **ULB** (real European card payments) | Yes | **Replaced as the card benchmark by IEEE-CIS**; kept in the hybrid arm |
| Sparkov | US card payments | **IEEE-CIS**, **ULB** | Yes | Same |
| IBM TabFormer | US card payments | **IEEE-CIS**, **ULB** | Yes | Same |
| Feedzai BAF | Account-opening applications (from real data, privacy-preserving) | None public with real labels | No | Kept for the onboarding model |
| RBA logins (Wiefling et al.) | Logins, generated from real login statistics | None public with real takeover labels | Partly | Kept for account-takeover tests |

## Real-label datasets used

| Dataset | Domain | Size | Positives | Used for |
|---|---|---|---|---|
| IEEE-CIS (Vesta) | Card-not-present payments | 590,540 payments | 20,663 fraud | Real card benchmark through the engine features; unified training; real-vs-synthetic comparison |
| ULB (Dal Pozzolo et al.) | European card payments | 284,807 | 492 | Real card check (PCA features, no account ids, so not through the engine) |
| Elliptic and Elliptic++ | Bitcoin transactions and wallets | 46,564 labelled transactions; 367,472 wallet rows | 4,545 illicit transactions; 28,601 illicit wallet rows | Real crypto, graph features |
| BitcoinHeist (UCI 526) | Bitcoin addresses per day | 2,916,697 address-days | 41,413 ransomware | Real crypto ransomware, time split |
| Ethereum flagged addresses | Ethereum accounts | 9,816 addresses | 2,179 flagged | Real crypto accounts; collection artefact measured (see report) |
| DGraph-Fin | Loan platform user graph | 3.7M users | real default-fraud labels | Real credit graph |
| Berka (PKDD99) | Real Czech bank, transactions and loans | 1.06M transactions | 76 bad loans | Real bank behaviour (loan default, not fraud) |
| UCI SMS spam, Hinglish scam messages | Text messages | thousands | real spam and scams | Message screen |

## How real and synthetic are compared (controlled)

Same engine features (29, no format or currency code), same learner and settings, each dataset split by time 60/20/20,
and three arms: trained on real only (IEEE-CIS), on synthetic only (Sparkov, BankSim, TabFormer), and on both (hybrid).
The arm is chosen on IEEE-CIS validation before the test part is read; the hybrid wins only if it costs at most 0.01
validation PR-AUC on the real data and generalises better to the synthetic sets. Test metrics: PR-AUC, ROC-AUC, and
precision, recall, F1 and false-positive rate at each set's validation F1 threshold, plus an unseen-payer test on IEEE-CIS.

## What this cannot fix

The served transfer model still learns from synthetic laundering, because nothing real exists for it in public. The
honest next step is a shadow pilot on a bank's own labelled data (`docs/PILOT.md`).
