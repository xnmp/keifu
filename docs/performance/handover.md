# Performance pass handover — 2026-10-16

The requested implementation and verification are complete. Changes are in the
working tree on `chong-dev`; baseline was `2b62866`. No commits, pushes, or PRs were
created. The pre-existing untracked `test.py` was not changed and must not be
included accidentally in a future commit.

Read `../performance.md` for the architecture, reproducible commands, all 33
before/after workloads, live TUI results, and limitations. Raw benchmark samples,
live summary JSON, and representative screens are in this directory. Executable
harnesses are `examples/performance.rs`, `scripts/performance_tui.py`, and
`scripts/compare_performance.py`.

Important design contracts:

- `LatestTask` permits one running job, one latest replacement, one result;
  generation checks reject stale delivery. Immutable jobs own their inputs.
  Production uses workers; embedded `App::from_repo` remains synchronous unless
  `background.threaded` is explicitly enabled.
- History/layout keys track exact inputs. Shallow-boundary identity is independent
  of force-refresh keys. Boundary changes reopen libgit2 before invalidating
  ancestry-derived views: libgit2 caches parentless boundary commits.
- Graph list items, chip hits, pixel dim overlays and avatar work are windowed.
  Folded row identity is invalidated on graph/filter changes.
- Worktree reuse requires complete metadata and an index-sensitive identity.
  Overflowed watcher events require an eventual conservative refresh.
- Merge classification signatures include tracking counts and shallow boundaries;
  open failures cannot be persisted as successful empty results.

Verification completed on final source:

- `cargo test --all-targets`: 1,469 passed, three ignored, 58 targets.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `git diff --check`: passed.
- Final release baseline/optimized benchmark pair: all 33 workloads.
- Real TUI before/after in Unicode and forced Kitty modes: 3,003 commits,
  top/bottom jumps, mouse selection, actual diff content, resize, path-filter
  result and clean exit; all assertions passed and Kitty escapes verified.
- Mixed-branch raster output byte-identical at three cell sizes.
- Independent adversarial review findings were fixed and contract-tested,
  including async pan/hunk position, author-filter actions, missing/repaired
  ancestry, classifier tracking identity, and failure retry.

Full local evidence and untouched baseline artifacts remain under
`/tmp/keifu-perf`. Final logs: `tests-final5.log`, `clippy-final5.log`,
`build-final7.log`; measured JSON: `before-verified.jsonl`,
`after-verified.jsonl`. Full live artifacts are
`live-{before,after}-{unicode,pixel}`. Earlier numbered logs include exploratory
failures and superseded measurements; use the final evidence above.

No requested work remains pending. Physical terminal protocol appearance and
remote-service latency were not measured; forced Kitty exercises encoding on a
PTY, and the benchmark report states that limit explicitly.
