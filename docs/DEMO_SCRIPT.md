# Demo script: five minutes on the live console

For a bank audience (fraud-risk lead, head of payments operations, an engineer). Live site:
[nemesis-rust1.vercel.app](https://nemesis-rust1.vercel.app). Console: `/console`; investigator: `/investigate`.
Everything on screen is synthetic data; say so at the start, and do not send real payments to the hosted demo.

Before the meeting: open both pages once to warm the function (a cold start takes a few seconds), and have
[PILOT_ONBOARDING.md](PILOT_ONBOARDING.md) and [PILOT_PROPOSAL.md](PILOT_PROPOSAL.md) ready to share.

## 0:00 to 0:30 · What this is

> "RuseTect scores each payment for scam and money-mule risk and answers with the lightest step that fits: allow,
> ask the customer to verify, hold for an analyst, or a block that two analysts must approve. Today you will see it on
> synthetic payments. The pilot we propose runs it in shadow mode inside your network, on your data, deciding
> nothing."

## 0:30 to 2:00 · The payment cascade (`/console`)

1. Click **1 · Load account history**. The engine ingests recorded IBM AMLworld payments to build account history:
   the features are about each account's past (fan-in, pass-through, new payees), not the single payment.
2. Click **2 · Stream payments**. Point at the tiles: most payments are cleared by the small L1 gatekeeper; the rest
   go to L2, which escalates few of them. The demo alert feed shows each hold and block with the reasons the model
   gave.
3. Point at the **L4 ring linker**: blocked payments that share accounts are merged into one case, which is how an
   analyst sees a mule ring rather than a list of payments.

> "Each reason is from the model's own feature attributions, so an analyst can check it against the account."

## 2:00 to 3:00 · Scam context and the review desk (`/investigate`)

1. Click **Replay mule fixture**: twelve victims pay one mule account, which forwards almost all of it to a collector.
   Show the alert and the case.
2. Pick **Digital arrest call**, then **Assess**: the message and session signals (a live call, a remote access app)
   raise the response, and the pause screen asks one question chosen from the evidence. Toggle English and Hindi.
3. Under **Alerts needing a human**, show that a block needs two different analysts, and that the audit log and the
   evidence pack are signed and hash chained.

## 3:00 to 4:00 · How a pilot connects (`/console`, top panels)

1. **Live events**: this panel lists only events pushed to the signed webhook (`POST /api/v2/events/payment`), tagged
   `live`, with the store badge (in-memory or Redis). In a pilot your system sends a copy of each payment here; nothing
   comes back into the payment flow.
2. **Drift**: the panel compares recent scores and inputs with the validation profile and turns amber or red when they
   differ.
3. Close with the integration in one sentence: HMAC-signed JSON per payment, idempotent by your transaction id, one
   binary inside your network, a rehearsal already run end to end (`results/pilot_rehearsal.json`, quoted in
   [PILOT_ONBOARDING.md](PILOT_ONBOARDING.md) section 10).

## 4:00 to 5:00 · What it does not do yet (say this, do not skip it)

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

Also true, if asked: every result is on synthetic or public data; there has been no external penetration test or
certification; and you would be the first organisation to evaluate it ([PILOT_PROPOSAL.md](PILOT_PROPOSAL.md),
"What NEMESIS does not claim").

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
