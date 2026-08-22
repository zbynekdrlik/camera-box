---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/rig-mode.sh"
  - "scripts/lib/imag-offline-ack.sh"
  - "scripts/lib/imag-leg-marker.sh"
  - "scripts/lib/cambox-offline-ack.sh"
  - "rig-fleet.txt"
  - "tests/harness_imag_offline_ack_1013.rs"
  - "tests/rig_mode.rs"
---

# imag-nb offline-ack leg skip (issue 1013)

imag-nb is ackable in the E2E gate exactly like a cam box: `imag:<reason>` in `CAMBOX_OFFLINE_ACK`
/ `rig-fleet.txt` (the `scripts/lib/cambox-offline-ack.sh` mechanism, #758/#827 — it is
box-name-agnostic, so imag needed NO new ack parser). When imag is a KNOWN-ABSENT box (notebook
taken after an event), `scripts/recording-e2e.sh` sets `IMAG_OFFLINE_ACKED=1` and SKIPS the whole
imag leg with a loud, report-only note (`imag_leg_skip_note`) per step, never a silent pass
("ONE full test, no partials", #798). The gate flag:

- `IMAG_OFFLINE_ACKED` inits `0`; `IMAG_OFFLINE_ACK_REASON="$(cambox_offline_ack_reason "imag")"`
  is non-empty **iff** imag is acked. The flag flips to `1` ONLY in the `[0/8] reachability
  preflight` loop, and ONLY when imag is acked **AND** genuinely unreachable. An acked-BUT-reachable
  imag is a STALE ack → `cambox_offline_ack_stale_message "imag"` + `exit 1` there (so the marker/
  guards downstream are only ever reached in the acked-unreachable or not-acked cases — that is why
  passing `IMAG_OFFLINE_ACK_REASON` straight to `imag_leg_run_marker`'s 3rd arg cannot misfire).

## The imag hard-abort site inventory — a "make imag optional" change must cover ALL of these

The obvious `[0/8] reachability preflight` is NOT the only site that `exit 1`s (or bare-command-
aborts under `set -e`) on an absent imag. If you only fix the preflight, the run just dies at the
NEXT one. The full set guarded by `IMAG_OFFLINE_ACKED` (issue 1013):

| Site | Why it aborts on an absent imag |
|---|---|
| `[0/8] reachability preflight` loop | `exit 1` on unreachable (sets the flag here) |
| `[0/8] imag display-path preflight` | `imag_display_path_preflight_assert … \|\| exit 1` (degrades on unreachable, but guarded to skip cleanly) |
| `[0/8] imag cmdline-isolation preflight` | `imag_cmdline_isolation_preflight_assert … \|\| exit 1` (issue 1105 — the issue-784 lib's E2E consumer; UNKNOWN warns, so it degrades on unreachable, but guarded to skip cleanly) |
| ALL_CAMBOX `[0/8]` imag OBS-prep | reachability probe / projectors / wmctrl / heal all `exit 1` |
| ALL_CAMBOX `[1/8]` imag render-health + MV-divisor | `exit 1` |
| `[0/8] dantesync-version-gate` | **names imag `imag-nb`, not `imag`** — its own ack-exclusion never matches the `imag` ack, and it REFUSES (exit 11) on an UNREAD node → drop `imag-nb` from `DANTESYNC_VERSION_LINUX` when acked |
| `[0/8] version-integrity gate` | issue 1164 — an empty imag genlock SHA → cross-box parity `UNKNOWN` (11), AND the issue-1100 ENFORCED imag `.so` byte facet `UNKNOWN` (11) on absent bytes → the whole E2E refused (run 32480962068: "2 box(es) UNKNOWN: genlock_parity imag:so_bytes"). When acked → skip the imag ssh gathers (SHA read + `.so` probe + manifest fetch) and invoke `version-integrity-gate.sh` with `--imag-acked-offline "$IMAG_OFFLINE_ACK_REASON"` (never `--genlock-sha imag=…`) — the gate then SKIPS the `.so` byte facet (loud line, counted ok) and drops the `imag` parity entry so parity certifies strih+stream. WITHOUT the flag the gate is byte-identical (absent imag still fail-closed UNKNOWN) |
| `[4a/8]` imag program-scene routing | bare `switch --host "$IMAG_IP"` under `set -e` |
| `[4d1/8]` MV-fps floor | report-only, but pass strih-only when acked |
| `[4d/8]` imag render-budget gate | `if ! …; then exit 1` |
| `[4e/8]` imag-nb headroom preflight | multiple `exit 1` (lspci/nvidia-smi/meminfo) |
| `[4b/8]` pre-record burn-ON gate | iterates `BURN_TARGETS` (which KEEPS its imag entry — `harness_imag_topology` anchor); `exit 1` when imag's burn can't be confirmed → skip the imag triple inside the loop |
| `[5/8]` imag StartRecord | bare command under `set -e` |

**Naming trap:** imag is `imag` in the ack / reachability loop / genlock-parity (`--genlock-sha
"imag=…"`), but `imag-nb` in the dantesync-version-gate node list. Any gate that consults the ack
under the node's own name will silently NOT match a `imag:` ack — check each gate's node name.

**Naturally skipped (no guard needed):** `cleanup()` StopRecord (guarded by
`IMAG_RECORDING_STARTED`, unset when acked), `cleanup()` scene restore (guarded by
`IMAG_PREV_SCENE`, empty when acked), the build-SHA / genlock-parity reads (`|| true`, dormant on
empty), `[4g/8b]` latency (`set +e`). Only the cleanup burn-verify loop needed an explicit
`_bn=imag && acked → continue` (it WARNs a phantom "burn still on" otherwise).

## Guard shape + anchor safety

Guards are `if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then imag_leg_skip_note "<step>" "$IMAG_OFFLINE_ACK_REASON"; else <byte-unchanged original step>; fi`. For a contiguous multi-step imag block the wrap
deliberately does **not** re-indent the body (bash-legal; the ~100 static-anchor tests match
substrings, not indentation) — smallest diff, zero anchor risk. Never remove an anchored token
(`imag=$IMAG_IP`, `record --host "$IMAG_IP"`, `BURN_TARGETS=(`, `--merge-partials`) and never add a
`\nfi\n` inside the `[8/8d]` merge-adjacency region (`harness_imag_topology` `!between.contains("\nfi\n")`).
After ANY recording-e2e.sh edit, run every affected static-anchor binary (compile `--no-run`, run
the compiled binaries directly — Tier-0; #477 disables the build-ok bypass).

## rig-mode.sh's #789 TEST-entry gate is a SECOND imag hard-abort site — also offline-ack-aware (issue 1171)

The `IMAG_OFFLINE_ACKED` flag + inventory above is `recording-e2e.sh`-scoped. `scripts/rig-mode.sh`
has its OWN imag hard-abort that the E2E flag never reaches: `require_imag_genlock_current` (the #789
TEST-entry HARD-BLOCK) runs `drift-guard.sh --check-imag` and `exit 30`s on any non-`OK` verdict —
and an acked-offline+unreachable imag yields `UNKNOWN` → BLOCK → `exit 30`, aborting
`rig-mode.sh test` and leaving cam2 with no painter (the documented lipsync/TEST restore path; live
2026-08-22). Since #1171 the gate consults the SAME `cambox-offline-ack.sh` mechanism BEFORE
fail-closing:

- rig-mode.sh now `. "$RIG_MODE_DIR/lib/cambox-offline-ack.sh"` (it did not before) and computes the
  effective ack LOCALLY inside the gate (no top-level source-time side effect, so `run_sourced` tests
  stay clean): `cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$RIG_MODE_DIR/../rig-fleet.txt"`
  (explicit env wins, same precedence as recording-e2e.sh).
- The skip/proceed decision is a PURE `imag_genlock_gate_offline_ack_action REASON REACHABLE` (I/O —
  the `ping -c1 -W2 "$IMAG_IP"` — stays in the caller; the decision is pure + Tier-0 testable, the
  same seam pattern as `imag_genlock_gate_verdict`). acked+UNREACHABLE → `skip` (loud named skip
  citing issue 1013 + reason, `return 0`, no drift-guard); acked+REACHABLE = STALE ack → `proceed`
  (falls through to the byte-unchanged gate — NOT exempted, mirrors the [0/8] stale-ack protection);
  not-acked → `proceed`.
- **Any NEW imag gate added to rig-mode.sh must do the same** (consult `cambox_offline_ack_reason
  "imag"` + a reachability probe before fail-closing) — the "make imag optional" inventory above is
  per-SCRIPT, and rig-mode.sh is now a second script in scope. The EVENT path stays advisory
  (`warn_imag_genlock_stale`, never blocks going live) and needs no ack seam.
- Anchor safety: the new pure function + prologue sit ABOVE `do_test()`/`do_event()`, so the
  `tests/rig_mode.rs` `do_test_body`/`do_event_body` splits are untouched; the new wiring test slices
  the gate body between `require_imag_genlock_current() {` and `\nburn_action_for_mode() {` (both
  count-1 anchors). The `#789` existing-body invariants (do_test calls the gate, do_event keeps only
  the advisory) still hold.

## Marker (report-only, #798 twin)

`imag_leg_run_marker <partial> <host_path> [acked_reason]` — the optional 3rd arg (issue 1013)
makes it emit `IMAG-LEG-NOT-VERIFIED: imag acked offline (<reason>)` instead of the generic "no
recording path". The 2-arg #798 calls are byte-unchanged. WoL (remote wake) for imag is SEPARATE
hardware work (issue 1053 is the strih/stream counterpart), never bundled into this gate change.
