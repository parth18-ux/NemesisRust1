# Drift monitoring

What the monitor compares, when it alerts, and how to wire the alert. Code: `crates/rusetect/src/drift.rs`.
What to do on an alert: [RUNBOOK.md](RUNBOOK.md) section 7.

## Checks and thresholds

Over a bounded window of the last payments this instance scored (`RUSETECT_DRIFT_WINDOW`, default and bounds in
`drift.rs`; the default was chosen by the rule in `research/drift_replay.py`, `results/drift_validation.json`):

| Check | warn | alert |
|---|---|---|
| PSI of the served risk and of each payment input (`log_usd`, `fmt_code`, `cur_code`, `cross_bank`, `round_100`) | `PSI_WARN` | `PSI_ALERT` |
| Each band's rate (verify, hold, block) over its reference rate, once the window expects `MIN_EXPECTED` payments in it | `RATIO_WARN` or its inverse | `RATIO_ALERT` or its inverse |

The status is the worst check; below `MIN_N` payments the window is warming up and stays `ok`. The thresholds are
constants in `drift.rs`, fixed before the checks were run (`docs/decision.md` 8ag); every report repeats them under
`thresholds`. They are not tuned on live traffic.

## Where the status shows

- `GET /api/health` field `drift` (`ok`, `warn`, `alert` or `unavailable`). It never changes `ready`.
- `GET /api/v2/drift`: `status`, `reasons` (each check that fired), `ranking` (score and every input by PSI, highest
  first, with its own status; `top_psi` is the first), `bands`, `window`, `thresholds`, and:
  - `traffic`: `live_and_api` (signed webhook and API payments) or `demo_stream`. Demo replay from the Live Console has
    a window of its own and never enters the real one; it is shown only until real traffic is past warm-up.
  - `status_since`, `previous_status`: when the status last changed (RFC 3339; null if it never did) and from what.
  - `reference`, `reference_sha256`: the reference split and the sha256 of `service/models/drift_reference.json`.
- `/app#/pilot`: the Drift card, from the same endpoint.

## Alerting

The status is re-read every `CHECK_EVERY` observations. On a change the server writes one JSON line to stderr:

```json
{"event":"drift_status","from":"ok","to":"alert","at":"<RFC 3339>","traffic":"live_and_api","reasons":["..."],"reference_sha256":"..."}
```

Route it with the host's log pipeline (a Vercel log drain, journald, a container log shipper) and alert on
`event = drift_status` with `to` in `warn`, `alert`. To poll instead, watch `/api/health` `drift` and act on a status
that persists. Each instance has its own window and its own log lines; in Redis mode the window holds risk and band
only, so input PSI is not available there.

## The reference is pinned

`service/models/drift_reference.json` is committed and loaded once at start; `reference_sha256` in every report and
log line says which file the window is compared with. To change it, rebuild it from your own validation period
(`research/drift_reference.py`, [PILOT_ONBOARDING.md](PILOT_ONBOARDING.md) section 9), commit it, restart, and record
the new hash. Drift read against two different references is not comparable.
