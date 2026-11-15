# Threat model

STRIDE review of the components RuseTect actually ships, for a pilot security reviewer. Each row names the threat, the
control that exists today (with the file that holds it) and what is still open. Written from the code, not from a
test: there has been **no external penetration test or independent security review**.

STRIDE: **S**poofing, **T**ampering, **R**epudiation, **I**nformation disclosure, **D**enial of service, **E**levation
of privilege.

## Trust boundaries

1. Sending system (bank core, switch, PSP) to the signed webhook `POST /api/v2/events/payment`.
2. Analysts and channel apps to the API, by `X-Analyst-Key`.
3. The server to its store (memory, or Redis through a `rediss://` URL).
4. One bank to the consortium ledger.
5. The public internet to the Vercel demo, which runs in demo mode.

## Components

| Component | Threat | Control that exists | Open gap |
|---|---|---|---|
| Signed webhook (`crates/rusetect-server/src/live.rs`, `docs/live.md`) | S, T: forged or altered events | HMAC-SHA256 over `{timestamp}.{raw body}`, checked in constant time (`verify_slice`); endpoint answers `503` until `RUSETECT_WEBHOOK_SECRET` (16+ characters) is set | Symmetric secret: whoever holds it can send events. No mTLS, no sender IP allowlist |
| | R, T: replayed events | Timestamp must be within 5 minutes of the server clock; a known `event_id` returns the stored result and is not scored again (in memory per instance, with Redis for 7 days) | Without Redis, idempotency lasts only for the life of one instance |
| | E: rotation | Several comma separated `v1=` signatures accepted, and `RUSETECT_WEBHOOK_SECRET_PREVIOUS` is accepted next to the current secret during a rotation (steps in `docs/RUNBOOK.md`) | No secret is configured on the public deployment yet, so the webhook is off there |
| API keys and roles (`crates/rusetect/src/security.rs`: `parse_keys`, `gate`, `allowed`) | S, E: callers acting beyond their role | Roles `viewer`, `channel`, `analyst`, `supervisor`; GET for any role, writes need `channel`, review and consortium publish need `analyst` or `supervisor`, demo reset and replay need `supervisor`; an unlisted write route falls to the channel rule. Keys of 16+ characters, checked at start-up. Key lookup compares every configured key in constant time (`lookup` in `security.rs`). A refused key is logged with method, path, client and the first 8 hex characters of its SHA-256, never the key (`auth_failure_line` in `lib.rs`) | Keys are static strings in an environment variable: no expiry, no per-key revocation without a redeploy, no SSO or MFA. The log line goes to stderr only, not to a SIEM |
| | S: running with the demo keys by mistake | Fail closed: `check_config` (`crates/rusetect-server/src/lib.rs`) refuses to start without `RUSETECT_ANALYST_KEYS` unless `RUSETECT_DEMO=1`, and with analyst keys it also requires `RUSETECT_SIGNING_KEY`, `RUSETECT_CONSORTIUM_SECRET`, `RUSETECT_HASH_SALT` | `RUSETECT_HASH_SALT` falls back to a published demo salt when unset in demo mode (`context_rules.rs`) |
| Rate limit (`RateLimiter` in `security.rs`, `rate_limit` in `lib.rs`) | D: request floods | Per client limit, `RUSETECT_RATE_LIMIT` requests per 10 s (default in `lib.rs`); the client is the TCP peer, or the right-most `X-Forwarded-For` entry only behind a trusted proxy (`RUSETECT_TRUST_PROXY=1` or Vercel). Account state is capped (`RUSETECT_MAX_ACCOUNTS`) and refuses new accounts with `503` when full. Request bodies are capped at `MAX_BODY_BYTES` (2 MiB, `lib.rs`), answered `413` before a handler runs | Counter is in memory per instance, so on Vercel each instance counts separately and a distributed flood is not limited. No WAF rule is configured |
| Review desk, four eyes (`crates/rusetect/src/review.rs`) | E: one person blocks a customer | The model never blocks on its own; `approve_block` needs two different analysts ("four-eyes" check) | In demo mode the analyst keys are published (`DEMO_KEYS`, `/api/v2/demo/analysts`), so anyone can play both analysts |
| | R, T: replayed or denied review actions | Every action needs a fresh `X-Nonce` (`NonceCache` in `security.rs`); every event is appended to a hash-chained audit log, `verify_chain` detects an edit | The audit chain is unsigned SHA-256: someone with write access to the store can rewrite it and recompute every hash. Not anchored outside the server. With Redis, two instances acting on one alert at once are not locked: the last write wins (`docs/live.md`, Limits) |
| Hash-chained decision ledger and signed verdicts (`Signer` in `security.rs`, `live.rs`) | T, R: altered or disowned decisions | Each assess result is signed with Ed25519 over canonical JSON; anyone can check it with `GET /api/v2/keys/public` and `/api/v2/verify`. Live decisions append a hash-chained ledger entry | Chained only within one instance; with Redis, entries from several instances are ordered by `event_seq` and `allow` entries are not chained (`docs/live.md`). Without `RUSETECT_SIGNING_KEY` (demo) the key is ephemeral per process, so old signatures cannot be checked after a restart |
| Consortium ledger (`crates/rusetect/src/consortium.rs`) | S, T: forged or edited mule entries | Each entry signed with Ed25519 by the publishing bank and chained to the previous hash; `verify` catches an edit even when the hashes are recomputed (unit test) | All member banks and their keys are created inside one process: a simulation of a consortium, not a multi-party network. Member keys are generated at start-up, so they do not survive a restart. Bounded, in memory |
| | I: identifiers leak between banks | Identifiers are shared only as an HMAC with `RUSETECT_CONSORTIUM_SECRET` | Every member holds the secret, so a member can test guesses of low-entropy identifiers (account numbers) offline. No legal or governance framework for sharing exists |
| Redis store (`crates/rusetect-server/src/scale/`, `docs/live.md` section 6) | I, T: store read or altered | `rediss://` (TLS) URL; keys under a prefix; nothing reported as committed when Redis is unreachable (`503` with `Retry-After`; the claim is released so the retry is processed once Redis is back); `/api/health` reports `ready` | **Upstash is not configured** on the public deployment, so decisions there are in memory and lost on a cold start. At-rest encryption, backups and region are the provider's settings, unreviewed. No backup or restore test. `live:alerts` has no expiry |
| | D: free-tier quota exhausted | Console polls every 5 s and stops while the tab is hidden; steps in `docs/RUNBOOK.md` section 2 | No alert when the daily command quota runs out; `ready` in `/api/health` tracks connection failures only, not quota errors |
| Vercel demo (`api/rusetect.rs`, `vercel.json`) | I: anything sent to it is public | Demo mode is explicit (`RUSETECT_DEMO=1`); the demo reset answers `409` once a live decision is in the ledger | **The demo is public by design**: published keys, every route open, CORS `*` by default (`CORS_ORIGINS`), and if a webhook secret were set the live feed would show real accounts and amounts to anyone. Never send real data to it |
| PRAMAAN account-takeover check (`crates/rusetect/src/pramaan.rs`, `/api/v2/authenticate`) | S: caller lies about device, network or behaviour signals | Signals are inputs from the bank's channel; the policy picks a step-up the likely attack cannot pass (no SMS code after a SIM swap). Remembered users, devices and IPs are bounded | Trusts the calling channel: a compromised channel app can send clean signals. Per-user history is per instance. Hindi reasons await review by a native speaker |

## Cross-cutting gaps (to close before handling a bank's data)

- No external penetration test or independent security review.
- No log export to a SIEM, no backup and restore test. The incident runbook (`docs/RUNBOOK.md`) is written but has
  not been exercised in a drill.
- Secrets live in environment variables; no managed key store or HSM, no scheduled rotation (manual steps in
  `docs/RUNBOOK.md` section 4).
- No written data processing terms, retention schedule beyond the context layer's message expiry, or DPDP Act
  assessment (see `docs/OUTREACH.md` for the checklist).

## Mitigated in phase B (hardening and operations)

Gaps from the first version of this review that are now closed in code, each with a test:

| Was open | Now | Test |
|---|---|---|
| Key lookup was a map lookup, not a constant time compare | `lookup` in `crates/rusetect/src/security.rs` checks every key with `subtle::ConstantTimeEq`, no early exit | `key_lookup_is_exact` (`security.rs`) |
| Request body size relied on axum's default limit | `DefaultBodyLimit::max(MAX_BODY_BYTES)` in `build_app` (`crates/rusetect-server/src/lib.rs`) | `oversized_body_is_refused_before_the_handler` (`crates/rusetect-server/tests/api.rs`) |
| Refused API keys left no trace | `require_key` logs `auth_failure_line` (key fingerprint, never the key) in `lib.rs` | `auth_failure_line_never_holds_the_key` (`lib.rs`) |
| No dependency vulnerability scan in CI | `cargo audit` step in `.github/workflows/ci.yml` (a RustSec vulnerability fails the build) | the CI step itself |
| A 503 from the webhook gave the sender no retry hint | every `503` carries `Retry-After` (`ApiError::into_response` in `lib.rs`); retry and backoff guidance in `docs/live.md` section 4 | `redis_outage_fails_closed_and_recovers` (`tests/live.rs`) |
| Redis outage behaviour was untested end to end | the webhook fails closed while Redis is down, `/api/health` says `"ready": false`, and the refused event is committed on retry once Redis returns | `redis_outage_fails_closed_and_recovers` (`tests/live.rs`) |
| Webhook secret rotation needed the sender to switch at the same moment as the server | `RUSETECT_WEBHOOK_SECRET_PREVIOUS` is accepted next to the current secret (`events_payment` in `live.rs`) | `previous_secret_is_accepted_during_rotation` (`tests/live.rs`) |
| No incident response runbook | `docs/RUNBOOK.md`: Redis down or over quota, signature failure spike, rotation, Vercel rollback, incident checklist, log locations | none (a document) |

For a shadow pilot the recommended deployment is the one in `docs/PILOT.md`: one binary inside the bank's network, analyst
keys and all production secrets set, no demo mode, nothing sent out.
