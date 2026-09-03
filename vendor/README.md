# vendor/ — the genlock monorepo (one repo, everything needed)

Per the project decision (#41): the WHOLE genlocked AV stack lives in THIS repository —
fresh copies of the latest upstream releases, with our genlock patches applied on top —
so the final working state is reproducible from one place and production boxes never run
an unpinned/stock build.

| dir | upstream | version | imported as |
|---|---|---|---|
| `vendor/obs-studio` | github.com/obsproject/obs-studio | **32.2.0** (commit `7546be726`) | git subtree --squash |
| `vendor/distroav` | github.com/DistroAV/DistroAV | **6.2.1** (commit `038d9d6`) | git subtree --squash |
| `vendor/av-sync-dock` | github.com/norihiro/obs-audio-video-sync-dock | **0.1.4** | vendored files (#188 A/V-sync dock) |
| `vendor/av-sync-dock/deps/quirc` | github.com/dlbeer/quirc | commit `927d680904dc95fdff4cd9d022eb374b438ff8f2` | vendored `lib/` sources (pin in `deps/quirc/QUIRC_PINNED_SHA.txt`) |
| NDI SDK headers | shipped inside DistroAV (`vendor/distroav/lib/ndi/`) | SDK v6 (plugin requires **NDI ≥ 6.3.0**) | part of the DistroAV tree |

The NDI **runtime** (`libndi.so` / `Processing.NDI.Lib.x64.dll`) is NOT committed —
licensing forbids redistribution (see the License Agreement PDF in `lib/ndi/`). Each
machine gets it via the NDI installer / `vendor/distroav/CI/libndi-get.sh`.
**Note (updated 2026-07-06, #132/#547):** the WHOLE fleet now runs NDI runtime **6.3.2.0**
uniformly (`/usr/lib/ndi/libndi.so.6` -> `libndi.so.6.3.2`, the old `6.2.1` kept as `.bak`) —
DistroAV needs ≥ 6.3.0. Confirmed live 2026-07-06: dev1, the production OBS boxes strih + stream,
imag-nb, AND all cameras **cam1-4** (each `libndi.so.6.3.2`, `strings` → `6.3.2.0`; the cams were
upgraded 2026-07-03 by the #547 fleet convergence, superseding the earlier `cam1-4 still run
6.2.1.0` state this note used to record). The earlier cross-version period (cams 6.2.1 vs boxes
6.3.2) is over — the fleet is single-version. `scripts/upgrade-fleet-ndi.sh` (#132) is the
canary-first tool that rolls a candidate `libndi.so.6.x.y.z` (fetched via `libndi-get.sh`,
same as dev1's own copy) onto the fleet once an operator runs it against the live rig.

## Pinned production settings — drift guard (#45)

`scripts/drift-guard.sh` (unit-tested in `tests/drift_guard.rs`) enforces that strih
(`10.77.9.202`) + stream (`10.77.9.204`) stay on the versions above **and** these critical
runtime settings — the known-good zero-loss state verified live on both boxes 2026-06-14. A
*deliberate* rollout (the 30→60 fps step #11, or activating genlock) re-pins the value here as
part of that change; an *unexpected* difference is drift and the guard fails loudly.

| setting | pinned value | live source (read-only) |
|---|---|---|
| `output_fps_strih` | `30` | OBS log `video settings reset: … fps: <n>/1` on STRIH (10.77.9.202) — **Topology v2 (#459, EPIC #466, 2026-07-03): strih dropped from the 60fps LED-wall IMAG role to a 30fps cut-to-stream-only box.** The 60fps low-latency IMAG program moved to the NEW imag-nb box (10.77.9.182, Linux, `output_fps_imag`-equivalent pin tracked separately, #463). Cam boxes still emit 60fps NDI; strih now DECIMATES that 60fps camera feed to its own 30fps canvas (the 60→30 beat that USED to sit at strih→stream now sits at cam→strih — `recording-verdict`'s #360 gap-ignore contiguity logic already handles either hop uniformly). A drift UP to `60` is drift. #11's original mixed 60/30 topology (cam(60)→strih(60)→stream(30)) is SUPERSEDED by #459: cam(60)→strih(30)→stream(30) |
| `output_fps_stream` | `30` | OBS log `video settings reset: … fps: <n>/1` on STREAM (10.77.9.204) — **Topology v2 (#459):** stream now receives an ALREADY-30fps feed from strih (no decimation on this hop any more — strih→stream is a plain 30→30 pass-through cut-to-stream). A drift to any value other than `30` is drift. Historically (pre-#459, #11) this box decimated strih's 60fps LED-wall feed to 30fps for the restreamer — that decimation now happens INSIDE strih itself (see `output_fps_strih` above), not on this hop |
| `genlock_wall_clock` | `1` | OBS log `genlock: wall-clock-slaved render tick ENABLED` (running state) — the genlock master gate. **#257: this is now a BUILD DEFAULT (always on, no `OBS_GENLOCK_WALL_CLOCK` env).** The pin value `1` is the build-default sentinel; the genlock proof is the capability marker (the `render tick ENABLED` / `timestamp-aligned release` log lines), gathered for `--compare genlock_capability=` / `--status`. The genlock latency is likewise a build const (3 ms, floor 3) with the per-source override in the OBS UI — no `OBS_GENLOCK_LATENCY_MS` / `_RESERVE_MS` / `_TS_ALIGN` / `_PRELOAD_FRAMES` env any more |
| `ndi_input_latency` | `0` | DistroAV NDI **input** `latency` mode = **Normal** (the obs-websocket `GetInputSettings` `latency` field; `2`=Lowest, `1`=Low, `0`=Normal). This is the **certified LOW-LATENCY zero-loss** ingest mode for the genlocked path (#84). A/B measurement (twice, reversed) found the DistroAV ingest buffer is NOT a real latency lever once genlock is active: the wall-clock render tick dominates emit timing, so **Normal(0) gives a ~33 ms LOWER strih abs_emit p50** (216 ms vs 249 ms at Lowest(2)) while staying zero-loss over a 30-min run — Normal is the more-buffered, lower-latency, loss-free state. It is checked on the **genlocked broadcast-path inputs**: on strih the camera ingests (`NDI cam1`, `NDI cam2`, `NDI cam3` — the default active set, 1:1 with CAM1/CAM2/CAM3 since #753's strih NDI mapping pivot; the pre-pivot `NDI cam5`/`NDI cam1`/`NDI cam3`=CAM1/CAM3/CAM4 slot naming is retired), on stream the strih→stream program feed (`NDI 2ME PGM`). Re-pin only on a deliberate latency rollout (this `0` value IS such a deliberate re-pin, applied + verified live 2026-06-16); an input drifted off `0` is drift the guard flags. Non-broadcast inputs (preview/CG/lyrics) are out of scope of the pin |
| `canonical_plugin_path` | `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit` | The **single canonical OBS plugin-load path** for the genlock DistroAV plugin (#124, EPIC #125). OBS scans MULTIPLE module locations — `C:\Program Files\obs-studio\obs-plugins\64bit` (first-party install dir), `C:\ProgramData\obs-studio\plugins\<plugin>\bin\64bit` (global third-party), and `%APPDATA%\obs-studio\plugins\<plugin>\bin\64bit` (per-user) — so the SAME `distroav.dll` present in more than one of them lets a **stale copy silently shadow the intended build** (the mixed-version incident #119: a pre-#97 DistroAV loaded while every version check still passed). The invariant: `distroav.dll` exists in **EXACTLY ONE** scan path, and that path is this ProgramData one (verified live on strih + stream 2026-06-25 — exactly one `distroav.dll` per box, 663040 bytes, loaded by the Program Files genlock `obs64.exe`; **none** in `Program Files\obs-studio\obs-plugins\64bit` — the `data\obs-plugins\distroav` folder there is resources/locale, **not** the binary). The first-party OBS plugins ship under `Program Files\obs-studio\obs-plugins\64bit`; DistroAV is the one deployed to ProgramData — a deploy MUST NOT also drop a `distroav.dll` into `Program Files\obs-plugins\64bit` (that recreates the shadow). The drift-guard reads every observed `distroav.dll` location (`distroav_dll_paths`, gathered via win-* MCP) and FAILS if there is more than one, or if the lone one is off this path |
| `genlock_source_latency_strih` | `NDI cam1=range:3-2000,NDI cam2=range:3-2000,NDI cam3=range:3-2000` | OBS log `genlock-fifo audit 'SOURCE': … latency_ms=N …` lines on STRIH (#357). Post-#753 1:1 pivot the genlocked broadcast-path camera ingests are `NDI cam1`/`NDI cam2`/`NDI cam3` (the default active set; retired grabbers cam4..7 excluded). Per-source strih camera latency is now the operator's A/V-align domain — cameras carry deliberate per-source overrides (live 2026-08-15: cam1=3, cam2=6, cam3=20), re-tuned on every A/V-sync recalibration. So this pin is now a **clamp-range backstop** (`range:MIN-MAX` = the same `GENLOCK_LATENCY_MS_MIN=3`..`GENLOCK_LATENCY_MS_MAX=2000` DistroAV clamp the stream pin uses) — only an egregiously out-of-clamp value FAILS — exactly like `genlock_source_latency_stream` below (#390): hard-pinning the exact ms values (the old `3,3,3`, pre-pivot slot names + a since-dead all-at-3ms-floor model) goes stale the next recalibration and false-DRIFTs. The AUTHORITATIVE per-source pin baseline is now `scripts/latency-pins-baseline.json` (issue 1061), live-verified REPORT-ONLY at OBS start by `scripts/latency_pins_verify.py` (WS key `genlock_latency_ms_src`). Read-off: win-strih MCP `EventLog` filtered for `genlock-fifo audit` |
| `genlock_source_latency_stream` | `NDI 2ME PGM=range:3-2000` | OBS log `genlock-fifo audit 'NDI 2ME PGM': … latency_ms=N …` on STREAM (#357, calibration-tracked #390). The `NDI 2ME PGM` source (the strih→stream program feed) carries a **deliberate per-source A/V-align override** that slows the video path to sync with the mastered audio — but that value is **NOT a fixed constant**: it is whatever the #188 A/V-sync calibration (`scripts/av_sync_calibrate.py`, #427) last measured and applied, and it changes every time the operator re-calibrates. Hard-pinning a single ms value (the pre-#390 `450`) goes stale the moment the align is re-calibrated — proven live 2026-07-01 (pin said `450`, live was `1000`, genuinely delivered: `src_latency_ms=1000 latency_ms=1000 reserve_ms=1000`, head_skew ~1 s, underruns=0 — a **false DRIFT**, #390). So the pin is now a **sane backstop range** (`range:MIN-MAX` = the DistroAV per-source genlock-latency clamp, `GENLOCK_LATENCY_MS_MIN=3`..`GENLOCK_LATENCY_MS_MAX=2000` in `scripts/drift-guard.sh`, mirrored from `LATENCY_MIN`/`LATENCY_MAX` in `scripts/av_sync_calibrate.py`) — only an egregiously out-of-range value (e.g. `5000`) FAILS the gate. **In addition**, when the operator/agent supplies `av_sync_calibrated_ms=` (the #427-persisted `applied_latency_ms` read from `%PROGRAMDATA%\camera-box\av-sync-last.json` on the stream box), `--compare` cross-checks the live value against THAT calibrated value (±10 ms) and flags genuine drift (e.g. a hand-nudge in the OBS UI since the last calibration) that the range check alone would miss — this facet is best-effort and degrades gracefully (range-checked only, no failure) when the file is not supplied/reachable (drift-guard itself runs on dev1, not on the OBS box). Re-pin the RANGE here only if the DistroAV clamp itself ever changes. Read-off: win-stream-snv MCP `EventLog` filtered for `genlock-fifo audit` (the live latency) + `FileRead` of `av-sync-last.json` (the calibrated value, best-effort) |

### imag-nb (`10.77.9.182`, Linux, EPIC #466 Topology v2) — `--check-imag`, gathered over SSH (#463)

imag-nb holds the 60fps low-latency IMAG role that strih dropped in #459 (see `output_fps_strih`
above). Unlike strih/stream (Windows, needs the win-* MCP tools), imag is a plain Linux box —
`scripts/drift-guard.sh --check-imag` SSHes to it directly and gathers these values itself; no
external `--compare KEY=VAL` round-trip is needed. See `scripts/drift-guard.sh --help` for the
gathering detail (paths, log location) and `check_imag_report`/`gather_and_check_imag` for the
pure check / SSH-glue split.

| setting | pinned value | live source (read-only, over SSH) |
|---|---|---|
| `output_fps_imag` | `60` | OBS log `video settings reset: … fps: <n>/1` on imag-nb — the 60fps low-latency IMAG role (Topology v2, #459/#463/EPIC #466). A drift DOWN to `30` (strih's rate) is drift |
| `genlock_latency_ms_imag` | `3` | OBS log `genlock: latency = N ms` (the #235 single-knob line) on imag-nb — same build-const floor as strih/stream (`genlock_wall_clock` above); imag has no per-source override configured. Re-pin only on a deliberate calibration change |
| `genlock_build_sha_imag` | *(no static pin — #531 dynamic check)* | `/opt/obs-genlock/GENLOCK_BUILD_SHA.txt` on imag-nb (the commit SHA `scripts/setup-imag.sh` writes on every hot-swap, #460). **#531: NO static pin any more** — the pre-#531 empty-pin compare was inert (always UNKNOWN, could never FAIL, so it never caught a merged-but-never-deployed genlock change — the #530 45fps disaster). `--check-imag` now compares this box SHA **DYNAMICALLY** against origin/main's vendored-genlock HEAD (`git log <box>..origin/main -- vendor/obs-studio vendor/distroav`): a non-empty range = the box is BEHIND merged genlock commits = **STALE = DRIFT** (fail loud), an empty range = current = OK. So this row is no longer read by the guard — the authoritative "what should be deployed" is origin/main, not a hand-maintained pin |
| `distroav_so_sha256_imag` | `b924252a11b23843194b81958b510d168983bc42ba87d9667a8c3af24a3f5fda` | SHA256 of `/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so` on imag-nb (the Linux plugin binary `scripts/setup-imag.sh`'s genlock hot-swap step hot-swaps, #460). A **SECONDARY** signal (`--check-imag` reports it OK/UNKNOWN — the PRIMARY build-staleness signal is now the #531 dynamic `genlock_build_sha_imag` compare above). Pinned 2026-09-03 from the linux-genlock run 33764143262 bundle (build SHA `3ffe2fbc5`, the release-candidate bundle deployed fleet-wide to imag/strih/stream on 2026-09-03 — vendor content AHEAD of origin/main by the issue-1287 `ndi-source.cpp` fix until the train merges; previously 2026-08-26 run 32750403528 / `7441bbd2c`). **This pin MUST advance with every imag genlock deploy** (early-gate-pin doctrine: an unadvanced pin reads UNKNOWN/DRIFT and fail-closes the rig gates — re-read the deployed `.so`'s sha256 and update this cell as part of the deploy step) |
| `dantesync_locked_imag` | `locked` | `journalctl -u dantesync` on imag-nb (#489, spun out of #479's setup-imag.sh provisioning-time dantesync check) — the SAME PTP LOCK/NANO or NTP-offset markers `scripts/setup-imag.sh`'s own provisioning-time restart check keys on (`scripts/setup-imag.sh:230`, `\[PTP\][[:space:]]+(LOCK\|NANO)\|\[NTP\] offset`). Unlike the two SHA pins above, this is a runtime STATE (not a build artifact) and does not go stale on a rebuild, so it is pinned to its expected steady state (`locked`) from day one — a drift to `unlocked` means dantesync is running but the clock never disciplined, compromising the wall-clock basis `genlock_wall_clock` (above) depends on |
| `power_pl1_w_imag` | `45` | MMIO RAPL `long_term` power constraint (watts) of the `intel-rapl-mmio:*` `package-0` zone on imag-nb (#1040, re-baselined 29→45 in #1162 for the i7-13620H). The imag render regression (issues 799/880/1029/1030) was a HARDWARE power clamp: thermald's DPTF policy programmed PL1 to **25 W**, starving the iGPU to `gt_act_freq` 600-850 MHz while every software freq knob sat at 1400 — MMIO RAPL wins over the decorative MSR 200/80 W values. The ORIGINAL i5 unit ran a sustainable **29 W** (35 W overheated it — TCPU 81→90 °C in 8 s). #1162: the REPLACEMENT i7-13620H imag-nb STARVES at 29 W (iGPU 150-450 MHz, 74-88 ms/frame); its sustainable ceiling is **45 W** (GPU 1200 MHz, 17-21 ms/frame; ACTUAL package draw plateaus ~36 W at the 93 °C chassis ceiling), so the pin is re-baselined 29→45 W for it. The durable fix pins MMIO PL1 = 45 W + `slpc_ignore_eff_freq = 1` at boot (`imag-power-envelope.service`, a root oneshot), **purges thermald** (the actor that programmed 25 W — a minimalist appliance purges a competing policy engine, same discipline as the sole-timesync-authority gate; PROCHOT stays as the hardware backstop), and supervises the envelope with a loud root guard (`imag-power-envelope-guard.timer`: TCPU ≥ 93 °C×2 → step down to 25 W, sustained < 85 °C → restore, foreign re-program → re-assert). `--check-imag` reads this over SSH via `imag_power_envelope_gather_remote_snippet` (`scripts/lib/imag-power-envelope.sh`) and FAILS if `long_term` ≠ `45`×1e6 µW or not `enabled`, if any slpc knob ≠ 1, if thermald is present, or if either envelope unit is not enabled+active. An in-progress legitimate step-down reads as DRIFT — correct (a clamp IS a degradation). Re-pin only after a longer soak proves a different sustainable ceiling |

The OBS/DistroAV **versions** come from the version table above (single source of truth); the NDI
runtime is checked `≥` the `NDI ≥ 6.3.0` minimum stated there. The facets:

```bash
./scripts/drift-guard.sh --check-pins    # CI: validate the pin set + cross-check vs vendored source
./scripts/drift-guard.sh --compare host=strih obs_version=… distroav_version=… \
    ndi_runtime=… output_fps=… genlock_wall_clock=… ndi_input_latency=… \
    distroav_dll_paths=…   # live box (values read via win-* MCP); distroav_dll_paths = every
                           # distroav.dll location found across the OBS scan paths (#124)
./scripts/drift-guard.sh --compare host=stream … \
    genlock_source_latency="NDI 2ME PGM=1000" av_sync_calibrated_ms=1000
    # #357 per-source held-latency (host-keyed pin; NDI 2ME PGM is range-checked, #390) +
    # #390 OPTIONAL best-effort cross-check vs the #427-persisted av-sync-last.json value
```

The live read-only run is driven by `/drift-guard` (`.claude/commands/drift-guard.md`), which gathers
the observed values off strih/stream through the win-* MCP tools and feeds them to `--compare` —
CI runners can't reach the production LAN, so the live facet is operator/agent-driven, not in CI.

## Per-component SHA manifest in the bundle (#120, EPIC #125)

drift-guard above pins the **marketing versions** (OBS 32.2.0 / DistroAV 6.2.1) — it cannot catch a
build that shipped the *right version* but the *wrong bytes*. That is exactly #119: the
windows-genlock artifact once bundled a **pre-#97 DistroAV** (a stale prebuilt `distroav.dll`), so the
preload knob was inert even though every version check passed. The windows-genlock build now
**rebuilds every vendored component from this pinned source** (OBS + DistroAV — no checked-in/cached
DLLs) AND ships a **per-component SHA manifest inside the bundle** (`stage/BUNDLE_MANIFEST.json`),
generated by `scripts/genlock-manifest.sh` (unit-tested in `tests/genlock_manifest.rs`):

- **`components[]`** — each rebuilt component's pinned SOURCE: OBS + DistroAV `version` + the vendored
  subtree `commit` (read from the table above), the DistroAV version cross-checked against
  `vendor/distroav/buildspec.json` (same source-of-truth as drift-guard), plus the non-redistributable
  NDI runtime `min_version`.
- **`files[]`** — every shipped file with its `sha256` + byte size, walked from the staged bundle, so
  the list is **self-consistent** with what ships by construction.

```bash
scripts/genlock-manifest.sh --stage stage --out stage/BUNDLE_MANIFEST.json   # emit (in the build)
scripts/genlock-manifest.sh --check stage/BUNDLE_MANIFEST.json --stage stage # self-consistency gate
```

The windows-genlock build fails if the produced manifest does not match the bundle (extra,
missing, or sha-drifted file). This is the artifact that **#121** (post-deploy byte/SHA verify) checks
a deployed stack against, and that **#122** (drift-guard per-component BUILD SHA) consumes.

The OBS **auto-update dialog stays disabled** (#43) is a *build-time* property, not runtime-readable
off a running box, so it is guarded at its proper layer — `tests/obs_updater_disabled.rs` against the
vendored source — rather than by this runtime guard.

## Why subtree --squash

- One repo (the user's requirement) — no submodule indirection, the source is HERE.
- No upstream history bloat — each import is a single squash commit.
- Updates stay tractable: `git subtree pull --squash` MERGES a new upstream release with
  our local genlock patches instead of overwriting them. This is the mechanism the
  release-bump slash command (#44) builds on.

## Updating to a new upstream release — `/update-av-stack` (#44)

Use the slash command `/update-av-stack` (engine `scripts/update-av-stack.sh`, unit-tested in
`tests/av_stack_update.rs`). It parses the version table above, checks each subtree component
against the latest upstream **stable** tag, and — for anything behind — runs the catch-up pull,
re-applying our genlock patches through the subtree merge and reporting conflicts loudly:

```bash
./scripts/update-av-stack.sh --check    # read-only: report drift + the exact catch-up commands
./scripts/update-av-stack.sh --apply    # run the git subtree pulls (clean tree required)
```

Each pull is equivalent to:

```bash
git subtree pull --prefix=vendor/obs-studio https://github.com/obsproject/obs-studio.git <NEW_TAG> --squash
git subtree pull --prefix=vendor/distroav  https://github.com/DistroAV/DistroAV.git  <NEW_TAG> --squash
```

After applying: resolve conflicts patch-by-patch (each `genlock:` commit is one patch), rebuild
per `BUILD.md`, run the strict harness (#35), and update the table above with the new tag/commit.

## Our patches

Genlock changes (#42) are normal commits in THIS repo touching `vendor/` files — `git log
-- vendor/` after the two import commits IS the patch series. Keep each patch commit
focused and prefixed `genlock:` so the #44 update flow can review conflicts patch-by-patch.

Beyond the genlock patches, the fork also carries a CORRECTNESS patch on top of upstream
DistroAV:

- **#93 NDI source-name use-after-free fix** (`src/ndi-source.cpp`): stock DistroAV
  `ndi_source_update` (UI / obs-websocket thread) `bfree`s + `bstrdup`s
  `config.ndi_source_name` on every update while the A/V thread borrows that exact pointer
  into `recv_desc` → heap corruption when a live source is re-pointed (the strih OBS
  crash). The patch adds a per-source `pthread_mutex_t config_mutex` (held only around the
  config-mutation section of `update` and the A/V thread's `reset_ndi_receiver` copy — never
  the render path, never across a blocking NDI call) plus A/V-thread-owned `bstrdup` copies
  of the name strings that `recv_desc` binds to. Guarded by
  `tests/distroav_source_config_lock.rs` so a `git subtree pull` can't silently revert it.

- **#97 per-source genlock preload as a runtime video-delay control**
  (`libobs/obs-source.c`, `libobs/obs-internal.h`, `libobs/obs.h`, `src/ndi-source.cpp`):
  promotes the #70 global env-set-at-launch preload into a **per-source, runtime-settable**
  `uint32_t genlock_preload` field (one preload frame = one frame of genlock-disciplined
  VIDEO DELAY), exposed via `obs_source_set/get_genlock_preload()` (clamped `[0,128]`,
  read/written under `async_mutex` — the #93 UAF lesson). `GENLOCK_PRELOAD_MAX` raised
  28→128 and the async FIFO drop-cap made per-source (`genlock_source_drop_cap()` =
  `preload+RESERVE` for a genlock source, fixed `MAX_ASYNC_FRAMES` otherwise) so a
  deliberately-delayed source parks its full buffer without force-draining (memory-safe,
  #89). DistroAV adds a "Genlock preload (video delay)" int slider (0–128) + a read-only
  "≈ N ms (@ F fps)" info text recomputed from `obs_get_video_info()`, applied in
  `ndi_source_update` via the runtime-resolved setter; the audit log gains the ms
  equivalent. Lets the operator delay the program video ~1 s to match late audio on
  stream.lan. Guarded by `tests/genlock_preload.rs` (+ the windows-genlock.yml pwsh gate)
  so a `git subtree pull` can't silently revert it.

- **#102 genlock consume-when-queued (the strih→stream frame-loss fix)**
  (`libobs/obs-source.c`, `libobs/obs-internal.h`): the #70 consume gate held until
  `depth > preload` on EVERY tick, so any NDI arrival-jitter dip below the preload reserve
  REPEATED the last frame and lost one DISTINCT frame; at a deep #97 preload (≈1 s) this was
  catastrophic — after any drain the FIFO refilled PAST the whole reserve (~30 repeats)
  before one new frame escaped (11.6 % @ preload=1 → 34.3 % @ preload=30 on the live stream
  box, underrun-dominated 990 vs 72 overruns). The patch replaces `genlock_should_consume`
  (`depth>preload` → repeat) with `genlock_decide(depth, preload, filled)` + a per-source
  one-time startup-fill latch `genlock_filled`: the FIFO BUILDS to the preload delay depth
  once (the delay), then consumes a distinct frame on EVERY tick a frame is queued, repeating
  ONLY on a TRUE empty. So a deep preload becomes a CLEAN delay line — it holds the ~1 s delay
  but never repeats/drops a distinct frame ⇒ ~0 distinct-frame loss at any depth. The latch
  re-arms on an overrun force-drain (`cache_video`) and on a runtime preload change
  (`obs_source_set_genlock_preload`), all under `async_mutex` (the #93 lesson). Preserves the
  #97 per-source drop-cap (with its `MAX_ASYNC_FRAMES` floor for burst tolerance) intact.
  Guarded by `tests/genlock_preload.rs` (+ the windows-genlock.yml pwsh gate) so a
  `git subtree pull` can't silently revert it.

- **#111 QR render-time burn filter (per-hop latency probe foundation, Path B)**
  (`src/ndi-burn-filter.cpp`, `src/burn-payload.hpp`, `src/burn-clock.hpp`,
  `src/burn-qr.hpp`, `src/qrcodegen/` [Nayuki, MIT], `src/plugin-main.cpp`,
  `CMakeLists.txt`): a NEW DistroAV effect filter ("DistroAV QR Burn (latency probe)")
  that burns a per-render QR into the rendered video each frame. The QR carries a payload
  **byte-identical** to the camera-box probe payload (`src/probe/payload.rs`:
  `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}`, CRC-32/ISO-HDLC) so the existing `rqrr`
  recorded-file decoder (`src/probe/recording.rs`, #106) reads the node's stamp UNCHANGED.
  `gen_ts_ns` is the boundary-snapped wall-clock (epoch ns — cam2's timebase via
  `burn_clock::gen_ts_ns`), so #108 (post-event) can subtract `node_stamp − cam2_gen_ts`
  per hop on one shared clock. NO libobs core change — render flow is
  texrender → `gs_stage_texture`/map → CPU-draw the QR (qrcodegen EC-High, white quiet
  zone) → re-upload → `gs_draw_sprite` (the same render→stage path as `ndi-filter.cpp`).
  Node identity: reserved per-node `run_id` derived from the **host role** (#257 — no env;
  911002 strih / 911004 stream, outside cam2's range), corner derived from the run_id. **Gated
  by the parent source's per-source `genlock_burn` bool (#257, default OFF)**: with the bool off
  the filter is a transparent pass-through, so it is inert on the production install until the burn
  is toggled ON over OBS WebSocket (no env, no relaunch). Guarded by `tests/burn_payload_parity.rs`
  (which
  compiles+runs the C++ encoder via g++ and asserts byte-identity with `Payload::encode`,
  round-trip through the decoder, and that the rendered QR decodes back via rqrr) + the
  windows-genlock.yml pwsh #111 gate, so a `git subtree pull` can't silently revert it.
  Scope: this is the BURN only — decoding the burned stamps + computing per-hop latency is
  #108 (post-event).

- **#505 Linux GL PBO-orphan fix (root-causes #501's imag-nb multiview CPU stall)**
  (`libobs-opengl/gl-texture2d.c`): `gs_texture_map()` mapped the persistent, per-texture
  Pixel Unpack Buffer with a bare `glMapBuffer(GL_PIXEL_UNPACK_BUFFER, GL_WRITE_ONLY)`. That
  PBO is allocated ONCE per dynamic texture and reused every frame with no
  re-specification, so the driver must guarantee the GPU has finished consuming the
  PREVIOUS frame's upload before handing back a CPU-writable pointer — an implicit
  CPU↔GPU sync/fence. Measured live on imag-nb: a 6-source OBS multiview (each source
  re-uploads its async texture every render) cost ~24ms/frame with the CPU pinned at 101%
  of one core and the GPU idle 7-9% — the D3D11/Windows backend never pays this because it
  maps with `D3D11_MAP_WRITE_DISCARD`, which never blocks (why strih's multiview is
  11.5ms). Fix: map with `glMapBufferRange(GL_PIXEL_UNPACK_BUFFER, 0, size,
  GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT)` instead — the same idiom already used
  by this file's neighbour `gl-helpers.c`'s `update_buffer()` for vertex/index buffer
  streaming — plus a shared `pixel_unpack_buffer_size()` helper so the map-time size and
  the allocation size can't drift apart. `gl-texture3d.c`'s mirrored PBO was NOT patched:
  this vendored OBS has no `gs_texture_map`/`gs_voltexture_map` path for `GS_TEXTURE_3D`
  on the GL backend at all, so there is no live instance of the anti-pattern there today.
  Guarded by `tests/gl_pbo_orphan.rs` (pure vendored-source-text assertions, no probe
  feature / GPU needed) so a `git subtree pull` can't silently revert it — including a
  guard that fails loudly if a future change ever adds a 3D map path without the same fix.

### POST-EVENT deploy + enable (#111) — USER-TIMED, do NOT run before the live event

The #111 code ships in the genlock distroav.dll. Deploying it is the **same in-place
DistroAV artifact swap as the 2026-06-13 / 2026-06-17 genlock upgrades** — user-timed,
off the live-event window (the user controls when). Steps:

1. **Build:** dispatch `windows-genlock.yml` on the merged `main` commit; download the
   `obs-genlock-windows-x64` artifact (the #111 pwsh gate proves the burn filter compiled
   in). `GENLOCK_BUILD_SHA.txt` records the commit.
2. **Deploy (per box, strih + stream):** back up the current install
   (`C:\obs-backup\<date>-pre111`), then surgical overwrite-keep-extras of
   `obs64.exe` + `obs.dll` + first-party `obs-plugins` into `C:\Program Files\obs-studio`
   and the genlock `distroav.dll` into
   `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit` (3rd-party plugins preserved).
   Byte-for-byte diff-verify against the artifact. Graceful OBS shutdown (WebSocket
   `ExitOBS` / `CloseMainWindow`, never force-kill), relaunch with `cwd = bin\64bit`.
   `drift-guard --check-pins` must show NO DRIFT.
3. **PROBE scene + enable (#257 — runtime, no env, no relaunch):** on a DEDICATED probe scene
   (NOT a production scene) toggle the burn ON by setting the program source's per-source
   `genlock_burn=true` over OBS WebSocket — `scripts/obs_burn_filter.py add --host <ip> --input
   "<NDI input>"` (or `scripts/rig-mode.sh test`, which does both boxes). The run_id (911002 strih
   / 911004 stream) and corner (strih → bottom-LEFT, stream → bottom-RIGHT) are derived from the
   box's host role automatically; the QR size is canvas-relative auto (no `OBS_BURN_QR_PX`). cam2's
   dual-QR rides through in the **TOP** band, so all four QRs (cam2 left/right + strih burn + stream
   burn) sit in the recorded frame WITHOUT overlapping — one stream recording carries every stamp.
   RECORD the probe scene's program output; #108 decodes the burned + ridden-through stamps and
   computes per-hop latency. (Layout assumes the production 1920×1080 strih/stream OBS canvas.)
4. **Disable after the probe run:** toggle `genlock_burn=false` (`scripts/obs_burn_filter.py remove`
   / `scripts/rig-mode.sh event`) — no relaunch. drift-guard's #246 facet asserts no prod source has
   genlock_burn=on.

## Build

Local prototyping happens on dev1 (Linux). The production target is a Windows build for
strih/stream against this exact tree. Build docs land with the first proven build (#41
acceptance); the OBS auto-update dialog is disabled in our build per #43 so a stock OBS
can never overwrite a genlocked install.
