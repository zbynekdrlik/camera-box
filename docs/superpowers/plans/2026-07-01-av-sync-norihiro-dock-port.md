# A/V-sync via norihiro dock port + norihiro-compatible marker (#188 / #145)

> Supersedes the chirp+cross-correlation design (`2026-07-01-cam2-av-sync-calibration-design.md` + `-calibration.md`). Reason: the user's reference IS norihiro/obs-audio-video-sync-dock; matching it (his explicit ask) means adopting norihiro's protocol + porting his dock, not inventing a chirp. Confirmed by reverse-engineering `vendor/av-sync-dock/tool/videogen.py` + `src/sync-test-output.cpp`.

**Goal:** the user does A/V sync **himself** in his own OBS (self-service, not relying on Claude), plus a phone-free measurable path off the real rig.

**Repo:** camera-box, branch `dev` (currently `1.7.0-dev.183`, strictly ahead of `main` — no bump needed to START; bump on the first commit of each PR per version-bumping).
**Plan doc:** this file.

## The two deliverables

1. **PHASE 1 — self-service dock (ships first).** Build `vendor/av-sync-dock` (norihiro's
   `obs-audio-video-sync-dock`) as a Windows DLL against OUR genlock-OBS 32.1.2 SDK, mirroring the
   DistroAV CI path, deploy it to strih + stream, and verify the user can measure LIVE A/V sync
   *inside his own OBS* with his existing manual method (norihiro's sync-pattern video on a phone +
   the dock). Zero Rust, zero rig-appliance change. This is the headline, and it unblocks the user
   immediately.

2. **PHASE 2 — QPSK marker from cam2 (measurable off the real rig, no phone).** A NEW pure Tier-0
   Rust module implementing norihiro's EXACT audio-marker + video-QR protocol, a probe-gated
   **continuous-feed** ALSA emitter (replacing the scrapped one-chirp emitter that XRUNs), and a
   recording-verdict decoder. Because the marker is byte-compatible with norihiro's, the SAME dock
   reads it — cam2 becomes the "phone", the rig audio path becomes the measurement path.

**The prior chirp+cross-correlation design is SCRAPPED.** It currently lives in:
- `src/av_sync.rs` (pure chirp gen + `detect_chirp_onsets` + `estimate_av_offset_ms`)
- `src/probe/audio_marker_io.rs` (the one-chirp ALSA emitter with the XRUN-replay hack)
- `src/probe/run.rs` (`--audio-marker*` flags, `ChirpParams`, marker-log serialize)

Phase 2 REPLACES these with the QPSK protocol. Do NOT keep the chirp code alongside — delete it in
the Phase-2 PR (mvp-philosophy: no dead code) after the QPSK path is green. The robust median/MAD
offset estimator in `av_sync.rs` (`estimate_av_offset_ms`, `median`, `required_delay_ms`) is
protocol-agnostic and is SALVAGED into the new module.

---

## PHASE 1 — Build + deploy the dock DLL (self-service)

### 1.1 Vendor the quirc submodule content INTO the tree (the one hard blocker)

`vendor/av-sync-dock/deps/quirc/` is EMPTY (the subtree/depth-1 import did not fetch the submodule;
confirmed: `ls` returns nothing). The build needs
`deps/quirc/lib/{decode.c,identify.c,quirc.c,version_db.c}` + `deps/quirc/lib/quirc.h`
+ `deps/quirc/LICENSE`. The vendored tree is a subtree (not a live submodule in camera-box), so the
quirc sources must be COMMITTED directly into the tree.

Steps (run at repo root):
```bash
cd vendor/av-sync-dock
git submodule update --init deps/quirc     # try the recorded submodule first
ls deps/quirc/lib/{decode.c,identify.c,quirc.c,version_db.c,quirc.h} 2>/dev/null
```
If that does not populate it (the camera-box outer repo has no submodule wiring for a subtree),
clone quirc pinned and copy the sources in:
```bash
tmp=$(mktemp -d); git clone --depth 1 https://github.com/dlbeer/quirc.git "$tmp/quirc"
mkdir -p vendor/av-sync-dock/deps/quirc/lib
cp "$tmp/quirc/lib/"{decode.c,identify.c,quirc.c,version_db.c,quirc.h} vendor/av-sync-dock/deps/quirc/lib/
cp "$tmp/quirc/LICENSE" vendor/av-sync-dock/deps/quirc/LICENSE
git -C "$tmp/quirc" rev-parse HEAD > vendor/av-sync-dock/deps/quirc/QUIRC_PINNED_SHA.txt
rm -rf "$tmp"
```
Remove the now-inert submodule pointer (content is vendored, not a submodule):
```bash
git rm --cached vendor/av-sync-dock/.gitmodules 2>/dev/null || rm -f vendor/av-sync-dock/.gitmodules
```

**Verify before any CI run:** `git status` shows the four `.c`, `quirc.h`, `LICENSE`, and
`QUIRC_PINNED_SHA.txt` staged under `vendor/av-sync-dock/deps/quirc/`. These files ARE the build
input — without them CMake fails at the `add_library(... deps/quirc/lib/*.c)` line. This is the ONLY
genuine build blocker for Phase 1.

### 1.2 CMake compatibility fixes (standalone-build-safe + Qt6-clean)

The dock's `CMakeLists.txt` self-configures ONLY when standalone
(`if(${CMAKE_SOURCE_DIR} STREQUAL ${CMAKE_CURRENT_SOURCE_DIR})`). We build it STANDALONE (its own
cmake root, against our installed obs-sdk prefix), exactly like distroav — so that branch runs and
must resolve against the obs-sdk. Concrete edits to `vendor/av-sync-dock/CMakeLists.txt`:

a. **Keep the vendored `cmake/ObsPluginHelpers.cmake`** (present — provides `find_qt()` +
   `setup_plugin_target()`). The obs-sdk install does NOT ship this file, so the vendored copy is
   required. No edit; just confirm it's committed (it is).

b. **Guard the alias redefinition.** OBS 32 SDK exports `OBS::obs-frontend-api`; it may also export
   `OBS::frontend-api`. Change:
   ```cmake
   add_library(OBS::frontend-api ALIAS OBS::obs-frontend-api)
   ```
   to:
   ```cmake
   if(NOT TARGET OBS::frontend-api)
       add_library(OBS::frontend-api ALIAS OBS::obs-frontend-api)
   endif()
   ```
   (Matches distroav's defensive pattern; harmless-warning → hard-safe.)

c. **Force Qt6** via the configure line (below), not a code edit — `-DQT_VERSION=6`.
   `find_qt(VERSION ${QT_VERSION} ...)` then resolves Qt6 from the obs-sdk prefix. AUTO would also
   pick Qt6, but pinning removes ambiguity and matches the runtime ABI.

**No source-code edits are required.** Every OBS-API version guard in the dock is already correct
for LIBOBS_API_VER > 29.1.3, OBS 31+, and OBS 29.1+ (see the §1.6 audit).

### 1.3 CI integration — a new `avsyncdock` build in the genlock workflows (mirror DistroAV)

**(A) `.github/workflows/windows-genlock.yml` (full production artifact).**
The OBS SDK is installed to `$GITHUB_WORKSPACE/obs-sdk` at the `Install OBS SDK prefix` step
(~line 547). AFTER the `Build DistroAV` step (~line 561) and BEFORE `Stage artifact` (~line 578),
add:

```yaml
      - name: Configure av-sync-dock against the genlocked OBS
        working-directory: vendor/av-sync-dock
        shell: bash
        run: >
          cmake -B build_x64 -G "Visual Studio 17 2022" -A x64
          -DCMAKE_PREFIX_PATH="$GITHUB_WORKSPACE/obs-sdk"
          -DQT_VERSION=6
          -DCMAKE_BUILD_TYPE=RelWithDebInfo

      - name: Build av-sync-dock
        working-directory: vendor/av-sync-dock
        shell: bash
        run: cmake --build build_x64 --config RelWithDebInfo --parallel
```

Then EXTEND the existing `Stage artifact` step (after the distroav copy block, ~line 591) with:

```powershell
          # av-sync-dock plugin + data into the OBS rundir layout (first-party OBS plugin path)
          $dockdll = Get-ChildItem -Recurse vendor/av-sync-dock/build_x64 -Filter obs-audio-video-sync-dock.dll | Select-Object -First 1
          if (!$dockdll) { Write-Error "obs-audio-video-sync-dock.dll not built"; exit 1 }
          Copy-Item $dockdll.FullName "stage/obs-plugins/64bit/"
          New-Item -ItemType Directory -Force -Path "stage/data/obs-plugins/obs-audio-video-sync-dock" | Out-Null
          Copy-Item -Recurse "vendor/av-sync-dock/data/*" "stage/data/obs-plugins/obs-audio-video-sync-dock/"
```

(The dock is a FIRST-PARTY OBS plugin — canonical live path
`C:\Program Files\obs-studio\obs-plugins\64bit\`, NOT the ProgramData distroav path. Staging it into
the rundir `obs-plugins/64bit/` alongside the OBS binaries is correct; NO shadow-DLL risk because it
lives ONLY under Program Files, never also under ProgramData.)

**(B) `.github/workflows/windows-genlock-fast.yml` (pre-merge compile gate).**
- Add `vendor/av-sync-dock/**` to the `paths:` trigger (~line 34) so a dock change fires the fast
  workflow.
- Add a job `avsyncdock-compile-check` mirroring `distroav-compile-check` (~line 246): restore/build
  the prebuilt libobs SDK the same way, then `cmake -B build_x64 -DCMAKE_PREFIX_PATH=<sdk>
  -DQT_VERSION=6` + `cmake --build build_x64` for the dock — catches C++ compile errors in minutes.
  `runs-on: windows-2022`. Keeps the 150-min full build off the critical path.

**(C) drift-guard pin cross-check.** Add the quirc pin (`QUIRC_PINNED_SHA.txt`) + the dock subtree
commit to `vendor/README.md`'s pin table and `scripts/drift-guard.sh --check-pins`, matching
distroav's version + subtree-SHA pins. This makes a future accidental `git subtree pull` that
reverts the quirc content fail loudly in the `drift-guard` CI job.

**(D) source-integrity assertions.** The dock carries NO camera-box patches (stock norihiro) → no
pwsh source-text assertion needed yet. If a dock-side patch is ever added (it should NOT — see §C.1),
add the lock-step Linux-guard + pwsh-assert pair then (the #269 pattern).

### 1.4 SUPERVISOR-driven deploy to strih + stream

Deploy is supervisor-driven (drive-rig-steps-in-supervisor: do NOT hand a long rig deploy to a
death-prone worker). Per `.claude/skills/obs-ops` + `.claude/skills/genlock` + the rig-deploy digest.
**Deploy source = the CI artifact only** (no local build; Tier-0). The DLL lives in the
`obs-genlock-windows-x64` artifact under `stage/obs-plugins/64bit/obs-audio-video-sync-dock.dll` and
`stage/data/obs-plugins/obs-audio-video-sync-dock/`.

Per box (strih `10.77.9.202`, stream `10.77.9.204`), via the `win-strih` / `win-stream-snv` MCP:

1. **Signed artifact URL** (dev1): `gh api repos/zbynekdrlik/camera-box/actions/artifacts/<id>/zip`
   → 302 `Location` signed URL (use the run that built the DLL).
2. **On the box, download with BITS** (survives MCP timeouts; `Start-Job`/http.server do NOT):
   ```powershell
   Start-BitsTransfer -Source "<signed-url>" -Destination C:\Temp\obs-genlock.zip
   Expand-Archive C:\Temp\obs-genlock.zip C:\Temp\obs-genlock -Force
   ```
3. **strih only — stop AutoHotkey FIRST** (SafeLoop respawns obs64 every 1s and locks the DLL
   mid-copy): `Stop-Process -Name AutoHotkey64 -Force`. Then gracefully stop OBS (StopStream /
   StopRecord via WebSocket if live, then close obs64 — graceful-shutdown-before-reboot applies to
   the app).
4. **Copy DLL + data into the OBS install** (canonical first-party plugin path):
   ```powershell
   Copy-Item C:\Temp\obs-genlock\obs-plugins\64bit\obs-audio-video-sync-dock.dll `
     "C:\Program Files\obs-studio\obs-plugins\64bit\obs-audio-video-sync-dock.dll" -Force
   New-Item -ItemType Directory -Force -Path "C:\Program Files\obs-studio\data\obs-plugins\obs-audio-video-sync-dock" | Out-Null
   Copy-Item -Recurse C:\Temp\obs-genlock\data\obs-plugins\obs-audio-video-sync-dock\* `
     "C:\Program Files\obs-studio\data\obs-plugins\obs-audio-video-sync-dock\" -Force
   ```
   Byte-verify each with `Get-FileHash` against the artifact's hash.
5. **Restart OBS** (obs-ops: correct cwd `C:\Program Files\obs-studio\bin\64bit`). strih: restart
   AutoHotkey AFTER OBS is confirmed healthy.

### 1.5 Verify the dock loaded + measures LIVE sync (functional, not liveness)

1. **Load proof (both boxes):** read the newest `%APPDATA%\obs-studio\logs\*.txt` (win-* MCP
   `FileRead`) for `[obs-audio-video-sync-dock] plugin loaded`. Absence, or
   `Failed to load 'obs-plugins/64bit/obs-audio-video-sync-dock.dll'`, means wrong Qt/MSVC runtime
   or wrong path → investigate (do NOT report done).
2. **Dock visible:** `win-strih` MCP `AnnotatedSnapshot`/`Snapshot` — confirm the "Audio Video Sync"
   dock is in the OBS Docks menu / panel (frontend dock).
3. **Functional measurement (the actual self-service claim):** with the user's manual method — play
   `sync-pattern-6000.mp4` (60fps rig) on a phone/display in view of a camera so the pattern reaches
   OBS program; press Start in the dock; read the measured latency-ms readout from the dock panel via
   `OCR`/`Snapshot`. A finite ms readout = the dock works end-to-end. Report the observed ms value,
   not "the dock is present".
   - **Audio-track caveat:** the sync-pattern AUDIO must be on an OBS audio track the dock monitors.
     The user's manual path is hand1 mic → stream OBS input "mbc". If the phone tone is not on an OBS
     audio track, the dock reads video-only and cannot compute an offset — surface this as the
     concrete next step (it is exactly what Phase 2 fixes by putting the tone on the rig audio path);
     it is NOT a Phase-1 failure to hide.

**Phase-1 PR = ONE PR:** quirc vendoring + CMake guards + both workflow edits + drift-guard pin.
Merge (auto per pr-merge-policy — repo has NO `merge=manual` marker), then supervisor-deploy +
verify. Completion report cites the dock load line + the measured ms.

### 1.6 Compat-risk audit (Phase 1) — each risk + resolution

| Risk | Status | Resolution |
|---|---|---|
| **Dock registration API** (`obs_frontend_add_dock_by_id`, OBS ≥ 29.1.4) | SAFE | OBS 32 > 29.1.3 → new API path taken; `dock-compat.cpp` shim only compiles on ≤29.1.3. |
| **Global config API** (`obs_frontend_get_global_config` removed in 31) | SAFE | Code guards `< 31` → `obs_frontend_get_app_config()` on OBS 32. |
| **Video-format enums** P216/P416 (added 29.1) | SAFE | Guard `>= 29.1.0` passes on OBS 32. |
| **`obs_register_output` struct fields** | SAFE | Uses only pre-28 stable fields. |
| **Qt5 vs Qt6** — `find_qt` auto-detect, pointer-to-member `connect`, AUTOMOC/AUTOUIC | SAFE | Widgets/signals used are Qt5.10+/Qt6 identical. Pin `-DQT_VERSION=6`; obs-sdk ships Qt6Config. |
| **Qt runtime at deploy** | SAFE | DLL links the SAME Qt6 the OBS 32.1.2 install bundles in `bin\64bit\`. Built-against == runtime ABI. |
| **`OBS::frontend-api` alias redefinition** | FIXED §1.2b | `if(NOT TARGET ...)` guard. |
| **MSVC runtime** (VCRUNTIME140_1) | SAFE | CMakeLists adds `/d2FH4-` to avoid the VCRUNTIME140_1 dep; boxes run OBS's bundled MSVC runtime. |
| **quirc sources missing** | FIXED §1.1 | Vendored into the tree + pinned. The ONLY genuine blocker. |

---

## PHASE 2 — QPSK marker from cam2 (norihiro-compatible, no phone)

Goal: cam2's appliance EMITS norihiro's EXACT audio marker (so the SAME dock reads it live) AND we
can decode it off the RECORDING (recording-verdict) for an automated, phone-free A/V-sync verdict.
Follow the Tier-0 pure-seam pattern (`av_sync.rs`/`colour_scale.rs`): pure math at the crate root,
probe-gated I/O glue in `src/probe/`.

### 2.0 SCRAP the chirp design

In the Phase-2 PR:
- Create `src/qpsk_marker.rs` (pure, §2.1); add `pub mod qpsk_marker;` to `src/lib.rs`. SALVAGE the
  protocol-agnostic estimator (`estimate_av_offset_ms`, `EmittedMarker`, `AvOffsetEstimate`,
  `OffsetSearch`, `default_search`, `median`, `required_delay_ms`) from `av_sync.rs` into it.
- **Delete** `src/av_sync.rs` and its `pub mod av_sync;` at `src/lib.rs:42` (chirp gen +
  cross-correlation are scrapped).
- **Delete** `src/probe/audio_marker_io.rs` (one-chirp XRUN emitter) → replaced by
  `src/probe/qpsk_emit.rs` (§2.3); update `src/probe/mod.rs`.
- **Update** `src/probe/run.rs`: `--audio-marker*` flags now drive the QPSK emitter; drop
  `ChirpParams` / `serialize_marker_log` chirp usage (the CSV marker-log stays, keyed on QPSK index).

### 2.1 Pure Tier-0 module: `src/qpsk_marker.rs` (crate root, default features)

Module doc MUST explain "why crate root not probe" (CLAUDE.md requirement; copy the
`colour_scale.rs` pattern). NO probe deps.

**EXACT protocol constants (from `tool/videogen.py` + `src/sync-test-output.cpp`):**
```rust
pub const AUDIO_SAMPLE_RATE_HZ: u32 = 48_000;   // --ar default
pub const CARRIER_HZ_DEFAULT: u32 = 442;        // f= default
pub const N_PAYLOAD_BITS: u32 = 20;             // 4 preamble + 8 index + 4 zero + 4 CRC
pub const N_SYMBOLS: u32 = N_PAYLOAD_BITS / 2;  // 10 symbols, 2 bits each
pub const PREAMBLE_NIBBLE: u32 = 0xF;           // bits[19:16] = 0b1111
pub const CRC4_POLY: u32 = 0x13;                // x^4 + x + 1 (CRC-4/ITU)
pub const AMPLITUDE: f64 = 0.8;                 // context default
pub const AUDIO_CONTINUOUS_CYCLES: f64 = 0.25;  // raised-cosine smoothing window (carrier cycles)
pub const Q_FRAMES: u32 = 2;                     // frames per QR/sync segment; cycle = q*3 video frames
```

**Symbol → waveform (2-bit symbol, exactly norihiro's):** `sym 0 (00)→sin`, `1 (01)→cos`,
`2 (10)→-cos`, `3 (11)→-sin`; `phase = sample_index * 2π * f / ar` (cumulative within a symbol run).

**Auto cycles-per-symbol `c`** (when caller passes `c=0`) — integer floor division matching Python `//`:
```rust
/// c = q * f * vr_den // (vr_num * n_sym_after_vsync), n_sym_after_vsync = 10. Must be > 0.
pub fn auto_c(q: u32, f_hz: u32, vr_num: u32, vr_den: u32) -> u32 {
    q * f_hz * vr_den / (vr_num * N_SYMBOLS)
}
```
Rig 60/1, q=2, f=442 → `c = 2*442*1 / (60*10) = 1`. vr=30 → `c = 2`. Assert `c > 0` (a 0 would emit
a zero-symbol marker — fail loudly per script-failure-policy).

**CRC4 encoder (matches `crc4()` in videogen.py):**
```rust
pub fn crc4(mut data: u32, size: u32) -> u32 {
    data <<= 4;
    let mut p = 0x13u32 << (size - 1);
    let mut s = size as i64;
    while s > 0 {
        if data & (0x8u32 << s) != 0 { data ^= p; }
        s -= 1;
        p >>= 1;
    }
    data
}
```

**CRC4 decoder-check (matches `crc4_check()` in sync-test-output.cpp):**
```rust
pub fn crc4_check(mut data: u32, size: u32) -> u32 {
    let mut p = 0x13u32 << (size - 5);
    let mut s = size;
    while s > 4 {
        if data & (1 << (s - 1)) != 0 { data ^= p; }
        s -= 1;
        p >>= 1;
    }
    data
}
```

**Payload word (20-bit, exactly norihiro's):**
```rust
/// bits[19:16]=0xF preamble, bits[15:8]=index, bits[7:4]=0, bits[3:0]=CRC4.
pub fn payload_word(index: u8) -> u32 {
    let data16 = 0xF000u32 | (index as u32 & 0xFF);   // pre-CRC word
    (data16 << 4) | crc4(data16, 16)
}
```

**Symbol sequence (MSB-first, symbol 0 = bits[19:18]):**
```rust
pub fn symbols(word20: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    for i in 0..10u32 {
        let shift = N_PAYLOAD_BITS - 2 - i * 2;   // 18,16,...,0
        out[i as usize] = ((word20 >> shift) & 0b11) as u8;
    }
    out
}
```
(Preamble `0xF` = symbols `[3,3]` = −sin,−sin — the shape the dock's preamble detector locks onto.)

**Continuous audio for ONE marker cycle** — one source of truth for emitter AND any re-render check:
```rust
pub struct AudioParams {
    pub sample_rate: u32,  // 48000
    pub carrier_hz: u32,   // 442
    pub c: u32,            // cycles per symbol (>0; auto_c or caller)
    pub amplitude: f64,    // 0.8
    pub rectangle: bool,   // false = raised-cosine smoothing
    pub vr_num: u32, pub vr_den: u32, pub q: u32,  // leading-silence anchor
}

/// Render one marker's audio as mono f32 in [-1,1]: n_blank_begin zeros
/// (= ar*(q*2)*vr_den // vr_num, positions symbol-0 at vsync centre), then the 10 QPSK symbols
/// with raised-cosine boundary smoothing at boundaries where the adjacent symbol differs.
/// `start_offset` subtracts already-emitted lead samples (continuous interleaving).
pub fn render_marker_audio(index: u8, p: &AudioParams, start_offset: usize) -> Vec<f32>;

/// mono f32 → interleaved stereo i16 LE (dup L=R), clamped.
pub fn to_stereo_i16(mono: &[f32], amplitude: f64) -> Vec<i16>;
```

**Video-QR content string (exactly norihiro's — for Phase-2 video pairing, §C.1 option b):**
```rust
/// "q={q_ms},i={i},f={f},c={c},t={t},I={I}"; q_ms = q*1000*vr_den // vr_num.
pub fn qr_text(q_ms: u32, index: u8, f_hz: u32, c: u32, type_flags: u32, index_max: u32) -> String;
```

**Decoder side (pure, for recording-verdict, §2.4):**
```rust
/// Running-cumulative IQ prefix sums at the carrier (real=v*sin, imag=v*cos), i64 to avoid overflow.
pub fn iq_prefix_sums(samples: &[f32], sample_rate: u32, carrier_hz: u32) -> (Vec<i64>, Vec<i64>);

/// Preamble-peak detect (det = det8_0 + det12_8 per the protocol digest) → decode the 20-bit word
/// at each peak → keep only crc4_check(word,20)==0. audio_ts anchor = peak − symbol_ns*N/2
/// (vsync-centre), exactly norihiro's. Returns (audio_ts_s, index) per valid frame.
pub fn decode_markers(samples: &[f32], sample_rate: u32, carrier_hz: u32, c: u32)
    -> Vec<(f64, u8)>;
```
Plus the SALVAGED `estimate_av_offset_ms` / `required_delay_ms` (protocol-agnostic offset math).

### 2.2 RED→GREEN tests for the pure module (Tier-0, default features)

All in `#[cfg(test)] mod tests` in `src/qpsk_marker.rs`. Observe RED→GREEN with the one-off bypass:
`cargo test --lib qpsk_marker # airuleset:build-ok`.

1. **`crc4_roundtrip_matches_norihiro`** — for index 0..=255: `crc4_check(payload_word(i), 20) == 0`.
   RED: `crc4` stub returns 0. GREEN: real poly.
2. **`preamble_symbols_are_neg_sin_neg_sin`** — `symbols(payload_word(0))[0..2] == [3,3]`.
3. **`auto_c_matches_python_floor_div`** — `auto_c(2,442,60,1)==1`, `auto_c(2,442,30,1)==2`.
4. **`round_trip_encode_decode_clean`** — for several indices: `render_marker_audio(i,..)` →
   `decode_markers(..)` recovers exactly `[(_, i)]`, audio_ts within ±1 symbol of the symbol-region
   start. RED: decoder stub empty. GREEN: full IQ demod. THIS proves byte-compatibility with the dock
   end-to-end in pure Rust.
5. **`round_trip_survives_noise_and_gain`** — add deterministic pseudo-noise (`sin*43758` trick) +
   0.3 gain; decoder still recovers the index.
6. **`decode_rejects_corrupt_crc`** — flip one payload bit → the frame is dropped (crc4_check != 0).
7. **`qr_text_exact_format`** — `qr_text(33,7,442,1,0,256) == "q=33,i=7,f=442,c=1,t=0,I=256"`.
8. **`stereo_i16_is_clamped_and_dup`** — clamps to i16 and L==R.
9. **`offset_estimator_recovers_constant_offset`** — reuse the salvaged estimator test (median/MAD).

### 2.3 Probe-gated CONTINUOUS-FEED ALSA emitter: `src/probe/qpsk_emit.rs`

Replaces `src/probe/audio_marker_io.rs`. The scrapped emitter wrote ONE chirp then went idle — the
tiny ALSA ring underran between markers (XRUN), and on the rig the `appl_ptr` stuck at one chirp so
only the FIRST marker ever played. **Fix = a continuous feed: the ring NEVER drains.** The device
plays SILENCE between markers from the SAME continuous stream, so there is no start/stop XRUN.

Design (`#[cfg(feature="probe")]`):
- Open `PCM::new("hw:CARD=PCH,DEV=3", Direction::Playback, false)` (the confirmed cam2 emit device),
  48000 Hz, stereo (fall back to mono if the device requires it — probe at open), `Format::S16LE`,
  `Access::RWInterleaved`. Period ~1024 frames, buffer ~4× period (bigger than the scrapped bursty
  buffer) so a single period miss doesn't XRUN.
- **Feeder loop (the core fix), written against a trait so it is TESTABLE without hardware:**
  ```rust
  pub trait PcmSink {
      fn avail(&mut self) -> i64;             // frames the ring can accept now
      fn write(&mut self, frames: &[i16]) -> Result<(), PcmErr>;  // Err(Epipe) on XRUN
      fn prepare(&mut self);                  // after XRUN
  }
  /// Drives a gap-free stream: each tick, fill `avail()` frames from a CONTINUOUS logical stream
  /// where a marker's samples are queued at each cadence tick and ZEROS between. On Epipe: prepare()
  /// and RESUME at the current logical position (never re-open, never single-marker replay).
  pub fn run_feeder<S: PcmSink>(sink: &mut S, params: FeederParams, ticks: u64, log: &mut Vec<(u8,i64)>);
  ```
- Between markers, queue ZEROS (never let `avail` drain to the whole ring). At a cadence tick, queue
  `render_marker_audio(index, ..)` and advance the 8-bit index. On Epipe → `prepare()` + resume at
  logical position; log every recovery (comprehensive-logging).
- **Emit-log for pairing:** on each marker emit append `(index, emit_wall_ts_ns)` where the ts comes
  from `snd_pcm_htimestamp` / `pcm.status().get_htstamp()` at the sample the marker STARTS, mapped to
  wall clock — so the recording decoder pairs video index ↔ audio index ↔ ts. Write the marker-log
  CSV (keyed on QPSK index).
- `run.rs` flags: reuse `--audio-marker` (enable), `--audio-marker-device` (default
  `hw:CARD=PCH,DEV=3`), `--audio-marker-cadence-ticks` (default 300 ≈ 5 s @ 60 Hz); add
  `--audio-marker-carrier-hz` (default 442) + `--audio-marker-fps num/den` (rig 60/1) so `c`
  auto-derives.

**RED→GREEN for the emitter (probe-gated, CI-only per Tier-0; the feeder is trait-driven so RED→GREEN
is observable on CI's `--features probe` job):**
- **`feeder_never_queues_a_gap`** — run `run_feeder` for N ticks against a fake `PcmSink` reporting
  steady `avail`; assert (a) the sink NEVER sees a drain to zero between markers (continuous silence
  written) and (b) exactly one marker's samples per cadence tick, index advancing 0,1,2,…. RED
  before the continuous feed. GREEN after. This is the PERMANENT guard against the one-chirp
  regression (strict-test mandate — a real signal, not a proxy).
- **`feeder_recovers_from_epipe_without_reopen`** — fake sink returns Epipe once; assert `prepare()`
  called and the logical position CONTINUED (no re-open, no single-marker replay).
- The ALSA-concrete `PcmSink` impl (open + avail_update + writei + prepare) is thin glue → add to
  the mutants `--exclude-re` list (hardware I/O). The trait-driven feeder is NOT excluded — fully
  mutation-tested.

### 2.4 recording-verdict decoder path

Wire `decode_markers` into recording-verdict so an E2E recording yields an A/V-sync ms:
- Extract the recording AUDIO track to mono f32 (ffmpeg, already in the recording-decode path — see
  `.claude/skills/recording-decode`). Feed to `decode_markers` → `(audio_ts_s, index)`.
- Pair with the VIDEO timestamp. Two sources (see §C.1):
  (a) if Phase-2 paints norihiro-format QR: decode the QR index + the sync-image brightness
      zero-cross (norihiro's own video-marker path); OR
  (b) pair the audio index against the emit-log `emit_wall_ts_ns` and the recording frame timestamps
      (no new video marker — RECOMMENDED, keeps the dual-QR gate untouched).
- `offset_ms = video_ts − audio_ts` per matched index; report median + MAD via the salvaged
  `estimate_av_offset_ms`.
- **RED→GREEN:** a synthesized audio + known-index stream (or a #186-style fixture recording) with a
  KNOWN injected offset; assert the decoder reports it within ±1 frame. RED before the decode wiring,
  GREEN after.

### 2.5 Phase-2 PR scoping

Phase 2 is ONE feature = ONE PR (single-feature multi-PR rollout is banned): `qpsk_marker.rs` (pure,
salvaged estimator) + `probe/qpsk_emit.rs` (continuous feed) + recording-verdict decode wiring +
scrap of the chirp code + all RED→GREEN tests. Touches only Rust — `ci.yml` gates it
(lint/test/coverage/build/windows-probe/mutants); it does NOT touch `vendor/` so the Windows genlock
workflows don't fire. If it exceeds the ~600-LoC/≤4-issue bundle ceiling, split ONLY along a real
seam (emitter vs decoder) — but it is one feature, so default to one PR.

---

## (C) RISKS + the genuine technical DECISIONS

### Decision 1 — Video marker: keep norihiro's QR/sync-image, or reuse our dual-QR? (RECOMMEND: phone for Phase 1; audio-index↔emit-log pairing for Phase 2; norihiro-QR only as a later opt-in)

The dock measures VIDEO by decoding norihiro's own QR (`q=..,i=..`) + the 4-corner sync-image
brightness zero-cross. Our rig paints a DIFFERENT dual-QR Vernier (#367/#364) for zero-loss. Options:
- **(a) Phase 1 = norihiro's ORIGINAL sync-pattern video on a phone** — zero rig video change, the
  user's current manual method. Ships now.
- **(b) Phase 2 = cam2 paints norihiro-format QR + sync images ALONGSIDE the dual-QR** — the dock (or
  recording-verdict) reads video sync off the RIG monitor, no phone. RISK: screen real-estate + the
  full-frame sync-image brightness alternation could disturb the dual-QR read / the zero-loss gate
  the user guards fiercely. Real work, real risk.
- **(c) Pair audio index ↔ emit-log wall-ts ↔ recording frame ts (no new video QR).** Compute offset
  WITHOUT painting any new video marker.

**RECOMMENDATION:** Phase 1 = (a) — self-service, ships immediately. Phase 2 automated verdict = (c)
— no new video marker, no risk to the dual-QR gate — AND expose the audio marker so the DOCK reads it
live (audio is byte-compatible; the dock's video side keeps using whatever is on program). Pursue (b)
ONLY if the user specifically wants the dock's VIDEO read off the rig monitor — treat as a separate
later ticket because of the dual-QR-gate risk (strict-test mandate: never disturb a guarded gate).

### Decision 2 — Qt version for the dock build (RECOMMEND: pin Qt6)

obs-sdk (OBS 32.1.2) ships Qt6; the boxes run Qt6. `find_qt` AUTO would pick Qt6 but can be perturbed
by a stray Qt5 config. **RECOMMEND: `-DQT_VERSION=6` explicitly** (§1.2c) — deterministic, matches
the runtime ABI, no code edit. Qt5 is not a real option (OBS 32 is Qt6-only on Windows).

### Decision 3 — Which OBS audio track carries the marker for the DOCK to read live (RECOMMEND: recording-verdict for the automated verdict; route cam2 emit into the "mbc" audio path for the live dock read)

The dock computes an offset only if the sync tone is on an OBS audio TRACK it monitors. The user's
manual path is hand1 mic → stream OBS input "mbc". cam2 emits QPSK on `hw:CARD=PCH,DEV=3` — that
audio must reach an OBS audio source. **RECOMMEND:** for the automated verdict use recording-verdict
(§2.4), which reads the recording's audio track directly (no live-OBS routing dependency); for the
LIVE dock read, route the cam2 emit into the same OBS audio path the user already uses for "mbc" (a
rig audio-routing config in the user's A/V-align domain per memory). Do NOT hard-wire an audio-routing
assumption in code — make the emit device a flag and document the OBS-side routing as a deploy step.

### Other risks

- **quirc content not committed** (Phase 1): the single hard blocker — resolved §1.1; verify the four
  `.c` are staged before the first CI run.
- **`c` auto-derives to 0** (§2.1): at 442 Hz/60 fps `c=1` — OK. Assert `c > 0` so a future fps/f
  combo fails loudly instead of emitting a zero-symbol marker.
- **XRUN regression** (the reason for the continuous feed): `feeder_never_queues_a_gap` is the
  permanent guard — without it a refactor could reintroduce the one-chirp bug.
- **Dock reads video-only with no audio track** (§Decision 3): expected in Phase-1 verify — the
  concrete signal that the audio path needs wiring; surface it, don't hide it.
- **AutoHotkey respawn locking the DLL on strih** (§1.4 step 3): stop AHK before copy, restart after
  — a known obs-ops gotcha, not a new risk.

---

## Ordering (self-service dock FIRST)

1. **PR 1 (Phase 1):** vendor quirc + CMake guards + both genlock workflow edits + drift-guard pin.
   → merge → supervisor-deploy to strih+stream → verify dock loads + measures live sync with the
   phone method. **User can self-serve immediately after this.**
2. **PR 2 (Phase 2):** `src/qpsk_marker.rs` (pure, salvaged estimator, RED→GREEN) +
   `src/probe/qpsk_emit.rs` (continuous feed, RED→GREEN) + recording-verdict decode (RED→GREEN) +
   scrap the chirp code. → merge → deploy cam2 appliance → verify a phone-free A/V-sync ms off the
   recording, and (Decision 3) the dock reading the cam2 marker live.
