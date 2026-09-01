---
paths:
  - "scripts/lib/mv-reverify-escalate.sh"
  - "scripts/lib/ps-encoded.sh"
  - "tests/harness_mv_reverify_escalate_1093.rs"
  - "tests/harness_mv_reverify_resolve_wait_1114.rs"
  - "tests/harness_received_tap_encoded_command_1258.rs"
  - "tests/harness_ps_encoded_fleet_1259.rs"
  - "tests/harness_received_tap_byte_safety_1258.rs"
  - "scripts/frozen-input-alert-watchdog.sh"
  - "scripts/lib/frozen-input-health.sh"
  - "scripts/cadence-alert-watchdog.sh"
---

# Sender-bounce reverify: painter-order proof + receiver-wedge escalation (#1093)

`preflight_mv_reverify()` (recording-e2e.sh) probes strih's `NDI camN` for pixel change after each
cam-box redeploy. `mv-reverify-escalate.sh` wraps it (`mv_reverify_or_escalate`, the deploy-site
drop-in for `preflight_mv_reverify … || exit 1`). Load before touching either.

## The dual root cause of a false "no pixel change" — and the DISCRIMINATOR

Every cambox leg's picture is **cam2-painter's HDMI** (one camera → splitter → all boxes). "No
pixel change" has TWO causes, both false-aborting the run:
1. **SOURCE dark** — cam2-painter mid-restart (its `KmsPresenter` start line lags the probe).
2. **RECEIVER wedge** — strih's DistroAV never re-locks after a sender bounce (issue 1096; only an
   OBS restart cures it).

**The discriminator is strih's `genlock-fifo audit '<src>': received=` counter:** a frozen SOURCE
keeps SENDING 60 fps of identical frames so `received=` keeps ADVANCING; a wedged RECEIVER stops it.
So (a) prove the painter is PAINTING before the probe, and (b) use the `received=` delta to fire the
heavy OBS restart ONLY for the receiver case — never a dead painter.

## Placement gotchas (both learned live, #1093)

- **The painter-up wait fires ONCE, before the cam1 probe ONLY.** The ALL_CAMBOX loop runs while
  cam2-painter is DELIBERATELY stopped (`[2b/8]` `systemctl stop cam2-painter` → `[3/8]` launches the
  transient painter), so a painter-up wait there would hang/always-warn. The reverify at `[2b/8]`
  passes on analog-grabber capture noise, not the painter being live.
- **A frozen SOURCE still advances `received=`; only a wedged RECEIVER freezes it** — this is why the
  painter-up proof and the `received=` delta are complementary, not redundant: the proof avoids a
  false OBS restart during a transient painter restart (HDMI drops → no recv → looks like a wedge).

## Headless strih-OBS restart — force-kill over ssh, AHK respawns (NEVER an ssh GUI launch)

The E2E harness is HEADLESS (no win-* MCP). To restart strih OBS: **clear `.sentinel` + `Stop-Process
obs64` over ssh** (session-agnostic file-delete + process-kill, allowed per `win-ssh-vs-mcp.md`
Context B) and let strih's session-1 `NL_STARTUP.ahk` respawn ONE clean genlock obs64. **NEVER
`Start-Process obs64` over ssh** (a session-1 GUI launch lands in session 0, invisible — obs-ops
"AHK on strih"). `launch-obs-genlock.sh --force` is a PURE PLANNER that PRINTS a session-1 program
for an agent's MCP — the harness cannot run it; it runs only the kill+sentinel half.

**MANDATORY guard: never kill obs64 unless `Get-Process AutoHotkey64` is alive** — a force-kill with
no respawner leaves strih OBS DOWN (worse than `exit 1`). The PS `exit 2`/`MV_REVERIFY_NO_AHK` before
the kill; the orchestrator fails loud (obs64 untouched) on that. (The `NL_STARTUP.ahk` `SafeLoop=0`
"No"-latch, #774, is NOT detectable over ssh — accepted residual.) A force-kill reload can restore a
saved `genlock_burn=true` (obs-ops) → sweep-off strih burns after the restart.

## Two disciplines that prevent a spurious OBS force-kill

- **READ_FAIL ≠ no-recv:** a healthy 400-line OBS-log tail is NEVER empty, so an EMPTY read = the
  READ itself failed (ssh blip / log absent), NOT "no recv". Never act on absence-of-evidence (mirror
  `frozen_input_classify`'s UNKNOWN). Split the reader: `mv_reverify_probe_raw` + `_extract_received`.
- **Sample gap ≥ 2× the ~5 s audit emit cadence** (default 12 s) so emit jitter/flush can't read the
  same newest `received=` line twice = false WEDGE.

## Recovery activates the input WITHOUT the Multiview projector

The reverify's `--warm-settle 0` relies on strih's built-in Multiview projector keeping `NDI camN`
inputs active — but a fresh OBS after a force-kill may NOT reopen it (SaveProjectors). So the
post-restart re-check sets `PREFLIGHT_MV_REVERIFY_WARM_SETTLE` > 0 (new env seam on the
`--warm-settle` line) → frozen-camera-gate PREVIEW-activates the input itself (#747, Studio Mode,
restores the operator's preview) — no projector dependency, no operator-display manipulation.

Restoring the operator's own strih Multiview after a restart is now done ACTIVELY (**#1098**):
after the WS-return wait + burn sweep-off, `mv_reverify_reopen_multiview_run` re-opens a FULLSCREEN
Multiview projector over OBS-WS (`obs_phase2.py open-multiview`; monitorIndex DERIVED from
`GetMonitorList` — strih is single-monitor, so `open-projectors`' dual-monitor panel+HDMI split
does NOT apply and would fail loud on the absent HDMI monitor). WARN-only (`MV_REVERIFY_REOPEN_MV_CMD`
seam for tests), so a failed re-open never fails a recovered run. strih runs `SaveProjectors=true`
but with an EMPTY `SavedProjectors`, and a force-kill never repopulates it — so OBS's own
save/restore cannot cover this gap; the active re-open is what restores the operator's view.

## Tier-0 testing

All pure/builder pieces are unit-tested with fakes on PATH (the #833/#716 pattern) —
`tests/harness_mv_reverify_escalate_1093.rs`. The env override seams (`MV_REVERIFY_RECEIVED_CMD`,
`MV_REVERIFY_OBS_RESTART_CMD`, `MV_REVERIFY_SWEEP_CMD`, `MV_REVERIFY_REOPEN_MV_CMD` (#1098),
`MV_REVERIFY_OBS_WS_WAIT_ITERS=0`,
`MV_REVERIFY_*_GAP_S=0`) let the orchestrator's decision flow run offline with zero ssh/OBS/network.
The LIVE strih-OBS restart itself is NOT exercisable at Tier-0 — flag it UNVERIFIED for the E2E run.

## After a CLEAR-then-SET receiver reset, give the fresh finder a bounded re-resolve WINDOW (#1114)

The merged WS-side `strih_mv_scenes.py reattach()` is a CLEAR-then-SET that TEARS DOWN + rebuilds
strih's NDI receiver, so its fresh DistroAV finder must RE-RESOLVE the live post-bounce burn sender
by URL — MEASURED at up to ~2 min on the live rig — far longer than the ~52s [2/8] attempt budget.
`mv_reverify_resolve_wait` (called once inside `preflight_mv_reverify`, right after the once-only
attempt-1 reattach kick, DEPLOY context only via `!= "cleanup"`) polls the SAME `frozen-camera-gate.py`
gate at `PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S` until a pixel changes OR a SECONDS-based wall-clock
deadline `PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S` (default 120s). This rides out the re-resolve in
ONE place and returns success early, removing the false "camera leg is dead" FAIL + the destructive
#1093 force-kill escalation on a genuinely-live-but-still-resolving leg.

- **Bound the WALL CLOCK, not accumulated sleeps.** Each poll iteration ALSO spends a
  frozen-camera-gate.py probe, so a `waited += cadence` counter runs ~2x past the documented window;
  use a `SECONDS`-based deadline so `RESOLVE_SETTLE_S` is a truthful bound (#1114 review 🔵-2).
- **Cleanup context stays fast:** the re-resolve window is DEPLOY-only — cleanup (`attempts=1`,
  `CONTEXT=cleanup`) must never outlast a GH-Actions cancellation grace window.
- **Anchor gotcha:** the resolve-wait uses `frozen-camera-gate.py`, never a 2nd `strih_mv_scenes.py`,
  so the #758 "reattach once" body-count invariant holds — and a comment inside
  `preflight_mv_reverify` must NEVER contain the literal `strih_mv_scenes.py` (it would become a 2nd
  body occurrence and break that count).

## ROOT FIX — kick the KNOWN-stale receiver PROACTIVELY, before the counted poll (#1114)

The resolve-wait above still fires REACTIVELY: `preflight_mv_reverify`'s attempt-1 pixel check is
spent FIRST against a receiver still holding the dead pre-bounce URL, so a bounced leg ALWAYS fails
attempt-1 (logging the alarming "no pixel change right after its deploy" / "camera leg is dead"
line) and only THEN kicks. `mv_reverify_proactive_reset` (a new sourced helper in
`mv-reverify-escalate.sh`, called at BOTH deploy sites in `recording-e2e.sh` — the cam1 `[2/8]` site
between `mv_reverify_painter_up_wait` and `mv_reverify_or_escalate`, and inside the ALL_CAMBOX
`[2b/8]` reverify loop before its `mv_reverify_or_escalate`) fires the CLEAR-then-SET reattach +
`mv_reverify_resolve_wait` PROACTIVELY, so the guarded reverify's attempt-1 then passes CLEANLY
(no counted failure, no alarming log, no reliance on the reactive escalation path). Owner directive
(issuecomment-5335833149): "sequence the burn deploy so the receiver is kicked BEFORE the
pixel-change poll starts counting."

- **DOMINANT kick is now proactive; the reactive path in `preflight_mv_reverify` stays the FALLBACK**
  for a genuinely-dead leg whose proactive resolve-wait timed out. `preflight_mv_reverify` is BYTE-
  IDENTICAL (the #758 body-count `strih_mv_scenes.py == 1` invariant is untouched — the helper's
  reattach lives in the lib).
- **SILENT pre-probe guards against a fast-recovery regression (#1114 review 🟡-1).** A LATER
  ALL_CAMBOX-loop camera may have re-resolved on its OWN during the preceding cameras' serial
  reverifies, so the helper first runs one UNCOUNTED `frozen-camera-gate.py` check: if the leg is
  already delivering it returns WITHOUT kicking (never tears down a working receiver). The pre-probe
  logs nothing as a failure and counts toward no attempt budget, so "kick before the counted poll"
  still holds for a genuinely stale leg.
- **WARN-only:** the helper ALWAYS returns 0 (deploy-context only via `!= "cleanup"`, ALL_CAMBOX-
  gated, opt-out `PREFLIGHT_MV_REVERIFY_PROACTIVE=0`); every guard is a set-e-safe `|| return`/`case`/
  `if`-condition idiom + `|| true`-hardened work calls (#1133). `call_timeout` chains through
  `PREFLIGHT_MV_REVERIFY_CALL_TIMEOUT` like `preflight_mv_reverify` (review 🔵-2).
- **`mv_reverify_resolve_wait` now coerces `RESOLVE_SETTLE_S` to an integer** (`${resolve_s%.*}`, the
  #1197 finder-heal precedent) so a float override cannot fatally abort the arithmetic even through
  `|| true` (review 🔵-3).
- **The two deploy-site call-line literals are THEMSELVES test anchors now** (`tests/harness_mv_reverify_proactive_reset_1114.rs`):
  `mv_reverify_proactive_reset "$CAMERA_NAME" "${CAMERA_NAME#cam}"` (cam1) and
  `mv_reverify_proactive_reset "$_cn" "${_cn#cam}"` (ALL_CAMBOX loop), each ordered BEFORE its
  `mv_reverify_or_escalate`; the adjacent comments must never duplicate those literals.

## `mv_reverify_probe_raw` reads strih's OBS log via `-EncodedCommand`, NEVER naive `-Command "..."` (#1258)

strih's Win32-OpenSSH default shell is **cmd.exe**. A naive
`ssh "powershell -NoProfile -Command \"gc (gci ... | sort ... | select ...).FullName -Tail N\""`
is MANGLED by the three-layer bash→ssh→cmd.exe→powershell quoting hazard (the same one
`scripts/lib/win-ssh-exec.sh`'s header documents + solves with `-EncodedCommand`): the unescaped `|`
pipes leak to cmd.exe, so the read returns non-tail noise. This left the `[4c/8]` frozen-camera gate's
`received=` tap (which reuses `mv_reverify_probe_raw`) reading `received=none` on EVERY source of EVERY
attempt of EVERY run — 4/4 INCONCLUSIVE, the abort gate silently never bit (only the QR sweep protected)
— across all runs since #1233. It is a DETERMINISTIC read bug, not a timing/race: the audit lines exist
on-box for `NDI cam1`..`NDI cam7` and `gc -Tail 800 | Select-String "genlock-fifo audit 'NDI cam1': "`
matches 27 lines; `win_ssh_run` (which already uses `-EncodedCommand`) reads strih fine in the SAME
failing run.

- **The read now base64-UTF16LE-encodes the tail command and sends `-NoProfile -NonInteractive
  -EncodedCommand <b64>`** — pure ASCII, no shell-special chars, so cmd.exe can't mangle it; PowerShell
  decodes back to the exact `gc/gci … -Tail N` command with pipes intact.
- **Inlined `iconv -f UTF-8 -t UTF-16LE | base64 -w0`, NOT `. win-ssh-exec.sh`** — that helper carries
  its own top-level `set -euo pipefail`, which would leak strict mode into this source-only lib's
  non-strict callers (the frozen-input watchdog + the Tier-0 harness). Self-guard the encode
  (`_enc="$(… 2>/dev/null)" || _enc=""`) and numeric-clamp any tail override (`case '' | *[!0-9]* → 400`)
  so a metachar override can never inject into the encoded payload.
- **Tier-0 without ssh (`.claude/rules/win-ssh-vs-mcp.md`: agent sessions read strih via win-* MCP, never
  ssh):** a fake `sshpass` on PATH echoing its argv proves the invocation shape — naive→`-Command "gc`
  (RED), fixed→`-EncodedCommand` whose payload `base64 -d | iconv -f UTF-16LE -t UTF-8` decodes exactly to
  the tail command (GREEN), for both the -Tail 400 default and the frozen-cam -Tail 800 override
  (`tests/harness_received_tap_encoded_command_1258.rs`).

## The fleet sweep landed (#1259) — the SHARED encode helper `scripts/lib/ps-encoded.sh`

#1258 fixed only `mv_reverify_probe_raw`; #1259 migrated the 8 OTHER naive `powershell -Command "…|
sort …| select …"` over-ssh OBS-log reads to `-EncodedCommand`, extracting the encode into ONE shared
source-only lib **`scripts/lib/ps-encoded.sh`**:

- **`ps_encoded_command <ps-text>`** — base64-UTF16LE-encode a PowerShell command (`iconv -f UTF-8 -t
  UTF-16LE | base64 -w0`, self-guarded to `""` on a missing encoder, ALWAYS exits 0). The lib carries
  **NO top-level `set -euo pipefail`** (source-only, like `mv-reverify-escalate.sh`) so it never leaks
  strict mode into the non-strict watchdog callers (they run `set -uo pipefail` WITHOUT `-e`) — the
  same reason #1258 inlined rather than sourcing `win-ssh-exec.sh` (whose top-level `set -euo pipefail`
  WOULD leak). This is why there are now three-plus copies of the same encode (win-ssh-exec.sh's
  `win_ssh_run`, the #1258 mv_reverify inline, ps-encoded.sh, and `rig-health-audit.py::_ps_encoded`);
  each copy has a verified rationale (strict-mode leak / #1258 test-lock / the bash↔python boundary).
- **`ps_clamp_numeric <value> <default>`** — clamp a caller/env-sourced `-Tail N` to a bare
  non-negative integer BEFORE it enters the encoded payload (rejects empty / non-digit / negative /
  decimal → default), so a typo'd/hostile env value can never inject shell/PS metachars. Apply it at
  EVERY `-Tail $count` splice (the #1259 review 🟡: it must be consistent — all 5 watchdogs, not 2).

Migrated sites (each sources ps-encoded.sh + emits `-NoProfile -NonInteractive -EncodedCommand`):
`asio-starve-alert-watchdog.sh` (fetch_box_log), `frozen-input-alert-watchdog.sh` (probe_received),
`ndi-halving-watchdog.sh` / `cadence-alert-watchdog.sh` (fetch_box_log), `mv-fps-alert-watchdog.sh`
(probe_mv_log win branch), `scripts/lib/mv-fps-preflight.sh` (mv_fps_preflight_read_cmd win branch —
LIVE in the `[4d1/8]` preflight). Python cannot source a bash lib, so `scripts/rig-health-audit.py`
gets its own `_ps_encoded` / `_windows_obs_log_tail_cmd` / `_windows_obs_count_cmd` (identical base64
UTF-16LE; both bash `iconv` and Python `utf-16-le` emit NO BOM → byte-identical).

**KEPT as-is (NOT the mangling class — do not "migrate" these):** `bundle-state-server.py`'s
LIST-form `subprocess.run(["powershell","-NoProfile","-NonInteractive","-Command",cmd])` runs LOCALLY
on the box (no ssh, no cmd.exe — argv goes straight to powershell.exe); Task Scheduler XML
`<Command>powershell</Command>` (a local scheduled task); `-File`-based invocations; and the doc
comments. `mv_reverify_probe_raw`'s OWN inline encode stays (test-locked, out of #1259's scope).

**Tier-0 test the migrated sites the same fake-sshpass way** (`tests/harness_ps_encoded_fleet_1259.rs`):
source the watchdog (its `[[ "${BASH_SOURCE[0]}" == "$0" ]] && main` guard means sourcing only defines
functions), call `fetch_box_log`/`probe_received`/`probe_mv_log` with a fake `sshpass` on PATH that
appends its argv to a log FILE (probe_received post-processes stdout, so the file — not stdout — is the
reliable capture surface), and `env_remove` the `*_PROBE_CMD` overrides so the REAL branch runs; assert
`-EncodedCommand` present + the payload base64-decodes to the intended PS. The pure builders
(`ps_encoded_command`, `ps_clamp_numeric`, `mv_fps_preflight_read_cmd win`) are tested by direct call.

## Layer 2 (#1258, 2026-09-01) — the EncodedCommand fix was NECESSARY but NOT SUFFICIENT

The layer-1 fix above (`-EncodedCommand`) made `mv_reverify_probe_raw` actually reach the strih OBS
log. A live proof run (1651316094, dev1 at 1.7.0-dev.607) then found `[4c/8]` STILL 4/4 INCONCLUSIVE
on every input, every read printing `grep: (standard input): binary file matches` /
`received= NDI cam1: prev=none curr=none -> UNKNOWN`.

**Root cause:** PowerShell 5.1's `gc` (Get-Content), invoked WITHOUT `-Encoding`, reads the strih OBS
log (UTF-8) as ANSI/CP1252 and re-encodes on output. Any non-ASCII glyph anywhere in the fetched tail
(the `≈` in `(≈F frames @ …)`) comes back as invalid-UTF-8 bytes. On a UTF-8-locale box (this fleet
runs `en_US.UTF-8`), GNU grep then treats stdin as BINARY — stdout goes empty, the notice is
stderr-only — and even where grep still finds the line, `sed`'s trailing `.*` refuses to consume the
invalid byte and leaves line-tail garbage after the captured digits. Either failure produces a value
that is NOT a clean bare integer, so every caller's `case '' | *[!0-9]*` reads it as "none" → UNKNOWN.

**Fix — byte-safe end to end, `LC_ALL=C` on BOTH stages (grep AND sed), plus `-a` on grep:**

```bash
LC_ALL=C grep -aF "genlock-fifo audit '$1': " | tail -n1 | LC_ALL=C sed -n 's/.*received=\([0-9][0-9]*\).*/\1/p'
```

`grep -a` alone is not always enough (an ambient locale can still binary-flag some inputs even with
`-a`, and `sed`'s `.*` needs its OWN `LC_ALL=C` or it silently leaves the tail-garbage failure mode
even when grep itself found the line) — apply BOTH, on BOTH commands, always. Applied identically to
every DIRECT consumer of the same `genlock-fifo audit '<src>': received=` tap family:

- `mv_reverify_extract_received` (this file) — the shared extractor `frozen-cam-received.sh` (the
  `[4c/8]` gate itself, `.claude/rules/frozen-cam-received-gate.md`) and `mv_reverify_probe_received`
  both reuse, so this ONE fix covers the actual gate path.
- `probe_received` (`scripts/frozen-input-alert-watchdog.sh`, `.claude/rules/frozen-input-watchdog.md`).
- `extract_sample` (`scripts/cadence-alert-watchdog.sh`, `.claude/rules/cadence-watchdog.md`) — a
  sibling #1259-migrated watchdog reading the SAME tap family via the SAME `ps_encoded_command` fetch.
- `frozen_input_cambox_sources` (`scripts/lib/frozen-input-health.sh`) — the enumeration reader
  sharing the SAME raw `mv_reverify_probe_raw` stream via `probe_enumerate`; fixed DEFENSIVELY
  (`grep -aoE` + `LC_ALL=C sed -E`) — its specific `-o`+piped-stdin shape did NOT reproduce a Tier-0
  RED via a pipe (any fixture size tried), though the IDENTICAL byte DOES trip GNU grep's binary
  detection through a direct FILE argument on the same grep binary — the hazard class is real even
  though this one shape resisted pipe-mode reproduction in this sandbox.

**Test recipe (Tier-0, `tests/harness_received_tap_byte_safety_1258.rs`):** write a raw multi-line
fixture as literal Rust BYTES (`b"...\xa0..."`, never a `&str` — a genuine invalid-UTF-8 byte cannot
live in a Rust string literal) to a temp FILE, `cat` it into the sourced function via `bash -c`. Two
lines carry the invalid byte (one of them the TARGET line itself, right after `received=N`) to prove
BOTH that an EARLIER corrupted line doesn't blind a LATER clean one, and that the target line's OWN
corruption doesn't leave garbage. RED against the pre-fix code reliably reproduces
`grep: (standard input): binary file matches` + empty extraction for the 3 direct-pipe consumers
(`mv_reverify_extract_received`, `probe_received`, `extract_sample`); `frozen_input_cambox_sources`'s
test is GREEN-only (see above) and is explicitly labeled as such, never overstated as a reproduced
regression.

**NOT covered by #1258 (deliberately, filed as issue 1262 for someone to verify + apply if warranted):**
every OTHER `ps_encoded_command` consumer parses a DIFFERENT audit-line family with the SAME
non-byte-safe grep/sed shape — `mv-fps-alert-watchdog.sh`'s `multiview-audit:` grep, `ndi-halving-
watchdog.sh`'s `recv-timing` tap, `asio-starve-alert-watchdog.sh`'s `asrc:` tap. Same fetch mechanism,
same locale, same theoretical exposure — just unverified and out of #1258's `received=`-scoped remit.
