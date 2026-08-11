---
paths:
  - "tests/av_sync_dock_*.rs"
---

# Writing a NEW `tests/av_sync_dock_*.rs` twin-harness test (#999/#1005 session, 2026-08-11)

Two gotchas hit writing two new twin-harness files (`av_sync_dock_lock_churn_999.rs`,
`av_sync_dock_video_ts_valid_1005.rs`) in the same session, following the established pattern
(`tests/av_sync_dock_audit_log.rs` etc. — a `const HARNESS_CPP: &str = r#"..."#;` raw string
containing a tiny C++ program compiled+run against the real vendored header).

## 1. A CHECK message starting `"#<issue-number>:` inside `r#"..."#` truncates the raw string early

Rust's `r#"..."#` raw string terminates at the FIRST `"#` sequence anywhere in its content. This
repo's own convention names every CHECK failure message with the issue number as a prefix
(`"#999: ..."`, `"#1005: ..."`) — but the literal text `"#999` (a quote immediately followed by a
hash) IS that exact terminator sequence. The raw string silently ends mid-content, and everything
after it gets parsed as real Rust source — producing bizarre, hard-to-place syntax errors dozens
of lines later (`unknown start of token: \`, `expected ;, found 999`) that do NOT point at the
real cause.

**Fix: use a wider hash fence, `r##"..."##`, for ANY `HARNESS_CPP` string whose CHECK messages
start with `"#<N>:`** — `"#`  no longer terminates it; only `"##` does, which never occurs by
accident. Every EXISTING harness in this repo's history that avoided this either never happened to
start a message with `#N:` right after a quote, or got lucky — do not assume the single-`#` fence
is safe just because older files use it; check for `"#` inside the string content before choosing
the fence width, or just default to `r##"..."##` for any new harness that will embed issue-number
prefixes (which is effectively all of them, per this repo's own message convention).

## 2. Building a synthetic bimodal test batch INCREMENTALLY can trigger a premature degenerate lock

When constructing a test batch for `RollingOffsetCluster`/`CbAvOffset`-style cluster estimators by
pushing samples ONE AT A TIME toward a target split (e.g. "N at value A, then N at value B" to
land on a known median/MAD), an INTERMEDIATE state where one side has a STRICT MAJORITY (e.g. 5
of A vs 4 of B, once total >= min_matched) can independently satisfy `matched >= min_matched` with
`mad_ms == 0` (or near it) — a majority side always drags the median onto itself, giving a
degenerate but technically-valid tight lock, well BEFORE the intended final target distribution is
reached. This silently invalidates a "must never lock before reaching my construction" test.

**Fix: either (a) never let the running total reach `min_matched` until the FINAL, already-balanced
push** (so no intermediate imbalanced state is ever evaluated against the trust gate — used for
`rolling_cluster_hysteresis_never_lowers_the_entry_bar_999`, which pushes exactly `min_matched`
samples split evenly and checks ONLY the final result), **or (b) build the target distribution
entirely BEFORE the cluster you're testing could act on it** (used for the "holds through
widening" test: a separate ALREADY-LOCKED tight cluster absorbs the growing wide batch first,
before a single time-jump evicts the tight part and leaves only the complete, already-balanced
wide batch to be evaluated). Either way, verify by actually RUNNING the test — a construction that
"should" avoid the trap is worth compiling and confirming (this cost one iteration in this session:
the first "entry bar" test construction pushed all of side A before any of side B, and failed with
a confusing "locked" panic exactly at the point A's count alone crossed `min_matched`).
