---
paths:
  - "scripts/setup-device.sh"
  - "scripts/create-usb-linux.sh"
  - "scripts/verify-device.sh"
  - "tests/setup_device_pure_functions.rs"
  - "tests/verify_device_pure_functions.rs"
---

# `setup-device.sh` / `verify-device.sh` — companion-script conventions (#863)

## The header comment list and `usage()`'s Checks block can DISAGREE — grep BOTH, trust neither alone (issue 1213)

The "document in three places" rule below assumes all three sites stay in lock-step, but they can
drift: `(x)` ffmpeg and `(x2)` mpv are documented in the top-of-file header "Checks (all must
pass)" comment list, but were NEVER added to `usage()`'s own heredoc Checks: doc block (found while
adding `(af)`, issue 1213) — `usage()` jumps straight from `(o)` to `(q)` with no `(x)`/`(x2)`
line. Two consequences: (1) `usage()`'s doc block ALONE is not proof a check does or doesn't exist
— always cross-check the header comment list AND grep the live-flow exec section directly before
concluding a letter is free or a check is missing; (2) when you find a gap like this while adding
your OWN new check, backfilling the missing older entry is optional polish, not required scope —
note it (as this entry does) rather than silently expanding your diff to fix unrelated drift.

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

## Adding a new `camera-box.service.d` drop-in + its `verify-device.sh` acceptance check (#1087)

To bake a NEW env drop-in into provisioning AND prove it takes effect, follow the (e) genlock pattern
(worked example: (z) publish-30p, `CAMERA_BOX_PUBLISH_30P=1`, the "CAMn (30p)" blend stream):

- **`setup-device.sh` write**: put the `cat > .../<name>.conf` heredoc INSIDE STEP 7, right beside the
  `genlock.conf` write — it shares STEP 7's existing `mkdir` + `daemon-reload` + `enable camera-box`,
  keeping every `camera-box.service.d` drop-in write co-located. Do NOT add a separate late step: a step
  after STEP 18 writes to a read-only root (STEP 18 flips root ro) and fails. Use `<< 'EOF'` (quoted) for
  a literal env drop-in and confirm byte-faithfulness by hashing the live fleet file
  (`sha256sum` on a box) against the heredoc body.
- **Drop-in-value parser** (pure, sourced + unit-tested): mirror `genlock_dropin_fps` EXACTLY —
  `printf '%s\n' "$1" | grep -oE 'ENV_VAR=[0-9]+' | tail -1 | cut -d= -f2 || true`. The trailing `|| true`
  is mandatory (the #458 footgun: a no-match `grep|tail|cut` fails under `pipefail` even though
  `tail`/`cut` succeed on empty input, and a bare `X="$(parser ...)"` caller must never abort).
- **"Feature actually running" facet**: don't stop at the drop-in file — prove the feature is LIVE by
  reusing the `CB_JOURNAL` already gathered in check (c) (the InvocationID-scoped last-300-lines read)
  and grepping it for the feature's own recurring journal marker. Use `grep -cE 'markerA|markerB' || true`
  (a COUNT; caller tests `!= "0"`) — **NEVER `grep -q`**: `-q` closes the pipe on first match, which can
  SIGPIPE the upstream `printf` and, under `pipefail`, return non-zero even on a real match. `grep -c`
  reads all input (no early close), so it is SIGPIPE-safe.
- **Exec block**: insert it BEFORE the `# (q) .bak cruft drift` block (the (q)-last rule above), follow
  the (e) shape (`rc=0; X="$(ssh_box "cat ...")" || rc=$?`), and FAIL on EVERY non-success path (missing
  / wrong-value drop-in, unreadable journal, drop-in-present-but-not-publishing — e.g. an old binary that
  predates the feature). Document the new letter in all THREE places (header Checks list, `usage()` Checks
  block, executable block).
- The next free check letter is whatever the header/usage/exec sequence has NOT used (they are NOT strictly
  alphabetical in file order); grep the three lists rather than assume. A letter cited only in a
  `setup-device.sh` comment ("verified by verify-device.sh's (N) check") is not proof the check exists —
  confirm against `verify-device.sh` itself. Once the single letters (a)-(z) are exhausted, the scheme is
  TWO-CHAR: (aa) was the #782 interkom check, (ab) the #1066 remoteos-mcp check, and so on.

## Mirroring an imag `setup-imag.sh` provisioning STEP into `setup-device.sh` (#1066)

`setup-imag.sh` and `setup-device.sh` provision the SAME agents on different box classes, so a step
added to one is often wanted on the other (#1066 mirrored setup-imag.sh's remoteos-mcp step 23 —
issue 858 — into setup-device.sh). Two adaptations are NOT optional copy-paste:

- **Use a lettered sub-step, not a renumber.** setup-imag.sh uses numbered `step N` + a bumped
  `TOTAL_STEPS`; setup-device.sh's numbered backbone (STEP 1..19) is anchored by
  `tests/setup_device_pure_functions.rs` / `setup_device_provisioner_hardening.rs` (`STEP 18:
  Configure read-only`, the `[19/` summary, restore_root_mode-after-STEP-18). Add the mirror as a
  lettered sub-step in the STEP 3b idiom (`# STEP Nb: …` + a `[Nb]` banner, NO `/${TOTAL_STEPS}`,
  TOTAL_STEPS unchanged) so nothing renumbers. Place it in the rw window — after its natural
  predecessor and BEFORE STEP 18's ro-root flip (an installer that writes /usr+/etc fails on a ro
  root).
- **Gate on `is-enabled`, not imag's `is-active`.** setup-device.sh is enable-only / defer-to-reboot
  (never live-start); assert the durable reboot-survival property with a LITERAL
  `is-enabled == enabled` compare (`--quiet`'s exit code passes for a `static` unit with no
  `[Install]`). Prove the LIVE runtime state (`is-active` + a listening port) in the paired
  verify-device.sh `(ab)`-style check, which runs post-reboot — that is where a cam-box install's
  liveness belongs (see its own header: "the fourth and final phase").

## A WARN-using check inserted before (q) trips the (aa)-hard-gate test, which slices [aa..q] (#899)

The `(q)`-last invariant above keeps `(q)` physically last, but a SECOND slice-based test also
spans multiple checks: `tests/verify_device_pure_functions.rs::check_aa_fails_on_each_drift_facet`
sliced the (aa) block as `&live_flow[aa..q]` — from the `# (aa)` marker all the way to the `# (q)`
marker — then asserted `!aa_block.contains("warn \"")` (the (aa) interkom check must FAIL, never
merely warn, on a drift). That slice silently folds in EVERY check between (aa) and (q). It stayed
green only because the intervening checks ((ab) remoteos) used `fail`/`ok` exclusively — so the
FIRST check inserted between (aa) and (q) that legitimately uses `warn "` (the #899 `(ac)`
realtime-isolation drift check, which is WARN-only by design) tripped the assertion. **Fix: scope
that slice to the (aa) block ALONE** — end it at the NEXT check boundary
(`live_flow[aa..].find("\n# (ab) ")`), not at `(q)`. This is the same over-slice class as the
`(q)`-last rule, but for a different test whose window happens to span from (aa) to (q). When
adding ANY new check that can `warn` (a report-only / informational check) before `(q)`, grep the
test file for slices bounded by `# (q)` / `.bak cruft drift` and confirm your new WARN lines don't
fall inside a `!...contains("warn \"")` window; narrow the offending slice to its own check block.

## Two-char check-letter scheme is now at (ac) (#899)

The single letters (a)-(z) are exhausted; the two-char scheme continues (aa) interkom (#782),
(ab) remoteos-mcp (#1066), **(ac) realtime-isolation drift (#899, WARN-only)**. Grep the header /
usage / exec lists for the next free two-char letter (they are NOT in file order) rather than
assume — a letter cited only in a `setup-device.sh` comment is not proof the check exists.

## The netplan LAN stanza must match the NIC by NAME (`enp*`), never `driver: "*"` (#1155)

Both netplan writers — `setup-device.sh` STEP 2 (static IP) and `create-usb-linux.sh`'s chroot
base image (DHCP) — write `/etc/netplan/01-netcfg.yaml`. The LAN stanza is pinned to
`match: name: "enp*"` (the PCI NIC), **never** `match: driver: "*"`.

**Why (live incident 2026-08-20, cam1):** netplan's `driver: "*"` glob claims EVERY driver-backed
link. When a camera is plugged into a cam box over USB (the bkshading architecture, issue 808), it
enumerates as a USB CDC-NCM ethernet device (`enx<MAC>`, driver `cdc_ncm`) and matched the same
stanza — so it inherited the box's static IP `10.77.9.61/23` **plus a duplicate default route**.
dantesync's PTP multicast join (224.0.1.129) then bound the camera link instead of the real NIC
`enp3s0`, and the box went PTP-deaf for ~5 h: free-running crystal, `[NTP] Stepped +12ms` every
~4 min, every clock gate FAIL, and every E2E verdict after the plug-in carried clock-caused CAM1
gaps that are NOT content regressions. The whole dantesync 1.8.50/1.8.51 canary chain was misread
as a servo regression before the duplicate-IP link was found. **The bkshading lane plugs a camera
into EVERY cam box over USB, so this trap fires on each box the moment the camera is connected.**

- **The discriminator:** PCI/onboard NICs enumerate as `enp*` (`enp3s0` on the current fleet); USB
  CDC-NCM camera links enumerate as `enx<MAC>`. `match: name: "enp*"` includes the former and
  structurally excludes the latter. netplan `match: name:` takes a SINGLE shell glob (no
  `enp*`-OR-`eno*` in one stanza), so the tight `enp*` glob is the correct minimal pin for this
  all-PCI-NIC fleet. A future box whose onboard NIC enumerates as `eno*` would need the pin
  widened — but that is not the current fleet, and widening on speculation is wrong.
- **Do NOT rename the stanza** (`all-ethernet`) or change anything else in the block — only the
  `match:` key. This is the exact minimal edit the owner applied by hand to cam1 (backup at
  `/root/netplan-backup-01-netcfg.yaml.bak`).
- **`verify-device.sh` guards it (check `(ad)`):** FAILs if the installed netplan still matches the
  driver wildcard (`netplan_driver_wildcard_count`), or if two interfaces carry the box IP
  (`interfaces_sharing_ip` over `ip -br addr` — the live proof the trap has not fired). Both pure
  fns are `run_sourced`-tested; the static-anchor test
  `both_netplan_writers_pin_lan_stanza_to_enp_never_driver_wildcard` pins both writers.
- **The dedicated camera-link stanza** (its own subnet / link-local, no default route) belongs to
  the bkshading lane (issue 808), not the LAN pin — it is complementary, not a substitute for the
  pin above.

## `setup-device.sh` re-run on an already-booted box: TWO separate root-writable defects, both live (#1289)

Re-provisioning cam5/cam6/cam7 (they rejoined `CAMERA_ACTIVE_SET` after being retired, so they
never got the newer STEPs `setup-device.sh` grew in the meantime) exposed the SAME "root must be
writable BEFORE the first mutating action" class twice in one run, at two different layers:

1. **The rw-remount CALL ran too late.** `ensure_root_writable()` (issue 599) is defined near the
   top of the file but was CALLED right before STEP 15 — the point issue 599 cared about
   (apt-get/dpkg/systemctl). It never accounted for STEP 1 through STEP 14 (hostname, netplan, the
   binary install, NDI/ALSA/config/systemd/GRUB/sysctl writes) ALSO writing under `/etc`/`/usr`
   BEFORE that point. On a first-provisioning run root is naturally rw so this never showed; on an
   in-place re-run against an already-booted **read-only** appliance, STEP 1's hostname write (no
   `|| true` guard) is the FIRST filesystem write in the whole script and aborted with `Read-only
   file system` before ANY remount logic ran. **Fix: move the bare call to right after the confirm
   prompt, before the pre-flight curl-install block — i.e. before the first write of any kind.**
   `restore_root_mode()` stays exactly where it was (after STEP 18).

2. **A `curl -o` download can straight-up truncate a RUNNING binary in place.** Once (1) was fixed
   and root stayed writable for the whole run, the SAME re-run died again at STEP 17:
   `curl -fsSL "$DANTESYNC_URL" -o /usr/local/bin/dantesync` while `dantesync.service` was ACTIVE
   and had that exact path open — the kernel refuses to open a currently-EXECUTING file for write
   (`ETXTBSY`), so curl failed and `fail()` aborted, even though the release URL itself answered
   200. The IDENTICAL shape existed in STEP 3's camera-box URL branch and STEP 3b's frame-probe URL
   branch (cam2-only) — only the LOCAL-path / CI-artifact branches were already safe, because they
   use `install -m 0755 src dest`: `install`'s default behavior replaces the destination via a NEW
   inode (unlink-then-create), which is safe over a running executable (the OLD inode stays open
   under the still-running process until it exits), unlike `curl -o`'s truncate-in-place onto the
   SAME inode. **Fix: download to a `mktemp` temp file, verify it non-empty (`[ -s "$tmp" ]`), then
   `install -m 0755 "$tmp" dest`, `rm -f "$tmp"` — never `curl ... -o /usr/local/bin/<name>`
   anywhere in this script.** STEP 17 additionally SKIPS the download entirely when
   `/usr/local/bin/dantesync --version` already reports the release URL's own tag (parsed from the
   URL path, `.../releases/download/vX.Y.Z/...`, via `grep -oE '/v[0-9]+\.[0-9]+\.[0-9]+/' | tr -d
   '/v'`) — an idempotent re-run then never touches the live binary/service at all, per
   `.claude/rules/dantesync-version-reading.md`'s `dantesync --version`-answers-everywhere finding.

**The general lesson for ANY future STEP that installs/replaces an executable this script (or
`setup-imag.sh`) might be re-run against on an already-provisioned box: `install -m 0755 src dest`
after downloading to a temp file, never a direct `curl -o`/`wget -O` onto the live path.** A
first-provisioning run can't tell you this is wrong (root is rw, nothing is running yet) — only a
genuine in-place re-run against a live box exercises it, which is exactly why both of these sat
latent since #599 (defect 1) and since the binary was first curl-installed (defect 2) until cam5/
cam6/cam7's actual re-provisioning surfaced them, one after the other, in the SAME live run
(2026-09-03 11:52–11:56Z).
