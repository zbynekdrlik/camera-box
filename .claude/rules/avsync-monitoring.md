---
paths:
  - "scripts/avsync-watchdog.ps1"
  - "scripts/avsync-vlc-monitor.ps1"
  - "scripts/avsync-keepalive.ps1"
  - "scripts/avsync-watchdog-install.sh"
  - "scripts/avsync-heartbeat-alert-watchdog.sh"
  - "scripts/lib/avsync-heartbeat.sh"
  - "systemd/avsync-heartbeat-alert-watchdog*"
  - "scripts/avsync-lineup-alert-watchdog.sh"
  - "scripts/avsync_lineup.py"
  - "systemd/avsync-lineup-alert-watchdog*"
  - "scripts/av_sync_apply_guard.py"
  - "scripts/lib/av-sync-apply-guard.sh"
---

# Stream-box avsync watchdog + VLC monitor + dev1 heartbeat alert (#812/#807)

Windows-side long-running loops on the stream box (10.77.9.204) that measure A/V sync and let an
operator listen to the program audio, plus the dev1-side alert that watches both for silence.
Distinct from `.claude/rules/imag-obs-supervision.md` (a DIFFERENT box, OBS process supervision,
not a measurement/monitor pair) — the topology below generalizes that file's dev1-side-alerting
principle to an arbitrary heartbeat FILE rather than a process/WebSocket probe.

## Task Scheduler has no `Restart=on-failure` — the idiom is a periodic idempotent keep-alive check

Unlike systemd, Windows Task Scheduler cannot automatically restart a crashed/hung process tied to
one of its own triggers. `scripts/obs-self-heal-install.sh` (#411) established the repo's answer
for OBS itself: a Repetition-triggered task running a check-and-relaunch script. `scripts/
avsync-keepalive.ps1` (#812/#807) reuses the SAME idiom but far simpler — since restarting IS the
only recovery action needed here (no GPU-cause branching, no AHK-race ordering, no destructive
reboot path), a bespoke Rust `decide()` state machine like OBS self-heal's would be
disproportionate. Match on the process's **CommandLine** via `Get-CimInstance Win32_Process
-Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like "*<script-name>*" }`, never
just the process name — several unrelated `powershell.exe` instances can be running at once
(including the keep-alive check's own invocation), and only the CommandLine substring
disambiguates which script a given process is actually running.

## Generalizing the dev1-side alert topology from a PROCESS/WS probe to an arbitrary heartbeat FILE

`scripts/imag-obs-alert-watchdog.sh` (#882) and `scripts/obs-liveness-watchdog.sh` (#391) already
established: a remote appliance/Windows box has no `~/devel/airuleset` checkout and no Discord
credentials, so the alert MUST fire from dev1. Both of those probe a live process/WebSocket state.
`scripts/avsync-heartbeat-alert-watchdog.sh` generalizes the SAME topology to polling an on-box
**heartbeat FILE** instead — any future "does this Windows/Linux process still write its own
liveness file" need can reuse this shape directly: a one-line `<epoch_seconds>\t<status>` file
written by the monitored process on every pass, read remotely, staleness-checked purely
(`scripts/lib/avsync-heartbeat.sh`'s `avsync_heartbeat_is_stale` — missing/corrupt data is ALWAYS
treated as stale, never defaults to "fresh"), and alerted via the EXISTING `scripts/lib/
obs-watchdog-decision.sh` confirm/throttle functions (never invent a second confirm/throttle
mechanism just because the probe shape changed).

**Reading multiple remote files in ONE ssh round-trip on a Windows box (cmd.exe default shell,
confirmed live via `ssh ... "echo %COMSPEC%"` → `cmd.exe`, NOT bash/powershell):**

```
type "<path1>" 2>nul & echo <UNIQUE_SEPARATOR> & type "<path2>" 2>nul
```

`2>nul` swallows a missing file's error so a not-yet-written heartbeat produces an EMPTY segment
(never a false "ssh failed" reading); `&` (not `&&`) unconditionally runs the next segment
regardless of the previous command's exit code, so one missing file never hides the other's
content. Split the combined output on the separator with `sed -n '1,/^SEP$/p' | sed '$d'` (before)
/ `sed -n '/^SEP$/,$p' | sed '1d'` (after). Reusable for any future "read N status files from one
Windows box in one ssh call" need instead of N separate ssh round-trips.

## Bounding an external call so it can never wedge the loop — pick the mechanism by what needs killing

Two different bounding mechanisms were used deliberately, not interchangeably:

- **`Start-Process -PassThru` + `$proc.WaitForExit(ms)` + `Stop-Process -Id $proc.Id -Force`** —
  used in `avsync-watchdog.ps1`'s `Invoke-Measurement` for the python `av_sync_measure.py` call.
  Gives a DIRECT process handle to the real child process, so a force-kill on timeout actually
  terminates the right PID. The root incident's own evidence ("two orphan python PIDs...consistent
  with a hung av_sync_measure.py") is exactly what a job-based wrapper risks reproducing: `Start-
  Job`'s background job runs in a SEPARATE child `powershell.exe` process, and `Stop-Job` stops
  that wrapper without a guarantee the wrapper's own child (the real python.exe) dies with it.
- **`Start-Job` + `Wait-Job -Timeout` + `Stop-Job`/`Remove-Job`** — used in `avsync-vlc-monitor.ps1`'s
  `Test-RtmpPublishing` for a short `ffprobe` check. Fine here because ffprobe is a single
  short-lived leaf process with no meaningful grandchild-orphan risk, and the job form is simpler
  to write for a "return a value or false on timeout" one-shot check.

Pick `Start-Process`+`WaitForExit`+`Stop-Process -Id` whenever the bounded call is (a) a python/
long-running interpreter that could itself spawn or block indefinitely, or (b) anything where an
orphaned grandchild process would be a real operational problem (stale processes accumulating
across many restarts, as the live incident showed). Reach for the simpler `Start-Job`/`Wait-Job`
shape only for short, leaf, no-grandchild external calls.

## A commit message merely MENTIONING another issue number can block the CURRENT commit

Extends the top-level CLAUDE.md's existing GOTCHA on this (originally documented for #855/#836):
hit AGAIN twice in the #812/#807 session, writing `(see #814's grab-freshness gate)` and `(reusing
the #391 pure confirm/throttle lib)` in commit-message PROSE — both blocked by
`block-commit-without-design.sh` demanding a design comment on #814/#391 (unrelated, already-closed
tickets, cited only for context). Same fix as documented at the top level: write "issue 814"/"issue
391" (no `#`) instead, or reference the RULE/PATTERN by name instead of the ticket number. Two
separate hits in one session confirms this is worth checking proactively — before writing ANY
commit message that cites a past ticket for context, scan it for a bare `#<digits>` first.

## Live-verify defects found post-merge (#968) — never trust a fixture that sanitizes the real wire shape

Three genuine defects shipped in the #812/#807 PR (#967) and were only caught by the supervisor's
LIVE post-merge deploy verification, not by the (green) test suite — each is its own reusable
lesson for this file's scripts:

1. **`echo TEXT & next` on cmd.exe includes the trailing SPACE before `&` as part of TEXT, and
   every remote line arrives CRLF-terminated over ssh — never exact-line-match a cmd.exe-echoed
   marker without stripping `\r` and tolerating trailing whitespace first.**
   `avsync_heartbeat_extract_segment()`'s sed anchors were an exact `^SEP$` match; the real
   separator line printed as `---AVSYNC-HB-SEP--- \r\n` (note the space before the CR) never
   matched it. The pre-968 behavioral test fixture emitted clean LF-only output with no trailing
   space and never caught this — **a test fixture for a cmd.exe-driven probe must byte-match the
   REAL captured shape (CRLF + any incidental trailing space from the shell's own `&`-splitting),
   not a hand-typed "clean" approximation.** Fix: normalize once, at the top of the parsing
   function — strip every `\r` byte, then tolerate `[[:space:]]*` before the end-of-line anchor.
2. **`Where-Object { $_.CommandLine -like "*<bare-script-name>*" }` matches ANY process whose
   command-line TEXT merely QUOTES that filename — not just a process actually RUNNING it.** Live
   incident (2026-08-03): a diagnostic MCP powershell whose command text contained the literal
   `watchdog.ps1` inside an unrelated `Get-CimInstance` filter string was counted as "the watchdog
   already running", masking a genuinely dead process. Fix: match must require the FULL invocation
   — the `-File` flag immediately adjacent to the exact (regex-escaped) script PATH, e.g.
   `-match "-File\s+\"?$([regex]::Escape($scriptPath))\"?"` — never a bare filename substring, and
   never just the full path alone without the `-File` anchor either (a path could still appear in
   unrelated log/print text).
3. **An unquoted heredoc (`cat <<PLAN`, needed for `${var}` interpolation) executes EVERY unescaped
   backtick pair in its body, including ones meant purely as prose emphasis** — same class as the
   top-level CLAUDE.md's `git commit -m` backtick GOTCHA, just inside a heredoc instead of a commit
   message. A missed `` `task-name` `` (vs. the correctly-escaped `` \`task-name\` `` used
   elsewhere in the SAME heredoc) silently deletes that text from the printed output and prints
   `line N: task-name: command not found` to stderr. **Before trusting "it must be fine, most of
   the backticks in this heredoc are already escaped" — grep every backtick in the heredoc body
   individually**; a partially-fixed heredoc (some escaped, some not) is exactly what shipped here.
   A regression test for this class should assert BOTH that a real run produces EMPTY stderr AND
   that specific backtick-quoted phrases survive verbatim in the output — a bare "does the plan
   still mention the task name" check can pass trivially even when ONE occurrence was deleted, as
   long as ANOTHER (already-escaped) mention of the same name exists elsewhere in the same output.

## Discord delivery: the bot cannot mint webhooks — the DURABLE path is a dev1-side bot-API forward, not the webhook file

`avsync-watchdog.ps1`'s original design reads a Discord webhook URL from an uncommitted local file
on the stream box (`C:\avsync\discord-webhook.txt`) and lets `av_sync_measure.py` POST directly to
it. This has **no self-service path to ever get populated**: the `claude_robot` bot has no
`MANAGE_WEBHOOKS` permission guild-wide (confirmed live via `POST /channels/.../webhooks` →
`Missing Permissions`), so nobody running as that bot identity can ever mint the webhook URL the
file is supposed to contain. The webhook-file mechanism stays wired as a harmless optional
fallback (a human can still paste a URL there by hand), but it is NOT the delivery path this repo
relies on.

**Incident that motivated the fix (issue 968, discovered 2026-08-03):** on 2026-07-26 the user WAS
actually receiving real A/V-sync verdict messages (`📐 A/V-sync meranie: [...] :: ... -> ZNIZ '2ME
PGM' latency o 80`) in the `alerts-snv` Discord thread (thread id `1373592666733940816`, parent
channel `🔥poznamky-live`, guild NewLevelMedia) — but they were posted by a **live agent-session
loop**, not a durable service. The moment that session ended, delivery silently died with no
trace. This is the SAME class of failure `.claude/rules/imag-obs-supervision.md`'s dev1-side-
alerting topology already exists to prevent (a remote box/process cannot durably self-report), and
the fix is the SAME shape: `scripts/avsync-heartbeat-alert-watchdog.sh`'s `maybe_forward_verdict`
now forwards a genuinely NEW measured misalignment verdict (a `"measured: "` heartbeat status
carrying a ZNIZ/ZVYS recommendation — silence when in sync, message when misaligned, mirroring
`av_sync_measure.py`'s own threshold semantics) directly from the dev1 systemd timer via a bot-API
POST, using the SAME `Authorization: Bot <token>` + `User-Agent: DiscordBot (...)` header shape
`~/devel/airuleset/notify/*.py`'s own `_post_discord`/`_discord_api` use (never invent a second
convention) — read at runtime from the local uncommitted `~/.claude/channels/discord/.env`
(`AVSYNC_DISCORD_ENV` env-overridable for tests), never committed. Only the channel/thread IDs
(not secrets) are committed to the repo. State (which epoch was last forwarded) persists via the
SAME state-file mechanism the confirm/throttle legs already use, so a repeated pass with the SAME
epoch never double-posts, and `--dry-run` still advances that state (mirrors `process_leg`'s own
convention — only the actual POST is skipped, never the bookkeeping).

# Measurement-line GO/NO-GO + stream-state-bound liveness alarm (#813)

`scripts/avsync_lineup.py` (PURE decider, mirror of `avsync_freshness.py`/`event_assert.py`) +
`scripts/avsync-lineup-alert-watchdog.sh` (dev1 timer, two modes: default run-time liveness pass +
`--assert` one-shot pre-event GO/NO-GO). Distinct from the `#812` heartbeat watchdog above: that one
alarms on heartbeat STALENESS only + unconditionally; this one binds to STREAM state and the audio
CONTENT, catching a live-stream-but-dead-line that a fresh heartbeat hides.

## The content-liveness signal is the audio dB — NOT the SyncNet verdict text (the whole #813 bug)

The first cut classified "is the measurement valid" from the heartbeat STATUS text and keyed on
`unknown`/`candidates: 0`. **That string is the E2E `recording-verdict --av-sync` path's vocabulary,
NOT what writes the on-box heartbeat.** `avsync-watchdog.ps1` writes `measured: <last line of
av_sync_measure.py>`, and `av_sync_measure.py` (verified: ZERO hits for `unknown`/`candidates`)
prints `[stamp] UNMEASURABLE window (...)` for silent/undecodable content and `[stamp] AV offset ...
:: A/V sync OK / ZNIZ / ZVYS` for a real reading. So the decider was classifying a case reality
never produces, and the tests were green against a fabricated string — a review caught it, not the
suite. **Two durable lessons:**

1. **When a dev1 decider classifies an on-box producer's output, verify the ACTUAL producer's
   vocabulary** (`grep` the real `print(...)`/`Write-Heartbeat` source), never a similar-looking
   string from a DIFFERENT path. The E2E recording-verdict and the SyncNet watchdog are two separate
   measurement paths with two separate output vocabularies.
2. **`UNMEASURABLE` cannot distinguish SILENT AUDIO from a normal no-face band/graphics segment** —
   av_sync_measure.py prints it for BOTH ("band/graphics segments are expected to skip"). The only
   signal that distinguishes the 2026-08-17 silent-chain incident from an ordinary band segment is
   the audio LEVEL in dB (digital silence ~-91 dB vs a live QPSK marker ~-5 dB — the
   `audio-presence-preflight.sh` -60 dB floor). `avsync-watchdog.ps1` now prefixes the heartbeat with
   `db=<max_volume>` (ffmpeg volumedetect on the SAME clip already grabbed each pass — no second grab,
   no fourth measurement path), and `avsync_lineup.py` classifies content-liveness on `db >= -60`.

## A fresh `measured:` heartbeat IMPLIES the stream is publishing — don't make the alarm depend on the OBS-WS read for the content-death case

The ps1's grab only SUCCEEDS when the RTMP relay is serving, i.e. the stream is live (the `#814`
freshness gate guarantees the clip is fresh). So a fresh `measured:` heartbeat is itself proof the
stream is publishing — the liveness alarm pages on silent audio (`db < -60`) WITHOUT needing an
`obs_phase2.py stream-status` read at all. The OBS-WS `outputActive` read only gates the AMBIGUOUS
cases (a `no-signal:` heartbeat = grab failed, or a STALE heartbeat = process dead), where the stream
might be legitimately off. This matters because the WS read is easily mis-configured to fail (an
empty password defaults to `None` -> SUPPRESSED forever -> an inert alarm); keying the headline case
off the heartbeat instead of the WS read makes the alarm robust to that. `--assert` still REQUIRES
the WS read to return a definite True/False (a `None` = NO-GO), so a mis-set password is caught
before the event rather than silently muting the alarm during it. Default the OBS-WS password to
`OBS_WS_PASSWORD` (rig-mode.sh's convention), never an empty string.

## Two shell gotchas this watchdog hit (both cost a debug/CI cycle)

- **A sourced lib's `set -euo pipefail` LEAKS `-e` into the caller, and `set -uo pipefail` does NOT
  clear it.** `avsync-heartbeat.sh` sets `-euo pipefail`; sourcing it turns `-e` ON in the watchdog.
  A `var="$(python3 decider ...)"` where the decider legitimately exits non-zero (a preflight NO-GO,
  exit 1) then ABORTS the whole pass at the ASSIGNMENT — before the verdict is ever printed and
  before the fail-loud alert fires (observed: `--assert` NO-GO produced EMPTY stdout + no alert).
  Fix: explicit `set +e -uo pipefail` after the sources (`set -uo pipefail` alone only turns options
  ON). This is ci-testing-gotchas.md's leaked-`set -e` note, applied to the `var="$(gate)"` shape.
- **The `avsync-watchdog.ps1` log line `"LIVE :: $out"` is a static test anchor**
  (`harness_avsync_watchdog_812.rs` does `body.find("LIVE :: $out")`). Appending to it is safe only if
  the exact prefix survives — `"LIVE :: $out (db=$db)"` keeps it. But a NEW COMMENT that spells out
  the literal anchor string creates an EARLIER occurrence that `.find()` grabs first (the self-
  collision class the top-level CLAUDE.md documents, hit again here). Reword any comment near that
  line to not contain the anchor string; verify `grep -c 'LIVE :: \$out' scripts/avsync-watchdog.ps1`
  is 1 before trusting the harness.

## Throttle the alarm on a COARSE stamp-free signature, never the timestamped heartbeat text

The decider emits a coarse `sig=` token (`no-audio`/`wedged`/`stale`/`no-signal`/`ok`/etc.); the
watchdog throttles on `lineup:$sig`. Building the signature from the raw heartbeat status (which
carries a `[stamp]` that changes every ~90 s) would change the signature every pass and defeat
`obs_watchdog_alert_throttle` into re-paging every 5 min instead of ~1 h — the same trap the `#812`
sibling avoids with its stable `"${leg}:stale"` token.

## Fail LOUD on a missing tool — a dev1 alarm must never fail OPEN on a tooling gap

`require_tools sshpass ssh python3 jq` (+ `curl` for a non-dry `--assert`) runs first: a missing jq
would otherwise yield empty facts -> the decider's `json.load` errors (swallowed by `2>/dev/null`)
-> `action=""` -> no alarm AND the pending state resets, i.e. a silent mute. Mirror the sibling dev1
watchdogs' fail-loud-by-name discipline.

## Verifying the v1 A/V-sync meter is ALIVE in production — read-only agent-session recipe (#801)

"Is the SyncNet av-sync daemon actually running on the stream box?" is answered read-only from an
AGENT session via the `win-stream-snv` MCP (never ssh for a Windows box in an agent session —
`win-ssh-vs-mcp.md`), safe even while a Full-path E2E rerun is in flight:

- `TaskList` / `Get-ScheduledTaskInfo avsync-keepalive` → `LastResult=0` + a recent `LastRunTime`
  (runs every ~5 min). `avsync-keepalive.log` tail shows `watchdog.ps1 already running (pid N) - no-op`
  AND `avsync-vlc-monitor.ps1 already running (pid N) - no-op` — that no-op pair IS the liveness proof
  (the keep-alive found both loops alive). A relaunch line instead means it just restarted one.
- Heartbeat files `avsync-watchdog-heartbeat.txt` / `avsync-vlc-monitor-heartbeat.txt` fresh (mtime
  within a few min) confirm the loops are writing.
- **There is NO separate `av_sync_measure.py --loop` process, and NO srt://:9998 tap** — the
  production wiring is `watchdog.ps1` (deployed from `scripts/avsync-watchdog.ps1`) grabbing from
  `rtmp://127.0.0.1:1234/live/obs-e2e-test` and calling `av_sync_measure.py --media` one-shot. So
  `ListProcesses python` shows restreamer/MCP/bundle-state python, NOT the meter — do NOT read that
  absence as "the daemon is down". Judge liveness by the keepalive task + heartbeats, not a python PID.
- `watchdog.log` reading `NO-SIGNAL - no verdict (ffmpeg rc=-5 (relay/stream down))` when no event is
  live is NORMAL healthy self-skip (nothing to measure off-air), not a fault — same as the `#814`
  no-signal semantics above. A GAP would be a STALE heartbeat (process dead) or `avsync-keepalive`
  `LastResult != 0`.

## The #856 rig-wide A/V controller HOLDs an apply computed from an unstable audio timeline (#1265)

`recording-e2e.sh`'s #856 controller (`av_sync_combine_offsets.py` → `av_sync_calibrate.py --apply`
in `cleanup()`) auto-tunes `NDI 2ME PGM`'s genlock latency toward the median of THIS run's measured
per-camera A/V offsets. On 2026-09-01 the stream box's reference source `mbc` ts_lag went bimodal
(107↔180 ms flap), which shifted the measured residual to −77/−126 ms with a rig-wide-CONSISTENT
(small-spread) shape — so the combiner's only guards (<2-measured-cams / >100 ms-spread) both passed
and the controller walked the pin 926→976 toward noise. The #1265 STABILITY GUARD holds it instead:

- **Pure decision `scripts/av_sync_apply_guard.py`** (`hold_reason(...)`, pytest Tier-0) HOLDs on ANY
  of three fail-safe signals: (1) the run's stream `mbc` ts_lag band is DRIFTING
  (`.claude/rules/audio-lag-watchdog.md` band arm, gathered at `[8/8g]`) — a supplementary
  "defer tuning while the timeline is unstable" hold, NOT the residual explanation; (2)
  `|residual_median_ms|` beyond a ±60 ms sanity ceiling (green series ±33; the bad runs −77/−111/−126
  — no history needed) — the PRIMARY gate, checked **REGARDLESS of the band** (supervisor finding: a
  flat/HEALTHY band still measured −111.5, a real oscillating upstream-audio-latency step, so the
  residual — not the flap — is what must gate; band-scoping condition 2 was rejected); or (3)
  `|proposed − last_applied| > 90 ms` vs `~/.camera-box/av-sync-last.json` — an anti-oscillation/step
  guard. The residual EARLY-WARNING (before the E2E runs) is the separate upstream-step detector,
  issue 1267.
- **Sourced lib `scripts/lib/av-sync-apply-guard.sh`** (#675) does the I/O gather (verdict residual,
  last-applied offset, the stream band verdict) + the persist, all `set -euo pipefail`-safe (it runs
  in the `cleanup()` EXIT trap, so every function ALWAYS returns 0 — the #1133 class).
- **Composition (`recording-e2e.sh cleanup()`):** the guard block sits AFTER the #358/#691 stream
  teardown restore and BEFORE the byte-unchanged #856 apply `if` (`.claude/rules/recording-e2e-cleanup-composition.md`).
  A HOLD clears `AV_SYNC_APPLY_OFFSET_MS` (skipping the apply) with a loud log + a per-run
  `av-sync-apply-hold-<run>.txt` AND a durable `~/.camera-box/av-sync-apply-hold-last.txt` reason
  (the per-run copy is swept); a landed apply COPIES the calibrate full-schema success file to the
  dev1 reference (preserving `applied_latency_ms` — a live data contract, not a rewritten
  offset-only schema) for the next run's jump baseline. When the guard says proceed, #856 is
  byte-identical.
- **Tier-0:** `pytest tests/python/test_av_sync_apply_guard_1265.py` (the predicate) +
  `tests/harness_av_sync_apply_guard_1265.rs` / `tests/harness_recording_e2e_av_sync_guard_1265.rs`
  (the sourced lib + the wiring; CI-run, cross-checked locally by running the python one-liners
  standalone + a `.find`/window simulation, since a worktree worker cannot `bash -c`-source the lib
  under the isolation guard and cannot run cargo).
