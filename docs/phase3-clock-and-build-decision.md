# Phase 3 (#7) — Clock source of truth + build-vs-fork decision

Committed decision record for the full-path source→endpoint zero-loss +
bounded-**absolute**-latency gate. Satisfies the #7 acceptance items "clock
source of truth justified with the measured offset" and "build-vs-fork decision
committed in `docs/`".

> Scope note: this gate is delivered and live-verified at the pipeline's current
> **30 fps**. The **60 fps** terminal certification is the separate **#11**
> (needs an operator OBS 60 fps reconfig). #7 builds the *mechanism*; #11 runs it
> at the terminal bar. See "Disposition" at the end.

---

## 1. Clock source of truth — **DanteSync, strih = master** (decided)

The candidate options the #7 spec listed were PTP, NTP/chrony, or an
OBS/DistroAV fork. **The cluster is already disciplined by DanteSync** — the
team's existing master-clock daemon (strih = master; NTP anchor + PTP fine
servo). It is the source of truth; no new daemon and no fork is introduced for
the clock.

### Why DanteSync is sufficient (and the others are not needed)

| Option | Verdict | Reason |
|---|---|---|
| **DanteSync (chosen)** | ✅ in use | Already deployed cluster-wide, already disciplines `CLOCK_REALTIME`, already the master clock the genlock relies on. Measured offset is far under the latency-gate bound (below). |
| Stand-up chrony/NTP | ❌ redundant | Would be a *second* time authority fighting DanteSync's servo. The job NTP/chrony would do (ms-level common clock) DanteSync already does better (sub-ms, measured). |
| Stand-up ptp4l/phc2sys | ❌ redundant | DanteSync's PTP fine servo already provides the sub-µs discipline; a parallel ptp4l would conflict. |
| DistroAV timestamp-fork | ❌ not for the clock | See §2 — the fork does NOT create a cluster-wide clock; the wall-clock approach below makes it unnecessary for absolute latency. |

### DanteSync disciplines `CLOCK_REALTIME` — verified live (2026-06-15)

The make-or-break fact for absolute latency: the harness reads the wall clock via
`SystemTime::now()` (→ `CLOCK_REALTIME`). DanteSync must steer *that* clock, not
just track an internal offset. Confirmed on cam2 (10.77.9.62) by stracing the
running daemon:

```
clock_adjtime(CLOCK_REALTIME, {modes=ADJ_FREQUENCY, offset=0, freq=446961, ...}) = TIME_ERROR
```

DanteSync is the **only** active time daemon (chronyd / ntpd / systemd-timesyncd
/ ptp4l / phc2sys all inactive) and it is **actively frequency-steering
`CLOCK_REALTIME`** (`ADJ_FREQUENCY`, freq ≈ 446961 scaled-ppm). dev1 (develbox)
also runs DanteSync, disciplined to the same master strih. Therefore the
painter's `gen_ts` (on the camera) and the probe tap's `recv_ts` (on dev1) share
one disciplined origin, and `recv_ts(endpoint) − gen_ts(source)` is a true
absolute latency.

> Nuance: DanteSync sets `status=STA_UNSYNC` (it does not advertise the kernel
> "synced" bit — it *is* the disciplining authority, not a kernel-NTP client).
> The ground truth of cluster agreement is the **measured offset**, not the STA
> flag.

### Measured cluster offset (the #7 "measured error" requirement)

`scripts/clock-offset-guard.sh` (#8) reads each node's DanteSync-reported
absolute offset vs master strih and fails past a documented ±2 ms bound:

| Node | Measured |offset| vs strih (master) | Within ±2 ms? |
|---|---|---|
| cam1 | ~40 µs | ✅ |
| cam2 | **+482 µs** (live, 2026-06-15) | ✅ |
| cam3 | ~67 µs | ✅ |
| cam4 | ~27 µs | ✅ |
| stream | ~302 µs | ✅ |
| strih (master→grandmaster) | ~1249 µs | ✅ |

Cluster spread ≈ **±0.4 ms** in steady state — three to four orders of magnitude
under the genlock frame period (16.7 ms @ 60 fps, 33.3 ms @ 30 fps) and far under
the ~110–250 ms end-to-end pipeline latency. **A ±0.4 ms clock error is noise on
a 100+ ms latency measurement**, so absolute latency on the DanteSync wall clock
is sound. The guard is the regression check: any node drifting past ±2 ms fails
the run before a meaningless absolute number is emitted.

### How the harness uses it (the wall-clock path)

Both timestamps that make up the absolute latency are stamped on
`CLOCK_REALTIME`, opt-in via flags that default OFF so the Phase-1 single-box
loopback (painter + reader share one *process* monotonic clock) is untouched:

- `frame-probe --wall-clock` → painter stamps `gen_ts_ns` on `CLOCK_REALTIME`.
- `multitap-probe --wall-clock` → taps stamp `recv_ts_ns` on `CLOCK_REALTIME`.
- `multitap-probe --max-abs-latency-ms N` → hard gate on the absolute
  source→endpoint p99 (**requires** `--wall-clock`; bails otherwise so a
  meaningless number can never be silently gated).

`scripts/multitap-e2e.sh` enables wall-clock by default and runs the
clock-offset guard as a pre-flight (`[0/5]`) before measuring.

---

## 2. Build-vs-fork — **GENLOCK chosen; the timestamp-patch is DROPPED** (decided)

The #7 spec framed two candidate code paths for the OBS side:
**(a)** the genlock build (vendored OBS 32.1.2 + DistroAV 6.2.1 with the
`genlock_fifo` + wall-clock-slaved render-tick patches), or **(b)** the older
`distroav-timestamp-fix.patch` (`translate_ndi_to_obs_time()`).

### Decision: GENLOCK. The timestamp-patch is superseded and NOT applied.

The genlock approach was chosen and is **already live in production**: the team
vendored OBS 32.1.2 + DistroAV 6.2.1 with the genlock patches (#41/#42/#43/#44)
and deployed it on strih + stream with `OBS_GENLOCK_WALL_CLOCK=1` (render tick
slaved to the DanteSync wall clock). Drift-guard (#45) verifies the pinned
zero-loss version + settings live on both boxes.

### Why the timestamp-patch is dropped (honest assessment)

`distroav-timestamp-fix.patch` re-bases NDI time onto **each OBS instance's local
`os_gettime_ns()`**. That fixes the DistroAV #1386 startup-drop bug, but for
Phase 3's purpose it is the wrong tool — and the #7 spec itself flags this:

> "this patch re-bases NDI time onto each OBS instance's local clock … it does
> NOT create a single cluster-wide authoritative clock … as written it makes
> per-hop absolute latency *harder* (each hop re-bases to its own
> `os_gettime_ns()`)."

The genlock build solves the real problem a different way: it slaves every OBS
render tick to the **shared DanteSync wall clock**, so the cluster ticks in phase
without re-basing timestamps per hop. Combined with the wall-clock harness path
(§1), absolute latency needs **no DistroAV timestamp fork at all** — the QR
payload already carries the source `gen_ts` end-to-end unchanged, and the
endpoint tap reads it against its own DanteSync wall-clock `recv_ts`.

### What is configuration vs. what is forked

- **Configuration (zero code):** DanteSync already deployed + disciplining all
  nodes; `OBS_GENLOCK_WALL_CLOCK=1` on strih + stream; DistroAV "NDI Main Output"
  enabled on both (the program-out the endpoint tap reads).
- **Forked C++ (already done, NOT new to #7):** the genlock patches in
  `vendor/obs-studio` + `vendor/distroav` (#41–#44), maintained via
  `/update-av-stack` and guarded by `/drift-guard`.
- **Dropped:** `distroav-timestamp-fix.patch` / `distroav-fixed/timestamp-fix.diff`
  — superseded by genlock; do **not** apply.

---

## 3. The endpoint (4th tap) — stream's OBS NDI program output

Investigated live via the `win-stream-snv` MCP (read-only, 2026-06-15). stream
(10.77.9.204, machine `stream-snv`) OBS active profile `Stream_Obs` emits three
finals: the DistroAV **NDI Main Output** (`MainOutputName=stream` →
`STREAM-SNV (stream)`), an **RTMP/RTMPS egress to YouTube**, and a local **MP4
recording**.

**Endpoint = `STREAM-SNV (stream)` (the NDI Main Output).** Per the #7 acceptance,
the testable endpoint MUST be the last NDI/recording point, **NOT** the lossy
RTMP/YouTube egress (it is re-encoded and cannot be QR-decoded frame-for-frame).
stream's OBS has exactly one NDI Main Output, which IS both the program output
and the final clean NDI endpoint — there is no separate "stream-program vs
stream-endpoint" NDI on this box. So the existing `stream` tap already reads the
endpoint; #7 adds the **source→endpoint full-span aggregate** (`full_span_diff`,
first tap vs last tap) as the headline, on top of the adjacent-hop diffs.

Full live topology (read-only, 2026-06-15):

| Tap | NDI source | fps |
|---|---|---|
| source | `CAM2 (usb)` (camera-box) | 30 (60 captured → 30 decimated) |
| strih | `STRIH-SNV (2ME PGM)` | 30 |
| stream (endpoint) | `STREAM-SNV (stream)` | 30 |

---

## Disposition

- **Delivered + gated at 30 fps:** absolute end-to-end latency (replaces the
  hard-coded UNAVAILABLE), source→endpoint full-span zero-loss aggregate, the
  clock-source decision (DanteSync), the build decision (genlock; timestamp-patch
  dropped), and the live full-path run numbers (see `docs/autopilot-log.md`).
- **Left to #11 (terminal 60 fps bar):** the #7 issue comment states final
  acceptance is at **60 fps**, which needs an operator OBS 60 fps reconfig (the
  pipeline runs 30 fps today, confirmed live: `FPSCommon=30` on strih + stream).
  The harness is fps-parameterized (`--capture-fps`, `--paint-fps`), so #11 reruns
  this exact gate at 60 fps once the operator enables it. #7 therefore stays OPEN
  with a remainder pointing at #11.
