---
paths:
  - "vendor/obs-vban/**"
  - "src/vban_pacing.rs"
  - "src/vban_pacing_bench.rs"
  - "tests/vban_pacing_parity_1372.rs"
  - "scripts/lib/genlock-plugin-deploy.sh"
---

# obs-vban send pacing: the vendored VBAN sender on the cg OBS (issues 1372 + 1381)

## What it is

`vendor/obs-vban` is norihiro/obs-vban 0.3.1 plus one camera-box change: the send thread is
paced. `vendor/obs-vban/CAMERA-BOX-VENDOR.md` records the upstream commit and the full diff.

- The cg OBS on RESOLUME-SNV sends program audio to FOH (fohabl) and lv1 with it.
- Stock 0.3.1 sent one packet per wake, so the stream left in bursts and gaps (issue 1372).
- The patched thread keeps a jitter buffer. The target is 64 ms by default, clamped to 20–200 ms,
  and set by the output setting `pacing_target_ms` ("Send Buffer").
- The FULL bundle replaces obs-vban on EVERY Windows OBS box, not only resolume: the stream box
  has the stock 0.3.1 in `Program Files\obs-studio\obs-plugins\64bit` too (checked read-only
  26.9.2026). Any VBAN output there picks up the 64 ms default: about 64 ms + one audio block of
  added latency. Check a box's VBAN use before its first FULL deploy.

## The fixed timeline (issue 1381)

On 27.9.2026 the issue-1372 pacer turned a late OBS audio thread into lost FOH audio. An underflow
re-primed the schedule, which put a target-long gap on the wire and moved the timeline later.
Above target + 200 ms it dropped the oldest audio, and a 2 s trim dropped more. All of it happened
before packetization with a contiguous `nuFrame`, so VB-Matrix on fohabl never counted a loss.
The pacer now runs the SongPlayer model: the pacer never throws audio away because the audio came
late.

- **Anchor.** `t0` = the first full packet + the target, set ONCE. Packet slot `n` is due at
  `t0 + n × packet_duration` on the disciplined `os_gettime_ns()`. Lateness never moves `t0`.
- **Late audio.** A due slot whose audio is not buffered yet waits up to `GRACE_MS` (100) past
  its deadline. The thread waits on the audio event, at most until the grace ends. The packet then
  leaves late but COMPLETE, and `late_sends` counts it. The catch-up after it is capped at twice
  real time: the next packet may leave half a packet duration after the previous one, until the
  schedule is met again.
- **A late WAKE is not late audio.** If the audio was there and only the thread woke late (0–30 ms
  measured), every due packet still goes out at once, uncapped. The cap applies only after a
  packet that had to wait for its audio, so the wake lateness never compounds.
- **Silence.** A slot still without audio at `deadline + GRACE` is sent as a zero-filled silence
  packet. So is every following slot, on schedule, until the buffer holds the TARGET again. One
  silence episode is ONE counted `discontinuities`.
  - Resuming at the first full packet instead was measured in the model. A buffering hole ending
    near the grace edge (171–192 ms) then cut 3–7 separate 5 ms holes, because each resume slid
    the content by only one packet.
- **Stale repay.** Every silence packet adds one packet to a debt (`stale_samples`). When the pacer
  is on schedule (no cap active) and the buffer holds target + debt, the stall's backlog has
  arrived. The debt is then dropped in ONE whole-packet drop, and the latency is the anchored one
  again. The thread advances `nuFrame` by the dropped packets, so the receiver's own loss counter
  sees it.
  - **A stall past the grace is therefore TWO audible splices** (review round 1). The listener
    hears the silence, then the stalled audio played late by the silence length while the audio
    thread catches up at its own 1.0–1.5× pace (1.2–2.9 s for a 290–600 ms stall on the logged
    pattern), then a forward skip of exactly the silence. The episode counts one
    `discontinuities`, the skip one `repays`.
  - Dropping the stale audio itself as one splice (the design's "drop exactly the stale samples
    once") would need the silence to last until the backlog covers the debt: about 5× the excess
    at a 1.2× catch-up, and forever after a buffering hole, which never brings one. The pacer
    instead resumes at the target depth and repays later. That trade-off was put to the main
    session.
- **A buffering hole has no backlog.** OBS raising its audio buffering skips ticks and delays
  everything after them. Such a debt is never repaid: nothing is discarded, and the silence IS the
  hole. It is forgiven when the next silence episode starts.
- **Ceilings.** More than 2 s buffered, or the next slot more than 2 s overdue (a thread frozen for
  seconds, a host that slept), is one counted `resyncs`. The buffer drops to the target and the
  schedule continues at the first slot at or after now, instead of chasing an old grid at 2×.
- **No trim, no overflow drop.** Only the repay, the ceiling resync and a retarget-down ever drop
  audio, and each one is counted and skips `nuFrame`.
- **Retarget.** A new Send Buffer value while running moves the schedule later (up), or drops the
  difference at the next wake, never below the new target (down, a counted discontinuity).

### Choosing G and the target (measured, 27.9.2026)

The lane's integer model and the bench use the logged callback pattern (14–21 ms callbacks, one of
28 ms a second) and a send thread that wakes 0–30 ms late.

- On a fresh schedule one mixer stall stays clean up to target + G = 164 ms at target 64 (the
  bench pins it; the model had it clean up to ~180 ms, the 14 ms anchor reference adds the rest).
- A 150 ms stall leaves the wire 66–79 ms late (bench / model), so G = 80 would keep only a few
  ms of margin there; 200 ms is the first stall that cuts a silence.
- G = 100 covers the list's 150 ms stall and the largest non-pathological 27.9 hole (128 ms). The
  quiet-regime worst stall (70 ms) sits far inside it ON A FRESH SCHEDULE; after a hole inside the
  grace it may not (see the known consequence).
- The target stays 64 ms: on a fresh schedule 64 + G covers every measured quiet-regime stall.

| scenario (64 ms) | shipped 1372: discarded / wire gap | 1381: silence / discarded | 1381: max wire gap |
|---|---|---|---|
| stall 55.3 / 70 ms | 0 / 0 | 0 / 0 | 35 ms (wake lateness) |
| stall 100 ms | 79.7 / 83.2 ms | 0 / 0 | 35 ms |
| stall 150 ms | 204.1 / 155.9 ms | 0 / 0 (104 late sends) | 66.2 ms |
| stall 290 ms | 293.8 / 292.8 ms | 254 / 254 ms, 1 discontinuity + 1 repay | 91 ms |
| hole 42 / 85 / 106 / 128 ms | 0 / 0–156 ms | 0 / 0 | 35–70 ms |
| hole 149 ms | — | 75 / 0 ms, 1 discontinuity | 91 ms |
| hole 490 ms | 0 / 500.1 ms | 468 / 0 ms, 1 discontinuity | 127 ms |
| 27.9 hole list (6 holes, 957 ms) | 0 / 1034 ms, 5 underflows | 941 / 0 ms, 3 discontinuities | 97 ms |
| hole 128 ms, then stall 55.3 ms | — | 0 / 0 in seed 1 (a silence in some seeds) | 62 ms |
| hole 128 ms, then stall 70 ms | — | 105 / 0 ms, 1 discontinuity | 62 ms |
| hole 128 ms, then stall 100 ms | — | 134 / 0 ms, 1 discontinuity | 62 ms |

The shipped column is the Python port of the 1372 header, worst of 4 seeds. The 1381 columns are
`the_1381_scenario_table`, seed 1.

"Loses nothing" means the SENDER silenced and discarded nothing. A stall inside the grace still
reaches FOH as a wire gap of up to about `stall − 78 ms` (66 ms for a 150 ms stall), followed
by a 2× catch-up. The receiver's own jitter buffer has to ride that out. How VB-Matrix on fohabl
handles such a gap is measured by the ≥ 2 h rehearsal, not by this bench.

### Known consequence: after a hole inside the grace the sender stays late

A buffering hole brings no backlog and `t0` never moves.

- **Which holes.** A hole longer than about target + 14 ms (78 ms at 64) that still stays inside
  the grace: up to about 130 ms in the bench (the 149 ms hole already cuts a silence, which
  rebuilds the margin). The 27.9 holes of 85, 106 and 128 ms are in that range; the 42 ms hole
  leaves nothing late.
- **What follows.** Every later packet leaves about `hole − 78 ms` behind its slot. Nothing is
  lost, but:
  - `late_sends` climbs about 200 per second;
  - the wire follows the audio thread's arrival jitter (smoothed by the 2× cap);
  - the grace left for the next stall is only `G − (hole − 78 ms)`.
- **Measured: the quiet regime itself is no longer covered.** After a 128 ms hole, the quiet
  regime's 70 ms stall and a 100 ms stall, both clean on a fresh schedule, cut 105 and 134 ms
  silences (4 of 4 seeds); the logged 55.3 ms stall does in some seeds.
  `known_limit_after_an_in_grace_hole_the_grace_left_for_a_stall_is_smaller_1381` pins the 70 and
  100 ms cases, as a RED for the fix.
- **Why it matters on resolume.** The cg OBS grows its audio buffering legitimately, from the
  85 ms floor to 128–362 ms (the audio-buffering floor rule of issue 1367), and every growth step
  is a hole of that size for this sender.
- **When it ends.** At the next silence episode (it rebuilds the target depth) or an OBS restart.
- **On the status line.** A `late_sends` count that climbs steadily with `silence_ms` flat means
  exactly this.
- **Why not fixed here.** Restoring the margin needs a policy decision: re-anchoring costs a silence
  of the target. It was reported to the main session as a follow-up candidate.

## The pieces and how they are held together

- `vendor/obs-vban/src/vban-pacing.h`: the pure C decision. It is header-only and has no OBS
  dependency.
- `src/vban_pacing.rs`: the Tier-0 Rust authority. `src/vban_pacing_bench.rs` is its test-only
  `#[path]` child, and it replays the logged callback pattern.
  - The bench adds stalls, buffering holes and the measured 0–30 ms wake lateness.
  - It checks conservation, the frame counter and the latency before and after.
- `tests/vban_pacing_parity_1372.rs` `#include`s the SHIPPED header by its absolute path and
  compiles it under `-Wall -Wextra -Wconversion -Wsign-conversion -Wformat=2 -Werror`. It then
  requires the C decision and state after every scripted wake to equal the Rust.
  - The scripts hit exact deadlines, the grace edge and one ns before it, the cap instants, the
    resume depth and one sample below it, the repay edge and one sample short, both ceilings and
    one over, and retargets while running, starved and silent.
  - A second test counts those boundary hits, so the gate cannot quietly go blind.
- **Mutation proof (issue 1381).** 44 hand mutants were run on a scratch copy of the final tree:
  - Rust mutants: the grace edge, the resume depth, the cap on/off/anchor, the repay condition and
    size, the `repays` count, the forgiveness, both ceilings, the constants, `wait_ms` rounding,
    the late-send condition, `late_max` for silence and the waiting flag after a retarget.
  - The same mutants in the C.
  - The same mutant in BOTH C and Rust, which only the behavioural tests can kill.
  - Wiring mutants: the frame-counter skip, the silence send, the send order, the event wait,
    the zero fill and the `repays=` field.
  - Every mutant was killed.
- **The bench's wake model.** A timed wake (a deadline, or an event wait that timed out) is
  0–30 ms late, as measured. An audio arrival ends an event wait within 0–2 ms, also one that
  lands in the overshoot of a wait that already timed out (`WaitForSingleObject` is still waiting
  then). A late anchor gives the bench more margin than the real thread has: review rounds 1 and 2
  each found one, and the second made the 149 ms hole and the hole-then-stall cases honest.
- The same test file pins the wiring:
  - both `windows-genlock*.yml` files build the plugin and assert the patch, including the
    `late_sends` status line and the `send_silence` call;
  - the full build stages `obs-plugins/64bit/obs-vban.dll`;
  - the thread calls the decision, sleeps to the deadline (`pacing_sleep_until`) or waits on the
    audio event (`vban_pacing_wait_ms`), zero-fills silence packets and advances `nuFrame` by the
    dropped packets, in the order drop and skip, audio packets, silence packets;
  - `obs-vban pacing:` appears on exactly one log line, and the retired `underflows` /
    `overflows` / `trims` / `OVERFLOW_HEADROOM` / `TRIM_WINDOW` never come back.
- `scripts/lib/genlock-plugin-deploy.sh` runs on a FULL fleet deploy. It keeps the box's old
  `obs-vban.dll` (step 3b) and byte-verifies the new one in `Program Files\obs-studio\obs-plugins\64bit`
  against the manifest (step 6c).
  - That folder is the one load path on the boxes: resolume has no ProgramData or AppData copy
    (checked read-only on 26.9.2026).
  - A bundle built before issue 1372 has no `obs-vban.dll`. The deploy then warns and leaves the
    box's copy in place.
- `scripts/drift-guard.sh` `genlock_parity_consumed_paths` counts `vendor/obs-vban` as a Windows
  path (the same lock-step with `windows-genlock-fast.yml` as av-sync-dock). A Linux strih
  (`version-integrity-gate.sh --strih-linux`) gets the Linux set, so a Windows-only obs-vban or
  av-sync-dock change never reads as a strih/stream parity DRIFT.

## The sleep: a high-resolution timer, then a short spin

`os_sleepto_ns()` alone sleeps `ms − 1` and then spins up to about 2 ms on `YieldProcessor()`:
about a third of a core at a packet every 4.98 ms, on the box whose audio thread is already near
its budget. `pacing_sleep_until()` waits on a Windows high-resolution waitable timer
(`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`) to 0.2 ms before the deadline and lets
`os_sleepto_ns()` spin only that last stretch. If the timer cannot be created it logs
`obs-vban pacing-sleep:` once and uses `os_sleepto_ns()` alone. On Linux `os_sleepto_ns()` already
sleeps without spinning. While a slot waits for its audio the thread waits on the audio event with
`os_event_timedwait` in whole ms (`vban_pacing_wait_ms`: rounded up to the grace end, at most
10 ms per wait); with the default 15.6 ms Windows timer resolution that wait can run late, which
only delays the grace check, never loses audio.

## Verify locally (Tier-0, no cargo)

```bash
CARGO_MANIFEST_DIR=$PWD rustc --test --edition 2021 tests/vban_pacing_parity_1372.rs -o <scratch>/t
<scratch>/t          # unit tests + the bench + the C parity + the wiring pin
<scratch>/t the_1381_scenario_table --nocapture   # the scenario table
```

`clippy-driver --edition 2021 --test -D warnings` on the same file gives CI's lint verdict (it
caught a `type_complexity` in the bench). The plugin sources only compile on Windows CI. The Linux
net for them is `gcc -fsyntax-only -Wall -Wextra -Wformat=2 -Werror -std=gnu11`, run with these
include paths:

- `vendor/obs-studio/libobs`
- `vendor/obs-vban/vban`
- a scratch dir that holds a generated `plugin-macros.generated.h` (the `.h.in` with
  `@PROJECT_NAME@` etc. filled in) and a two-define `obsconfig.h` (`OBS_RELEASE_CANDIDATE 0`,
  `OBS_BETA 0`).

This caught a missing `#include <util/platform.h>` (issue 1372). It also caught a `PRIu64`
mismatch: `x / 1000000ULL` is `unsigned long long` on Linux while `uint64_t` is `unsigned long`
(issue 1381). Cast an arithmetic result to `(uint64_t)` before passing it to a `PRIu64`.

## Reading it live (supervisor)

After a FULL-bundle deploy on resolume, read these lines in the OBS log:

- `obs-vban pacing-config: target_ms=… grace_ms=100 packet_samples=… rate=… counters=reset
  stream='…'` once per output.
- `obs-vban pacing: depth_ms=… late_sends=… discontinuities=… repays=… silence_ms=…
  discarded_ms=… resyncs=… late_max_ms=… target_ms=… dest=<ip>:<port> stream='…'` every 10 s.
  The counters are cumulative since the thread started. `late_max_ms` is the largest lateness of
  an AUDIO packet in the window (a silence slot is late by design and does not count).

What healthy looks like:

- `discontinuities`, `repays`, `silence_ms`, `discarded_ms` and `resyncs` stay flat. This is the
  owner's bar: every one of them is audible.
- `late_sends` stays flat in the quiet regime. A step with flat `silence_ms` means an audio-thread
  stall was absorbed inside the grace. A steady climb means a buffering hole left the sender late
  (see the known consequence above).
- `depth_ms` stays near the target, within about one audio block (21 ms).
- After a stall's discontinuity, `repays` steps and `discarded_ms` catches up with `silence_ms`
  once the backlog arrives. After a buffering hole, `discarded_ms` stays behind (no backlog).
- `dest=` names the receiver, so FOH and lv1 are told apart.

A growing `late_max_ms` with flat `late_sends` means the send thread wakes late. Suspect the timer
resolution (Windows 11 may ignore `timeBeginPeriod(1)` for a hidden or minimized process). A
`discontinuities` step matching an `audio-stall #1367:` window means the audio thread stalled for
longer than target + grace. That upstream load problem is issue 1381's lane 1 (the dock demod) and
issue 1372; raising the target only buys margin.
