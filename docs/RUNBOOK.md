# Runbook

What to do when the live service misbehaves. Each section names the signal, the commands and the files involved. No
numbers are typed here: limits and thresholds are in the code or files named, and measurements in `results/`.

Deployment as shipped: one Vercel project (`vercel.json`, `api/rusetect.rs`), optional Upstash Redis through `KV_URL`
or `REDIS_URL` (`docs/live.md` section 6). For a pilot inside a bank the same binary runs from `deploy/` and the same
steps apply, with the bank's own process manager in place of Vercel.

## 0. Signals and where logs are

| Signal | Where |
|---|---|
| `GET /api/health`: `ready`, `durable`, `store.configured`, `store.reachable`, `store.note`, `engine_loaded`, `auth` | the service itself (`health` in `crates/rusetect-server/src/lib.rs`) |
| `GET /api/v2/drift` (any key): `status`, `reasons`, `score_psi`, `features`, `bands`; `/api/health` `drift` | the service itself (`crates/rusetect/src/drift.rs`) |
| Webhook status codes (`401`, `409`, `429`, `503`) and their `detail` | the sender's logs; the request log in Vercel |
| Process log (stderr): start-up failures, `live store: ...`, refused API keys (`auth_failure_line`, key fingerprint only) | Vercel dashboard → project → Logs, or `vercel logs <deployment-url>`; under `deploy/`, `docker compose logs api` |
| Redis commands, storage, errors | Upstash console → the database → Usage and Data Browser; `redis-cli -u "$KV_URL" INFO` |
| Deploys and their commits | Vercel dashboard → Deployments, or `vercel ls` |

Logs are not exported to a SIEM (open gap, `docs/THREAT_MODEL.md`). Vercel keeps runtime logs for a short time only, so
copy what an incident needs into its post-mortem while it is still there.

## 1. Redis down

Signals: the webhook answers `503` with `detail` starting `durable store unavailable:` and a `Retry-After` header;
`/api/health` shows `"ready": false, "durable": false, "store": {"reachable": false}`; `/api/v2/live` answers `503`.

Behaviour (`crates/rusetect-server/src/live.rs`, `ingest_redis`): the webhook fails closed. An event whose claim, scoring or
commit fails is not reported as committed; its claim is released (or expires after `CLAIM_TTL_S` when the commit's
outcome is unknown), so the sender's retry is scored and committed once Redis is back. Nothing falls back to memory
while Redis is configured. Each request tries to reconnect, so recovery needs no restart. Test:
`redis_outage_fails_closed_and_recovers` and, with concurrent senders, `concurrent_outage_fails_closed`
(`crates/rusetect-server/tests/live.rs`).

Steps:
1. Confirm: `curl -sS https://<project>.vercel.app/api/health` and `redis-cli -u "$KV_URL" PING`.
2. Check the Upstash status page and the database in the Upstash console (region, paused, deleted).
3. Tell senders their retries are expected to fail for now; they must keep events queued (`docs/live.md` section 4).
4. When Redis answers again, health returns `"ready": true` on the next request; watch the senders' queues drain.
5. Do not unset `KV_URL` to "fix" it: the service would then accept events into one instance's memory and lose them on
   a cold start.

## 2. Redis full or over quota (Upstash free tier)

Signals: `503` with `detail` holding the Redis error text (Upstash's request or storage limit message, or `OOM`);
`/api/health` can still say `"ready": true`, since `ready` tracks connection failures only. Upstash console → Usage shows
the daily commands or storage at the plan's limit.

Steps:
1. Read the exact error from the `503` `detail` or the Vercel log.
2. Commands over quota: they reset at the provider's daily boundary. Meanwhile reduce load: close idle consoles (each
   open console polls `/api/v2/live`, `docs/live.md` section 5), and ask senders to slow down. If the pilot needs more,
   move the database to a paid plan in the Upstash console; no code change.
3. Storage full: `live:alerts` has no expiry. Export, then `HDEL` final alerts (status `released` or `blocked`):
   `redis-cli -u "$KV_URL" HGETALL rusetect:live:alerts > alerts-$(date +%F).jsonl`, then
   `redis-cli -u "$KV_URL" HDEL rusetect:live:alerts <alert_id> ...`. `live:ledger` and `live:feed` are already trimmed;
   `live:ev:*` and account keys expire on their own.
4. Keep the eviction policy `noeviction` (as in `deploy/docker-compose.yml`): evicting `live:ev:*` keys would break
   idempotency and could commit an event twice.

## 3. Webhook signature failures spike

Signals: many `401` on `POST /api/v2/events/payment` in the Vercel request log or the sender's logs. The `detail`
says which check failed: `bad signature`, `missing X-RuseTect-Signature`, or a timestamp message.

Steps:
1. Timestamp messages: the sender's clock is off or it retries with the first attempt's timestamp. Fix NTP on the
   sender; retries must re-sign (`docs/live.md` section 4).
2. `bad signature` from one known sender after a change: a secret mismatch (a rotation half done, section 4) or the
   sender signing different bytes than it sends (re-serialising JSON after signing).
3. `bad signature` from unknown sources: treat as an attack on a public endpoint. The rate limit applies
   (`RUSETECT_RATE_LIMIT`); add a Vercel Firewall rule for the source; if the secret may have leaked, rotate it now
   (section 4). Refused requests are not scored or stored.

## 4. Secret and key rotation

Webhook secret (no downtime; `events_payment` in `live.rs`, test `previous_secret_is_accepted_during_rotation`):
1. `openssl rand -hex 32` for the new secret.
2. In Vercel → Settings → Environment Variables, set `RUSETECT_WEBHOOK_SECRET_PREVIOUS` to the current value and
   `RUSETECT_WEBHOOK_SECRET` to the new one (or `vercel env rm` / `vercel env add` for each), then redeploy. Both
   secrets are now accepted.
3. Give the sender the new secret; it switches when ready (it may send two `v1=` signatures meanwhile).
4. When the sender's 401 count is zero on the new secret, remove `RUSETECT_WEBHOOK_SECRET_PREVIOUS` and redeploy.
   For a leaked secret skip the overlap: set only the new secret and redeploy at once.

API keys (`RUSETECT_ANALYST_KEYS`, a JSON object `{key: {"analyst": name, "role": role}}`, parsed by `parse_keys` in
`crates/rusetect/src/security.rs`):
1. Generate a new key (`openssl rand -hex 24`) and add it next to the old one for the same analyst and role.
2. Redeploy (keys are read at start-up), hand out the new key, confirm the analyst uses it.
3. Remove the old key and redeploy. A refused old key then shows in the log as an `auth_failure_line` with its
   fingerprint, which tells you who has not switched.
4. A leaked key: remove it and redeploy at once; check the log for its fingerprint to see what it was used for.

Other secrets (`RUSETECT_SIGNING_KEY`, `RUSETECT_CONSORTIUM_SECRET`, `RUSETECT_HASH_SALT`) have no overlap mode.
Changing the signing key makes earlier verdict signatures unverifiable with the new public key: publish the old public
key (`GET /api/v2/keys/public` before the change) with its date range. Changing the salt or consortium secret changes
every hashed identifier, so do it only with every party at once.

## 5. Deploy rollback on Vercel

Signals: errors or `"engine_loaded": false` / `"ready": false` right after a deploy; a failing smoke test.

Steps:
1. `vercel ls` to find the last good production deployment (or Dashboard → Deployments).
2. `vercel rollback <deployment-url>` (or Dashboard → that deployment → Instant Rollback). It switches the production
   alias back with no rebuild. Environment variables are the ones saved with that deployment.
3. Check `/api/health` and send one signed test event (`docs/live.md` section 2).
4. Production stays pinned to the rolled-back deployment until you promote another (`vercel promote <url>`). Fix
   forward on a branch, then promote.
5. Redis needs no rollback: keys are compatible across these versions. If a release changed key layouts, its entry in
   `docs/decision.md` says so.

## 6. Incident checklist

1. **Detect**: a signal from section 0. Note the start time, the deployment (`vercel ls`) and the first error text.
2. **Contain**: stop the harm first. Rotate a leaked secret or key (section 4); roll back a bad deploy (section 5);
   block an abusive source with a Vercel Firewall rule. The webhook already fails closed when Redis is down.
3. **Communicate**: tell each sender what is failing, since when, and that queued events must be retried with the
   same `event_id`; tell analysts if alerts are delayed. For a pilot, tell the bank's technical contact and
   fraud-risk owner named in `docs/PILOT_PROPOSAL.md`. Say what is known, not guesses.
4. **Recover**: confirm `/api/health` `ready`, watch senders' queues drain, and check the ledger for gaps: every
   event the sender reports sent should be in `live:ledger` (`LRANGE`) or answered by a replay of its `event_id`.
5. **Post-mortem** (within a week, blameless): timeline, cause, what detected it, what slowed recovery, actions with
   owners. Add a test for the failure where code changed, and a `docs/decision.md` entry when a decision changed.

## 7. Drift alert

Signals: `/api/health` shows `"drift": "warn"` or `"alert"` (`ready` is not affected); `GET /api/v2/drift` lists the
checks that fired in `reasons` and ranks the score and inputs by PSI in `ranking`; a change of status is logged as a
`drift_status` JSON line (wiring, `traffic`, `status_since` and the pinned reference: [DRIFT.md](DRIFT.md)); the
console's Drift panel turns amber or red.

What it measures (`crates/rusetect/src/drift.rs`, thresholds as constants there and in `docs/decision.md` 8ag): over
the last N payments this instance scored (N in `window.capacity`; `RUSETECT_DRIFT_WINDOW` overrides the default), the
PSI of the served risk and of five payment inputs against HI-Small validation (`service/models/drift_reference.json`),
and each band's alert rate over its validation rate. Each instance has its own window: on Vercel with Redis, different
instances can disagree, and a cold instance reports `warming_up` until it has enough payments. In Redis mode the window
holds risk and band only, so input PSI is not available there.

Steps:
1. Read `reasons`. Input PSI (amount, format, currency, cross bank, round amount) means the traffic itself changed:
   a new channel, a new customer segment, a mapping or currency error at the sender. Check the sender first.
2. Score PSI or band ratios with inputs in range means the scores moved for other reasons: account history is thin
   (a new deployment, a restart without history; the console demo shows this), or the population differs from the
   training generator. The AMLSim holdout is the example: `results/drift_amlsim.json`, `docs/decision.md` 8af.
3. A band rate well above validation means analysts would be flooded and the thresholds no longer mean what they meant
   on validation. Tell the fraud-risk owner. Do not retune thresholds on live traffic (thresholds are fitted on
   validation only). In a shadow pilot (`docs/PILOT.md`) nothing acts on the score, so note the period and treat its
   alerts as out of domain when comparing with the bank's system.
4. Short windows see time-of-day mix: a warn after a batch of scheduled payments can be noise
   (`results/drift_validation.json` gives the rate on validation itself). Act on a status that persists.
5. Record the episode (start, reasons, action) in the incident log. A lasting shift needs a model trained on that
   population (`docs/RETRAINING.md`) and a fresh check under a rule written first.

## 8. A high-volume sender gets `429`

Signals: the webhook answers `429` to one sender while `/api/health` is `ready`.

The per-client rate limit (`RUSETECT_RATE_LIMIT`, requests per 10 s per client, default in `lib.rs`) applies to the
webhook like every other route, and the default is far below what one node's webhook can take
(`results/e2e_bench_s1.json`, `docs/SCALING.md` section 3c). The webhook is deliberately not exempt.

Steps:
1. Confirm the `429` comes from one known sender (its source address or the right-most `X-Forwarded-For` entry behind
   a trusted proxy, `docs/THREAT_MODEL.md`).
2. Set `RUSETECT_RATE_LIMIT` above that sender's peak requests per second times 10, with headroom, and redeploy. On
   Vercel each instance counts separately, so the limit is per instance.
3. Under a self-hosted deploy, `RUSETECT_LIVE_POOL` sets how many webhook requests do Redis work at once (default
   2 x logical CPUs). Leave the tokio worker count alone.
