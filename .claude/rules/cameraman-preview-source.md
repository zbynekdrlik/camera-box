---
paths:
  - "src/preview_source.rs"
  - "src/ndi_display.rs"
  - "src/ndi.rs"
---

# Cameraman HDMI preview source — resolved from the LAN, never one baked box name (#1362)

## The rule

Every cambox shows the strih box's `interkom` NDI output on its HDMI monitor. This is the
fleet-wide unconditional preview from the #528 ruling: no per-box config, no env knob. The source
is RESOLVED on every (re)connect by the pure `src/preview_source.rs`:

1. The **preferred** name (`DEFAULT_PREVIEW_SOURCE` = `STRIH-LX (interkom)`, or an explicit
   `--display` / `[display]` source) wins **the moment it is announced**, matched by exact name.
2. Otherwise the **ONE** discovered `STRIH-<box> (interkom)` output is used. The match is exact
   and case-sensitive: the `STRIH-` host prefix plus the ` (interkom)` output suffix, with a
   non-empty host that has no parentheses. It is used **only on the final pass of the find window**
   (`window_elapsed`), so a slow mDNS announce of the preferred box is never pre-empted by a
   sibling strih.
3. **Two or more** such outputs and no preferred one: pick NOTHING. Keep waiting for the preferred
   name and log the candidates once. Never alternate between two strih boxes.
4. Nothing matching: retry on the existing 5 s cadence, exactly as before.

WHY: the preview used to be the exact name `STRIH-SNV (interkom)`. When the Windows strih was
replaced by strih-lx (M4 cut-over, 20.9.2026), every cameraman monitor went black until a new
binary shipped. The Poprad strih (`STRIH-PP`) will be the next swap, and it now needs no code
change.

## Wiring (where the pieces live)

- `NdiReceiver::connect_with(timeout, label, pick)` (`src/ndi.rs`) runs the ONE finder loop. On
  every ~1 s pass it hands the discovered names to `pick(names, window_elapsed)` and connects to
  the EXACT name returned. A final pass with `window_elapsed == true` is guaranteed before it
  gives up. `NdiReceiver::connect(name)` is a thin wrapper that keeps the old SUBSTRING semantics
  for the probe callers (`probe::reader`, `probe::multi_reader`, `ndi-recv-probe`). Never build a
  second finder just to "list sources": the source struct's name pointers are finder-owned and
  must be used before the next `find_get_current_sources` / `find_destroy`.
- `run_display_loop` (`src/ndi_display.rs`) logs a resolution only when it is FINAL (a pick, or
  the end of the window) and differs from the last one logged (`last_resolution` persists across
  reconnects). The decision is the pure `preview_log_level`, so it is Tier-0-tested. Keep any
  new log decision in the pure module, never inline in the CI-only closure.
- The preferred name is matched EXACTLY, a change from the old receiver's substring match. A
  partial `--display "STRIH-SNV"` value no longer matches directly: it reaches the strih fallback
  only after the 30 s window, and a partial non-strih value never matches. Journal lines: `NDI display: preview source resolved to '<name>' (#1362)` (INFO),
  the ambiguity / not-found line (WARN), then `NDI display: connected to '<name>' -> framebuffer`.
- `DEFAULT_DISPLAY_SOURCE` in `src/main.rs` is sourced from the pure constant. Change the
  preferred strih ONLY there, in `preview_source::DEFAULT_PREVIEW_SOURCE`.

## Verify (Tier-0; main.rs/ndi.rs/ndi_display.rs are CI-only compiles)

- `rustc --edition 2021 --test src/preview_source.rs -o <scratch>/ps && <scratch>/ps`, plus
  `clippy-driver --edition 2021 --test -D warnings src/preview_source.rs -o <scratch>/psc`.
- The closure / borrow shape of the display-loop wiring can be checked with a small standalone
  replica: `#[path]`-include the real `preview_source.rs` and pair it with a mock `connect_with`
  of the same signature, then run it through `clippy-driver -D warnings`. That catches FnMut
  capture / borrowck mistakes that CI would otherwise find first.
- Live acceptance (supervisor, after the fleet deploy): read the journal on a cambox with a
  monitor (`journalctl -u camera-box | grep 'NDI display'`) for `preview source resolved to
  'STRIH-LX (interkom)'` + `connected to 'STRIH-LX (interkom)'`. The deploy's own service
  restart is the test. Never remote-reboot a cambox for it, and after a strih rename the next
  reconnect re-resolves on its own.
