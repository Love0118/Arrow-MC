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

## Platforms, performance and compatibility

- Target only 64-bit Linux AArch64/x86_64, Apple Silicon macOS AArch64 and Windows x86_64.
  Initial target choices are GNU Linux, Apple Darwin and Windows MSVC; minimum OS/MSRV remain to be fixed by CI.
- Prioritize multicore chunk-loading/tick throughput and latency. Bounded, measured RAM increases for
  owned drafts, snapshots, double buffers and worker scratch are authorized when they improve performance.
  Do not optimize solely for minimum RAM; keep all queues, in-flight results and retained caches budgeted.
- Aggressive optimization may change Vanilla's internal thread layout, storage and independent execution order.
  Preserve observable gameplay, necessary same-tick dependencies, RNG and per-connection packet causality.
- Vanilla 26.3-pre-2 view distance is 2..32; the user explicitly confirmed preserving that full range.
  The separate 0.01..64 chunks/tick rate is not a view radius. No radius-64 extension is currently requested.
- Develop dependency-aware parallel ticking early alongside a synchronous reference/fallback.
  Do not copy unqualified mutable parallel loops, delay boundary effects to the next tick, or assume that
  ordered commit makes computation from stale same-tick snapshots correct.
- Use async for I/O waiting, ordinary synchronous kernels on a shared bounded CPU budget for heavy work,
  and explicit ownership/dependency boundaries for game-state mutation. Avoid per-world CPU pool multiplication.
- Reserve memory before allocation; cancelled running jobs retain their permits until their buffers are freed.
- Avoid speculative crate splits, generic propagation, per-entity async/boxed futures, broad custom macros,
  and unused plugin/Bedrock/WebAssembly dependencies. Record clean/incremental build cost with runtime results.
- Read `docs/optimization-plan.md` and `docs/architecture.md` before choosing an optimization.
  Research candidates are not implemented or benchmark-proven behavior.

## Verification and delivery

- Read `C:\Users\Jaeyun\.codex\RTK.md` before repository work; prefer supported RTK filters.
- Tooling tests: `python -m unittest discover -s tools/tests -v`.
- Reference setup: `pwsh -File tools/Sync-References.ps1`.
- Finish repository work with CodeGraph sync, status, and an affected-symbol query.
- No explicit Git delivery request: commit and push `beta`. An explicit commit/push
  request without a branch targets `main`; a named branch takes precedence.
- Use English for execution and intermediate updates; Korean for final responses and reports.
