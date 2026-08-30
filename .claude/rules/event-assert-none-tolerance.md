---
paths:
  - "scripts/event_assert.py"
  - "scripts/qr_screenshot_check.py"
  - "tests/python/test_event_assert*.py"
  - "tests/python/test_qr_screenshot_check.py"
---

## `event_assert.py`'s `compute_item_results()` facets MUST be None-tolerant, fail-closed, by construction (#1225)

`scripts/event_assert.py`'s 8-item EVENT-mode CONTRACT (#722) is a chain of pure `*_ok(...)`
decision functions, each consuming one or two "facets" gathered elsewhere (fleet ssh sweeps,
OBS-WS reads via `qr_screenshot_check.py`/`obs_phase2.py`/`set-ndi-mapping.py`) and assembled
into a `facts` dict by `scripts/rig-mode.sh`'s `event_mode_assert()`. A gather step can
LEGITIMATELY fail for exactly ONE item (an unreachable box, an RPC timeout, a screenshot/decode
failure) — that failure must become an honest FAIL, never a Python crash.

**Live incident (2026-08-30, #1225):** `qr_screenshot_check.py`'s `screenshot_qr_findings()`
deliberately writes a per-scene `None`/error value on an RPC failure — its own docstring already
documented this as intentional, "distinct from a genuinely empty (clean) result" — but
`pixel_proof_ok()` was never actually written to tolerate it: `len(v) == 0` crashed with
`TypeError: object of type 'NoneType' has no len()`, printing a FALSE "CONTRACT FAILED" on a rig
the supervisor had manually verified clean. The bug sat there since #722 first shipped this
module, because nothing forced the None-path to be exercised until a real RPC hiccup did.

### The rule for every `*_ok` function (existing or new)

1. **Never call `len(v)` / `int(v)` / `v.values()` on a value that could be `None`** — at BOTH
   levels: the WHOLE facet argument itself, and any PER-ITEM value inside a dict it iterates.
   `dict.get(key, default)` only substitutes `default` for a MISSING key — an explicit
   `{"key": null}` in the facts JSON (`json.load()` turns JSON `null` into Python `None`) passes
   straight through as a bare `None`, bypassing the `.get()` default entirely. Guard for it
   explicitly (`if x is None: return False`, or `isinstance(v, list) and ...` inside a
   comprehension) — don't rely on the call site's default alone.
2. **A None/unreadable value is an UNKNOWN facet — it FAILS CLOSED, it is never treated as
   "nothing was checked, so nothing was found" and never crashes.** This matches the pre-existing
   `pixel_proof_ok`/`paint_processes_ok`/`burns_off_ok` "fails closed on an EMPTY dict" contract
   — None is just the more specific "this ONE item is unreadable" case of the same idea.
3. **Preserve any EXISTING "empty dict/list is vacuously OK" semantics unchanged** where a
   function already documents it (`no_recordings_ok`, `services_healthy_ok`'s `stray_units` —
   "a box legitimately not covered" / "no boxes reported anything" is a DIFFERENT, valid state
   from "the facet gather itself failed"). Add the None-guard as an ADDITIONAL, more specific
   check — never fold None into the existing empty-collection branch, since that would silently
   convert a real gather failure into a false PASS.
4. **When a per-item value can go unreadable (not just per-facet), consider naming it in the
   printed/Discord summary** — see `pixel_proof_detail()` for the pattern: sort the unreadable
   keys, add a `"facet unreadable: <keys>"` string to `main()`'s `details` dict via
   `details.setdefault(item_name, detail)` so an operator can tell "we couldn't check this" apart
   from "a QR is actually live" without reading a traceback.
5. **A producer that can legitimately fail per-item should emit an EXPLICIT, non-`None` error
   record** (`{"error": "<reason>"}`, not a bare `None`) — see `qr_screenshot_check.py`'s
   `screenshot_qr_findings()`. This is defense-in-depth on TOP OF the consumer guard above, not a
   substitute for it: a future caller that forgets rule #1 still gets an unambiguous, non-`None`
   value instead of a crash-prone bare `None`.

### Test pattern

`tests/python/test_event_assert_1225.py` is the canonical shape for testing this: build a
`_clean_facts()` fixture that passes every item, then inject exactly ONE `None` (per-item AND,
separately, whole-facet) and assert the affected `*_ok` function returns `False` — never raises —
both directly and through `compute_item_results()` (the real call path `event_assert.py`'s CLI
uses). Add a case here whenever a NEW facet/item is added to `compute_item_results`.
