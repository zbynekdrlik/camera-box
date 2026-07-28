---
paths:
  - "scripts/install-imag-nb.sh"
  - "scripts/setup-imag.sh"
  - "scripts/verify-imag.sh"
  - "scripts/imag-obs-start.sh"
  - "scripts/imag-obs-stop.sh"
  - "scripts/obs_phase2.py"
  - "scripts/imag_scenes.py"
  - "tests/install_imag_nb_pure_functions.rs"
  - "tests/setup_imag_hardware_agnostic.rs"
  - "tests/setup_imag_guards.rs"
  - "tests/setup_imag_pure_functions.rs"
  - "tests/verify_imag_pure_functions.rs"
  - "tests/harness_imag_obs_start_stop_840.rs"
---

# Replacing the imag notebook — install the OS, provision it, then VERIFY it (#791 / #815 / #816 / #821)

Three scripts, in this order. None of it is manual work; a notebook swap is repo tooling.

```
1. INSTALL OS   scripts/install-imag-nb.sh --target-disk /dev/nvme0n1 --ip <addr> --yes
                (run FROM the box's own Ubuntu desktop live-USB, as root)
2. REBOOT       into the installed system (NVRAM entry is written + set first)
3. PROVISION    IMAG_IP=<addr> sudo -E ./setup-imag.sh --yes        (on the box)
4. VERIFY       scripts/verify-imag.sh                              (from dev1, #821)
```

**Step 4 is MANDATORY — no imag box is ever reported "ready" without its output.** #821: the
replacement notebook (.187) was once reported "verified booted from disk" on the strength of the
installer's OWN claim of success, when it was in fact still on gdm3 with a login prompt — no
autologin, no openbox kiosk, no OBS. `verify-imag.sh` re-derives every fact fresh over SSH/network
AFTER steps 1-3 (kernel/cmdline, lightdm+autologin, OBS log + `:4455`, the OBS base-version pin,
NDI runtime pin, dantesync PTP lock + FRESH offset + the SAME grandmaster as the rest of the rig
(#834), the #791 operator scaffolding, and — as part of its own checks — seeds scenes/Multiview
and opens both projectors via `scripts/imag_scenes.py` / `scripts/obs_phase2.py` (no separate
manual "SCENES" step is needed any more; `verify-imag.sh` runs them itself and fails loud if either
comes back short). See `scripts/verify-imag.sh`'s own header comment for the full checks list.

## The install-layer facts (live-verified 2026-07-27 on a 24.04.2 desktop ISO)

- The live ISO's `/cow` overlay stacks `lowerdir=minimal.standard.live:minimal.standard:minimal`.
  Installing = copying the lower layers **without** the `.live` one — that layer carries casper and
  the live `ubuntu` user, and copying it produces a live-session clone, not an installed system.
- The install layers ship **NO kernel and no `/lib/modules`** — `/rofs/boot` holds only memtest
  binaries. The kernel lives in the `.live` layer and the ISO pool. So the installed system MUST get
  its kernel from `apt` **inside the chroot** (`linux-generic`), and the "is there a kernel"
  assertion belongs AFTER that install. Asserting it right after the rsync aborts every otherwise
  correct install — that was the first cut's live failure, now pinned by
  `kernel_is_installed_in_the_chroot_not_expected_from_the_layers`.
- Stack the layers read-only with one `mount -t overlay -o ro,lowerdir=top:…:bottom` and rsync the
  merged view, rather than sequential `unsquashfs` — overlayfs then resolves the whiteouts natively.
- Skip the per-language deltas and note that `minimal.enhanced-secureboot.squashfs` is a delta on
  `minimal`, while `minimal.standard.enhanced-secureboot.squashfs` is the one that belongs on top of
  `minimal.standard` — picking the wrong branch is a silently broken chain.
- Reused cam-fleet boot lessons that apply here too: a NAMED NVRAM entry **plus** the removable
  `\EFI\BOOT\BOOTX64.EFI` fallback, `systemd-networkd-wait-online` masked, `ssh.service` (not
  `ssh.socket`, which is what noble enables by default), UUID-based fstab, blank machine-id.

## The five things a FRESH box exposes that the incumbent never did (live, .187, 2026-07-27)

The incumbent box (.182) hides these because it accumulated state over months. Every one of them
aborted provisioning on the replacement notebook, and every one is now fixed + regression-tested.
When a NEXT swap fails, check this list before theorising:

- **The kernel line (#819).** `install-imag-nb.sh` must install `linux-generic-hwe-24.04`, not
  `linux-generic` — the imag role runs HWE, step 6 holds the HWE names and step 7's
  `linux-lowlatency-hwe-24.04` depends on exactly them. A GA baseline aborts step 7 outright.
- **Holding a NOT-installed package is not a no-op (#820).** `apt-mark hold <name>` on a package
  this box has never installed makes apt refuse any later install that would pull it in
  (`E: Held packages were changed and -y was used without --allow-change-held-packages`) — step 6
  held step 7 out. Hold only what `dpkg -s` confirms, and let the kernel install pass
  `--allow-change-held-packages`.
- **A missing TOOL is not a failed check (#822).** `readelf`/`nm` come from `binutils`, absent on a
  fresh Ubuntu. Their empty output made step 12 report `SONAME check failed — refuse a mismatched
  ABI` while the hot-swap had actually succeeded. Install binutils and preflight with
  `imag_require_tools`, which names the missing tool. The SAME class recurs for checks that run
  REMOTELY over SSH from `recording-e2e.sh` (gate-time, long after provisioning) rather than
  locally during provisioning — see `.claude/rules/imag-ssh-remote-tool-preflight.md` (#833).
- **usrmerge breaks literal path compares (#823).** `/lib` IS a symlink to `/usr/lib`, so
  `readlink -f` of the DM link always answers `/usr/lib/...` and a compare against the literal
  `/lib/systemd/system/lightdm.service` can never match. Canonicalise BOTH sides
  (`imag_same_unit`).
- **The OBS base version must MATCH the genlock build (#824).** The PPA moved to 32.2.0 while the
  genlock build is 32.1.2; libobs then refuses every stock plugin (`compiled with newer libobs
  32.2` × 41) and OBS comes up with ONLY `distroav.so` — no obs-websocket (so `imag_scenes.py`
  gets `ConnectionRefused` on :4455) and no encoders. `IMAG_OBS_BASE_VERSION` pins it; a superseded
  PPA binary is gone from the pool but still served by
  `launchpad.net/~obsproject/+archive/ubuntu/obs-studio/+files/` (live: pool 404, +files 200).
  `apt-mark hold obs-studio` keeps it there. The durable answer is bumping the vendored genlock
  build to the current OBS release (#825).

A healthy fresh box, post-reboot: `uname -r` on the HWE line, `/proc/cmdline` carrying the DERIVED
`isolcpus=`/`nohz_full=`/`irqaffinity=` + `preempt=full`, DM = lightdm with the autologin drop-in,
openbox + obs running as the desktop user, gdm3 purged, zero failed units, `libndi.so.6 ->
libndi.so.6.3.2`, dantesync PTP LOCKED, OBS log showing `genlock: wall-clock-slaved render tick
ENABLED` + ~24 loaded modules including `obs-websocket.so`, and :4455 listening.

## setup-imag.sh is hardware-agnostic — do not re-introduce a literal (#816)

The replacement notebook is a different machine (i5-13420H, 12 threads, **no dGPU**) than the box
the script was written against (16 threads, RTX 5050). Three things are therefore DERIVED, and a
future edit must keep them derived:

- **CPU isolation** — `imag_cpu_isolation_plan` reads `thread_siblings_list`: SMT-paired CPUs are
  P-core threads, unpaired ones E-cores; P-core0 + every E-core stay for housekeeping/IRQs, the rest
  of the P-core block is isolated for OBS, `nohz_full` covers only the last isolated pair (the #484
  render-tick pair). The #483 DECISION is unchanged — the old box's hand-tuned
  `isolcpus=2..11 nohz_full=10,11 irqaffinity=0,1,12-15` is reproduced byte-for-byte, and a test
  pins exactly that. Both OBS launch paths pin to the derived set (the openbox autostart via an
  `__ISOLCPUS__` placeholder sed'd in at provisioning time — the heredoc is quoted, so a `$VAR`
  there would NOT expand).
- **NVIDIA driver + `prime-select`** — gated on `imag_has_discrete_nvidia` (an `lspci` display-class
  match). It used to be mandatory + fail-hard, which aborts provisioning on a box that simply has no
  dGPU. On such a box the iGPU drives the HDMI program output directly.
- **`STATIC_IP`** — `IMAG_IP` overrides it. Two imag notebooks cannot both hold `.182` while the
  incumbent is still live; the default stays the incumbent so old invocations are unchanged.
- **The NDI runtime peer** — no longer pinned to cam1. `imag_pick_ndi_peer` takes the first
  REACHABLE cam of the fleet list (cam boxes carry the identical runtime) and fails loud only when
  none answers. cam1 being down (grabber card lent out) used to abort the whole provisioning run.

## Editing these scripts: the anchor-collision rule applies here too

`tests/setup_imag_guards.rs` pins ~113 literal strings and adjacencies in `setup-imag.sh`. After ANY
edit run the **full** `cargo test` (`# airuleset:build-ok`), not just the file you added — a failure
elsewhere right after touching this script is far more likely a textual collision than a real
regression. Same trap as `scripts/recording-e2e.sh` (see the project CLAUDE.md GOTCHA).

The pure functions are unit-tested by SOURCING the real script — its `BASH_SOURCE[0] != $0` guard
skips the destructive flow. Keep every new decision function pure (stdin/args in, text out, `fail`
on the impossible case) so it stays testable without a box.

## Operator PARITY is a SEPARATE concern from system parity (#791)

`verify-imag.sh`'s pre-#791 checks (kernel, lightdm, OBS log markers, NDI runtime pin, dantesync
lock, and the (n) `Cam N`/`MV Cam N` **count**) all proved the box is a healthy OBS installation --
none of them proved the box reproduces what the OPERATOR actually sees/uses. A box can pass every
one of those and still be missing whole scenes (`Cam 7`/`MV Cam 7`), whole NDI-source bindings
(the Resolume/overlay chain), and have a scrambled scene ORDER -- exactly what shipped on the
replacement notebook (.187) before this fix. The general lesson: an acceptance gate for an
appliance with a human-curated UI state (scenes, dock layout, menu, wallpaper) needs a check for
the FULL curated state, not just "the automated parts came up" -- a count (`6/6 OK`) is not the
same claim as a full set+order match.

## Getting a canonical scene collection JSON: pull it from the LIVE box, never hand-author it

A committed "canonical" artifact for anything OBS persists as JSON (a scene collection) should be
captured from a REAL running box (`cat ~/.config/obs-studio/basic/scenes/*.json` over SSH), never
hand-typed. It is plain JSON (UUIDs, scene items, per-scene `private_settings`) -- safe to
`python3 -m json.tool` for a diffable, pretty-printed commit; OBS loads a pretty-printed file
identically to its own compact one (verified live). **OBS auto-discovers and loads a pre-placed
scene collection with ZERO explicit config wiring** — dropping a file named to match the
collection name (`Untitled.json`, matching `[Basic] SceneCollectionFile=Untitled` if that's what
the box already uses, or simply the ONLY `.json` under `basic/scenes/` on an otherwise-fresh
profile) BEFORE OBS's first launch is enough; no `[Basic]` section needs to be hand-written into
`global.ini` for this to work (verified with a disposable stock OBS 30.0.2 + Xvfb on dev1: a truly
empty profile picked the pre-placed file straight up, including its `current_scene`).

**`GetSceneList`'s own array order is the REVERSE of the scene collection JSON's `scene_order`
field** (live-verified against `.187`, 2026-07-28: WS index 0 = the LAST scene in the JSON's
`scene_order`, and vice versa). Any code comparing a live WS scene order against a JSON-derived
canonical list must `reversed()` one side first.

## Generating a Qt `DockState`/`geometry` blob OFF-RIG: Xvfb + openbox + a disposable stock OBS

OBS only persists `[BasicWindow]` `geometry=`/`DockState=` (a base64 `QMainWindow::saveState()`/
`saveGeometry()` blob) on a CLEAN exit -- a box that has run 24/7 since bring-up has never shed
one, and hand-authoring the value is not viable (an internal, versioned Qt binary stream). The
safe way to get a REAL blob without ever touching the live rig:

```bash
sudo apt-get install -y xdotool openbox   # scrot/import from imagemagick already covers screenshots
Xvfb :77 -screen 0 1920x1080x24 &
DISPLAY=:77 openbox &                      # a WM is REQUIRED -- without one, top-level dock
                                            # windows (e.g. the undocked Stats dialog) render
                                            # completely unmapped/invisible to `import -window root`
HOME=/tmp/throwaway-home DISPLAY=:77 obs --disable-shutdown-check &
# xdotool mousemove <x> <y> click 1  to dismiss first-run dialogs (Auto-Config Wizard, DistroAV
# plugin-version popup, etc — read their positions via `import -window root` screenshots first)
# Docks menu -> Stats -> its little dock/undock icon in the floating window's title bar docks it
# into the main window immediately (click at the icon's coordinates)
# Then Controls -> Exit (a CLEAN exit) so OBS itself writes geometry=/DockState= into
# $HOME/.config/obs-studio/global.ini -- grep those two keys out and seed them elsewhere.
```

A DIFFERENT, older OBS version (30.0.2 stock vs the rig's 32.1.2 genlock build) is fine for this --
Qt's dock-widget object names (`scenesDock`/`sourcesDock`/`mixerDock`/`transitionsDock`/
`controlsDock`/`statsDock`) have been stable for years, and `restoreState()` degrades gracefully
(best-effort per-widget-by-name) if a future build adds/removes an unrelated dock.

**Base64 blobs can contain literal `/` — never insert them with `sed`'s `s/.../.../ ` (delimiter
collision).** Use `awk` (string concatenation, no regex substitution) to insert generated content
containing an uncontrolled/opaque payload into an existing ini section instead.

## `block-sensitive-staging.sh` false-positives on long base64/JSON payloads — bypass, don't strip

Committing a legitimate long base64-ish blob (the DockState/geometry values above) or a JSON file
containing plausible-looking strings trips the repo's secret-scanning `git add`/`git commit` hook
(Gate 2: "32+ char base64-ish blobs"). This is a FALSE POSITIVE for a genuine non-secret payload
(a Qt UI-layout blob, an OBS scene collection) — don't strip or reformat the legitimate content to
dodge the pattern; append `# airuleset:secret-ok <reason>` to the `git add`/`git commit` command
(same convention as the other `airuleset:*-ok` bypasses) and state plainly what the flagged content
actually is.

## `verify-imag.sh`/similar checks can OPEN what they OWN — audit checks used from TWO callers separately (#840)

`scripts/obs_phase2.py open-projectors` is called from TWO places: `recording-e2e.sh`'s `[0/8]`
preflight (which WANTS to open the projectors as a self-heal before a run) and, before #840,
`verify-imag.sh` check (o) (which used the SAME call to establish the very condition it then
asserted with `wmctrl -l`). A check whose job is to PROVE a state exists must never call the
action that CREATES that state first — always read the boot-produced signal directly (here:
`wmctrl -l` with zero side effects), and separately prove PERSISTENCE by exercising the box's own
real restart path (`imag-obs-stop.sh` + `imag-obs-start.sh`, or an actual reboot) rather than
trusting a one-shot open-from-dev1 to mean anything about what survives a restart. When adding a
new acceptance check, ask: "does this check CALL the same action it then MEASURES?" — if yes, it's
the #840 bug shape.

## Verify a captured-output PARSER against the REAL current output text, never an assumed format (#843)

`imag_scenes_output_ok()` in `verify-imag.sh` required the literal regex
`^MV scenes: ${count}/${count} OK`, but `imag_scenes.py`'s real print line is
`MV scenes: N/N (multiview, low-bw) OK` — the regex never accounted for the
`(multiview, low-bw)` clause sitting in between, so the check silently FAILED on every healthy
box since the line was first written (confirmed via `git log -p`, not a #840 regression). Any
check function that parses another script's printed text: paste the REAL current output (run the
producer script live, or read its exact `print(...)` f-string) against the checker function
directly (`. verify-imag.sh; imag_foo_output_ok "<pasted real text>" ...`) before trusting it —
never assume an old regex still matches after the producer's print format changed.

## `wmctrl -c` substring-closes the MAIN OBS window safely — Projector titles never collide (#840)

`wmctrl -c "OBS Studio"` (default: case-insensitive SUBSTRING match against the full title,
first match wins — confirmed via `man wmctrl`) is safe to use for a graceful OBS close because the
MAIN window's title always starts with `OBS Studio <ver> - ...` while the two Projector windows
are titled exactly `Projector - Program` / `Projector - Multiview` (live-verified `wmctrl -l -G`
on 10.77.9.187) — neither ever contains "OBS Studio", so there is no risk of the close request
hitting a projector instead. This is the ONLY mechanism that reaches OBS's own Qt
`Controls -> Exit` code path (which is what actually persists `saved_projectors` and
`DockState`/`geometry` into the scene collection/`global.ini`) — a bare `pkill -TERM` never runs
it, live-proven by re-reading `saved_projectors` before/after each (`[]` after SIGTERM-only,
2 populated entries after `wmctrl -c` + a clean exit).

## `imag-obs-start.sh`/`imag-obs-stop.sh` were NEVER provisioned by setup-imag.sh (#840)

Despite `verify-imag.sh` check (p) checking for `/usr/local/bin/imag-obs-start.sh`'s presence
since #791/#788, `setup-imag.sh` never actually fetched/installed either operator script — they
only existed on the live box because a prior session hand-placed them (confirmed: `grep -rn
"imag-obs-start\|imag-obs-stop" scripts/setup-imag.sh` returned nothing before #840). A passing
check (p) on a hand-patched box can hide a genuine provisioning gap that only bites on the NEXT
from-scratch reprovision — when a check verifies a file's presence, also grep the provisioner for
whether it actually WRITES/FETCHES that file, not just that some check expects it.
