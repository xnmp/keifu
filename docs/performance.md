# Performance investigation and overhaul

This pass starts from raw Git objects, source code, CPU samples, real terminal
frames, and measured operation timings. It preserves an untouched release build
at revision `2b62866` for differential measurements. The approach follows the
[Rust Performance Book's profiling](https://nnethercote.github.io/perf-book/profiling.html)
and [benchmarking guidance](https://nnethercote.github.io/perf-book/benchmarking.html):
find expensive work, measure representative workloads, change the algorithm or
ownership boundary, then check both behavior and cost.

## Changes

- **Graph algorithms:** indexed first-parent descendants and lane-local children
  replace repeated scans of all commits. Trace and old-HEAD layout work scales
  with the graph rather than repeatedly searching it.
- **Viewport rendering:** construct only visible list items and chip hit regions.
  Jumping from top to bottom costs one viewport. Pixel row mappings are cached by
  graph generation; dimming and avatar work use a bounded window. Raster primitives
  clip their loop bounds to the canvas before doing pixel math.
- **Git paths and diffs:** ordered hash-set deduplication replaces quadratic path
  checks. A single patch pass supplies both binary detection and line statistics,
  removing an otherwise discarded full diff-stat pass.
- **Ref-heavy repositories:** equal-tip branch aliases bypass pointless unique
  history walks. Tracking comparisons are deduplicated during enumeration. Merge
  classification shares patch IDs, ancestry, squash and containment results within
  one immutable classification snapshot.
- **Refresh:** exact history/layout input keys reuse unchanged commits and graph
  layout. Worktree diff identity includes index changes and high-resolution file
  metadata; incomplete metadata never authorizes cache reuse. Conflict counting
  reads the index instead of scanning worktree status again.
- **Input responsiveness:** file diff loading, syntax/word highlighting, reflow,
  path indexing, author attribution, and merge classification use background work.
  Each worker has one running job, one latest replacement, one result slot, and
  generation-validated delivery. Superseded path/highlight/reflow/author work checks
  cancellation between units. Individual libgit2 calls and a running merge
  classification are not preemptible. Summary diff loading also retains its active
  worker identity across invalidation, preventing overlapping abandoned jobs.
- **Bounded work under load:** notify events use a bounded queue and per-poll work
  budget. Overflow requests a conservative eventual refresh. Word emphasis has a
  per-comparison time budget; extremely long source lines retain all text but use
  plain syntax styling to avoid pathological parser latency.
- **Consistent invalidation:** shallow-boundary changes invalidate all
  ancestry-derived views together, even when displayed OIDs do not change. Normal
  and forced refresh, pending jobs, diffs, authors, and classification signatures
  account for that change, including reopening libgit2 cached commit objects.
  Tracking counts participate in classifier identity; repository-open failure
  remains retryable and is never persisted as an empty success.
  Async delivery preserves hunk position, clamps current
  viewport state, resets horizontal pan when wrapping, and rejects stale results.

## Reproduce

```sh
cargo build --release --example performance --bin keifu
./target/release/examples/performance > after.jsonl
# Optional substring selects one benchmark; fixture setup remains outside timing.
./target/release/examples/performance graph.lineage.bottom.20000
python3 scripts/performance_tui.py ./target/release/keifu /tmp/keifu-unicode
python3 scripts/performance_tui.py ./target/release/keifu /tmp/keifu-pixel --pixel
python3 scripts/compare_performance.py before.jsonl after.jsonl
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

For a valid baseline, export `2b62866` into a separate directory, copy the exact
current `examples/performance.rs` harness into it, and build with the same lockfile,
Rust version and release profile. Run before and after sequentially, without
compilation or profiling competing for CPU. Never compare a debug build with a
release build.

The fixture has 1,000 commits and 5,000 checked-out files; 60 feature branches with
remote aliases exercise attribution. Classification uses a bounded 13-ref subset.
Synthetic histories at 2k/10k/20k isolate graph scaling. Setup is excluded. Each
measurement drops its result inside the timed region, warms once, and records 11
samples (three for the expensive classification workload). JSON contains every
sample, median, and nearest-rank p95; with these small sample counts p95 is the
maximum. These are warm local measurements, not production percentile estimates.

The startup constructor initially loads 500 commits from the 1,000-commit fixture.
The separate live scenario starts the real application, loads 3,003 commits,
scrolls repeatedly between both ends, verifies mouse selection, opens actual diff
content, resizes, applies a file-path filter, and quits cleanly. Debug-request
latency includes the event-loop poll interval. The headless terminal cannot answer
color queries, so its approximately two-second startup includes a terminal-query
wait; do not attribute that delay to Git or graph computation. Forced Kitty mode
exercises rasterization/encoding but cannot certify a physical terminal's graphics
protocol behavior.

## Independent correctness evidence

Regression tests assert graph/lineage results, Git diff and staging outcomes,
viewport equivalence to ratatui, full-versus-windowed pixel dimming, real shallow
clone/deepen behavior, worker cancellation/delivery, author-filter actions, async
resize/wrap and hunk selection, and eventual watcher refresh after overflow.
An independent adversarial review identified async and invalidation regressions;
those findings were fixed and covered with behavior tests.

The live harness retains screen dumps, terminal bytes, and tracing/performance
summaries. Raster comparison uses the repository's existing `render_rows` harness
against both revisions. Measurements and verification details are recorded below.

## Recorded results — 2026-10-16

AMD Ryzen 9 5900X, Linux x86_64, Rust 1.97.1, release LTO and one codegen unit.
The final pair ran optimized first, then baseline, with no competing builds.
An earlier pair used the reverse order; timing variation is expected on a shared
workstation. All claims below describe these fixtures and operations, not a
universal application speedup. Raw samples: [before](performance/before.jsonl),
[after](performance/after.jsonl).

| Workload | Before median ms | After median ms | Speedup |
| --- | ---: | ---: | ---: |
| `git.load.1000` | 8.946 | 8.737 | 1.02× |
| `git.status.5000_files` | 7.989 | 7.608 | 1.05× |
| `git.paths.5000_files` | 68.745 | 1.534 | 44.82× |
| `diff.quick.5000_files` | 1.197 | 1.025 | 1.17× |
| `diff.full.5000_files` | 153.834 | 86.973 | 1.77× |
| `app.startup.1000` | 37.509 | 28.661 | 1.31× |
| `app.refresh.1000` | 24.539 | 6.085 | 4.03× |
| `branches.authors.121` | 61.490 | 0.958 | 64.18× |
| `branches.classify.13` | 7689.323 | 751.809 | 10.23× |
| `diff.highlight.5000_lines` | 376.830 | 324.987 | 1.16× |
| `diff.layout.5000_lines` | 11.587 | 9.305 | 1.25× |
| `graph.build.tip.2000` | 2.050 | 1.617 | 1.27× |
| `graph.build.old_head.2000` | 5.326 | 1.696 | 3.14× |
| `graph.lineage.bottom.2000` | 4.644 | 0.524 | 8.86× |
| `render.widget.bottom.pixel_false.2000` | 0.123 | 0.022 | 5.65× |
| `render.widget.bottom.pixel_true.2000` | 0.171 | 0.022 | 7.64× |
| `render.widget.jump.2000` | 1.612 | 0.021 | 75.09× |
| `filter.message_author.2000` | 0.428 | 0.203 | 2.11× |
| `raster.100_rows` | 1.006 | 0.917 | 1.10× |
| `graph.build.tip.10000` | 13.756 | 9.461 | 1.45× |
| `graph.build.old_head.10000` | 160.797 | 10.464 | 15.37× |
| `graph.lineage.bottom.10000` | 173.110 | 2.682 | 64.53× |
| `render.widget.bottom.pixel_false.10000` | 0.403 | 0.022 | 18.40× |
| `render.widget.bottom.pixel_true.10000` | 0.563 | 0.039 | 14.36× |
| `render.widget.jump.10000` | 9.916 | 0.022 | 445.82× |
| `filter.message_author.10000` | 3.763 | 1.030 | 3.65× |
| `graph.build.tip.20000` | 27.949 | 20.411 | 1.37× |
| `graph.build.old_head.20000` | 684.484 | 22.196 | 30.84× |
| `graph.lineage.bottom.20000` | 680.485 | 5.749 | 118.36× |
| `render.widget.bottom.pixel_false.20000` | 0.673 | 0.021 | 31.45× |
| `render.widget.bottom.pixel_true.20000` | 0.826 | 0.060 | 13.84× |
| `render.widget.jump.20000` | 16.756 | 0.021 | 788.50× |
| `filter.message_author.20000` | 5.369 | 2.379 | 2.26× |

The largest gains remove repeated history scans and off-screen rendering. The
baseline CPU profile attributed 71.32% of samples to `git_oid_equal` and 25.02%
to `lineage_oids`; indexed traversal addresses that work directly. At 20k commits,
tracing falls from 680 ms to 5.75 ms and old-HEAD layout from 684 ms to 22.2 ms.
The viewport-jump number measures widget construction, not a complete terminal
frame. Standalone pixel-widget benchmarks do not prepopulate the folded-row cache
that the full UI reuses.

Git history loading, clean status scans, quick diffs, and simple rasterization
remain close to baseline. Syntax highlighting still requires hundreds of
milliseconds for the 5,000-line fixture; the responsiveness improvement there is
that loading/highlighting/reflow no longer blocks input. Initial startup is
modestly faster, not an order of magnitude. These distinctions matter when
interpreting the much larger component speedups.

Verification: **1,469 tests passed**, three ignored, across 58 test targets;
`cargo clippy --all-targets -- -D warnings` passed. The mixed-branch graph raster,
including merge connectors, HEAD and an uncommitted node, produced byte-identical
baseline/optimized PNGs at 10×20, 20×40, and 7×15 pixel cell sizes.
[Verified raster sample](performance/graph-raster.png).

The same real-TUI scenario passed before and after in both Unicode and forced
Kitty modes, including an assertion that Kitty image escapes were emitted.
[Live measurements and screen hashes](performance/live.json):

| Real draw operation | Before average / max ms | After average / max ms |
| --- | ---: | ---: |
| Unicode | 7.49 / 41.91 | 1.86 / 5.40 |
| Kitty pixels | 7.08 / 29.43 | 2.10 / 8.03 |

These are event-loop draw measurements over the complete scripted scenario,
not a statistical tail-latency claim. Async delivery produces a few additional
frames. Refreshes in this scenario mostly load more history, so they intentionally
rebuild; their averages remain 14–18 ms. Quiet unchanged refresh is the separate
4× microbenchmark above. Headless startup stays around 2.06 seconds in all four
runs because of the terminal color-query wait.

Sample asserted screens: [file diff](performance/diff.txt),
[path filter result](performance/filtered.txt),
[mouse selection](performance/mouse-after.txt). Full terminal bytes, logs and
screens are retained in `/tmp/keifu-perf/live-{before,after}-{unicode,pixel}`.

## Practical limits

This pass removes demonstrated bottlenecks and the architectural consistency
issues exposed by the investigation. It does not make repository I/O independent
of storage or history size. Initial large graph layout and immutable file-diff
preparation still do real linear work; long individual Git operations cannot be
interrupted mid-call. Worker queues and viewport rendering are bounded, while
loaded history, path indexes, and prepared diff data still scale with requested
content. Physical terminal rendering and remote-service latency require measurements
in the user's own environment.
