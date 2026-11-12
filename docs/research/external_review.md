# External review: open-source work RuseTect can learn from

Kept by the improvement loop. Each entry: repository, stars and licence when checked, what it does better or differently,
and whether to port. Code is copied or adapted only from permissive licences (MIT, Apache-2.0, BSD), with credit.

## 2026-11-12

| Repository | Stars | Licence | What it offers | Verdict |
|---|---|---|---|---|
| [Stream-AD/MIDAS](https://github.com/Stream-AD/MIDAS) | 778 | Apache-2.0 | Streaming anomaly score per edge on a time-evolving graph (AAAI 2020, TKDD 2022): count-min sketches of edge, source and destination counts, decayed per tick; flags sudden bursts (microclusters) in constant memory and time | **Tested, not adopted**: failed M1-M2 (no block-recall gain; scored at chance alone). See `research/out/midas_eval.json` |
| [IBM/Multi-GNN](https://github.com/IBM/Multi-GNN) | 127 | Apache-2.0 | Graph neural networks (GIN, PNA, multi-graph, reverse message passing, ego IDs) on the same IBM AMLworld data | Not now: needs GPU training and full-graph inference; RuseTect's trees with causal graph features already serve in under a millisecond. Revisit if a GNN beats the cascade under the same chronological protocol |
| [IBM/AMLSim](https://github.com/IBM/AMLSim) | 412 | Apache-2.0 | Multi-agent simulator that generates labelled laundering typologies | Data source: a fresh run would give the untouched confirmation set the served model needs (docs/decision.md, "HI-Small test is spent") |
| [SantanderAI/gen-fraud-graph](https://github.com/SantanderAI/gen-fraud-graph) | 295 | Apache-2.0 | Synthetic fraud-graph generator for benchmarking graph models | Data source, as above |
| [safe-graph/DGFraud](https://github.com/safe-graph/DGFraud) | 766 | Apache-2.0 | Toolbox of graph-based fraud detectors (GEM, Player2Vec, SemiGNN, ...) | Not now, same reason as Multi-GNN |
| [awslabs/realtime-fraud-detection-with-gnn-on-dgl](https://github.com/awslabs/realtime-fraud-detection-with-gnn-on-dgl) | 225 | Apache-2.0 | Reference architecture: graph database plus GNN for real-time card fraud | Architecture reference only |
| [jube-home/aml-fraud-transaction-monitoring](https://github.com/jube-home/aml-fraud-transaction-monitoring) | 116 | AGPL-3.0 | Real-time transaction monitoring platform: rules, abstraction (aggregation) rules, sanctions screening, case management | Ideas only (AGPL: no code copied). Sanctions-list screening is a gap in RuseTect |
| [safe-graph/graph-fraud-detection-papers](https://github.com/safe-graph/graph-fraud-detection-papers) | 1906 | none | Curated paper list for graph fraud detection | Reading list |
| [DilshanSenarath/BehaveFormer](https://github.com/DilshanSenarath/BehaveFormer) | 26 | MIT | Transformer for keystroke plus touch behavioural biometrics | Candidate for the touch layer once a touch dataset is reachable (HMOG returns 403 here) |
