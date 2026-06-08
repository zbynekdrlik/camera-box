# NDI Frame-Loss & Latency E2E Harness — Phase 2 Design (per-hop NDI-output differencing)

> Builds on Phase 1 (`2026-06-08-ndi-frame-loss-e2e-design.md`). Implements GitHub issue **#6**.
> Phase 1 spec sections referenced as §N below.

## 1. Summary

Extend the Phase-1 QR-frame-ID harness from the single cam2 HDMI-loopback box to the OBS
hops in the live signal chain. The harness taps the **digital NDI output** each node emits
(camera `CAM2 (usb)` → OBS `strih` NDI-out → OBS `stream` NDI-out), decodes the QR frame ID
at each tap, and **differences the decoded ID-sets between adjacent taps**. Because the
upstream HDMI/ShadowCast resample (§5.2) is common to every downstream tap, differencing
**cancels the resample** — so for the first time **true single-frame loss is detectable per
hop**, which Phase 1's single point could not do. Cover **strih first, then stream**.

## 2. Motivation

Phase 1 proved the QR-ID approach on one box, but its coverage gate (§15) can only assert
*sustained* loss + reorder + latency — single-frame loss on the loopback is rig-confounded by
the async ShadowCast resample (§5.2). The operator's real complaint ("I see frame lag/loss")
happens **downstream**, at the OBS compositing/forwarding hops, which Phase 1 never measures.
Phase 2 makes per-hop loss machine-checkable and **localizes** which hop drops frames,
replacing human observation with deterministic per-hop evidence that catches regressions (the
overarching project goal).

The methodological win: between two NDI taps there is **no resample** — both downstream nodes
consume the *same already-sampled* digital frames. So `IDs(upstream) − IDs(downstream)` is a
clean, beat-free count of frames a specific hop actually dropped.

## 3. Environment (verified 2026-06-08)

| Fact | Value | How verified |
|---|---|---|
| NDI runtime on dev1 | `/usr/lib/ndi/libndi.so.6.2.1` present | `find / -iname 'libndi*.so*'` |
| strih OBS | running, WebSocket :4455 up, DistroAV `C:\ProgramData\obs-studio\plugins\distroav`, NDI 6 Tools | `win-strih` MCP |
| stream OBS | running, WebSocket :4455 up, DistroAV present | `win-stream-snv` MCP |
| Operating contract | **strih + stream are off-air freely** during runs — scenes/NDI-output may be reconfigured and restored | user decision, 2026-06-08 |
| Topology | CAM2 `10.77.9.62`, strih `10.77.9.202`, stream `10.77.9.204`; chain camera→strih→stream | `targets.md` |

## 4. Architecture — taps colocate on dev1

```
 cam2 (10.77.9.62, off-air rig)            dev1 (10.77.9.21 — orchestrator + future CI host #9)
 ┌───────────────────────────┐            ┌──────────────────────────────────────────────┐
 │ frame-probe --paint-only   │  HDMI loop │ multitap-probe  (libndi.so.6)                 │
 │   QR(run_id,id,ts) → fb0 ──┼──► capture │   tap A: NdiReceiver "CAM2 (usb)"   → Vec<Obs>│
 │ camera-box (no --display)  │            │   tap B: NdiReceiver "STRIH-PHASE2" → Vec<Obs>│
 │   capture → NDI "CAM2(usb)"┼─NDI(LAN)──►│   tap C: NdiReceiver "STREAM-PHASE2"→ Vec<Obs>│
 └───────────────────────────┘            │   differ: A−B (cam→strih), B−C (strih→stream) │
        │ NDI "CAM2 (usb)"                 └──────────────────────────────────────────────┘
        ▼
 strih OBS: scene ingests "CAM2 (usb)" → program → DistroAV NDI-out "STRIH-PHASE2"
        │ NDI "STRIH-PHASE2"
        ▼
 stream OBS: scene ingests "STRIH-PHASE2" → program → DistroAV NDI-out "STREAM-PHASE2"
```

**Why dev1 hosts the taps:** dev1 has the NDI runtime, is the orchestrator, and is the future
self-hosted CI runner (#9). Colocating all taps on one machine means every `recv_ts` shares
**one monotonic clock** — which makes per-hop relative latency a valid measurement (§6) with no
cross-machine sync. The painter stays on cam2 (the camera is the QR source); the camera-box
capture→NDI path runs **unchanged** (§Non-goals).

## 5. Components

### 5.1 Multi-source NDI reader — `src/probe/multi_reader.rs` (NEW, hardware glue)

Generalizes Phase-1 `reader.rs::run_reader`. Subscribes to **N named NDI sources concurrently**:
one `NdiReceiver` + reader thread each, each recording `(frame_id, gen_ts_ns, recv_ts_ns)` into
its own `Arc<Mutex<Vec<Observed>>>`, filtered to the shared `run_id`. `recv_ts_ns` is taken from
a **single `Instant` start** shared by all tap threads, so all taps share dev1's clock. Reuses
`luma` (`uyvy_to_luma`/`bgra_to_luma` + padded stride), `qr::decode_qr_luma`, and `crop_center`
(the §15 ROI speed fix) per tap. Returns one labelled `Vec<Observed>` per tap.

### 5.2 Per-hop differ — `src/probe/differ.rs` (NEW, pure / unit-tested)

```rust
pub struct HopInput<'a> {
    pub name: String,                 // e.g. "cam→strih"
    pub upstream: &'a [Observed],     // ordered, run_id-filtered
    pub downstream: &'a [Observed],
    pub capture_fps: f64,
    pub freeze_periods: f64,
    pub min_frames: usize,            // non-vacuous guard
}

pub struct HopReport {
    pub name: String,
    pub upstream_unique: usize,
    pub downstream_unique: usize,
    pub dropped_ids: Vec<u32>,        // IDs(up) − IDs(down) = real single-frame drops at this hop
    pub reorders: Vec<(u32, u32)>,    // on downstream stream
    pub freezes: Vec<Freeze>,         // on downstream stream (reported, not gated)
    pub latency: Option<HopLatency>,  // single-clock per-hop arrival delta
    pub pass: bool,
}
```

- `dropped_ids = unique(upstream) − unique(downstream)`. Any ID present upstream-but-not-downstream
  is a **real single-frame drop at that hop** (the capability Phase 1 lacked).
- `reorders` / `freezes` computed on the downstream stream, reusing analyzer helpers
  (window-pair reorder; `chunk_by` run grouping — keeps mutation testing free of infinite-loop
  timeout mutants, per Phase-1 §15).
- **Verdict:** `pass = downstream_unique >= min_frames && upstream_unique >= min_frames &&
  dropped_ids.is_empty() && reorders.is_empty()`. The `min_frames` guard makes a disconnected /
  empty tap **FAIL**, never vacuously pass (mirrors Phase-1 `!emitted_set.is_empty()` guard).

### 5.3 Per-hop latency — `HopLatency` (single-clock, valid)

For each ID present in **both** taps, `delta_ns = recv_ts_downstream − recv_ts_upstream`. Both
timestamps come from dev1's one clock, so the delta is a real per-hop transport+processing
latency. Report `min/mean/p50/p95/p99/max` (reuse `percentile`). This is **relative per-hop
latency**, reported for every hop. **Absolute glass-to-glass** (cam2 `gen_ts` → dev1 `recv_ts`)
requires cam2↔dev1 clock sync, which is NOT satisfied (§4 Phase-1; #8) → emitted as
`absolute_latency: "UNAVAILABLE — clock not synced (Phase 3 / #8)"`, never fabricated.

### 5.4 `multitap-probe` binary — `src/bin/multitap-probe.rs` (NEW, behind `--features probe`)

clap CLI: `--run-id`, repeated `--tap NAME=NDI_SOURCE_SUBSTRING` (ordered; adjacent pairs are
differenced), `--duration-secs`, `--capture-fps`, `--qr-size`, `--freeze-periods`,
`--connect-timeout-secs`, `--settle-ms`, `--min-frames`, `--out`. Spawns `multi_reader`, applies
the trailing settle-window trim (§ run.rs precedent — frames still in flight at teardown are not
losses), runs `differ` on each adjacent tap pair, writes one JSON artifact with all hops, exits
non-zero if any hop fails.

### 5.5 `frame-probe` additions (painter sharing)

- `--run-id <u32>` (default = existing autogen): lets the cam2 painter and dev1 taps share a run.
- `--paint-only`: paint fb0 only, skip the self-loopback NDI reader/analysis (on cam2 the QR
  reaches NDI via camera-box's own capture→NDI path; the self-reader is redundant for Phase 2).

### 5.6 Orchestration — `scripts/multitap-e2e.sh` (NEW, dev1; models `loopback-e2e.sh`)

`set -euo pipefail`. Steps: (1) generate `RUN_ID`; (2) **set up OBS via MCP+OBS-WebSocket** —
on strih: ensure a scene with an NDI source `CAM2 (usb)`, set program, enable DistroAV NDI Output
named `STRIH-PHASE2`; on stream: NDI source `STRIH-PHASE2`, program, NDI Output `STREAM-PHASE2`;
confirm all three NDI names discoverable before the run (screenshots used **only** for this
liveness/scene-setup confirmation, never for loss measurement); (3) build `frame-probe` +
`multitap-probe` `--features probe`; (4) ssh cam2 → stop camera-box display, start
`camera-box` (no `--display`) + `frame-probe --paint-only --run-id $RUN_ID`; (5) run dev1
`multitap-probe --run-id $RUN_ID --tap cam="CAM2 (usb)" --tap strih=STRIH-PHASE2 --tap
stream=STREAM-PHASE2 ...`; (6) pull JSON, assert per-hop gate; (7) **`trap cleanup EXIT HUP INT
TERM`** restores OBS scene/output state on both boxes and the cam2 camera-box service. Exits by
verdict.

## 6. Gate semantics (Phase 2)

- **Per-hop PASS** = `dropped_ids` empty AND `reorders` empty AND both taps saw ≥ `min_frames`.
- **Run PASS** = every adjacent hop passes. Exit non-zero otherwise.
- **Reported, not gated:** freezes, per-hop relative latency. The hard latency + freeze ratchet
  is **#10** (blocked, Phase 2 follow-on) — Phase 2 surfaces the numbers; #10 turns them into a
  threshold gate.
- **Acceptance run:** 5-min coverage-mode run (Phase-1 precedent), both hops zero-loss,
  zero-reorder, JSON artifact with both hops + latency.

## 7. Testing (TDD)

- **`differ.rs` (pure, RED before GREEN):** synthetic upstream/downstream `Vec<Observed>` with
  injected (a) single-frame drop downstream-but-not-upstream → detected with exact dropped-ID
  list; (b) reorder → FAIL; (c) resample dups (same ID repeated, present in both) → **not** a
  drop, PASS; (d) clean → PASS; (e) empty/short downstream → FAIL via `min_frames`; (f) latency
  deltas → exact stats. Runs in the existing GitHub `test` job, **no hardware**.
- **`multi_reader.rs` + `multitap-probe` bin:** hardware glue — added to the coverage
  `--ignore-filename-regex` and the mutants `-e` exclude list (like painter/reader/run/fb/bin).
- **Production build unchanged:** probe deps stay behind `--features probe`; `camera-box` build
  and capture→NDI path untouched.
- **Live acceptance:** `scripts/multitap-e2e.sh` on dev1 (off the GitHub-hosted matrix, per
  Phase-1 §10 / #9), both hops zero-loss.

## 8. Non-goals

- **No absolute cross-machine latency** — needs synced clocks (Phase 3 / #8). Phase 2 reports
  loss + ordering + **relative single-clock** per-hop latency; absolute glass-to-glass is
  `UNAVAILABLE` and tracked in #8, not silently dropped.
- **No screenshot-based measurement** — OBS WebSocket / remoteos screenshots are liveness /
  scene-setup only; loss is measured solely from decoded per-frame NDI output.
- **No production-path changes** — `camera-box` capture→NDI runs unchanged; this is measurement
  instrumentation behind `--features probe`.
- **No self-hosted CI runner / on-PR hardware job** — deferred (#9); Phase 2 is a runnable dev1
  command.
- **No OBS/DistroAV fork or clock fix** — Phase 3 (clock source of truth).

## 9. Version

New feature → minor bump on `dev` to `1.7.0-dev.1` as the FIRST implementation commit
(per version-bumping rule; dev must stay strictly above main).

## 10. References

- Phase-1 spec: `2026-06-08-ndi-frame-loss-e2e-design.md` (§4 clocks, §5.2 resample, §11 phasing,
  §15 outcome / gate semantics / "single-frame-accurate loss is a Phase-2 capability").
- Issue **#6** (this work); follow-ons **#10** (latency+freeze gate, blocked), **#9** (self-hosted
  CI), **#8** (cluster clock sync), **#7** (full-path gate), **#11** (60 fps final bar).
- Reused code: `src/probe/{payload,qr,luma,analyzer}.rs`, `src/probe/reader.rs` (generalized),
  `src/ndi.rs::NdiReceiver`, `scripts/loopback-e2e.sh` (orchestration template).
- Clock/OBS-timestamp context: `distroav-issue-response.md`, `distroav-timestamp-fix.patch`.
