---
paths:
  - "src/optical_preflight.rs"
  - "scripts/lib/optical-preflight.sh"
  - "tests/harness_optical_preflight_1141.rs"
---

# Head-end OPTICAL blur/shutter preflight (#1141)

The `[0/8]` head-end optical preflight catches a genuinely misconfigured CAMERA (slow shutter
1/60, PAL/50 Hz, anti-flicker) BEFORE a run — the #216 class the capture-RATE preflight (#656)
is blind to.

## The signal — head-end `rough=`, NOT the imag verdict
- The running `camera-box` service ALREADY logs `capture chroma: u_dev=.. v_dev=.. rough=N -> ..`
  every ~5 s (`src/capture.rs::luma_roughness`, #1079): mean |Y0−Y1| of horizontally-adjacent luma
  pairs — HIGH for a crisp high-contrast pattern, LOW when motion blur smears adjacent pixels. The
  painter dual-QR ADVANCES every 60 Hz flip (a MOVING pattern), so a slow shutter (16.7 ms exposure
  = a full frame period) smears consecutive ticks → low roughness AND optically undecodable.
- **Healthy fleet baseline (measured LIVE, CAM1 2026-08-20, 1/1000 shutter, 60 fps, 800+ samples):
  `rough ∈ 7.1–8.0`, median 7.6, VERY tight.** Blur collapses it toward the flat floor (~0–2). The
  abort floor is **2.5** (`OPTICAL_PREFLIGHT_ROUGH_FLOOR`), ~3× below healthy with margin; it aborts
  only on a SUSTAINED (median-based) breach so a lone low sample never false-aborts a CI gate.
- **TRAP — do NOT calibrate a camera-health gate from `imag_optical_stuck_density` (≈0.195).** That
  is the OBSERVER EFFECT (imag OBS x264 recorder load during the E2E recording, #1130 hop-by-hop
  finding), not the camera — the head-end raw v4l2 capture decodes +1/frame monotonic (~0 %). The
  ticket-body "19.5 %/0.70" numbers are that observer effect; they are NOT a sick-camera signature.
  A blocking flip of the imag stuck signal is deferred to #1144 (needs the observer-effect fix +
  a genuinely-sick clean-run calibration first).

## Decode locus — journal-mine, never a service-stopping v4l2 grab
The cam boxes have **NO zbar and NO probe binary** (only the default-features `camera-box` — verified
live 2026-08-20: `command -v zbarimg` empty, `/usr/local/bin` has only `camera-box`). So on-box QR
decode is impossible, and pulling frames to dev1 + a v4l2 grab would need a `systemctl stop
camera-box` (free the device) + a verified restore — fragile. Journal-mining `rough=` needs none of
that, is immune to the observer effect (capture chain, before the recorder), and mirrors the #656
capture-rate preflight architecture. If you ever DO need QR decode at the head end, the decoder must
be deployed to the cam boxes first — do not assume zbar/probe are there.

## Architecture (mirrors #656 capture-rate + #860 optical-chain-preflight)
- **Pure crate-root `src/optical_preflight.rs`** = SOURCE OF TRUTH: `OPTICAL_PREFLIGHT_ROUGH_FLOOR`
  (2.5), `OPTICAL_PREFLIGHT_MIN_SAMPLES` (5), `classify(&[f32]) -> OpticalPreflightVerdict`
  (median-vs-floor; `< MIN` finite samples → `InsufficientData` → NOTE+proceed), and the fixed
  Slovak `OPTICAL_PREFLIGHT_ABORT_MESSAGE`.
- **`scripts/lib/optical-preflight.sh`** REPLICATES it (const echo fns + an awk median-vs-floor
  classify that extracts ONLY `rough=N` tokens — a bare "16.0" in "NDI display: 16.0 fps" must NOT
  count) + `optical_preflight_assert` (InvocationID-scoped journal read, #656 freshness; NOTE never
  abort on thin data / ssh hiccup; ABORT NAMED on sustained blur).
- **`tests/harness_optical_preflight_1141.rs`** imports the real Rust consts + `classify` (in-crate
  integration test) and pins the shell floor/min/message to them + cross-checks the two classifiers
  on shared fixtures + asserts the recording-e2e.sh wiring. This is the parity gate — the shell and
  Rust can never drift.
- recording-e2e.sh gets ONE source line + ONE `[0/8]` call after the #656 preflight (the #675
  sourced-helper pattern — no existing static anchor edited; new banner/comment avoids the
  `capture-delivery-rate preflight` anchor string).

## Verifying this class of change locally under Tier-0 #557 (NO cargo, not even `--no-run`)
#557 disabled EVERY compiling cargo shape locally (the CLAUDE.md "compile with `--no-run`, run the
binary directly" pattern is DEAD here — the dispatch/hook confirm "no cargo"). So for a pure
crate-root module + a shell replica:
- `cargo fmt --all --check` (allowed — parses + format-checks, catches a stray brace / broken literal).
- Re-implement the classifier logic in a throwaway Python/awk replica and run it against the SAME
  test vectors the Rust unit tests use — proves the calibration + median/floor logic without cargo.
- `source` the shell lib in bash and drive its pure functions with fixtures directly (median, floor,
  message, extraction) — a real local green for the shell half.
- Byte-check the shell abort message against the Rust const (mind Rust's `\`-newline continuation,
  which strips the newline + leading whitespace): both were 240 bytes here.
- The Rust `tests/harness_*.rs` runs on CI — that is the FIRST place the Rust classifier + parity
  test actually execute.
