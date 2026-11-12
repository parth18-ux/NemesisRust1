# RuseTect: working notes for Claude

## Navigate with graphify first

Use the graphify knowledge graph to find code, docs and research before reading files or relying on memory.
It is cheaper in tokens and covers code, docs and the research review in one place.

- Graph: `graphify-out/graph.json`, report `graphify-out/GRAPH_REPORT.md`, interactive `graphify-out/graph.html`
  (`graphify-out/` is git ignored, so a fresh clone has no graph).
- If the graph is missing, build it: run `/graphify .` (code is extracted from the AST at no token cost; docs need
  the semantic pass). Leave out `ml/exported/*.txt` (raw model tree dumps), the PDFs in `docs/research/` and
  `report/report_template.html`, which duplicate text files already in the corpus.
- To find something: `graphify query "<question>"`; trace a link: `graphify path "<A>" "<B>"`;
  explain a node: `graphify explain "<name>"`. Read a file only after the graph has pointed to it.
- After changing code, refresh the graph with `/graphify . --update` so it stays current.

## Project rules

- Every metric in docs or pages comes from a file in `results/` or `research/out/`; never type numbers by hand.
  The research page and PDF are generated: `python3 -I research/build_report.py`, then `node research/make_pdf.js`.
- Evaluations stay chronological; thresholds are fitted on validation only.
- Commits: author Parth Sharma <parth18-ux@users.noreply.github.com>, no AI co-author lines. Batch work into few
  commits and push only when the work is finished and the user agrees.

## How to work (the user's standing rules)

- Never do the work yourself. Always dispatch a sub-agent.
- Don't always use Fable. Use Opus 5.5 for easier tasks.

### Model routing

- Fable 5.2: architecture, hard bugs, review
- Opus 5.5: edits, tests, docs, refactors
- Haiku 4.5: lookups and summaries
- Pass `model` on every Agent call (`fable`, `opus`, `haiku`)

### Delegation

- One sub-agent per task, plan first
- Run independent sub-agents in parallel
- Read the report, never the files

### Skills

- Use graphify and ponytail on every task: navigate with graphify first; ponytail for the smallest complete change.
