"""Regenerate and verify the archived AMLSim holdout in an isolated workspace."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from urllib.request import urlopen


ROOT = Path(__file__).resolve().parents[1]
MAVEN = "https://repo.maven.apache.org/maven2"
ARTIFACTS = {
    "ecj": ("org.eclipse.jdt", "3.38.0"),
    "json": ("org.json", "20180813"),
    "webgraph": ("it.unimi.dsi", "3.6.1"),
    "commons-math3": ("org.apache.commons", "3.6.1"),
    "dsiutils": ("it.unimi.dsi", "2.4.2"),
    "fastutil": ("it.unimi.dsi", "8.1.0"),
    "slf4j-api": ("org.slf4j", "1.7.25"),
}

# Keep the generator unchanged. These aliases cover imports and set sampling used by networkx 1.11 and AMLSim.
GRAPH_COMPAT = """
import collections
from collections import abc
import fractions
import math
from pathlib import Path
import random
import runpy
import sys

sys.path[:0] = [sys.argv[1], str(Path.cwd() / 'scripts')]
for name in ('Mapping', 'MutableMapping', 'Iterable', 'Set'):
    setattr(collections, name, getattr(abc, name))
fractions.gcd = math.gcd
sample = random.sample
def sample_set(population, size, **kwargs):
    if isinstance(population, abc.Set):
        population = tuple(population)
    return sample(population, size, **kwargs)
random.sample = sample_set
sys.argv = ['scripts/transaction_graph_generator.py', 'holdout.json']
runpy.run_path(sys.argv[0], run_name='__main__')
"""


def run(*command, cwd=None, env=None):
    subprocess.run(list(map(str, command)), cwd=cwd, env=env, check=True,
                   stdout=sys.stderr, stderr=sys.stderr)


def retrieve_sources(repo, destination, revision):
    run("git", "init", "--quiet", destination)
    url = f"https://github.com/{repo}.git"
    run("git", "fetch", "--quiet", "--depth=1", url, revision, cwd=destination)
    run("git", "checkout", "--quiet", "--detach", "FETCH_HEAD", cwd=destination)
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=destination, text=True).strip()
    if actual != revision:
        raise RuntimeError(f"{repo} revision differs from the archived provenance")


def generate(work, provenance):
    amlsim, mason = work / "amlsim", work / "mason"
    retrieve_sources("IBM/AMLSim", amlsim, provenance["amlsim_commit"])
    retrieve_sources("eclab/mason", mason, "b799a4a3bf0a9f562a3532edeb2aac1bd106c42e")
    jars = {}
    for name, (group, version) in ARTIFACTS.items():
        path = f"{group.replace('.', '/')}/{name}/{version}/{name}-{version}.jar"
        jars[name] = work / f"{name}.jar"
        with urlopen(f"{MAVEN}/{path}", timeout=30) as response, jars[name].open("wb") as output:
            shutil.copyfileobj(response, output)
    classpath = os.pathsep.join(str(path) for name, path in jars.items() if name != "ecj")
    mason_src = mason / "mason/src/main/java"
    amlsim_src = amlsim / "src/main/java"
    classes = work / "classes"
    sources = [*amlsim_src.rglob("*.java"), *(mason_src / "sim/engine").rglob("*.java")]
    source_file = work / "java-sources.txt"
    source_file.write_text("".join(f'"{source}"\n' for source in sorted(sources)), encoding="utf-8")
    run("java", "-jar", jars["ecj"], "-21", "-nowarn", "-d", classes, "-classpath", classpath,
        "-sourcepath", os.pathsep.join((str(amlsim_src), str(mason_src))), f"@{source_file}")

    pylibs = work / "pylibs"
    run(sys.executable, "-m", "pip", "install", "--disable-pip-version-check", "--no-deps", "--target",
        pylibs, "networkx==1.11", "decorator==5.3.1")
    config = json.loads((amlsim / "conf.json").read_text(encoding="utf-8"))
    config["input"]["directory"] = "paramFiles/10K"
    config["general"].update(simulation_name="holdout", random_seed=provenance["seed"])
    (amlsim / "holdout.json").write_text(json.dumps(config), encoding="utf-8")
    run(sys.executable, "-I", "-c", GRAPH_COMPAT, pylibs, cwd=amlsim)
    run("java", "-Xmx1g", "-cp", os.pathsep.join((str(classes), classpath)),
        "amlsim.AMLSim", "holdout.json", cwd=amlsim)
    return amlsim / "outputs/holdout/tx_log.csv"


def replay(binary, work, archived):
    provenance = archived["provenance"]
    transactions = generate(work, provenance)
    with transactions.open("rb") as source:
        tx_hash = hashlib.file_digest(source, "sha256").hexdigest()
    if tx_hash != provenance["tx_log_sha256"]:
        raise RuntimeError(f"generated input hash {tx_hash} differs from the archived holdout")

    scoring = work / "scoring"
    (scoring / "service").mkdir(parents=True)
    (scoring / "service/models").symlink_to(ROOT / "service/models", target_is_directory=True)
    (scoring / "research/out").mkdir(parents=True)
    for name in ("holdout_amlsim.py", "structuring_defence.py"):
        shutil.copyfile(ROOT / "research" / name, scoring / "research" / name)
    target = scoring / "target/release/examples/research_dump"
    target.parent.mkdir(parents=True)
    shutil.copy2(binary, target)
    provenance_file = work / "provenance.json"
    provenance_file.write_text(json.dumps(provenance), encoding="utf-8")
    env = dict(os.environ, RUSETECT_ROOT=str(scoring), OMP_NUM_THREADS="2", OPENBLAS_NUM_THREADS="2", MKL_NUM_THREADS="2")
    run(sys.executable, "-I", scoring / "research/holdout_amlsim.py", transactions, work / "replay",
        provenance_file, cwd=scoring, env=env)

    result = json.loads((scoring / "research/out/holdout_amlsim.json").read_text(encoding="utf-8"))
    if result != archived:
        different = sorted(key for key in archived.keys() | result.keys() if archived.get(key) != result.get(key))
        raise RuntimeError(f"regenerated input matched, but archived result fields differ: {', '.join(different)}")
    return {"exact_result_match": True, "tx_log_sha256": tx_hash, "rows_total": result["rows_total"],
            "test_rows": result["test"]["rows"], "test_positives": result["test"]["positives"],
            "pr_auc": {key: result[key]["pr_auc"] for key in ("served", "cascade", "baseline_amount")},
            "verdict": result["rule"]["verdict"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/examples/research_dump")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error("build research_dump or pass its path with --binary")
    for requirement in ("git", "java"):
        if shutil.which(requirement) is None:
            parser.error(f"{requirement} is required on PATH")
    archived = json.loads((ROOT / "research/out/holdout_amlsim.json").read_text(encoding="utf-8"))
    try:
        with tempfile.TemporaryDirectory(prefix="rusetect-amlsim-") as work:
            result = replay(args.binary.resolve(), Path(work), archived)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as exc:
        parser.exit(1, f"AMLSim reproduction: {exc}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
