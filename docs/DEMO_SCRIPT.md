# Demo script: ten minutes on the live demo

For a bank audience (fraud-risk lead, head of payments operations, an engineer). Live site:
[nemesis-rust1.vercel.app](https://nemesis-rust1.vercel.app), or a local copy in demo mode (below). Pages: `/console`,
`/investigate`, the workspace `/app`. Everything on screen is synthetic data; say so at the start, and do not send real
payments to the hosted demo.

**Run it locally** (no secrets needed; `RUSETECT_DEMO=1` is the public-demo mode, with demo analyst keys and no
production secrets, never for real data). Setting a throwaway webhook secret lets you show live webhook events too:

```sh
cargo build --release -p rusetect-server
export RUSETECT_WEBHOOK_SECRET=$(openssl rand -hex 32)   # throwaway, for this demo only
RUSETECT_DEMO=1 ./target/release/rusetect-server --port 8001
python3 -I tools/pilot_sender.py --self-test             # second terminal: the sender's offline check
```

For the live-events step, send any small JSON-lines file of webhook bodies (fields in
[PILOT_ONBOARDING.md](PILOT_ONBOARDING.md) section 7) with
`python3 -I tools/pilot_sender.py events.jsonl --url http://127.0.0.1:8001/api/v2/events/payment`.

Before the meeting: open each page once (on the hosted demo a cold start takes a few seconds), and have
[PILOT_ONBOARDING.md](PILOT_ONBOARDING.md), [PILOT_PROPOSAL.md](PILOT_PROPOSAL.md) and
[PILOT_REHEARSAL.md](PILOT_REHEARSAL.md) ready to share.

## 0:00 to 0:45 · What this is

> "RuseTect scores each payment for scam and money-mule risk and answers with the lightest step that fits: allow,
> ask the customer to verify, hold for an analyst, or a block that two analysts must approve. Today you will see it on
> synthetic payments. The pilot we propose runs it in shadow mode inside your network, on your data, deciding
> nothing."

## 0:45 to 2:30 · The payment cascade (`/console`)

1. Click **1 · Load account history**. The engine ingests recorded IBM AMLworld payments to build account history:
   the features are about each account's past (fan-in, pass-through, new payees), not the single payment.
2. Click **2 · Stream payments**. Point at the tiles: most payments are cleared by the small L1 gatekeeper; the rest
   go to L2, which escalates few of them. The demo alert feed shows each hold and block with the reasons the model
   gave.
3. Point at the **L4 ring linker**: blocked payments that share accounts are merged into one case, which is how an
   analyst sees a mule ring rather than a list of payments.

> "Each reason is from the model's own feature attributions, so an analyst can check it against the account."

## 2:30 to 4:00 · Scam context and the review desk (`/investigate`)

1. Click **Replay mule fixture**: twelve victims pay one mule account, which forwards almost all of it to a collector.
   Show the alert and the case.
2. Pick **Digital arrest call**, then **Assess**: the message and session signals (a live call, a remote access app)
   raise the response, and the pause screen asks one question chosen from the evidence. Toggle English and Hindi.
3. Under **Alerts needing a human**, show that a block needs two different analysts, and that the audit log and the
   evidence pack are signed and hash chained.

## 4:00 to 5:30 · The analyst workspace (`/app`)

1. **Executive dashboard** (`/app#/dashboard`): every figure names its source endpoint; "Confirmed fraud" says
   "not available", because no bank has confirmed an outcome yet. Point at the demo label on replayed data.
2. **Alert management** (`/app#/alerts`): open an alert; show the reasons, the review actions and that a block needs
   two different analysts. Pick an analyst key in the top bar to act.
3. **Network intelligence** (`/app#/network`): **Load demonstration network** and click the mule account: payers fan in,
   the money passes through to a collector. A connection is not evidence of wrongdoing; the view says so.
4. **Reports and evidence** (`/app#/reports`): open a case report; section 10 checks the signature and the hash chain
   at render time.

## 5:30 to 7:30 · How a pilot connects (`/console` top panels, `/app#/pilot`)

1. **Live events**: this panel lists only events pushed to the signed webhook (`POST /api/v2/events/payment`), tagged
   `live`, with the store badge (in-memory or Redis). In a pilot your system sends a copy of each payment here; nothing
   comes back into the payment flow.
2. **Drift**: the panel compares recent scores and inputs with the validation profile and turns amber or red when they
   differ.
3. **Pilot health** (`/app#/pilot`): the three daily checks of a pilot on one page, drift PSI per input, flagged
   decisions per UTC day against the review budget, and server latency of the latest live decisions.
4. Close with the integration in one sentence: HMAC-signed JSON per payment, idempotent by your transaction id, one
   binary inside your network, eight weeks ([PILOT_ONBOARDING.md](PILOT_ONBOARDING.md) section 12), and a rehearsal
   already run end to end on synthetic data, including the fixed-budget comparison
   ([PILOT_REHEARSAL.md](PILOT_REHEARSAL.md), from `results/pilot_rehearsal.json`). Say what broke in it, too.

## 7:30 to 9:00 · What it does not do yet (say this, do not skip it)

Read the three limits from the files, not from memory:

1. **It has not been shown to transfer to other data.** On a fresh holdout from a different synthetic generator (IBM
   AMLSim), pre-registered and scored once, the verdict is `"does not transfer"`
   ([research/out/holdout_amlsim.json](../research/out/holdout_amlsim.json), `rule.verdict`; protocol in
   [HOLDOUT_PREREG.md](HOLDOUT_PREREG.md)). The served model did not beat a simple amount baseline there. This is why
   the offer is a shadow pilot measured on your data, and why we expect to retrain on it.
2. **Known evasion weaknesses.** In the pre-registered evasion suite on HI-Small validation, two attacks are marked as
   known weaknesses: `timing_jitter` (each laundering payment sent up to two days later) and `new_payee` (each
   laundering payment sent to a fresh, never-seen mule account) ([research/out/evasion_validation.json](../research/out/evasion_validation.json),
   `known_weaknesses`, with the PR-AUC drop of each under `attacks`). Against an adversary who studies the model,
   these are a floor, not a ceiling ([PILOT.md](PILOT.md)).
3. **Drift alerts on data that does not match the reference.** The drift monitor compares traffic with a profile of
   synthetic validation data, so on any other population it alerts: it did on AMLSim (`results/drift_amlsim.json`),
   and it does on a fresh install with no account history (`results/pilot_rehearsal.json`, `drift`). On your data it
   will alert from day one until the reference is rebuilt from your own validation period
   ([PILOT_ONBOARDING.md](PILOT_ONBOARDING.md) section 9).

Also true, if asked: every result is on synthetic or public data, so accuracy on a bank's real payments is unverified; there has been no external penetration test or
certification; and you would be the first organisation to evaluate it ([PILOT_PROPOSAL.md](PILOT_PROPOSAL.md),
"What NEMESIS does not claim").

## 9:00 to 10:00 · The ask

> "So the question for a pilot is not whether these numbers hold for you. It is whether, on your data and at an alert
> volume your team can absorb, it finds coordinated fraud and mule accounts your current system misses. We agree the
> success rule before either of us reads a result."

## Questions you will get

| question | answer, with the source |
|---|---|
| Does any data leave our network? | No, when self-hosted ([PILOT_ONBOARDING.md](PILOT_ONBOARDING.md) section 2). |
| What do you need from us? | A payment stream, labelled outcomes with dates, three months of history, an alert budget ([PILOT.md](PILOT.md)). |
| Can it block payments? | Not in the pilot. In production a block needs two analysts. |
| How is it secured? | Signed webhook, analyst keys and roles, fail-closed start-up; open gaps in [THREAT_MODEL.md](THREAT_MODEL.md). |
| What does it cost? | See [PILOT_PROPOSAL.md](PILOT_PROPOSAL.md), commercial terms. |
