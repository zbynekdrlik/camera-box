---
paths:
  - "src/probe/presenter.rs"
  - "src/probe/kms.rs"
  - "src/presenter_kind.rs"
  - "src/probe/painter.rs"
---

# DRM card numbering is NOT a stable ABI — never hardcode `/dev/dri/cardN` and trust it forever

**Confirmed regression (#854, 2026-07-28):** the CLI `--drm-device` default (`/dev/dri/card1`,
`src/bin/frame-probe.rs`) genuinely worked on cam2 on 2026-07-04 (the #464 doc comment in
`src/presenter_kind.rs` quotes a live log line proving it). By 2026-07-28, cam2 (single i915 GPU,
one reboot + kernel update later) enumerated the SAME physical device as `/dev/dri/card0` — no
`card1` node existed at all. `PresenterKind::Auto` treated the failed `KmsPresenter::open` as an
ordinary silent fallback to the imperfect single-buffered vsync-gated `fbdev` presenter — no
error, no crash, no log anyone would notice — permanently degrading the tear-free double-buffered
KMS guarantee from that reboot onward. The visible symptom (fleet-wide ~62-64% optical-QR
undecodable, #854) looked nothing like "wrong DRM device" until traced back through
`presenter: DRM/KMS unavailable ... falling back to fbdev`.

**Diagnosis, always do this FIRST when a painter/presenter run looks "off" (low decode rate,
tearing, wrong cadence) on a box that used to work:** SSH in and check the ACTUAL log line, not
just whether the painter process is alive:
```
grep -E "presenter:|KmsPresenter|VsyncFb" /tmp/painter.log
```
`"presenter: using DRM/KMS page-flip (...)"` + `"vblank-locked DRM page-flip — tear-free 1:1 at
60Hz"` = genuine double-buffered KMS. `"VsyncFb: single-buffer ... vsync-gated"` = the box has
silently fallen back to the WEAKER #68 fbdev path — check the preceding WARN line for why (almost
always: the configured DRM device doesn't exist any more). Cross-check with `ls /dev/dri/` +
`dmesg | grep -i i915` (which DRM minor did i915 register as).

**The fix already shipped (#854): `PresenterKind::Auto`'s `open_presenter` (in
`src/probe/presenter.rs`) now enumerates `/dev/dri/card*` and tries every candidate — ordered
ascending by `order_drm_card_candidates` (`src/presenter_kind.rs`, Tier-0 pure + tested) — before
giving up on KMS and falling back to fbdev.** This means a FUTURE renumbering self-heals
automatically; you should not need to hand-fix `--drm-device` again for THIS failure mode. If you
ever see the fallback WARN log (`"configured DRM device ... unavailable, found a working KMS
device at ... instead"`), that is the auto-discovery firing correctly, not a bug — but it is a
signal the CLI default is now stale and worth updating so the fast-path (no discovery needed)
resumes.

**The companion regression this ticket ALSO fixed — the dual-QR Vernier's "settled" half was
never actually stable.** `paint_one_frame` (`src/probe/painter.rs`) used to stamp ONE fresh
`gen_ts_ns` into BOTH halves' payloads every tick, even though only ONE side's `frame_id` actually
changes per refresh (`vernier_ids`). That silently changed the "settled" side's rendered QR pixels
every tick too, defeating the whole point of the Vernier scheme (a capture straddling the refresh
boundary was supposed to read IDENTICAL bits on the settled side regardless of which side of the
seam it landed on). Fixed by threading a small `VernierGenTs { left, right }` per-side
last-stamped-`gen_ts_ns` state through the painter loop — only the side whose `frame_id` actually
changed this tick gets a fresh clock read baked into its payload. **If you ever touch the
dual-QR/Vernier payload logic again: the settled side's payload must be byte-identical
tick-to-tick — write a real encode→render→decode round-trip test (see
`settled_left_half_payload_is_byte_identical_across_the_next_tick` in `src/probe/painter.rs`) to
prove it, not just a geometry-level assertion.**

**Both changes are in `src/probe/**`, which never compiles under this repo's default
(Tier-0-restricted) local build — CI's `--features probe` build is the FIRST place either change
actually compiles or runs.** Review such changes extra carefully by hand (types, control flow,
the exact contract of the function you're changing) before pushing; do not expect a local RED/
GREEN cycle to catch a mistake here.
