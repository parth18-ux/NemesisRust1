# Pilot onboarding: integration guide for the bank's engineer

This guide takes a bank engineer from an empty host to a running shadow pilot on one payment channel. In short, the
partner sends a signed copy of each payment (sections 3, 4 and 7), replays its history first (section 4), and gets back
a decision log, an alert queue, a pilot-health view and a fixed-budget comparison with its current system (section 11),
over eight weeks (section 12). Accuracy on real bank payments is not yet measured: every result so far is on synthetic
or public data, which is what the pilot is for. It links to the reference documents rather than repeating them: [live.md](live.md) (webhook reference), [RUNBOOK.md](RUNBOOK.md) (operations),
[THREAT_MODEL.md](THREAT_MODEL.md) (security review),
[PILOT.md](PILOT.md) (evaluation protocol) and [PILOT_PROPOSAL.md](PILOT_PROPOSAL.md) (scope and timeline).

## 1. What shadow mode means

RuseTect scores a copy of your payment stream and logs its decision. It never blocks, holds or delays a payment, never
contacts a customer and never writes to your case system. Your core banking system keeps deciding exactly as today;
the only integration is a one-way copy of each payment, sent after (or alongside) your own processing. The `action`
RuseTect returns (`allow`, `verify`, `hold`, `block`) is a recommendation written to its log and alert queue, for your
team to compare with what your current system did. Nothing in the response should be wired back into the payment flow
during the pilot.

## 2. Self-host: what runs where, and what data leaves the bank

Self-hosted, **no data leaves the bank**: the server, its models and its state run on a host in your network. The
server makes no outbound calls of its own (no telemetry, no model downloads; the models ship in the repository and the
binary); its only connection out is to the Redis you configure, if any. In the analyst's browser, the console pages
load web fonts from Google Fonts and `/docs` loads Swagger UI from jsDelivr; no payment data is in those requests, and
your proxy can block them (the pages still work, with fallback fonts).
The hosted demo at nemesis-rust1.vercel.app is for the walkthrough only; do not send real payments to it.

Steps (Linux host, or the container in `deploy/`):

1. Build: `cargo build --release -p rusetect-server` (Rust toolchain as in `.github/workflows/ci.yml`), or
   `docker build -f deploy/Dockerfile -t rusetect .` from the repository root. The image runs as an unprivileged user.
2. Generate secrets on the host and keep them in your secret store, never in the repository:

   | variable | value |
   |---|---|
   | `RUSETECT_WEBHOOK_SECRET` | `openssl rand -hex 32`; shared with the sender only |
   | `RUSETECT_ANALYST_KEYS` | JSON `{key: {"analyst": name, "role": role}}`, keys from `openssl rand -hex 24` (section 6) |
   | `RUSETECT_SIGNING_KEY` | `openssl rand -base64 32` (Ed25519 seed for signed verdicts and evidence packs) |
   | `RUSETECT_CONSORTIUM_SECRET` | `openssl rand -hex 32` |
   | `RUSETECT_HASH_SALT` | `openssl rand -hex 16` (salts hashed identifiers in logs) |

   The server refuses to start without the last four (it fails closed; `RUSETECT_DEMO=1` is for the public demo only).
3. Run: `HOST=0.0.0.0 PORT=8000 RUSETECT_ROOT=/path/to/repo target/release/rusetect-server` (or
   `docker run -p 8000:8000 --env-file <your env file> rusetect`). Put it behind your TLS terminator; the sender should
   reach it over TLS on your internal network.
4. Optional durable store: set `REDIS_URL` to a Redis inside your network so decisions survive restarts
   ([live.md](live.md) section 6, [SCALING.md](SCALING.md) for the worker layout). Without it state is in memory and
   `/api/health` says `"durable": false`.
5. Check `GET /api/health` shows `"ready": true` and `"auth": "configured keys"`, then send one signed test event
   (section 3) and see it in `/api/v2/live`.
6. Rebuild the drift reference from your own data (section 9) before reading drift status.
7. Replay your history (at least three months, [PILOT.md](PILOT.md) step 1) through the same webhook in time order with
   the pilot sender (section 4), then switch the sender to the live copy of the stream. For the replay raise
   `RUSETECT_RATE_LIMIT` (requests per 10 s per client) above the sender's `--rate`, size `RUSETECT_MAX_ACCOUNTS`
   for your account count and, in memory mode, `RUSETECT_MAX_EVENTS` above the number of events (section 3) ([live.md](live.md), limits).

Have your security team review [THREAT_MODEL.md](THREAT_MODEL.md) first; its open gaps are listed there.

## 3. Sending payments: `POST /api/v2/events/payment`

Body: one JSON object per payment (fields in section 7) with your unique `event_id`. Headers:

- `X-RuseTect-Timestamp`: current time in epoch seconds;
- `X-RuseTect-Signature`: `v1=` + hex HMAC-SHA256 of `"{timestamp}.{raw body}"` under `RUSETECT_WEBHOOK_SECRET`.

Sign the exact bytes you send. A timestamp more than 5 minutes from the server clock is refused (`401`), so keep the
sender's clock on NTP and sign each attempt afresh. The full response and error table is in [live.md](live.md)
section 3.

**curl / bash**

```sh
URL=https://rusetect.internal.example/api/v2/events/payment
BODY='{"event_id":"pay-0001","timestamp":"2026-11-15 09:30","from_bank":"070","from_account":"C123","to_bank":"011","to_account":"M456","amount_paid":9000,"payment_currency":"US Dollar","payment_format":"ACH"}'
TS=$(date +%s)
SIG=$(printf '%s.%s' "$TS" "$BODY" | openssl dgst -sha256 -hmac "$RUSETECT_WEBHOOK_SECRET" -hex | sed 's/^.*= //')
curl -sS "$URL" -H 'Content-Type: application/json' -H "X-RuseTect-Timestamp: $TS" \
  -H "X-RuseTect-Signature: v1=$SIG" --data-raw "$BODY"
```

**Python**

```python
import hashlib, hmac, json, os, time, urllib.request

def send(event: dict, url: str) -> dict:
    body = json.dumps(event, separators=(",", ":")).encode()
    ts = str(int(time.time()))
    sig = hmac.new(os.environ["RUSETECT_WEBHOOK_SECRET"].encode(), ts.encode() + b"." + body, hashlib.sha256).hexdigest()
    req = urllib.request.Request(url, data=body, method="POST", headers={
        "Content-Type": "application/json", "X-RuseTect-Timestamp": ts, "X-RuseTect-Signature": "v1=" + sig})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r)
```

**Idempotency.** The `event_id` (1 to 128 characters) is the idempotency key. Re-sending a known `event_id` returns the
stored result unchanged with header `Idempotent-Replayed: true`; it is not scored or logged again. Always retry with
the same body and `event_id`. In memory mode this holds for the life of the process, for up to `RUSETECT_MAX_EVENTS`
event ids (default 100,000); once that many are stored a new `event_id` is refused with `503` and nothing is committed,
so size it above your replay plus the pilot's live volume, or use Redis. With Redis it holds for 7 days.

**Retries and `Retry-After`.** Retry network errors, `409` (the same `event_id` is still in flight), `429` (rate
limit, `RUSETECT_RATE_LIMIT` requests per 10 s per client) and any `5xx`. A `503` (secret not set, account state full,
or the durable store unreachable) commits nothing and carries `Retry-After` in seconds: wait at least that long, with
exponential backoff and full jitter on top. Do not retry other `4xx` (`401` signature or clock, `422` invalid field):
fix the record. After the last attempt keep the event in your own queue and alert ([live.md](live.md) section 4).

## 4. The pilot sender (`tools/pilot_sender.py`)

A standard-library Python script that reads a CSV or JSON-lines file of webhook bodies and signs and posts each one,
with a rate limit, the retry rules above and a per-event result log. It reads the secret from an environment variable
and never prints it.

```sh
python3 -I tools/pilot_sender.py --self-test        # offline check against a stub server
export RUSETECT_WEBHOOK_SECRET=...                  # from your secret store, not typed into history
python3 -I tools/pilot_sender.py history.jsonl --url https://rusetect.internal.example/api/v2/events/payment \
  --rate 50 --log sent.jsonl
```

It prints a JSON summary (`sent`, `accepted`, `replayed`, `failed`, `retried`, counts by action and by failure code).
`sent.jsonl` has one line per event: `event_id`, HTTP status, attempts, action, band, risk, payment timestamp, alert id and latency. A re-run of
the same file is safe: every event already committed comes back as `replayed`.

**Replaying history in shadow mode.** Export at least three months before the pilot window as one file sorted by
`timestamp`, start a fresh server (a fresh Redis database if you use one) with the build and model version you will
pilot, and send the file with `--log replay.jsonl`; then point the sender at the live copy of the stream. Keep the
replay log: it is the input of the fixed-budget comparison in [PILOT.md](PILOT.md) ("Compare rankings on a historical
replay"), together with your incumbent scores and confirmed outcomes. The replay is the engine's account history; no
label is read during it.

## 5. Secret rotation

No downtime, using `RUSETECT_WEBHOOK_SECRET_PREVIOUS` (full steps in [RUNBOOK.md](RUNBOOK.md) section 4):

1. Generate the new secret. Set `RUSETECT_WEBHOOK_SECRET_PREVIOUS` to the current value and `RUSETECT_WEBHOOK_SECRET`
   to the new one; restart. Both are now accepted.
2. Give the sender the new secret. During the switch it may send two comma-separated `v1=` signatures.
3. When no request is signed with the old secret any more (no `401`s from the sender), remove
   `RUSETECT_WEBHOOK_SECRET_PREVIOUS` and restart. For a leaked secret, skip the overlap.

## 6. Analyst keys and roles

`RUSETECT_ANALYST_KEYS` is a JSON object mapping each key (16+ characters) to `{"analyst": name, "role": role}`.
Clients send the key as `X-Analyst-Key`. With keys configured, every `/api` route needs a valid key except
`/api/health`, the webhook (it has its own signature), `/api/v2/keys/public`, `/api/v2/verify` and
`/api/v2/model-card` (`gate` in `crates/rusetect/src/security.rs`). Roles:

| role | can |
|---|---|
| `viewer` | read only (`GET`): live feed, drift, alerts, evidence packs, STR drafts, cases, audit log |
| `channel` | read, and call the scoring routes (`POST /api/v2/assess`, `/api/v2/payment`, ...) for a channel app |
| `analyst` | as channel, and review alerts: `release`, `verify`, `hold`, `approve_block` |
| `supervisor` | as analyst, and the demo reset and replay routes |

A block needs `approve_block` from two different analysts; review actions also need a fresh `X-Nonce`. A refused key
is logged with the first 8 hex characters of its SHA-256, never the key. Rotate keys as in [RUNBOOK.md](RUNBOOK.md)
section 4. Give the pilot team `viewer` keys for monitoring and `analyst` keys to the reviewers. In a shadow pilot a
"block" approval blocks nothing outside RuseTect.

## 7. Field mapping from a core-banking payment record

The webhook body is flat JSON. Map one record of your payment ledger or switch log as follows:

| our field | required | typical core-banking source | notes |
|---|---|---|---|
| `event_id` | yes | transaction reference / end-to-end id (e.g. ISO 20022 `EndToEndId` or `TxId`) | unique and stable across re-sends; 1 to 128 characters |
| `timestamp` | yes | value date-time or posting time | `YYYY-MM-DD HH:MM`, **UTC** (convert from local time) |
| `from_bank` | yes | debtor agent (sort code, IFSC, BIC, internal branch code) | string, up to 128 characters |
| `from_account` | yes | debtor account number or IBAN | may be a salted hash, the same hash every time for the same account |
| `to_bank` | yes | creditor agent | as `from_bank` |
| `to_account` | yes | creditor account number, IBAN, or payee handle | as `from_account` |
| `amount_paid` | yes | instructed amount | number above 0, in `payment_currency` |
| `payment_currency` | no (default `US Dollar`) | instructed currency | one of the trained names: Australian Dollar, Bitcoin, Brazil Real, Canadian Dollar, Euro, Mexican Peso, Ruble, Rupee, Saudi Riyal, Shekel, Swiss Franc, UK Pound, US Dollar, Yen, Yuan (map ISO 4217 codes, e.g. `INR` to `Rupee`) |
| `receiving_currency` | no | settlement currency, if different | same names |
| `payment_format` | yes | rail or channel | one of `ACH`, `Bitcoin`, `Cash`, `Cheque`, `Credit Card`, `Reinvestment`, `Wire` |

The format and currency lists are the ones the served models were trained on (`PAYMENT_FORMATS` and `CURRENCIES` in
`crates/rusetect-server/src/inputs.rs`); anything else is refused with `422`. Rails that have no exact match (UPI, IMPS,
Faster Payments, SEPA Instant) must be mapped to the nearest trained format, agreed with us and written down before the
pilot, and kept fixed: the models lean heavily on the format, so a changed mapping shifts scores (and drift will show
it). Mapping the same rail consistently matters more than the choice. Optional fields (`message`, `consent`,
`session`, `auth`) are described in [live.md](live.md) and [SESSION_AND_NETWORK.md](SESSION_AND_NETWORK.md); leave
them out at first. Hash account numbers before export if your data terms require it; use one salt for the whole
pilot so the same account always gets the same hash.

## 8. Watching it: `/api/health`, `/api/v2/live`, `/api/v2/drift`

| endpoint | key | read it for |
|---|---|---|
| `GET /api/health` | none | `ready` (engine loaded, store reachable), `durable` and `store`, `auth` (`configured keys` in a pilot), `drift` (`ok`, `warn`, `alert`; reported only, never changes `ready`) |
| `GET /api/v2/live` | `X-Analyst-Key` | the latest 50 live decisions: event id, action, band, risk, reasons, alert id, ledger seq and hash, latency |
| `GET /api/v2/drift` | `X-Analyst-Key` | `status`, `warming_up`, `reasons` (which check fired), `window.size`, score and input PSI, band rates against the reference |
| `GET /api/v2/alerts`, `/api/v2/audit` | `X-Analyst-Key` | the review queue; the hash-chained decision ledger and `chain_valid` |

The workspace view **Pilot health** (`/app#/pilot`) puts the three daily checks on one page, from the endpoints above:
drift (score and per-input PSI, band rates against the reference), flagged decisions per UTC day (to compare with the
agreed review budget) and server latency over the latest 50 live decisions. The console at `/console` shows the live
feed and drift status (it polls every 5 s). What to do on each signal is in
[RUNBOOK.md](RUNBOOK.md) sections 0 and 7.

## 9. Rebuild the drift reference from your own validation period

The shipped drift reference (`service/models/drift_reference.json`) is a profile of synthetic IBM AMLworld HI-Small
validation payments. Your traffic has different amounts, formats, currencies and account history, so **against the
shipped reference drift will alert from day one**; that alert says "this is not the synthetic data", not "your traffic
changed". The AMLSim holdout shows the effect (`results/drift_amlsim.json`).

Before reading drift status, rebuild the reference from a validation period of your own history (after the warm-up
months, before the pilot window), with [research/drift_reference.py](../research/drift_reference.py) as the template:
it scores the validation rows with the served engine and writes score and input bin edges, shares and band rates,
fitting nothing. Point its input at your exported history (it currently reads the HI-Small CSV layout and its
validation row range), write the result to `service/models/drift_reference.json` on your host, and restart. Do not
build it from the pilot window itself, and do not retune band thresholds on live traffic ([PILOT.md](PILOT.md):
thresholds are fitted on the earlier labelled months only).

## 10. Rehearsal

The whole path above, and the fixed-budget evaluators of [PILOT.md](PILOT.md), were rehearsed locally on synthetic
data by `tools/pilot_rehearsal.py`. What was measured, what worked and what broke is in
[PILOT_REHEARSAL.md](PILOT_REHEARSAL.md), generated from [results/pilot_rehearsal.json](../results/pilot_rehearsal.json).

## 11. What you get back

| what | where | when |
|---|---|---|
| A decision per payment: `action`, band, `risk`, reasons, alert id, ledger sequence and hash | the webhook response (logged by the sender), `GET /api/v2/live` | at once |
| An alert queue for anything above `allow`, with evidence packs and STR drafts (recommendations only) | `/app#/alerts`, `GET /api/v2/alerts`; [API.md](API.md) | at once |
| A hash-chained, signed decision ledger | `GET /api/v2/audit` (`chain_valid`) | at once |
| Pilot health: drift, flagged decisions per day, latency | `/app#/pilot` (section 8) | daily |
| Fixed-budget comparison with your current system: confirmed-case recall and yield at the same daily review budget, cases found by one side only, minutes to first review | `tools/pilot_evaluate.py`, `tools/pilot_queue.py` ([PILOT.md](PILOT.md)) | weekly as labels arrive, and at the read-out |
| The read-out: adopt, retrain on your data under the same rule, or stop | [PILOT_PROPOSAL.md](PILOT_PROPOSAL.md) | week 9 |

None of it changes a payment. The comparison is only as good as the outcome labels and their dates; unconfirmed
payments never count as hits.

## 12. Week by week

The phases are those of [PILOT_PROPOSAL.md](PILOT_PROPOSAL.md) (timeline) and [PILOT.md](PILOT.md) (steps); this is
the engineer's checklist for each week.

| week | partner | RuseTect | done when |
|---|---|---|---|
| 0 | NDA and data terms; security review of [THREAT_MODEL.md](THREAT_MODEL.md); pick the channel and agree the rail and currency mapping (section 7) | answer the review; write down the mapping | mapping and review signed off |
| 1 | host, secrets and keys (sections 2 and 6); export history; rebuild the drift reference (section 9) | build check, one signed test event (section 3) | `/api/health` ready with `configured keys`; test event in `/api/v2/live` |
| 2 | replay history with `--log replay.jsonl` (section 4); fix the daily review budget, holdout dates and label snapshot | freeze the success rule and the queue policy (`tools/pilot_freeze_policy.py`) before any label is joined | replay `failed` is 0; policy file and rule frozen and signed |
| 3 | switch the sender to the live copy of the channel | watch pilot health daily (section 8); runbook on any signal ([RUNBOOK.md](RUNBOOK.md)) | a full UTC day scored with no sender failures |
| 4 to 7 | send confirmed outcomes weekly with the date each was known | run the fixed-budget comparison on the frozen holdout; report alerts per day against the budget | weekly report shared; nothing retuned on live data |
| 8 | final outcome snapshot | final comparison, latency on your hardware ([SCALING.md](SCALING.md)) | report frozen |
| 9 | joint read-out | | decision recorded with the signed audit log |

## Where to go next

- [PILOT.md](PILOT.md): the evaluation protocol (freeze the success rule before reading results).
- [PILOT_PROPOSAL.md](PILOT_PROPOSAL.md): scope, timeline and success metrics.
- [RUNBOOK.md](RUNBOOK.md): incidents, rotation, rollback, drift.
- [THREAT_MODEL.md](THREAT_MODEL.md): trust boundaries and open gaps.
- [DEMO_SCRIPT.md](DEMO_SCRIPT.md): the 10-minute walkthrough, with the known limits.
- [PILOT_REHEARSAL.md](PILOT_REHEARSAL.md): the local rehearsal of this guide on synthetic data.
