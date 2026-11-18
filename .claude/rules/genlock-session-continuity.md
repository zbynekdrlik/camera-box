---
paths:
  - "scripts/genlock_session_continuity.py"
  - "scripts/lib/genlock-session-continuity.sh"
  - "scripts/e2e_report_session_continuity.py"
  - "scripts/lib/strih-log-read.sh"
  - "tests/python/test_genlock_session_continuity*.py"
  - "tests/fixtures/genlock_session_continuity_1534/**"
---

# The whole-session video gate: zero loss since the OBS start, not only in the window (issue 1534)

Owner ROZHODNUTÉ 18.11.2026 (repeated since the project start): the full-path E2E is a STRICT zero
frame / zero sample loss gate for the WHOLE OBS session. A 4-5 min recording window is a sample; a
frame repeated or a slice lost between two runs made every run green while the rig was not. The audio
half is `dante_input_continuity.py`'s `session` check (a gated counter above 0 on the opening status
line FAILs). This is the video half, BLOCKING.

## The decision (`scripts/genlock_session_continuity.py`, pure)

Input = one box's whole-session read (below). Per input:

- **`genlock-fifo audit '<input>':`** (cumulative since the source started): `underruns` (the render
  tick found no frame, the previous one repeated) and `relocks` at the log's last audit line minus their
  value at the input's SETTLE point = the last audit line at or before its FIRST audit line + 60 s. Any
  growth FAILs naming the input, the counter, the count and the first line it grew on (`first_growth`).
  The startup underruns are normal (strih-snv 18.11: 0-4 in the first 30 s).
- **`[distroav] jxs-source '<input>': fps=`** windows are PER 5 s WINDOW counts (frames=150/151 every
  line; read live 18.11.2026), so the session total is a SUM. The window gate's set (`lost_slices`,
  `concealed`, `undecodable`, `dropped_unread`, `foreign`, `reopens`; a concealed slice is a LOST slice)
  summed over every window after the input's first window WITH FRAMES + 60 s must be 0: the first window
  after the decoder opens always loses 40-54 slices, and a camera can connect late (`frames=0` windows).
- Context, never graded: `dropped_due` (advances by design on a 60 -> 30 input, issue 1221), `holds`,
  `late_holds`, `n2_early`, `stamp_gap`.
- **Every** genlock input in the log is graded (the owner: every genlock input), not only the declared
  ones; a DECLARED input (ALL_CAMBOX = `camera_strih_input` of the active cameras minus the preflight
  exclusions, else `STRIH_PROG_SOURCE`; stream = `STREAM_PROG_SOURCE`) with no audit line, or a declared
  `JXS ...` input with no window line, is UNKNOWN.
- UNKNOWN (fails the run): a `received` that went backwards after the settle (the source restarted, its
  counters with it), a settle point the read does not reach past, another newest log than the window's
  (OBS restarted after the window), a log that has not reached the stop mark (`retryable`), an empty
  read, a Windows-class box.
- Midnight: the times unwrap in file order, a line > 12 h behind the previous starts the next day; a
  line a moment before midnight written right after one past it stays on its own day.

## The box read (`strih_log_remote_cmd linux session`, the ONE strih log reader)

A `#newest<TAB>name<TAB>bytes` header, then `nice -n 19 ionice -c 3 grep -a -b` of the two line kinds over
the WHOLE newest log (9-15 ms on the live 11-17 MB logs), reduced ON THE BOX by awk (mawk-safe, `\047`
for the quote): per input and kind the first 40 lines (a JPEG XS input also its first 40 from its first
window with frames), every audit line whose underruns/relocks changed or whose received went backwards
(200 each), every lossy JPEG XS window (2000), the last line, and `#count<TAB>kind<TAB>lines<TAB>lossy
windows<TAB>input`. A multi-day session therefore never ships its lines (owner 1.11.2026: never ship the
whole log). Lossy windows past the awk cap are later than every listed one, so they are counted
(`unlisted_lossy_windows`) and FAIL. `ident` = the newest log's `name<TAB>bytes` (a `stat`, never a read).
Linux only: a Windows platform gets no command (empty read = UNKNOWN). The strih goes through the reader's
own transport (`strih_log_session` / `strih_log_ident`), the stream through `stream_box_exec` with the same
text.

## The E2E side (`scripts/lib/genlock-session-continuity.sh`)

- `genlock_session_continuity_mark start|stop DIR STRIH STREAM` right after each JPEG XS link mark
  (fail-open; a missing mark is UNKNOWN at the gate).
- `genlock_session_continuity_gate HERE STRIH STREAM DIR VERDICT` in BOTH verdict tails right after the
  JPEG XS link gate, folded `if ! ...; then [ "$GATE" -ne 0 ] || GATE=1; fi` (never lowers a red gate).
  It grades through the log end at the read (after the stop mark): never a miss; a loss between the stop
  and the gate fails the run too. Block `genlock_session_continuity` in the verdict JSON.
- Seam: `GENLOCK_SESSION_CONTINUITY_READ_CMD` (`CMD HOST REMOTE_TEXT`); knobs `_READ_TIMEOUT_S` (60),
  `_WAIT_S` (30) / `_STEP_S` (10) for a log short of the stop mark.
- The 703 merge->exit distance after this gate: 13020 of the 14000-byte window.

## The report (`scripts/e2e_report_session_continuity.py`)

`blocks` joins `_run_passed`, the FAIL summary (`Strata obrazu od štartu OBS (celá session, nie len okno):
ZLYHALA — ...`, at most 6 failures named) and `_section_overall`; the full report gets `🎞 Obraz od
štartu OBS` with one line per input (`underruns settle→closing`, the JPEG XS lossy windows, `dropped_due`
as context). A PASS block keeps the 3-line PASS.

## Live reading when it landed (18.11.2026 ~20:20, read-only)

- strih-snv FAIL: `JXS cam3 src` underruns 4 -> 5 (20:05:23); `CG-obs` (the cg feed from resolume)
  0 -> 499, from 18:39 and a stall at 19:53 with `received` flat. cam2 / cam5 PASS.
- strih-pp FAIL (the PP event): every JPEG XS camera 15-16 lossy windows (lost_slices 47-134, concealed
  19-20 each), underruns cam1 +3067, cam4 +3290, cam6 +3016, cam7 +35; `CG-obs` +55 underruns, +1 relock.
- stream-snv FAIL: `JXS PGM src` underruns +4004 from 18:30:31 (the strih OBS restart left the link
  without frames) and 36 lossy windows (322 lost slices, 37 concealed); `Zaloha kamera` +4442.
The gate is red on all three until the causes are fixed or OBS restarts clean.

## Tests

`tests/python/test_genlock_session_continuity_1534.py` (the decision on synthetic sessions, the awk RUN in a
fake home and compared with the raw grep, the awk cap, the two real reductions of 18.11.2026 in
`tests/fixtures/genlock_session_continuity_1534/`), `tests/python/test_genlock_session_continuity_e2e_1534.py`
(the lib under `set -euo pipefail` against fake homes, the recording-e2e.sh wiring, the report).
