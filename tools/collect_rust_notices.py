"""Collect unmodified license notices from Cargo's locked registry packages.

The output covers all resolved platform packages, not just the calling host's
compiled subset. No dependency implementation is vendored by this tool.
"""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tomllib

REPOSITORY = Path(__file__).resolve().parents[1]
DESTINATION = REPOSITORY / "third_party" / "rust"

# These are original files shipped inside the pinned Cargo packages. In
# particular, r-efi's AUTHORS is its license notice, not merely a contributor
# list; the same bytes are at the package's recorded upstream revision:
# https://github.com/r-efi/r-efi/blob/7e1b0322d31d625f81a5656096330934f9cd835d/AUTHORS
# New versions must be reviewed again rather than silently reusing this audit.
SUPPLEMENTAL_NOTICES = {
    "r-efi": ("6.0.0", {
        "AUTHORS": {
            "sha256": "d027e91dbc9cdbb2f1190068e498bd6b61cff022b6a032b191021ba658d96111",
            "license": "MIT OR Apache-2.0 OR LGPL-2.1-or-later",
            "scope": "Package license and copyright notice; includes the complete MIT option.",
        },
    }),
    "openssl-src": ("300.6.1+3.6.3", {
        "openssl/LICENSE.txt": {
            "sha256": "7d5450cb2d142651b8afa315b5f238efc805dad827d91ba367d8516bc9d49e7a",
            "license": "Apache-2.0",
            "scope": "Embedded OpenSSL 3.6.3 library, separately from the Rust build wrapper.",
        },
        "openssl/external/perl/Text-Template-1.56/LICENSE": {
            "sha256": "9837f05336ef3cbacb6a96e1672a0426d81ad01191f214b8d48e22ca62338181",
            "license": "Artistic-1.0-Perl OR GPL-1.0-or-later",
            "scope": "Embedded Text::Template 1.56 build tool; not linked into the server.",
        },
    }),
    "tracing-core": ("0.1.36", {
        "src/spin/LICENSE": {
            "sha256": "58545fed1565e42d687aecec6897d35c6d37ccb71479a137c0deb2203e125c79",
            "license": "MIT",
            "scope": "Embedded spin synchronization implementation.",
        },
    }),
}


def is_notice(path):
    # Do not mistake implementation files such as OpenSSL/copyright.pm for
    # legal notices and distribute their source through this collector.
    return (path.suffix.lower() not in {".rs", ".py", ".pm", ".c", ".h"}
            and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE")))


def package_notices(package):
    root = Path(package["manifest_path"]).parent.resolve()
    supplemental = {}
    if package["name"] in SUPPLEMENTAL_NOTICES:
        version, supplemental = SUPPLEMENTAL_NOTICES[package["name"]]
        if package["version"] != version:
            raise ValueError(f"Review supplementary license notices for {package['name']} {package['version']}")
    files = {path for path in root.iterdir() if path.is_file() and is_notice(path)}
    if package["license_file"]:
        files.add(root / package["license_file"])
    files.update(root / relative for relative in supplemental)
    # A newly bundled component can carry terms absent from the wrapper's
    # SPDX declaration. Fail until its nested notice is explicitly reviewed.
    for path in root.rglob("*"):
        if path.is_file() and is_notice(path) and path not in files:
            raise ValueError(f"Review nested license notice: {path.relative_to(root)}")
    notices = []
    for path in sorted(files, key=lambda path: path.relative_to(root).as_posix()):
        relative = path.resolve().relative_to(root).as_posix()
        content = path.read_bytes()
        digest = hashlib.sha256(content).hexdigest()
        audit = supplemental.get(relative)
        if audit and digest != audit["sha256"]:
            raise ValueError(f"Audited license notice changed: {package['name']}/{relative}")
        notices.append((relative, content, {
            "source_path": relative,
            "sha256": digest,
            "license": audit["license"] if audit else package["license"],
            "scope": audit["scope"] if audit else "Package notice.",
        }))
    if not notices:
        raise ValueError(f"Missing upstream license notice: {package['name']}")
    return notices


def collect():
    completed = subprocess.run(
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"],
        cwd=REPOSITORY, check=True, capture_output=True, encoding="utf-8",
    )
    metadata = json.loads(completed.stdout)
    lock_bytes = (REPOSITORY / "Cargo.lock").read_bytes()
    locked = {(package["name"], package["version"], package.get("source")): package
              for package in tomllib.loads(lock_bytes.decode("utf-8"))["package"]}
    outputs = {}
    packages = []
    for package in sorted(metadata["packages"], key=lambda package: (package["name"], package["version"])):
        if package["source"] is None:
            continue
        if not package["source"].startswith("registry+"):
            raise ValueError(f"Review non-registry dependency before collecting: {package['name']}")
        root = Path(package["manifest_path"]).parent.resolve()
        directory = f"{package['name']}-{package['version']}"
        if Path(directory).name != directory:
            raise ValueError("Invalid package output directory")
        notices = []
        for relative, content, notice in package_notices(package):
            name = f"{directory}/{relative}"
            outputs[name] = content
            notices.append({"path": name, **notice})
        vcs_file = root / ".cargo_vcs_info.json"
        vcs = json.loads(vcs_file.read_text(encoding="utf-8")) if vcs_file.is_file() else None
        checksum = locked[(package["name"], package["version"], package["source"])]["checksum"]
        packages.append({"name": package["name"], "version": package["version"],
                         "license": package["license"], "source": package["source"],
                         "repository": package["repository"], "cargo_checksum": checksum,
                         "cargo_vcs_info": vcs, "notices": notices})
    inventory = {
        "cargo_lock_sha256": hashlib.sha256(lock_bytes).hexdigest(),
        "scope": "All resolved Cargo registry packages and explicitly audited embedded-component notices; original bytes only, no implementation vendoring.",
        "packages": packages,
    }
    outputs["sources.json"] = (json.dumps(inventory, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    lines = ["# Rust 의존성 고지", "", "고정된 `Cargo.lock`의 모든 플랫폼 registry package에서 라이선스 고지를 원본 bytes 그대로 보존한다.",
             "소스 구현을 vendor하는 디렉터리는 아니다. 바이너리 배포 시 필요한 고지를 배포물에 함께 포함한다.",
             "포함된 OpenSSL 라이브러리, Text::Template 빌드 도구, tracing-core의 spin 구현에는 별도 원문 고지도 보존한다.",
             "각 package의 전체 조건은 아래 원문 고지에서 확인하며 이 목록을 프로젝트 전체의 법적 적합성 인증으로 해석하지 않는다.", "",
             "| Package | SPDX 선언 | 원문 고지 |", "| --- | --- | --- |"]
    for package in packages:
        links = ", ".join(f"[{notice['source_path']}]({notice['path']})" for notice in package["notices"])
        lines.append(f"| {package['name']} {package['version']} | {package['license']} | {links} |")
    lines += ["", "재생성: `python tools/collect_rust_notices.py`; 검사: 같은 명령에 `--check`.",
              "Cargo registry cache가 비어 있으면 먼저 `cargo fetch --locked`를 실행한다. 수집과 검사는 offline으로 실행한다.",
              "`sources.json`은 lock·Cargo package·각 고지의 SHA-256, 제공된 VCS revision, 원래 경로와 적용 범위를 기록한다.",
              "r-efi의 `AUTHORS`는 전체 MIT 조건과 저작권 고지를 포함하는 원문이다. package의 SPDX 선언과 내부 구성요소의 조건은 다를 수 있다.", ""]
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
