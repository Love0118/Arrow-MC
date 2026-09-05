# Arrow MC agent instructions

## Scope and paths

This directory is the sole Arrow MC implementation Git root. Its parent is a workspace
container, not another repository. Derive sibling paths from this repository's parent:

| Purpose | Relative to this repository | Current absolute path |
| --- | --- | --- |
| Implementation and tooling | `.` | `E:\projects\Arrow MC\Arrow MC` |
| Official decompiled Java | `../Decompile` | `E:\projects\Arrow MC\Decompile` |
| Pinned Pumpkin clone | `../Pumpkin MC` | `E:\projects\Arrow MC\Pumpkin MC` |
| Local progress and porting ledger | `../Roadmap` | `E:\projects\Arrow MC\Roadmap` |

Do not create code in the workspace parent or modify reference code to implement features.
The sibling Roadmap is local and is not included in this repository's commits or pushes.
Repository documentation is under `docs/`; the active task backlog is in the sibling Roadmap.

## Baseline and discovery

- Read `references.lock.json` for the current Minecraft version and Pumpkin revision.
- `Decompile/sources/<version>/` holds the Java reference. It is never Cargo `target/`.
- Use CodeGraph first with the intended project's absolute `projectPath`. Initialize a
  missing index and sync after coherent code changes. Never index the workspace parent.
- Decompile's `.gitignore` negations intentionally include Java package directories named
  `target`. Do not remove them or ignore all decompiled/generated Java.
- After a new reference is prepared, run `tools/verify_reference_index.py`: all Java files
  and AI target goals must be present, with a fresh index and working symbol queries.
- Read exact source before editing. A successful decompile or index is not behavior parity.

## Implementation direction

- Target Java Edition 26.3. Use the locked official preview until a 26.3 stable baseline
  is explicitly selected with the update workflow. Do not silently move to 26.4.
- Build a new Rust implementation. Pumpkin is a source of individually evaluated logic
  and optimizations, not the implementation's dependency or default behavior authority.
- Follow `docs/architecture.md` and record each port in `../Roadmap/PORTING.md`.
- Preserve Java numeric semantics, random streams, iteration order, state transitions,
  and tick ordering before optimizing. Record intentional differences and measured gains.
- Keep upstream source references, revision and attribution for any copied implementation.
- Do not mark a component complete based only on stub compilation or decompiler success.

## Verification and delivery

- Read `C:\Users\Jaeyun\.codex\RTK.md` before repository work; prefer supported RTK filters.
- Tooling tests: `python -m unittest discover -s tools/tests -v`.
- Reference setup: `pwsh -File tools/Sync-References.ps1`.
- Finish repository work with CodeGraph sync, status, and an affected-symbol query.
- No explicit Git delivery request: commit and push `beta`. An explicit commit/push
  request without a branch targets `main`; a named branch takes precedence.
- Use English for execution and intermediate updates; Korean for final responses and reports.
