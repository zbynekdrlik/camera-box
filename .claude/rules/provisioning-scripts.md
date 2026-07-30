---
paths:
  - "scripts/setup-device.sh"
  - "scripts/verify-device.sh"
  - "tests/setup_device_pure_functions.rs"
  - "tests/verify_device_pure_functions.rs"
---

# `setup-device.sh` / `verify-device.sh` — companion-script conventions (#863)

## `setup-device.sh` never starts/restarts services live — it only writes files + `enable`s

Every STEP that touches a systemd unit (camera-box.service at STEP 7, cam2-painter.service at
STEP 3b, dantesync.service, etc.) writes the unit/drop-in file and runs `systemctl daemon-reload`
+ `systemctl enable <unit>` — it never calls `systemctl start`/`restart`. The whole provisioning
run defers taking effect to the box's NEXT REBOOT (STEP 19's own summary literally tells the
operator to `reboot` at the end). **When adding a new provisioning STEP that installs a service,
follow this convention** — do not add a live `start`/`restart` call "to see it working sooner";
`scripts/verify-device.sh` is the dedicated POST-REBOOT acceptance gate and is where a fresh
install's liveness is actually proven (see its own header: "the fourth and final phase").

## Adding a NEW acceptance check to `verify-device.sh`? Insert it BEFORE check `(q)`, never after

`(q) .bak cruft drift` is the intentionally-LAST check before the `ALL CLEAR`/`VERIFY FAILED`
summary — `tests/verify_device_pure_functions.rs::check_q_is_wired_into_the_live_flow_as_a_
warning_never_a_fail` locates `(q)`'s implementation block via `rfind` and asserts it **runs to
end-of-file** (its own comment: "(q) is the LAST check before the summary, so the block runs to
end-of-file"). A new check appended AFTER `(q)` gets silently folded into that test's slice and
trips the "must never call `fail()`" assertion even though the new check legitimately calls
`fail()`. **Fix: always insert a new check block immediately BEFORE the `# (q) .bak cruft drift`
comment**, not after it — `(q)` must remain the true last check. Document the new check in
THREE places (all three exist for every existing letter): the top-of-file header comment's
"Checks (all must pass)" list, the `usage()` function's own `Checks:` doc block, and the
executable check itself.

## `cam2-painter.service` + camera-box display ownership (#863)

cam2 is the ONE fixed painter box (permanently excluded from `camera_strih_route()`). Its
`camera-box.service` carries a PERMANENT `camera-box.service.d/cam2-no-display.conf`
(`Environment=CAMERA_BOX_NO_DISPLAY=1`) so camera-box's own `--display` thread (a plain
`/dev/fb0` writer) never contests the framebuffer/DRM master with `cam2-painter.service` (a KMS
page-flip presenter, `presenter=auto` per `.claude/rules/presenter-drm-selection.md`). Both
services can be `active` simultaneously with zero conflict as long as this drop-in is present —
confirmed live (`fuser /dev/dri/cardN` held ONLY by `frame-probe`, camera-box still emitting
NDI). If you ever see BOTH trying to paint cam2's monitor, check this drop-in is still installed
before assuming a genuine regression in either binary.

## Two commented-out prose lines that legitimately compose to a false test failure

If a comment you add near an existing `systemctl ... cam2-painter` call ALSO contains the
literal words "systemctl" and "cam2-painter" (even split across an English sentence, e.g. "the
PERMANENT cam2-painter.service ... systemctl start cam2-painter"), it trips
`tests/harness_cam2_painter_coordination.rs`'s `cam2_painter_stop_and_start_are_best_effort_
guarded` — that test scans EVERY line of `recording-e2e.sh` containing both substrings and
demands a `|| true`/`2>/dev/null` guard, comments included. Reword so the two literal substrings
never land on the same line (e.g. "...came back active + painting after cleanup() restarts it
below..." instead of quoting the actual command in the same sentence).
