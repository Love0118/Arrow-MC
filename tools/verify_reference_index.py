"""Fail when the locked Java reference has missing or stale CodeGraph coverage."""

import json
import shutil
import subprocess

from prepare_minecraft import DECOMPILE, LOCK_PATH, write_json


def graph_json(*arguments):
    command = shutil.which("codegraph.cmd") or shutil.which("codegraph")
    if not command:
        raise RuntimeError("CodeGraph is not on PATH")
    result = subprocess.run([command, *arguments, "--json"], capture_output=True,
                            text=True, encoding="utf-8", check=True)
    return json.loads(result.stdout)


def main():
    version = json.loads(LOCK_PATH.read_text(encoding="utf-8"))["minecraft"]["id"]
    prefix = f"sources/{version}/"
    sources = DECOMPILE / "sources" / version
    actual = {p.relative_to(DECOMPILE).as_posix() for p in sources.rglob("*.java")}
    if not actual:
        raise RuntimeError(f"No Java source found at {sources}")
    status = graph_json("status", str(DECOMPILE))
    files = graph_json("files", "--path", str(DECOMPILE))
    indexed = {item["path"].replace("\\", "/") for item in files
               if item["path"].replace("\\", "/").startswith(prefix) and item["path"].endswith(".java")}
    target_ai = {p for p in actual if "/ai/goal/target/" in p}
    suspicious = [item["path"] for item in files if item["path"] in actual and item["nodeCount"] <= 1]
    queries = {}
    for symbol in ("MinecraftServer", "GoalSelector", "NearestAttackableTargetGoal"):
        matches = graph_json("query", symbol, "--path", str(DECOMPILE), "--limit", "5")
        queries[symbol] = [match for match in matches
                           if match["node"]["name"] == symbol
                           and match["node"]["kind"] == "class"
                           and match["node"]["filePath"].replace("\\", "/").startswith(prefix)]
    report = {
        "version": version,
        "source_java_files": len(actual),
        "indexed_java_files": len(indexed),
        "missing_java_files": sorted(actual - indexed),
        "unexpected_java_files": sorted(indexed - actual),
        "target_ai_source_files": len(target_ai),
        "target_ai_missing_files": sorted(target_ai - indexed),
        "files_without_symbols": suspicious,
        "status": status,
        "queries": queries,
    }
    output = DECOMPILE / "reports" / version / "index-verification.json"
    write_json(output, report)
    index = status.get("index", {})
    pending = status.get("pendingChanges", {})
    if (actual != indexed or not target_ai or suspicious or not all(queries.values())
            or any(pending.values()) or status.get("worktreeMismatch")
            or index.get("state") != "complete" or index.get("pendingRefs")
            or index.get("reindexRecommended")):
        raise RuntimeError(f"Reference index verification failed; inspect {output}")
    print(f"PASS: {len(actual)}/{len(actual)} Java files, {len(target_ai)} target AI files; fresh index and symbol queries.")
    print(output)


if __name__ == "__main__":
    main()
