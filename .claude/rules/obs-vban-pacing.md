---
paths:
  - "vendor/obs-vban/**"
  - "src/vban_pacing.rs"
  - "src/vban_pacing_bench.rs"
  - "tests/vban_pacing_parity_1372.rs"
  - "scripts/lib/genlock-plugin-deploy.sh"
---

# obs-vban send pacing: the vendored VBAN sender on the cg OBS (issue 1372)

## What it is

`vendor/obs-vban` is norihiro/obs-vban 0.3.1 plus one camera-box change: the send thread is
paced. `vendor/obs-vban/CAMERA-BOX-VENDOR.md` records the upstream commit and the full diff.

- The cg OBS on RESOLUME-SNV sends program audio to FOH (fohabl) and lv1 with it.
- Stock 0.3.1 sent one packet per wake, so the stream left in bursts and gaps.
- The patched thread keeps a jitter buffer. The target is 64 ms by default, clamped to 20–200 ms,
  and set by the output setting `pacing_target_ms` ("Send Buffer").
- Packet `n` leaves at `t0 + n × packet_duration` on the disciplined `os_gettime_ns()`. Every due
  packet goes out in the wake. An underflow waits, with no zero-fill; an overflow drops the oldest.
  Both are counted.
- The packets carry the same bytes and the same `nuFrame` as in 0.3.1, so VB-Matrix and every
  other VBAN receiver see no wire change.

## The anchor is "first full packet + target", NOT "depth reached the target"

The block that completes the depth is already inside the depth when it lands. Starting the
schedule then leaves only `target − block (21.3 ms) − packet (5 ms)` of tolerance. The logged
resolume pattern has a lateness spread of 41 ms (14 ms at best, 55.3 ms after the stall), so a
depth anchor underflows at 64 ms. The bench proves it: mutate the anchor and
`logged_pattern_at_64ms_is_smooth_with_no_underflow` fails. With the head-plus-target anchor the
margin is 64 − 5 − 41 ≈ 18 ms. So a target much below ~48 ms underflows on that pattern. Leave the
64 ms default unless the audio-thread load changes.

## The pieces and how they are held together

- `vendor/obs-vban/src/vban-pacing.h`: the pure C decision. It is header-only and has no OBS
  dependency.
- `src/vban_pacing.rs`: the Tier-0 Rust authority. `src/vban_pacing_bench.rs` is its test-only
  `#[path]` child, and it replays the logged callback pattern.
- `tests/vban_pacing_parity_1372.rs` `#include`s the SHIPPED header by its absolute path and
  compiles it under `-Wall -Wextra -Wconversion -Wsign-conversion -Wformat=2 -Werror`. It then
  requires the C decision and state after every scripted wake to equal the Rust.
  - The scripts hit exact deadlines, the overflow limit exactly and one sample over it, underflows
    part way through a burst, and clamped targets.
  - A second test counts those boundary hits, so the gate cannot quietly go blind.
  - 11 hand mutations of the C (every comparison, the headroom, the resets) were each killed by
    value when the gate was written.
- The same test file pins the wiring:
  - both `windows-genlock*.yml` files build the plugin and assert the patch;
  - the full build stages `obs-plugins/64bit/obs-vban.dll`;
  - the thread calls the decision and sleeps to the deadline;
  - `obs-vban pacing:` appears on exactly one log line.
- `scripts/lib/genlock-plugin-deploy.sh` runs on a FULL fleet deploy. It keeps the box's old
  `obs-vban.dll` (step 3b) and byte-verifies the new one in `Program Files\obs-studio\obs-plugins\64bit`
  against the manifest (step 6c).
  - That folder is the one load path on the boxes: resolume has no ProgramData or AppData copy
    (checked read-only on 26.9.2026).
  - A bundle built before issue 1372 has no `obs-vban.dll`. The deploy then warns and leaves the
    box's copy in place.
- `scripts/drift-guard.sh` `genlock_parity_consumed_paths` counts `vendor/obs-vban` as a Windows
  path (the same lock-step with `windows-genlock-fast.yml` as av-sync-dock).

## Verify locally (Tier-0, no cargo)

```bash
CARGO_MANIFEST_DIR=$PWD rustc --test --edition 2021 tests/vban_pacing_parity_1372.rs -o <scratch>/t
<scratch>/t          # unit tests + the bench + the C parity + the wiring pin
```

`clippy-driver --edition 2021 --test -D warnings` on the same file gives CI's lint verdict. The
plugin sources only compile on Windows CI. The Linux net for them is `gcc -fsyntax-only -Wall
-Wextra -Wformat=2 -Werror`, run with these include paths:

- `vendor/obs-studio/libobs`
- `vendor/obs-vban/vban`
- a scratch dir that holds a generated `plugin-macros.generated.h` (the `.h.in` with
  `@PROJECT_NAME@` etc. filled in) and a two-define `obsconfig.h` (`OBS_RELEASE_CANDIDATE 0`,
  `OBS_BETA 0`).

This caught a missing `#include <util/platform.h>`.

## Reading it live (supervisor)

After a FULL-bundle deploy on resolume, read these lines in the OBS log:

- `obs-vban pacing-config:` once per output.
- `obs-vban pacing: depth_ms=… underflows=… overflows=… late_max_ms=…` every 10 s.

What healthy looks like:

- `depth_ms` stays near the target, within about one audio block (21 ms), dipping while the audio
  thread runs late.
- `underflows` stays flat.
- `late_max_ms` stays well under 1 ms.

A growing `late_max_ms` with a flat `underflows` means the send thread wakes late. Suspect the
timer resolution (Windows 11 may ignore `timeBeginPeriod(1)` for a hidden or minimized process,
and `os_sleepto_ns` then sleeps a full scheduler quantum). An `underflows` step matching an
`audio-stall #1367:` window means the audio thread stalled for longer than the buffer. That
upstream load problem is tracked separately on issue 1372; raising the target only buys margin.
