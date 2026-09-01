---
paths:
  - "scripts/lib/mv-fps-health.sh"
  - "scripts/lib/mv-fps-preflight.sh"
  - "scripts/mv-fps-alert-watchdog.sh"
  - "scripts/lib/asio-starve-health.sh"
  - "scripts/asio-starve-alert-watchdog.sh"
  - "scripts/lib/mv-reverify-escalate.sh"
  - "scripts/frozen-input-alert-watchdog.sh"
  - "scripts/cadence-alert-watchdog.sh"
  - "scripts/lib/frozen-input-health.sh"
  - "scripts/ndi-halving-watchdog.sh"
  - "scripts/ndi_halving_decision.py"
---

# Byte-safe extraction from a PS-fetched OBS-log tail (issue 1258 / issue 1262)

Every dev1-side watchdog that reads strih/stream's OBS log over ssh via a Windows PowerShell 5.1
`gc` (no `-Encoding`) inherits the SAME hazard: `gc` reads the UTF-8 log as ANSI and re-encodes on
output, so any non-ASCII glyph anywhere in the fetched tail (a `genlock-fifo audit` line's `≈`) can
come back as one or more invalid-UTF-8 bytes. In a UTF-8 locale, GNU grep can then flag stdin as
"binary" (empty stdout) or a locale-sensitive `sed`'s `.*` can refuse to consume the invalid byte
and leave garbage in a captured value.

## The trigger condition is narrower than "anywhere in the stream" — verify before assuming

GNU grep 3.11's binary-content detection does **NOT** trigger merely because SOME invalid byte
exists anywhere in stdin. Verified empirically (issue 1262, multiple fixture shapes up to a
realistic 2000-line/241 KB tail): a plain `grep -F` matches CLEANLY when the invalid byte sits on a
SEPARATE, `\n`-terminated line from the one being matched — even with dozens of corrupted lines
present. The detection only fires when the invalid byte is **co-resident on the same "line" grep
must decode to confirm/print the match** — either because the target line's own format genuinely
carries the corrupted glyph (the issue-1258 `received=…` / `(≈N frames @ …)` family, where both
sit on ONE line), or because a missing `\n` at a transport-chunk boundary glues a corrupted line
directly onto a clean one (constructed adversarially for issue 1262's tests — not observed live;
the mv-fps/asio-starve `multiview-audit:`/`asrc:` line families are never co-emitted with `≈`
themselves). **Before applying the `LC_ALL=C grep -a` / `LC_ALL=C sed` fix defensively to a NEW
tap, reproduce the failure with a real fixture first** (a fixture FILE with genuine invalid bytes,
piped via `cat` into the sourced function — Rust string literals cannot hold invalid UTF-8) so the
comment you write states what you actually proved, not what you assumed.

## `LC_ALL=C grep -a` fixes the SHELL side only — check the DOWNSTREAM consumer too

Un-blinding the shell extraction is not sufficient if the extracted text still carries the raw
invalid byte and flows into something that itself REQUIRES valid UTF-8. Confirmed live (issue
1262 review): `mv-fps-gate` reads stdin via Rust's `std::io::Read::read_to_string`, which REJECTS
any invalid byte outright — so `mv_fps_extract_audit_lines` returning an un-blinded but still-dirty
line just moved the failure downstream (gate exit 2, "stream did not contain valid UTF-8", read as
UNKNOWN with a MISLEADING "gate binary broken?" log line, not fixed). Where the target line's
content is pure ASCII by construction (no operator-controlled string field — `multiview-audit:`
qualifies, `asrc: source '<name>'` does NOT since the name is a free string), append
`LC_ALL=C tr -d '\200-\377'` to strip any byte ≥ 0x80 — lossless for legitimate content, and a
parser that finds its marker via `line.find(MARKER)` (not a line-start match) tolerates a garbled
prefix. Where the final captured value is itself a pure digit group (asio-starve's
`starved_blocks=\([0-9][0-9]*\)`), no strip is needed — the capture group can never contain an
invalid byte regardless of what surrounds it in the source line.

## A byte-safety RED test's discriminating power depends on the AMBIENT locale — pin it explicitly

Confirmed live (issue 1262 review): under a plain POSIX `C` locale (or no `LANG`/`LC_ALL` set at
all), grep does NO multibyte validation whatsoever, so a fixture that reliably reproduces "binary
file matches" under a UTF-8-aware locale (`en_US.UTF-8`, `C.UTF-8`) returns a CLEAN, non-garbled
value under plain `C` — the exact bug the test exists to catch silently stops reproducing. Never
rely on whatever locale a CI runner happens to export; put `export LC_ALL=C.UTF-8` as the first
line of every bash harness script in a byte-safety test (the fixed function's own per-command
`LC_ALL=C` overrides still win for GREEN, so this only pins the AMBIENT process locale used to
exercise the pre-fix/regression path).

## Consumer-blind assertions look like proof but aren't

A test asserting `!stderr.contains("binary")` on a function that ALREADY redirects `2>/dev/null`
internally can never fail regardless of whether the fix works — it is stderr-blind by construction.
Comparing `String::from_utf8_lossy(...)` output instead of the RAW bytes has the same blind spot:
`from_utf8_lossy` silently replaces invalid bytes with U+FFFD, so a `String`-level assertion can
pass even when the underlying bytes are exactly what a strict downstream reader (Rust's
`read_to_string`) would reject. Capture and assert on raw `Vec<u8>` (`std::str::from_utf8(&bytes).
is_ok()`) when the property under test is "the output is valid UTF-8", not just "the output
contains the expected substring".
