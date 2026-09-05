"""Observe bounded synthetic configuration packets through the pinned local Java APIs.

No downloads, server launch, EULA acceptance, or original registry data export.
The source JAR and its bundled libraries are verified using the reference lock.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from prepare_configuration_data import digest_file, read_json, verified_artifacts


REPOSITORY = Path(__file__).resolve().parents[1]
SOURCE = REPOSITORY / "tools/oracles/ConfigurationPacketOracle.java"
FIXTURE = REPOSITORY / "tests/fixtures/configuration_packet_oracle.json"


def validate(data, version, protocol):
    if (data.get("format_version") != 1 or data.get("minecraft_version") != version
            or data.get("protocol") != protocol):
        raise ValueError("Oracle metadata differs from the pinned reference")
    cases = data["cases"]
    if not 1 <= len(cases) <= 80 or len({case["name"] for case in cases}) != len(cases):
        raise ValueError("Oracle must contain 1..80 uniquely named cases")
    for case in cases:
        if case["direction"] not in ("serverbound", "clientbound") or type(case["ok"]) is not bool:
            raise ValueError(f"Invalid oracle case: {case['name']}")
        if "payload_hex" in case:
            payload = bytes.fromhex(case["payload_hex"])
        else:
            repeat = bytes.fromhex(case["payload_repeat_hex"])
            count = case["payload_repeat_count"]
            if type(count) is not int or not 0 <= count <= 65536 or len(repeat) > 4:
                raise ValueError("Unbounded synthetic repetition")
            payload = (bytes.fromhex(case["payload_prefix_hex"]) + repeat * count
                       + bytes.fromhex(case["payload_suffix_hex"]))
        if (len(payload) != case["payload_bytes"] or len(payload) > 100000
                or not 0 <= case["consumed_bytes"] <= len(payload)):
            raise ValueError(f"Inconsistent payload size: {case['name']}")
        if case["ok"]:
            if "result" not in case or "error_class" in case:
                raise ValueError(f"Missing successful decode observation: {case['name']}")
            if case.get("canonical_same_as_payload") is not True:
                bytes.fromhex(case["canonical_hex"])
        elif "error_class" not in case or "result" in case:
            raise ValueError(f"Missing failed decode observation: {case['name']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-root", type=Path, default=REPOSITORY.parent / "Decompile")
    parser.add_argument("--java", default=shutil.which("java"))
    parser.add_argument("--output", type=Path, default=FIXTURE)
    parser.add_argument("--check", action="store_true", help="Compare observations without replacing the fixture")
    args = parser.parse_args()
    if not args.java:
        parser.error("Java 25 or newer is required")
    lock = read_json(REPOSITORY / "references.lock.json")
    version = lock["minecraft"]["id"]
    server, libraries, protocol, _ = verified_artifacts(args.reference_root.resolve(), version, lock)
    with tempfile.TemporaryDirectory(prefix="arrow-configuration-oracle-") as temporary:
        output = Path(temporary) / "observations.json"
        run = subprocess.run(
            [args.java, "--enable-native-access=ALL-UNNAMED", "--class-path",
             os.pathsep.join(map(str, [server, *libraries])), str(SOURCE), str(output)],
            cwd=temporary, capture_output=True, encoding="utf-8", errors="replace", timeout=60)
        if run.returncode:
            raise RuntimeError(f"Configuration oracle failed:\n{run.stdout[-4000:]}\n{run.stderr[-4000:]}")
        data = read_json(output)
    validate(data, version, protocol)
    data["source_jar_sha256"] = digest_file(server)
    data["oracle_source_sha256"] = digest_file(SOURCE)
    rendered = json.dumps(data, ensure_ascii=False, indent=2) + "\n"
    if args.check:
        previous = read_json(args.output)
        # Java patch releases are recorded but do not change behavioral comparison.
        previous["java_version"] = data["java_version"]
        if previous != data:
            raise ValueError("Configuration packet fixture differs from current Java observations")
        print(f"Verified {len(data['cases'])} configuration packet observations")
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
        print(f"Wrote {len(data['cases'])} configuration packet observations to {args.output}")


if __name__ == "__main__":
    main()
