# API: website endpoints

The endpoints the website's dashboard, transaction, network, alert and case pages read and write. The scoring,
review, evidence and model routes are listed in `/docs` (Swagger UI, `/openapi.json`); the signed webhook is in
[live.md](live.md). Code: `crates/rusetect-server/src/intel.rs` (reads), `workflow.rs` (case work) and
`crates/rusetect/src/narrative.rs` (the narrative draft). Tests: `crates/rusetect-server/tests/intel.rs` and `intel_auth.rs`.

Nothing here scores a payment or runs a second model. Every value is read from what the engine, the live store and the
review desk already hold, and each response says where it came from.

## Access

| Routes | Demo mode | With `RUSETECT_ANALYST_KEYS` |
| --- | --- | --- |
| `GET /api/v2/dashboard`, `/alerts`, `/transactions`, `/transactions/{id}`, `/graph`, `/investigations`, `/investigations/{id}`, `/investigations/{id}/narrative`, `/messages`, `/whoami` | open (`/whoami` needs a key) | `X-Analyst-Key` of any role (viewer, channel, analyst, supervisor); else `401` |
| `POST /api/v2/alerts/{id}/triage`, `/investigations`, `/investigations/{id}/actions` | a demo analyst or supervisor key | analyst or supervisor key; viewer and channel get `403`, no key `401` |

Writes also need a fresh `X-Nonce` (8 to 128 characters, not seen in the last 10 minutes; `409` otherwise), as a review
does. Within the analyst roles: an analyst can assign work to themselves only, a supervisor to any analyst or
supervisor (`403` / `400`); reopening a closed investigation needs a supervisor. All routes are rate limited like the
rest of `/api`.

## What is stored

Three kinds of stored payment decision are read (`source` in every row):

| `source` | What | Kept |
| --- | --- | --- |
| `live` | events ingested by `POST /api/v2/events/payment`, approved ones included | memory: every event of the process, up to `RUSETECT_MAX_EVENTS` (default 100,000; a new event is then refused with 503). Redis: the newest 200 are listed (the feed); every flagged one stays as an alert; any event is found by id for 7 days |
| `assess` | alerts opened by `POST /api/v2/assess` | flagged payments only; an approved `/assess` or `/payment` call is counted by the engine but not stored |
| `demo_stream` | the Live Console replay of recorded IBM AMLworld (synthetic) payments | the latest 60 escalated rows; `demo: true` on every row |

Every list response carries `window` (store kind, live events ingested, how many were read, rows by source) and `data`:
`live`, `demo_stream`, `mixed` or `none`. A page must show `demo_stream` rows as demonstration data.

`band` is the action of the final decision: `allow`, `verify`, `hold` or `block`. A `block` band is a recommendation to
the review desk: the payment's alert stays `open` until two analysts approve the block (`/review`). No route here
blocks anything.

## GET /api/v2/dashboard

Query: `bucket` = `hour` (default) or `day`.

Each metric is `{"value": ..., "source": "..."}`.

- `data`: `kind`, `live_events`, `demo_payments_replayed`.
- `payments`: `scored` and `by_decision` (engine counters, this instance), `seen` (scored plus history loaded without
  scoring), `live_events` (store sequence), `demo_replayed`, `stored`, `flagged`, `by_band` (stored decisions read).
- `alerts`: `total`, `open` (not yet released or blocked), `closed`, `by_status`, `by_priority`, `unassigned`.
- `investigations`: `total`, `by_status`, `by_priority`, `sla_breached`.
- `entities`: `flagged_payees` (distinct payees named in alerts), `ring_cases`, `accounts_tracked`, `campaigns`.
- `typologies`, `reason_codes`: top ten motif names and SHAP reason features among flagged stored decisions.
- `messages`: the lure detector's counters.
- `activity`: `series` of up to 72 buckets by payment timestamp, each `{t, total, allow, verify, hold, block}`.
- `detection`: `demo_stream_truth` (tp / fp / fn against the synthetic labels of the replay, null without a replay);
  `confirmed_fraud` is null: no confirmed-fraud outcome is stored.
- `health`: engine, hardening, message model, PRAMAAN, sanctions and payee check loaded; store; `ready`; version;
  ruleset; `automated_blocking: false`.
- `drift`: the monitor's status (`GET /api/v2/drift` has the detail).

## GET /api/v2/transactions

Query (all optional): `account`, `bank` (either side of the payment; together they must match the same side), `band`,
`source`, `typology` (a motif name: `cycle`, `scatter_gather`, `pass_through`, `fan_in`, `fan_out`), `min_amount`,
`max_amount`, `from`, `to` (`YYYY-MM-DD` or `YYYY-MM-DD HH:MM`, UTC, by payment timestamp), `limit` (default 50, at
most 200), `offset`. A bad value is `422`.

Returns `{total, offset, limit, data, transactions, window}`, newest payment first. A row: `id`, `source`, `demo`,
`timestamp`, `from_bank`, `from_account`, `to_bank`, `to_account`, `amount`, `currency`, `format`, `risk`, `band`,
`decision`, `typologies`, `reasons`, `alert_id`, `alert_status`; live rows add `event_id`, `seq`, `received_at`,
`payment_decision`, `context_band`, `pramaan`, `ledger`, `store`; demo rows add `case_id` and `synthetic_label`.

`id` is the event id (live), the alert id (assess) or `D-` plus a hash of the feed row (demo).

## GET /api/v2/transactions/{id}

`404` for an unknown id. Returns:

- `transaction`: the row above.
- `risk`: `score`, `band`, `decision`, and the `verify` / `hold` / `block` thresholds of `cascade_config.json`.
- `explanations`: `model_reasons` (tree-SHAP contributions of the L2 XGBoost model), `motif_reasons` (the temporal
  motifs the payment completed), `available`, and a `note` saying why when there are none (an approved payment; Redis
  mode, whose bank-scale path returns no reason codes).
- `model`: `service_version`, `ruleset`, `trained_on`, `hardened`. The model files carry no version of their own.
- `alert`: status, shown decision, triage fields, investigation id and actions, or null.
- `related_accounts`: up to 50 accounts one stored payment away from the payer or the payee, with the relation,
  payment count, total and flagged count.
- `beneficiary_history`: for the payee: payments, distinct payers, total, flagged, first and last seen, the newest 50.
- `timeline`: this instance's ledger entries naming the event or its alert.

## GET /api/v2/graph

Query: `account` (required), `bank`, `depth` (1 to 3, default 1), `from`, `to`, `limit` (accounts, default 100, at most
300). `404` when no stored payment names the account.

- `nodes`: accounts (`id` `account:<bank>:<account>`, `depth`, `seed`, `sent`, `received`, `flagged_payments`, up to 10
  `alerts`, `indicators`) and banks (`bank:<code>`).
- `edges`: one `payment` edge per stored payment, directed payer to payee, with `amount`, `currency`, `timestamp`,
  `band`, `risk`, `typologies` and `evidence` (`source`, `transaction`, `event_id`, `alert_id`, `ledger`, `demo`); and
  one `held_at` edge from each account to its bank.
- `counts`, `truncated` (the account or the 1,000 payment bound was hit), `limits`, `data`, `window`.
- `indicators` on an account are `motif` (the motif names of its flagged payments, with up to five payment ids as
  evidence) and `ring_case` (the engine's ring linker, this instance). They are leads for an analyst: the response's
  `disclaimer` says that neither a link nor an indicator is a finding, and a page must not present them as one.
- `not_available`: devices, customers and merchants. Stored payments name accounts and banks only.

## GET /api/v2/alerts

Without a query (or with `status` only) the answer is what it always was: a plain array of alerts, newest first. Each
alert now also carries `demo`: `true` for an alert opened by `POST /api/v2/assess` on a demo-mode instance (public demo
keys, replays and fictional fixtures), `false` for every alert from the signed webhook and on an instance with
configured keys. The website labels only the `demo: true` rows as demonstration data.

Any of these parameters returns one page, searched, filtered and sorted by the server:

| Parameter | Meaning |
| --- | --- |
| `q` | text in the alert id, payer or payee (`bank:account`), event id, typology, assignee, investigation id or shown decision (case-insensitive, at most 200 characters) |
| `status` | the decision status (`open`, `held`, `customer_verification`, `block_pending_second_approval`, `released`, `blocked`, ...) |
| `band` | `allow`, `verify`, `hold`, `block` |
| `state`, `priority`, `assigned_to` | the triage fields; `priority=none` and `assigned_to=unassigned` find the unset ones |
| `demo` | `true` or `false` |
| `sort`, `order` | `time` (payment time, the default), `score`, `id`, `priority`, `assignee`, `status`, `source`, `typology`, `entity`, `txn`; `asc` or `desc` (default `desc` for time, score and priority) |
| `limit`, `offset` | page size 1 to 200 (default 50) and start |

Answer: `alerts` (the page), `total` (matching), `all` (before the filters), `offset`, `limit`, `sort`, `order`,
`assignees` (every assignee, for a filter list) and `demo` (how many matching alerts are demo). A bad `sort`, `order`,
`demo` or a non-number `limit` or `offset` is a `422`.

## GET /api/v2/whoami

The analyst id and role behind the presented `X-Analyst-Key` (`{analyst, role, demo_mode}`), so the website can
disable what the role or the four-eyes rule would refuse (an analyst who approved a block cannot give the second
approval; the server refuses it anyway). `401` without a known key. The key itself is never returned.

## POST /api/v2/alerts/{id}/triage

Body, at least one of: `assignee`, `priority` (`low`, `medium`, `high`, `critical`), `state` (`new`, `in_progress`,
`escalated`, `done`), `sla_hours` (above 0, at most 2160), `comment` (at most 1,000 characters).

The fields are stored under the alert's `workflow` (`assignee`, `priority`, `state`, `sla_due`); the change and the
comment are added to its `actions`, and the evidence pack carries both. The alert's decision `status` is not touched.
The first priority sets `sla_due` from the desk default (critical 4 h, high 24 h, medium 72 h, low 168 h; a default of
this desk, not a regulatory deadline) unless `sla_hours` is given. Returns the alert.

## Investigations

`POST /api/v2/investigations`: body `title` (1 to 200 characters), `alert_ids` (1 to 50, each alert in one
investigation at a time: `409`), and optionally `summary`, `priority` (default `medium`), `assignee`, `sla_hours`.
Returns the investigation: `case_id` (`INV-00001`), `status`, `priority`, `assignee`, `alert_ids`, `created_by`,
`created_at`, `updated_at`, `sla_due`, `sla_breached`, `comments`, `history`, `closure`.

`GET /api/v2/investigations`: query `status`, `assignee`, `priority`, `limit` (default 100, at most 500).

`GET /api/v2/investigations/{id}`: the investigation, its alerts as they are now (status, payment, triage fields, the
evidence link) and its ledger entries.

`POST /api/v2/investigations/{id}/actions`: body `action` and its field.

| `action` | Field | Rule |
| --- | --- | --- |
| `assign` | `assignee` | analyst: themselves; supervisor: anyone |
| `priority` | `priority` | |
| `status` | `status`: `open`, `in_progress`, `pending` | not while a closure request is pending |
| `sla` | `sla_hours` | |
| `comment` | `text` | at most 200 per investigation; allowed on a closed one |
| `add_alert` | `alert_id` | at most 50 alerts |
| `request_close` | `text` (the reason) | status becomes `pending_closure` |
| `approve_close`, `reject_close` | | a different analyst or supervisor than the requester (`400` four-eyes) |
| `reopen` | | supervisor; only a closed investigation |

Maker-checker is supported for the closure, on the same footing as the block approval: two authenticated people. A
closed investigation accepts only `comment` and `reopen`. Closing one decides no payment: its alerts keep their status.

## GET /api/v2/investigations/{id}/narrative

A draft narrative of the investigation, built from fixed templates (`generated_by: "template v1"`), not a language model.
It reads only stored data: the investigation (history, comments, closure), the evidence pack of each alert the desk still
holds, the live record of a webhook event (PRAMAAN result) and the desk's campaigns. Same roles as the other case reads;
`404` for an unknown investigation. It never blocks, releases or changes anything, and the same stored data gives the
same draft (`digest`, SHA-256 of the statements, checks and questions).

- `requires_human_review: true`, `status: "DRAFT - requires analyst review"`, `notice`, `decides_nothing`.
- `sections`: `summary`, `timeline`, `signals` (signals explained), `related_evidence`, `analyst_conclusions`.
- `statements`: `{id, section, type, template_id, text, sources}`. `type` is one of:

| `type` | What |
| --- | --- |
| `observed_fact` | copied from a stored payment, alert or ledger entry |
| `model_output` | the cascade's risk and decision, tree-SHAP reasons, temporal motif features, message model, PRAMAAN |
| `rule_finding` | a context rule that fired (with its ruleset), the context layer raising a decision, the consortium registry, a campaign link |
| `interpretation` | a fixed, conservative phrasing ("consistent with ...", never "is fraud") of signals that fired, sourced to them |
| `analyst_conclusion` | only case comments, the closure reason, and analysts' review decisions and notes on the alerts |

- `sources`: at least one per statement (a statement without one is dropped), each `{kind, ...}`: `alert` (`id`),
  `transaction` (`id`, the id `/transactions/{id}` takes), `ledger` (`seq` in `GET /api/v2/audit`), `evidence_pack`
  (`alert_id`, `pack_hash` as returned by `/alerts/{id}/evidence` at generation time), `comment` (`case_id`, `index`).
- `suggested_checks` and `open_questions`: `{id, template_id, text, sources}`, chosen by which signals fired (KYC of a
  pass-through account, next-hop tracing, payer contact, message review, device history, campaign alerts, sanctions and
  payee-name screening, which stored payments cannot run since they carry no names, and signature verification).
- `inputs`: the alerts, transactions and pack hashes read, and the case's ledger seqs.
- `llm_rephrase: {enabled: false}`: an optional, operator-configured rephrasing hook is documented only. Nothing calls
  an external API.

The website shows the draft in the case workspace and on `#/assistant`. Accepting it saves the analyst's edited text
with `POST /api/v2/investigations/{id}/actions` (`action: "comment"`, a fresh `X-Nonce`), so it is in the ledger like
any comment; a viewer or channel key gets `403`.

## Investigation report (website)

`#/reports/{id}` builds a printable report in the browser (print CSS, browser print to PDF; no new dependency) from
`/investigations/{id}`, `/transactions/{id}`, `/alerts/{id}/evidence`, the narrative and `/audit`. Its verification
section calls `POST /api/v2/verify` for each pack and `GET /api/v2/audit` when the report is rendered and shows
"Verified" or "Chain valid" only when those calls return `valid: true` / `chain_valid: true`; otherwise "Not verified"
with the reason. The signature covers `{pack_hash, alert_id}` as signed by this instance when the pack is read; the
pack hash is not recomputed independently (there is no `/api/v2/evidence/verify` yet). "Download JSON" saves the data
the report shows; each alert links its STR draft (`/alerts/{id}/str`).

## Scam intelligence (website `#/scam`)

The page scores a message with `POST /api/v2/message` (body `text` of 1 to 5,000 characters, `channel`, and optionally
`bank` with `account` to link it to that payer for lure fusion; an empty or oversized text is `422`). The response is the
model's own: `band` (`safe`, `caution`, `high`), `risk` and `model_risk`, `workflow`, `tactics`, `hard_signals`,
`key_phrases` (the n-grams with the largest positive contribution; the page marks them where they occur literally and
keeps no phrase list of its own), `entities`, the lure `alert` outcome, and `message_id`.

### GET /api/v2/messages

The message history: the last 200 scored messages (`MSG_LOG_MAX`), newest first. Query: `q` (substring of the redacted
text), `band` (`safe`, `caution`, `high`; else `422`), `limit` (default 50, at most 200). Each row: `message_id`, `at`,
`channel`, `text` redacted with the context layer's `redact` (links, e-mails, UPI ids, phones, accounts and codes become
`[LINK]`, `[UPI]`, `[PHONE]` and the like), `band`, `risk`, `model_risk`, `workflow` label, `tactics`, `hard_signals`,
`key_phrases` (redacted), `entity_counts` (counts only), `notified`, `alert_reason`. The response also gives `total`,
`stored`, `capacity`, `scope` and `model` (the served model's `meta` and thresholds; the backend publishes no version
string for it). Kept in the process's memory: a restart or `POST /api/v2/demo/reset` empties it, and with Redis each
instance keeps its own log (a shared log would need a Redis list like the live feed).

Linked payments are read from `GET /api/v2/alerts`: an alert is linked only when the lure memory raised its payment
(reasons "Payer received a scam-like message" or "Payee named in a scam message"). Campaigns come from
`GET /api/v2/campaigns`. A history row is not joined to an alert.

## Behavioural intelligence (website `#/behaviour`)

Every form is a consent-based demonstration started by the visitor; values go only to this server. Endpoints used:
`POST /api/v2/behaviour/enrol` and `/check` (the browser sends mean key-down to key-down times per letter pair, from
what the visitor typed or from a labelled synthetic generator; three enrolments make a profile), `POST
/api/v2/authenticate` and `/challenge/verify` (PRAMAAN; `model_version` from the response), `POST
/api/v2/identity/check`, `POST /api/v2/duress/register` and `/payments/confirm`. Session signals are read from stored
`/api/v2/assess` alerts (`assessed.context.session_detail`: points, cues, coercion index, identity, forced verify).

Not served: mouse dynamics (`crates/rusetect/src/mouse_dynamics.rs` needs a per-customer model fitted against an
impostor population the server does not ship, and `results/balabit_eval.json` is desktop mouse data, not phone touch),
behaviour summary counts, and PRAMAAN or typing history. Typing profiles, PRAMAAN account state and duress PINs are in
memory only.

## Ledger

Every triage and every investigation action appends an entry to the hash-chained ledger (`GET /api/v2/audit`,
`chain_valid`): `alert_triage` (alert, analyst, changes) and `case_event` (investigation, analyst, action, status,
detail). Comment and reason texts stay in the alert or investigation; the ledger holds their SHA-256. An
investigation's `history` rows carry the `ledger_seq` and `ledger_hash` of their entries.

## Memory and Redis

| | Memory | Redis (`REDIS_URL` / `KV_URL`) |
| --- | --- | --- |
| Live decisions searched | newest 20,000 of up to `RUSETECT_MAX_EVENTS` (default 100,000) | newest 200 (the feed) plus every stored live alert |
| Reason codes on live rows | SHAP and motifs | none (the bank-scale path returns none); the detail says so |
| Live alerts and their triage | this process | `live:alerts`, shared by every instance |
| Investigations | this process | `live:cases` and the `live:case_seq` counter, shared |
| `/assess` alerts, demo feed, ring cases, engine counters, drift, message history | this process | this instance |
| Ledger entries of triage and case work | this process | this instance (as analyst reviews are); `live:ledger` holds live decisions only |

Known gaps: in Redis mode an approved live event older than the feed is not in search or the graph; two instances
writing the same alert or investigation at the same moment keep the last write; memory mode loses everything on
restart.
