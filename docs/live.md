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
  `v1=` values are accepted, so a secret can be rotated.

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
`/api/v2/assess`); `409` the same `event_id` is still being processed (retry after a minute); `503` secret not set or
durable store unreachable (nothing is reported as committed; retry).

## 4. Idempotency and retries

Retry on any network error or 5xx with the same body and `event_id` (and a fresh timestamp and signature). A known
`event_id` returns the stored result unchanged, with header `Idempotent-Replayed: true`, and is not scored or written
again. With Redis this holds across all function instances for 7 days; in memory it holds for the life of one instance.

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
   `durable` turns `false` when that command could not reach Redis.

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
  pair window and the context layer from account history. The message, session and lure signals and per-feature
  reasons are applied only in memory mode, and PRAMAAN's per-user history is kept per function instance.
- The ledger is hash chained within one server instance; with Redis, entries from several instances are ordered by
  `event_seq`, not chained to each other, and `allow` entries are not chained at all.
- Review actions on one live alert from two instances at the same moment are not locked: the last write wins. The
  pause-screen, re-verification and duress routes change only the instance's copy and are not written back.
- Redis calls are made while the server's state lock is held, so one instance handles one request at a time (on
  Vercel an instance serves one request at a time anyway).
- The per-client rate limit (`RUSETECT_RATE_LIMIT`, requests per 10 s) applies to the webhook too; raise it for a
  high-volume sender.
