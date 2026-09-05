"""Collect unmodified license notices from Cargo's locked registry packages.

The output covers all resolved platform packages, not just the calling host's
compiled subset. No dependency implementation is vendored by this tool.
"""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

REPOSITORY = Path(__file__).resolve().parents[1]
DESTINATION = REPOSITORY / "third_party" / "rust"


def collect():
    completed = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=REPOSITORY, check=True, capture_output=True, encoding="utf-8",
    )
    metadata = json.loads(completed.stdout)
    outputs = {}
    packages = []
    for package in sorted(metadata["packages"], key=lambda package: package["name"]):
        if package["source"] is None:
            continue
        if not package["source"].startswith("registry+"):
            raise ValueError(f"Review non-registry dependency before collecting: {package['name']}")
        root = Path(package["manifest_path"]).parent.resolve()
        directory = f"{package['name']}-{package['version']}"
        if Path(directory).name != directory:
            raise ValueError("Invalid package output directory")
        # Registry packages use both LICENSE-MIT and lowercase license-mit.
        # Select names explicitly so Linux/Windows filesystem case rules agree.
        files = {path for path in root.iterdir() if path.is_file()
                 and path.name.upper().startswith(("LICENSE", "COPYING", "COPYRIGHT"))}
        if package["license_file"]:
            files.add(root / package["license_file"])
        notices = []
        for path in sorted(files):
            relative = path.resolve().relative_to(root)
            if len(relative.parts) != 1:
                raise ValueError(f"Review nested license file: {path}")
            content = path.read_bytes()
            name = f"{directory}/{path.name}"
            outputs[name] = content
            notices.append({"path": name, "sha256": hashlib.sha256(content).hexdigest()})
        if not notices:
            raise ValueError(f"Missing upstream license notice: {package['name']}")
        packages.append({"name": package["name"], "version": package["version"],
                         "license": package["license"], "source": package["source"],
                         "repository": package["repository"], "notices": notices})
    inventory = {
        "cargo_lock_sha256": hashlib.sha256((REPOSITORY / "Cargo.lock").read_bytes()).hexdigest(),
        "scope": "All resolved Cargo registry packages; original notices only, no implementation vendoring.",
        "packages": packages,
    }
    outputs["sources.json"] = (json.dumps(inventory, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    lines = ["# Rust 의존성 고지", "", "고정된 `Cargo.lock`의 모든 플랫폼 registry package에서 라이선스 고지를 원본 bytes 그대로 보존한다.",
             "소스 구현을 vendor하는 디렉터리는 아니다. 바이너리 배포 시 필요한 고지를 배포물에 함께 포함한다.",
             "각 package의 전체 조건은 아래 원문 고지에서 확인하며 이 목록을 프로젝트 전체의 법적 적합성 인증으로 해석하지 않는다.", "",
             "| Package | SPDX 선언 | 원문 고지 |", "| --- | --- | --- |"]
    for package in packages:
        links = ", ".join(f"[{Path(notice['path']).name}]({notice['path']})" for notice in package["notices"])
        lines.append(f"| {package['name']} {package['version']} | {package['license']} | {links} |")
    lines += ["", "재생성: `python tools/collect_rust_notices.py`; 검사: 같은 명령에 `--check`.",
              "Cargo registry cache가 비어 있으면 고정된 package 다운로드가 필요하다. `sources.json`은 lock과 각 고지의 SHA-256을 기록한다.", ""]
    outputs["README.md"] = "\n".join(lines).encode("utf-8")
    return outputs, len(packages)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    outputs, count = collect()
    for name, content in outputs.items():
        path = DESTINATION / name
        if args.check:
            if not path.is_file() or path.read_bytes() != content:
                raise SystemExit(f"Missing or changed dependency notice: {name}")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
    existing = {path.relative_to(DESTINATION).as_posix() for path in DESTINATION.rglob("*") if path.is_file()}
    stale = existing - outputs.keys()
    if stale:
        raise SystemExit(f"Review notices for dependencies removed from the lock: {sorted(stale)}")
    print(f"{'Verified' if args.check else 'Collected'} original notices for {count} locked Rust dependencies")


if __name__ == "__main__":
    main()
