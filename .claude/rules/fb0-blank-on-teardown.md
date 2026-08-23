---
paths:
  - "src/shutdown.rs"
  - "src/fb_blank.rs"
  - "src/probe/fb.rs"
  - "src/probe/kms.rs"
  - "src/probe/painter.rs"
  - "src/probe/run.rs"
  - "src/bin/frame-probe.rs"
---

# /dev/fb0 blank-on-teardown + frame-probe graceful shutdown (#660, #1176/#1186)

How the cam2 painter (`frame-probe`) leaves a CLEAN framebuffer when it stops, so a
dead/stopped painter never masquerades as a live picture on cam2's HDMI monitor.

## Who blanks /dev/fb0, and when

- The pure geometry decision — the visible byte range to zero — is `src/fb_blank.rs`
  (`visible_page_range`), std-only, Tier-0 unit-tested. The actual ioctl-read + zero-write
  is `src/probe/fb.rs::blank_fbdev(device)` (opens the device fresh, re-reads LIVE
  `FBIOGET_VSCREENINFO` geometry, blanks whichever page is currently visible).
- **BOTH presenters blank on `Drop`** (each best-effort, log-and-continue, never panic):
  - `KmsPresenter::Drop` (`src/probe/kms.rs`, #660) blanks fb0 **BEFORE** `release_master_lock()`
    — it drives the CRTC through its own DRM dumb buffers, so the blank has to land while it
    still holds master; releasing master then reveals the just-zeroed fbdev memory.
  - `VsyncFb::Drop` (`src/probe/fb.rs`, #1186) blanks fb0 too — the fbdev fallback writes the
    device directly, so a dropped painter otherwise leaves its last frame scanned out. It stores
    the device path in a `device: String` field for this.
- `camera-box`'s own `--display` module (`src/display.rs`) writes fb0 directly and does NOT
  blank on exit — the remaining un-clearing writer (out of scope for #660/#1186).

## Graceful shutdown on SIGTERM (#1176 prong 1)

`Drop` runs only on a normal return. `systemctl stop cam2-painter.service` sends SIGTERM, whose
DEFAULT disposition terminates the process with NO stack unwind — so without a handler, `Drop`
(and the blank) is skipped and fb0 keeps the last frame. The fix (`src/shutdown.rs`):

- An async-signal-safe handler for SIGTERM/SIGINT/SIGHUP whose ENTIRE body is one `AtomicBool`
  store (`request_shutdown`). `install()` registers it via `libc::sigaction` (cfg linux, on the
  existing Linux-only `libc` dep — NOT probe-gated, so `cargo fmt` + the CI default build compile it).
- The paint loops POLL the flag and break, so the EXISTING tested `Drop` teardown runs:
  `run_painter` (`src/probe/painter.rs`) loops on `shutdown::painter_should_continue(stop, is_shutdown_requested())`;
  `run_paint_only` + `run` (`src/probe/run.rs`) call `shutdown::install()` at start and break their
  outer loops on `is_shutdown_requested()`.

**Hard invariant — NEVER blank fb0 directly from a signal handler or a dedicated signal thread.**
The blank MUST go through the presenter's `Drop` (a) because a signal handler can only do
async-signal-safe work (an atomic store — never an ioctl/open/write), and (b) because the KMS
blank-before-release-master ordering can't be guaranteed by an independent thread racing a
still-page-flipping painter. A flag polled by the loops is the only correct shape.
`libc::atexit` does NOT work either — it never runs on signal-driven termination.

## Testing (Tier-0, #477/#557: no local cargo compile)

The pure half (`shutdown`'s flag + `painter_should_continue`, and `fb_blank::visible_page_range`)
is proven RED→GREEN via a `rustc --test --edition 2021` replica (std-only). The `sigaction` glue,
both `Drop` impls, and the probe-wiring are probe-/hardware-gated — fmt-clean locally, first
type-checked at CI, blank behavior exercised only on the rig / E2E (mirrors the untested #660
`KmsPresenter::Drop` precedent). `rustfmt --check` DOES parse probe-gated files, so run it on every
changed file before pushing.
