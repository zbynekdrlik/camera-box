# vendor/ — the genlock monorepo (one repo, everything needed)

Per the project decision (#41): the WHOLE genlocked AV stack lives in THIS repository —
fresh copies of the latest upstream releases, with our genlock patches applied on top —
so the final working state is reproducible from one place and production boxes never run
an unpinned/stock build.

| dir | upstream | version | imported as |
|---|---|---|---|
| `vendor/obs-studio` | github.com/obsproject/obs-studio | **32.1.2** (commit `fb4d98bf8`) | git subtree --squash |
| `vendor/distroav` | github.com/DistroAV/DistroAV | **6.2.1** (commit `038d9d6`) | git subtree --squash |
| NDI SDK headers | shipped inside DistroAV (`vendor/distroav/lib/ndi/`) | SDK v6 (plugin requires **NDI ≥ 6.3.0**) | part of the DistroAV tree |

The NDI **runtime** (`libndi.so` / `Processing.NDI.Lib.x64.dll`) is NOT committed —
licensing forbids redistribution (see the License Agreement PDF in `lib/ndi/`). Each
machine gets it via the NDI installer / `vendor/distroav/CI/libndi-get.sh`.
**Note:** dev1 + cam2 currently run NDI runtime 6.2.1 — DistroAV 6.2.1 needs ≥ 6.3.0, so
the runtime upgrade is part of the rollout. The production OBS boxes strih + stream already
run NDI runtime **6.3.2.0** (≥ 6.3.0 ✓, verified 2026-06-14).

## Pinned production settings — drift guard (#45)

`scripts/drift-guard.sh` (unit-tested in `tests/drift_guard.rs`) enforces that strih
(`10.77.9.202`) + stream (`10.77.9.204`) stay on the versions above **and** these critical
runtime settings — the known-good zero-loss state verified live on both boxes 2026-06-14. A
*deliberate* rollout (the 30→60 fps step #11, or activating genlock) re-pins the value here as
part of that change; an *unexpected* difference is drift and the guard fails loudly.

| setting | pinned value | live source (read-only) |
|---|---|---|
| `output_fps` | `30` | OBS log `video settings reset: … fps: <n>/1` (current zero-loss rate; re-pin to `60` on the #11 rollout) |
| `genlock_wall_clock` | `1` | OBS log `genlock: wall-clock-slaved render tick ENABLED` (running state) — the genlock master gate, **active** on both boxes since 2026-06-13 (the measured 0-drop strih→stream state). Persistent source is the **Machine** env var `OBS_GENLOCK_WALL_CLOCK=1` (`HKLM\…\Session Manager\Environment`); the gate is read at OBS launch, so the *running* truth is the log line, not a later `$env:` read (which a long-lived launcher/MCP process can hold stale) |
| `ndi_input_latency` | `0` | DistroAV NDI **input** `latency` mode = **Normal** (the obs-websocket `GetInputSettings` `latency` field; `2`=Lowest, `1`=Low, `0`=Normal). This is the **certified LOW-LATENCY zero-loss** ingest mode for the genlocked path (#84). A/B measurement (twice, reversed) found the DistroAV ingest buffer is NOT a real latency lever once genlock is active: the wall-clock render tick dominates emit timing, so **Normal(0) gives a ~33 ms LOWER strih abs_emit p50** (216 ms vs 249 ms at Lowest(2)) while staying zero-loss over a 30-min run — Normal is the more-buffered, lower-latency, loss-free state. It is checked on the **genlocked broadcast-path inputs**: on strih the camera ingests (`NDI cam5`=CAM1, `NDI cam1`=CAM3, `NDI cam3`=CAM4), on stream the strih→stream program feed (`NDI 2ME PGM`). Re-pin only on a deliberate latency rollout (this `0` value IS such a deliberate re-pin, applied + verified live 2026-06-16); an input drifted off `0` is drift the guard flags. Non-broadcast inputs (preview/CG/lyrics) are out of scope of the pin |
| `canonical_plugin_path` | `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit` | The **single canonical OBS plugin-load path** for the genlock DistroAV plugin (#124, EPIC #125). OBS scans MULTIPLE module locations — `C:\Program Files\obs-studio\obs-plugins\64bit` (first-party install dir), `C:\ProgramData\obs-studio\plugins\<plugin>\bin\64bit` (global third-party), and `%APPDATA%\obs-studio\plugins\<plugin>\bin\64bit` (per-user) — so the SAME `distroav.dll` present in more than one of them lets a **stale copy silently shadow the intended build** (the mixed-version incident #119: a pre-#97 DistroAV loaded while every version check still passed). The invariant: `distroav.dll` exists in **EXACTLY ONE** scan path, and that path is this ProgramData one (verified live on strih + stream 2026-06-25 — exactly one `distroav.dll` per box, 663040 bytes, loaded by the Program Files genlock `obs64.exe`; **none** in `Program Files\obs-studio\obs-plugins\64bit` — the `data\obs-plugins\distroav` folder there is resources/locale, **not** the binary). The first-party OBS plugins ship under `Program Files\obs-studio\obs-plugins\64bit`; DistroAV is the one deployed to ProgramData — a deploy MUST NOT also drop a `distroav.dll` into `Program Files\obs-plugins\64bit` (that recreates the shadow). The drift-guard reads every observed `distroav.dll` location (`distroav_dll_paths`, gathered via win-* MCP) and FAILS if there is more than one, or if the lone one is off this path |

The OBS/DistroAV **versions** come from the version table above (single source of truth); the NDI
runtime is checked `≥` the `NDI ≥ 6.3.0` minimum stated there. The two facets:

```bash
./scripts/drift-guard.sh --check-pins    # CI: validate the pin set + cross-check vs vendored source
./scripts/drift-guard.sh --compare host=strih obs_version=… distroav_version=… \
    ndi_runtime=… output_fps=… genlock_wall_clock=… ndi_input_latency=… \
    distroav_dll_paths=…   # live box (values read via win-* MCP); distroav_dll_paths = every
                           # distroav.dll location found across the OBS scan paths (#124)
```

The live read-only run is driven by `/drift-guard` (`.claude/commands/drift-guard.md`), which gathers
the observed values off strih/stream through the win-* MCP tools and feeds them to `--compare` —
CI runners can't reach the production LAN, so the live facet is operator/agent-driven, not in CI.

## Per-component SHA manifest in the bundle (#120, EPIC #125)

drift-guard above pins the **marketing versions** (OBS 32.1.2 / DistroAV 6.2.1) — it cannot catch a
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
  Node identity: reserved per-node `run_id`, env-overridable via `OBS_BURN_RUN_ID`
  (defaults 911002 strih / 911004 stream — outside cam2's range). **Gated behind
  `OBS_BURN_QR` (default OFF)**: with the env unset the filter is a transparent
  pass-through, so registering it on the production install is inert until #108 enables it
  on the dedicated PROBE scene. Guarded by `tests/burn_payload_parity.rs` (which
  compiles+runs the C++ encoder via g++ and asserts byte-identity with `Payload::encode`,
  round-trip through the decoder, and that the rendered QR decodes back via rqrr) + the
  windows-genlock.yml pwsh #111 gate, so a `git subtree pull` can't silently revert it.
  Scope: this is the BURN only — decoding the burned stamps + computing per-hop latency is
  #108 (post-event).

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
3. **PROBE scene + enable:** on a DEDICATED probe scene (NOT a production scene), add the
   "DistroAV QR Burn (latency probe)" filter to the node's program source. Launch that
   OBS with `OBS_BURN_QR=1` and `OBS_BURN_RUN_ID=911002` (strih) / `911004` (stream).
   The #111 4-corner layout (do NOT override the size up to 700 — that re-overlaps the
   camera QR): each node's burn renders ~300px (`OBS_BURN_QR_PX` default 300) in its BOTTOM
   CORNER — **strih → bottom-LEFT, stream → bottom-RIGHT** — while cam2's dual-QR rides
   through in the **TOP** band. All four QRs (cam2 left/right + strih burn + stream burn)
   then sit in the recorded frame WITHOUT overlapping, so one stream recording carries every
   stamp. The corner derives from the standard run_ids above (911004 → bottom-right, else →
   bottom-left); if you use a CUSTOM `OBS_BURN_RUN_ID`, you MUST also set `OBS_BURN_CORNER`
   (`bottom-left` / `bottom-right`) per node, or both burns default to bottom-left and
   re-collide. RECORD the probe scene's program output; #108 decodes the burned +
   ridden-through stamps and computes per-hop latency. (Layout assumes the production
   1920×1080 strih/stream OBS program canvas.)
4. **Disable after the probe run:** unset `OBS_BURN_QR` (or remove the filter) and relaunch
   so the production install is unaffected. The filter is inert by default regardless.

## Build

Local prototyping happens on dev1 (Linux). The production target is a Windows build for
strih/stream against this exact tree. Build docs land with the first proven build (#41
acceptance); the OBS auto-update dialog is disabled in our build per #43 so a stock OBS
can never overwrite a genlocked install.
