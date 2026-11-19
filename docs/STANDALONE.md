# RuseTect standalone v1

The standalone candidate runs the signed payment webhook and the analyst workspace with the bundled synthetic
payments and checked-in model files. It can be installed and exercised on a single Linux host with loopback Redis
and configured analyst roles. The private rehearsal uses the same Redis scoring path as the signed webhook.

## Build and check the inputs

From the repository root, install Rust 1.97.0, Python 3.11 or newer, and Redis 7 with both `redis-server` and
`redis-cli` on your PATH. Run:

```sh
sha256sum -c results/standalone_models.sha256
cargo +1.97.0 build --locked --release -p rusetect-server --bin rusetect-server
cargo +1.97.0 test --locked --release --workspace
python3 -I tools/pilot_sender.py --self-test
```

## Open the workspace

Start a loopback demo with the bundled assets:

```sh
RUSETECT_DEMO=1 ./target/release/rusetect-server --host 127.0.0.1 --port 8001
```

Open [the workspace](http://127.0.0.1:8001/app) and follow the synthetic payment walkthrough in
[DEMO_SCRIPT.md](DEMO_SCRIPT.md). Exit the demo process before starting your own instance. For a persistent
self-hosted installation, configure analyst keys, a stable signing seed, the webhook secret and a Redis instance
with persistence enabled. The required environment and signed sender commands are in
[PILOT_ONBOARDING.md](PILOT_ONBOARDING.md). Keep a single API instance for this deployment.

## Verify backup and restore

Run the fixture against an isolated server and Redis with configured roles:

```sh
python3 -B tools/standalone_restore.py > /tmp/rusetect-restore.json
```

The drill starts private services on loopback, ingests the bundled payment fixture, records an analyst hold,
copies a Redis snapshot into a fresh store, then starts a fresh application process with the same signing seed.
It checks stored decisions, alert and action persistence, idempotent replays, role enforcement and both signatures.
The drill uses temporary credentials and cleans up its processes and data directories. See the recorded run in
[results/standalone_restore.json](../results/standalone_restore.json).

For an operational restore, preserve the Redis data and signing seed separately. Restore from a consistent
snapshot or your provider's supported backup into an isolated destination and check the stored sequence,
the durable ledger, a sampled alert and an identical replay before switching traffic. Retain the original
signed evidence pack and signing public key with exported reports. Redis restores the alert data and live
decision ledger. The application audit endpoint and the audit entries attached to freshly generated evidence
packs reflect the current server process. Regenerating a pack after a restart can change its hash.

## Understand the measurements

- [Scaling protocol](SCALING.md#3e-concurrent-webhook-and-analyst-reads-on-a-populated-desk) describes the loopback
  mixed workload. Throughput and latency depend on alert volume and simultaneous desk reads.
- [Model card](MODEL_CARD.md) and [AMLSim holdout protocol](HOLDOUT_PREREG.md) describe the historical research.
  The [archived holdout result](../research/out/holdout_amlsim.json) records failed transfer to AMLSim. The bundled
  fixture exercises the workflow. Evidence for real payment accuracy requires separately labelled data.
- [Pilot protocol](PILOT.md#compare-chronological-review-queues) shows how to freeze queue thresholds on an earlier
  interval and measure case coverage under the same analyst capacity on a later interval.


## Reproduce the archived AMLSim holdout

This command uses Java 21, Python 3.12 and internet access to fetch the pinned generator and dependencies.
It regenerates the transaction log, checks its SHA-256 before scoring, and compares every result field
with the archived report. The generator, transaction data and scoring scratch files stay in a temporary
directory for the run. The repository's archived report stays intact.

```sh
cargo +1.97.0 build --locked --release -p rusetect --example research_dump
python3.12 -m venv /tmp/rusetect-holdout-venv
/tmp/rusetect-holdout-venv/bin/pip install numpy==2.3.5 pandas==2.2.3 scipy==1.17.0 \
  scikit-learn==1.8.0 xgboost-cpu==3.1.3
/tmp/rusetect-holdout-venv/bin/python -I research/reproduce_amlsim.py
```

See the [reproduction result](../results/standalone_amlsim.json) for the input hash, comparison metrics
and verdict. This repeat uses the same historical simulator and sample. Generalization requires a new holdout.
