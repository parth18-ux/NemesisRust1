# Live ingest: sending payment events to RuseTect

An outside system (core banking, payment switch, PSP) pushes each payment to RuseTect as it happens. In the same HTTP
request RuseTect verifies the signature, scores the payment, decides the action, opens a review alert when a person
must look, appends a ledger entry and commits the result. There is no background worker, so it runs on Vercel's free
tier as it is.

## 1. Create the shared secret

Generate a random secret (32+ bytes) and store it as `RUSETECT_WEBHOOK_SECRET` in the Vercel project
(Settings → Environment Variables), and give the same value to the sending system. Never commit it. Until it is set
(16+ characters), the endpoint answers `503`.

```sh
openssl rand -hex 32
```

## 2. Send an event

`POST /api/v2/events/payment` with:

- body: the `/api/v2/assess` body (payment fields, optional `message`, `consent`, `session`) plus `event_id`
  (your unique id, 1 to 128 characters) and, optionally, `auth`: a PRAMAAN request (`user_id`, `ts` and the
  `device`, `network`, `behaviour`, `transaction` objects, as for `/api/v2/authenticate`);
- `X-RuseTect-Timestamp`: the current time in epoch seconds;
- `X-RuseTect-Signature`: `v1=` + hex HMAC-SHA256 of `{timestamp}.{raw body}` with the secret. Several comma separated
  `v1=` values are accepted, and during a rotation a signature under `RUSETECT_WEBHOOK_SECRET_PREVIOUS` is accepted
  too (steps in `docs/RUNBOOK.md`, section 4).

Sign the exact bytes you send. Requests more than 5 minutes from the server clock are refused. The existing input
rules apply (`payment_format` and currencies from the trained set, ids up to 128 characters, amount above 0,
`timestamp` as `YYYY-MM-DD HH:MM` UTC).

### curl

```sh
URL=https://YOUR-PROJECT.vercel.app/api/v2/events/payment
BODY='{"event_id":"pay-0001","timestamp":"'"$(date -u '+%Y-%m-%d %H:%M')"'","from_bank":"070","from_account":"C123","to_bank":"011","to_account":"M456","amount_paid":9000,"payment_currency":"US Dollar","payment_format":"ACH"}'
TS=$(date +%s)
SIG=$(printf '%s.%s' "$TS" "$BODY" | openssl dgst -sha256 -hmac "$RUSETECT_WEBHOOK_SECRET" -hex | sed 's/^.*= //')
curl -sS "$URL" -H 'Content-Type: application/json' -H "X-RuseTect-Timestamp: $TS" -H "X-RuseTect-Signature: v1=$SIG" --data-raw "$BODY"
```

### Python

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

### Node

```js
const crypto = require("crypto");

async function send(event, url) {
  const body = JSON.stringify(event), ts = String(Math.floor(Date.now() / 1000));
  const sig = crypto.createHmac("sha256", process.env.RUSETECT_WEBHOOK_SECRET).update(`${ts}.${body}`).digest("hex");
  const r = await fetch(url, {method: "POST", body, headers: {
    "Content-Type": "application/json", "X-RuseTect-Timestamp": ts, "X-RuseTect-Signature": `v1=${sig}`}});
  if (!r.ok) throw new Error(`${r.status} ${await r.text()}`);
  return r.json();
}
```

## 3. Response

`200` with the committed decision:

| field | meaning |
|---|---|
| `event_id`, `seq` | your id; the store-wide sequence number |
| `action` | `allow`, `verify`, `hold` or `block` |
| `final_decision`, `payment_decision` | the engine's band names (`Approve`, `Step-up verification`, `Hold for analyst`, `Block + STR draft`) after and before the context layer |
| `risk`, `context_band`, `reasons` | engine risk (null when the L1 gatekeeper cleared it), context band, top reasons (in-memory mode) |
| `pramaan` | null, or the account-takeover decision (`allow`/`challenge`/`block`), its `decision_id` and whether it `raised` the action |
| `alert_id` | the review alert opened for verify/hold/block (null for allow); analysts act on it at `/api/v2/alerts/{id}/review` |
| `ledger` | `seq` (the entry's position in this server's hash-chained log) and `hash` of the ledger entry written for this decision; the entry carries the event's `seq` as `event_seq`. With Redis, `allow` decisions are written to `live:ledger` only, unchained, with `seq` null |
| `payment`, `received_at`, `latency_ms`, `source` | what was decided on, when, how long it took; `source` is `live` |
| `durable`, `store` | `true`/`redis` when committed to Redis; `false`/`memory` otherwise |

Errors: `401` bad or missing signature, or timestamp outside 5 minutes; `422` invalid body (same messages as
`/api/v2/assess`); `409` the same `event_id` is still being processed (the response carries `Retry-After: 60`, the
claim's lifetime); `429` rate limited; `503` secret not set, account state full, or durable store unreachable (the
response carries `Retry-After`; retry; section 4 covers the one case where the commit may already be applied).

## 4. Idempotency and retries

Retry on any network error, `409`, `429` or 5xx with the same body and `event_id`, and a fresh timestamp and
signature (a retry signed with the first attempt's timestamp is refused after 5 minutes). Do not retry other 4xx: fix
the request. A known `event_id` returns the stored result unchanged, with header `Idempotent-Replayed: true`, and is
not scored or written again. With Redis this holds across all function instances for 7 days; in memory it holds for the
life of one instance.

Every 5xx is safe to retry. When Redis confirms that the commit did not land, the claim on the `event_id` is released
and the retry is scored again. The account write saves the original account features and deduplicates per-account
totals (`seen:`). The first pair-window update saves the count and combined risk through the payment's own minute
(`pair-score:`). Both records expire after 26 h. With these records present, the retry uses the same payment-cascade
inputs even if another payment added to or pruned the pair window. With an expired or missing record, scoring uses
the available history for that input. The alert and desk-chain entries an attempt opened are taken back out when its commit did not land, so the retry
leaves one alert, one ledger entry and one decision, and the stored `ev:` record, the alert, its evidence pack and the
ledger agree. When the outcome of the commit is unknown, the claim expires after 60 s and the retry then gets either
the stored result or a fresh scoring, never a second commit; on the instance that lost the outcome, the earlier
attempt's desk-chain entries are then voided by a `live_decision_void` entry (the chain stays append-only and valid).
A success always means the decision and its feed, ledger and alert records are all written. The commit checks those
keys first and writes nothing when one has the wrong type (a `503`, claim released, the retry is processed afresh).
Otherwise it lists the records in `live:repair:{event_id}` before storing the decision; a record whose write still
fails stays listed and the answer is a `503`. Every replay completes the listed records first (each written once) and
answers with the stored result, or with a `503` again while the store cannot take them. Each claim is a token unique to its
request, and the commit and the release apply only while the token is still there: a request that outlives its claim
cannot commit over a retry or delete the retry's claim or result; it answers with the retry's result, a `409`, or a
`503` when nothing was committed.

Back off exponentially with full jitter, honouring `Retry-After` when it is larger:

```python
import http.client, random, time, urllib.error

def send_with_retry(event, url, attempts=8, base=1.0, cap=60.0):
    for n in range(attempts):
        try:
            return send(event, url)            # signs with a fresh timestamp each call
        except urllib.error.HTTPError as e:
            if e.code < 500 and e.code not in (409, 429):
                raise                          # 401, 413, 422: retrying will not help
            try:                               # seconds; an HTTP-date or garbage counts as 0
                after = min(max(float(e.headers.get("Retry-After") or 0), 0.0), 120.0)
            except ValueError:
                after = 0.0
            wait = max(after, random.uniform(0, min(cap, base * 2 ** n)))
        except (OSError, http.client.HTTPException, ValueError):  # network error, timeout, garbled reply: may be committed
            wait = random.uniform(0, min(cap, base * 2 ** n))
        if n < attempts - 1:
            time.sleep(wait)                   # no sleep after the last attempt
    raise RuntimeError(f"gave up on {event['event_id']}; queue it and alert")
```

After the last attempt, keep the event in the sender's own queue and alert: RuseTect has not committed it.

## 5. Watching it live

The Live Console (`/console`) polls `GET /api/v2/live` every 5 s, and not at all while its tab is hidden, and lists
real ingested events only, tagged `live`. The replay of recorded synthetic payments below it is tagged `demo`. A badge
shows the store: `durable: redis` or `not durable: in-memory`. With analyst keys configured, `/api/v2/live` needs an
`X-Analyst-Key`. In demo mode (no `RUSETECT_ANALYST_KEYS`) it is public: once `RUSETECT_WEBHOOK_SECRET` is set, anyone
can read the live feed (accounts and amounts of real events), so configure analyst keys before sending real payments.

Once this server's ledger holds a live decision (in memory: any event; with Redis: verify, hold or block, whose copy
on the instance would be wiped), `POST /api/v2/demo/reset` answers `409` (the console shows the message), so the
demo reset cannot wipe the alerts and ledger entries of real events.

## 6. Durable storage for free (Upstash Redis on Vercel)

Without a store, decisions live in the memory of one function instance and are lost on a cold start;
`/api/health` reports `"durable": false`. To make them durable at no cost:

1. Vercel dashboard → the RuseTect project → **Storage** (or **Marketplace**) → **Upstash** → **Redis** → create a
   database on the **Free** plan, in the region closest to the function.
2. **Connect** it to the project (all environments). This sets `KV_URL` / `REDIS_URL` (a `rediss://` URL) and the
   `KV_REST_API_*` variables. RuseTect uses the `rediss://` URL; the REST variables alone are not enough.
3. Redeploy. `/api/health` should now show `"durable": true, "store": {"kind": "redis"}`. `store.configured` says a
   Redis URL is set and `store.reachable` the result of this instance's last command (null before the first);
   `durable` turns `false` when that command could not reach Redis. `ready` is `true` when the engine is loaded and a
   configured Redis was not unreachable on that last command (health itself sends no command, to save the quota).

What is written, under the prefix `RUSETECT_REDIS_PREFIX` (default `rusetect:`): `live:ev:{event_id}` (result, 7 days),
`live:feed` (latest 200), `live:alerts` (alert by id), `live:ledger` (ledger entries, latest 100,000 kept), `live:seq`,
and the per-account state of `scale/redis_state.rs` (`a:`, `p:`, `wo:`, `wi:`, `pr:`, `seen:` keys, 24 to 48 hour
windows). `live:alerts` has no expiry: it holds one entry per verify/hold/block alert until you delete it, so archive
and `HDEL` final (released or blocked) alerts when it grows. Watch the free plan's daily command and storage limits; each
event costs a few tens of commands, and each console poll one.

Any instance can review a live alert: `GET /api/v2/alerts`, the review action, the evidence pack and the STR draft read
the alert from `live:alerts`, and a review action writes the updated alert back.

## Limits to know

- With Redis, scoring uses the bank-scale path (the streaming workers' code): the payment cascade, hardening, the 24 h
  pair window and the context layer from account history. The message, session and lure signals, per-feature
  reasons and temporal-motif reasons are applied only in memory mode, and PRAMAAN's per-user history is kept per function instance.
- The ledger is hash chained within one server instance; with Redis, entries from several instances are ordered by
  `event_seq`, not chained to each other, and `allow` entries are not chained at all.
- Review actions on one live alert from two instances at the same moment are not locked: the last write wins. The
  pause-screen, re-verification and duress routes change only the instance's copy and are not written back.
- With Redis, scoring and Redis calls run outside the server's state lock on a small connection pool
  (`RUSETECT_LIVE_POOL`, default 2 x logical CPUs); payments that share a payer or a payee are scored one at a time
  (both accounts' stripes), and the state lock is held only for PRAMAAN, the alert and the desk chain
  (`docs/SCALING.md` section 3c). Reading, listing and reviewing live alerts and the live feed also run their Redis calls
  outside the state lock. On Vercel an instance serves one request at a time anyway.
- Known limits, left as they are: when a commit's outcome is unknown and the retry lands on another instance, the
  first instance's desk-chain entries for the attempt stay unvoided there (its alert copy is dropped; live alerts are
  read from Redis); a retry rescored after `seen:` expired (26 h) is scored from current state; PRAMAAN's per-user
  history and the drift monitor see each scored attempt; the drift monitor is fed by demo replay
  traffic as well as live events; with Redis an instance holds two engines in memory (the desk's and the stateless one
  the Redis path scores with).
- The per-client rate limit (`RUSETECT_RATE_LIMIT`, requests per 10 s) applies to the webhook too and the webhook is not
  exempt; a high-volume sender needs it raised (`docs/RUNBOOK.md` section 8).
