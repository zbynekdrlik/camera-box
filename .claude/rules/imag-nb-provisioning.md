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
  - "tests/harness_imag_intel_display_841.rs"
  - "scripts/lib/imag-gpu-guard.sh"
  - "tests/harness_imag_gpu_guard.rs"
  - "tests/harness_imag_mem_guard_845.rs"
---

# Replacing the imag notebook — install the OS, provision it, then VERIFY it (#791 / #815 / #816 / #821)

> **Addresses — read this before chasing any IP in this file.** Since the **2026-07-29 IP swap**
> (user directive) the imag ROLE permanently owns **`10.77.9.182`**: the replacement notebook was
> moved ONTO that address and the retired original was moved OFF it to `10.77.9.189` (OBS stopped,
> autostart disabled). The point is that a box swap must never again drift the address every
> script, MCP and operator has memorised. **Every `10.77.9.187` in this file is HISTORICAL** — that
> was the replacement's temporary address between 2026-07-27 and the swap; the live findings
> recorded against it still stand, only the number moved. Never hardcode either address: resolve
> the active box through `scripts/imag-host.sh` (`IMAG_HOST_ACTIVE`, facts for BOTH boxes).

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

A healthy fresh box, post-reboot: `uname -r` on the HWE line, `/proc/cmdline` carrying
`preempt=full` and **NO** `isolcpus=`/`nohz_full=` token (#842 — see the CPU-affinity section
below; kernel-level isolation was REMOVED, only the taskset affinity pin remains), DM = lightdm
with the autologin drop-in, openbox + obs running as the desktop user, gdm3 purged, zero failed
units, `libndi.so.6 -> libndi.so.6.3.2`, dantesync PTP LOCKED, OBS log showing `genlock:
wall-clock-slaved render tick ENABLED` + ~24 loaded modules including `obs-websocket.so`, and
:4455 listening.

## setup-imag.sh is hardware-agnostic — do not re-introduce a literal (#816)

The replacement notebook is a different machine (i5-13420H, 12 threads, **no dGPU**) than the box
the script was written against (16 threads, RTX 5050). Things below are therefore DERIVED, and a
future edit must keep them derived:

- **CPU affinity (#483/#816, AFFINITY-ONLY since #842 — see the dedicated GOTCHA section below
  for the full incident)** — `imag_cpu_isolation_plan` reads `thread_siblings_list`: SMT-paired
  CPUs are P-core threads, unpaired ones E-cores; P-core0 + every E-core stay for
  housekeeping/IRQs, the rest of the P-core block is the set OBS's taskset pin restricts itself
  to. **The function's derivation logic is unchanged, but its OUTPUT IS NO LONGER WRITTEN TO THE
  KERNEL CMDLINE** — `isolcpus=`/`nohz_full=`/`irqaffinity=` disabled scheduler load balancing for
  a many-threaded OBS process (#842, a recurrence of #784), so `setup-imag.sh` now writes ONLY the
  taskset-affinity persisted config (`/etc/imag-isolated-cpus.conf`), never the grub.d cmdline
  drop-in. Both OBS launch paths still pin to the derived set via `taskset` (the openbox autostart
  via an `__ISOLCPUS__` placeholder sed'd in at provisioning time — the heredoc is quoted, so a
  `$VAR` there would NOT expand) — restricting OBS to a core MASK is fine; kernel-level *isolation*
  of those cores is what broke it.
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

**Confirmed again (#842, 2026-07-28) — a NEW EXPLANATORY COMMENT can be the collision, not just
code.** Rewriting step 8's header comment, one sentence mentioned the literal path
`/etc/imag-isolated-cpus.conf` in passing (explaining what the affinity pin consumes) BEFORE the
real derive-then-write code a few lines later. `tests/harness_imag_intel_display_841.rs`'s
`setup_imag_persists_the_derived_isolated_cpus_for_the_wrapper_841` does
`body.find("/etc/imag-isolated-cpus.conf")` (first occurrence) to assert ordering against the
derivation line — it silently grabbed the COMMENT instead of the real write, and the ordering
assertion failed even though the actual code was correct. Caught immediately by running the full
suite (it always is, if you run it) — fixed by rewording the comment to describe the file without
spelling out its literal path. The lesson generalizes past "code you add" to "prose you add near a
pinned literal": before writing an explanatory comment, check whether the exact string you're
about to use for color also happens to be a `.find()`/`.split()` anchor elsewhere in the file.

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

## A "port the NVIDIA setting to Intel" idea MUST be live-tested, never shipped from the analogy alone (#841)

`setup-imag.sh` had ZERO NVIDIA display-tuning code at all before #841 (the incumbent .182's
`nvidia-settings --assign ... ForceFullCompositionPipeline=On` + `GPUPowerMizerMode=1` block in
`~/.config/openbox/autostart` was hand-placed, same "provisioning gap hidden by a hand patch"
shape as the #840 entry above — never provisioned by any script, on EITHER box). The obvious-looking
port to Intel — `Option "TearFree" "true"` under `Driver "modesetting"` in a fresh
`/etc/X11/xorg.conf.d/` snippet — was written, deployed, and Xorg (`systemctl restart lightdm`)
logged back `(WW) modeset(0): Option "TearFree" is not used`. `strings
/usr/lib/xorg/modules/drivers/modesetting_drv.so` confirmed no "TearFree" text exists in the
binary at all: **TearFree is a feature of the LEGACY `xf86-video-intel` DDX, not of the built-in
`modesetting` driver** — and Xorg autoconfigures `modesetting` for this PCI id
(`(==) Matched modesetting as autoconfigured driver 0`), never matching the installed-but-unused
`xf86-video-intel` package. Shipping the dead option anyway (it "looks right", references a real
xorg.conf.d syntax, and produces no ERROR, only a WARNING easy to miss in a long boot log) would
have been exactly the cargo-culted-NVIDIA-semantics mistake this class of ticket exists to catch.

**The check that actually proves an xorg driver option took effect: grep `Xorg.0.log` for
`(WW) ... Option "X" is not used` AFTER restarting the X session that loads the new
xorg.conf.d file — a silent absence of an error is NOT proof the option did anything.**
`strings <driver>.so | grep -i <option-name>` is the fast pre-check (if the literal option name
isn't even a string in the binary, it cannot possibly be implemented) before ever touching the
live box. The same live-test-before-shipping discipline surfaced that `Option "VariableRefresh"`
(VRR/adaptive-sync) IS real on this build (`strings`-confirmed, and visible as an X property
`VariableRefresh: disabled` in the log even with it off) — but the specific HDMI-1 projector
output reports `vrr_capable: 0` in `xrandr --verbose` (only the eDP-1 laptop panel supports it),
so it wasn't applicable to the actually-affected output either. What this driver stack DOES
provide tear-free by default, confirmed in the same log: `Present`+`DRI3` extensions initialize
cleanly and `PageFlip`/`Atomic` are compiled into the driver — a full-screen client with no
compositor running (this box's whole design) gets direct page-flipped scanout automatically, with
no xorg.conf.d knob needed or available for it.

**The genuinely-applicable Intel/i915 counterpart to `GPUPowerMizerMode=1` turned out to be real
and IS the fix that shipped:** the iGPU actively DVFS-scales its clock (`gt_cur_freq_mhz`/
`gt_act_freq_mhz` sampled cycling well below the box's own `gt_RP0_freq_mhz` ceiling under live
render load) — the same ramp-hitch class of stutter `GPUPowerMizerMode=1` avoids on NVIDIA. i915
has no PowerMizer, but pinning the frequency FLOOR (`gt_min_freq_mhz`) up to the hardware's own
reported ceiling (`gt_RP0_freq_mhz`, NEVER a hardcoded MHz literal — a future Intel notebook's
ceiling will differ) gets the same "always at max clock" outcome, applied via a dedicated systemd
oneshot unit (sysfs resets every boot) mirroring the existing `cpu-performance.service` pattern.

## The #841 lesson generalizes past SETTINGS to CHECKS/GATES too — search `nvidia`/`nvidia-smi` repo-wide after any dGPU-swap-adjacent ticket (#845)

#841 (above) is about a config SETTING baked against the incumbent NVIDIA box. #845 is the same
root failure shape applied to a GATE: `scripts/recording-e2e.sh`'s `[4e/8]` headroom preflight
(#709) unconditionally shelled `nvidia-smi`, written before the notebook swap (#816) — it aborted
EVERY E2E gate run on the replacement box (10.77.9.187, Intel iGPU only, no discrete GPU) with
"returned an unreadable value", blocking #791/#840/#841 from ever merging behind it, because
nobody had grepped for OTHER incumbent-only assumptions when the swap landed.

**The fix pattern, reusable for any future NVIDIA-only check found this way:**
1. Detect dGPU presence via the ONE existing detector, `imag_has_discrete_nvidia`
   (`setup-imag.sh`, an `lspci` display-class match, #816) — reuse it by SOURCING `setup-imag.sh`
   from the calling script (verify-imag.sh already did this; recording-e2e.sh now does too) rather
   than writing a second detector with different semantics.
2. Preflight the TOOL you're about to trust for the detection itself (`lspci`) via
   `imag_require_remote_tool_cmd`/`imag_remote_tool_probe_missing` (#833) — a missing `lspci` must
   fail loud by name, never be silently misread as "no discrete GPU" (that would be exactly the
   measured-zero bug class #833 exists to prevent, one level up from the check it's guarding).
3. Branch: dGPU present → the ORIGINAL NVIDIA-specific check, byte-for-byte unchanged (a box that
   still has a dGPU must never regress). No dGPU → investigate LIVE on the actual box (never
   invent the substitute metric by analogy — the #841 TearFree lesson) what the genuinely
   equivalent, assertable signal is on THIS hardware, and name the real condition in every error
   message ("no discrete GPU", never "check the NVIDIA driver" on a box that never had one).
4. NEVER skip/degrade the check on the no-dGPU box just because the exact NVIDIA mechanism doesn't
   exist there — the requirement the check exists to prove (headroom before StartRecord, encoder
   capability, whatever) survived the GPU swap even though the mechanism didn't.

**#845's own live investigation, as a worked example of step 3:** an integrated GPU is UMA
(unified memory architecture) — no separate VRAM pool exists (confirmed on .187: no per-GPU memory
accounting anywhere under `/sys/class/drm/card1/`, only `gt_*_freq_mhz` clock-scaling files;
`/sys/kernel/debug/dri/*/i915_gem_objects` needs root, unavailable over the provisioning SSH user).
So `/proc/meminfo`'s `MemAvailable` is the correct headroom analogue — implemented as its own
`imag_mem_*` function set in `scripts/lib/imag-gpu-guard.sh` (query/parse/headroom/messages),
deliberately NOT a renamed reuse of the `imag_gpu_*` functions (they measure genuinely different
resources; only the pure-compare SHAPE is shared).

**After landing a fix like this, grep the WHOLE repo for the same literal** (`grep -rn "nvidia"
scripts/ scripts/lib/`) before closing out — #845's sweep found one more sibling,
`scripts/imag-gpu-contention-sampler.sh` (#674), with the identical unconditional
`command -v nvidia-smi || FATAL` shape. It was NOT wired into any automated gate (a standalone
manual diagnostic, zero callers anywhere else in the repo), so it was out of #845's own scope —
filed as its own follow-up issue (#846) rather than fixed in the same PR, per the bundling gate
(a genuinely separate script, not "the same preflight"). **#847's own sweep found ONE more**:
`scripts/imag-obs-watchdog.py`'s wedge-forensic `snapshot()` unconditionally shells `nvidia-smi`
(x3) and hardcodes PCI address `01:00.0` — filed as #849 (different subsystem, needs its own
hardware-equivalent-forensics design, not fixed in the same PR).

## A config value written over the websocket does NOT take effect on an already-running OBS — it needs a RESTART (#847)

`SetProfileParameter` (obs-websocket) just does `config_set_string(profile_config, ...);
config_save(profile_config);` — confirmed by reading
`vendor/obs-studio/plugins/obs-websocket/src/requesthandler/RequestHandler_Config.cpp`'s handler.
It does **not** call anything equivalent to `OBSBasic::ResetOutputs()`, which is what actually
(re)creates OBS's Advanced-output encoder objects from the CURRENT config. Those output objects
are built ONCE, when OBS's own frontend starts up (reading whatever `RecEncoder`/etc. was
persisted in `basic.ini` at THAT moment) — changing the ini value later via the websocket writes
the file correctly, but the ALREADY-RUNNING OBS process keeps using its stale, already-built
output object until it is restarted.

**Live-observed trap (#847):** setting `RecEncoder=obs_qsv11_v2` via `SetProfileParameter` while
OBS was running, then calling `StartRecord`, silently did nothing (`outputActive` stayed `False`,
0 bytes) — not because the new encoder was wrong, but because OBS's already-running recording
output object was still the one built at ITS OWN startup (with the OLD, broken NVENC config). The
tell: `GetProfileParameter` correctly echoed the NEW value back, yet the behavior stayed exactly
like the OLD one — a config write that "worked" per its own read-back but had ZERO runtime effect
is this exact bug, not a flaky encoder.

**The fix pattern for testing/applying any `AdvOut`/output-shaped `SetProfileParameter` change on
a LIVE box:** write the new value FIRST (or via `seed_profile()`'s own boot-time re-application),
THEN restart OBS through the normal operator path (`imag-obs-stop.sh` + `imag-obs-start.sh` — the
graceful `wmctrl -c` close + relaunch, which re-reads `basic.ini` fresh at OBS's own startup and
also re-runs `imag_scenes.py --bootstrap`, harmlessly re-applying the same value). Testing a NEW
encoder id (or any other Advanced-output setting) live, without an intervening restart, will
silently prove NOTHING either way — a false negative if the new value would actually have worked,
and (as happened here) a confusing false trail if you don't realize the restart is missing.

## `isolcpus=`/`nohz_full=` on the kernel cmdline is a scheduler footgun for a MULTI-threaded process — affinity masks alone are safe (#784, recurred as #842)

**This is a REPEAT regression — read it before touching CPU isolation/affinity on this box again.**
#483/#816 derive a CPU set and, until #842, wrote it into `GRUB_CMDLINE_LINUX_DEFAULT` as
`isolcpus=<set> nohz_full=<pair> irqaffinity=<rest>`. `isolcpus=` removes the listed CPUs from the
kernel scheduler's *load-balancing domains* — it is designed for explicit PER-THREAD pinning
(one realtime thread, one dedicated core), never for handing a whole range mask to a many-threaded
process. OBS on imag is ~106-119 threads; once isolated, the scheduler stopped rebalancing THOSE
CPUs among themselves and piled the vast majority of OBS's threads onto a single one (measured
twice now: #784 on the incumbent .182 box 2026-07-15, #842 on the replacement notebook .187
2026-07-28 — same 114-ish-threads-on-one-core signature both times). Direct-measured consequence:
NDI receive drops from 60fps to ~53fps with 7-10 underruns/s, because the starved cores can't keep
up with 4-6x 1080p60 decode.

**Why it recurred:** #784 found the defect and hand-deleted `/etc/default/grub.d/98-imag-isolation.cfg`
directly on the LIVE box (.182) — a hotfix, never ported back to `scripts/setup-imag.sh` (the
SOURCE that generates that same file). #816 then generalized the CPU-set DERIVATION (topology-based
instead of a hardcoded literal) without touching the underlying isolcpus DECISION, so provisioning
the replacement notebook on 2026-07-27 regenerated the identical defect on new hardware. **The
guard that would have caught this at provisioning time — "fail loud if `/proc/cmdline` carries
isolcpus/nohz_full" — was written down as an action item on #784 on 2026-07-15 and then deferred
between #780 and #791 for two weeks without ever being implemented.** Lesson: a live hand-fix on
one box is not a fix — the SOURCE script must change, AND the acceptance gate must assert the new
contract, in the SAME sitting, or the exact same defect reprovisions itself on the next box.

**The fix that shipped (#842):** stop writing `isolcpus=`/`nohz_full=`/`irqaffinity=` to the
kernel cmdline entirely — not just `isolcpus`, all three, since `nohz_full`/`irqaffinity` only had
meaning paired with an isolated block and neither serves any purpose alone. **The taskset AFFINITY
pin is NOT the same mechanism and is NOT the problem** — `imag-obs-start.sh`'s
`taskset -c "$IMAG_ISOLATED_CPUS"` (fed by the unchanged `imag_cpu_isolation_plan` derivation,
persisted to `/etc/imag-isolated-cpus.conf`) restricts WHICH cores OBS may run on without removing
those cores from the scheduler's load-balancing domain — threads still migrate freely WITHIN the
mask. Live-verified: OBS restricted to 6 cores via taskset alone (no isolcpus) spreads its threads
19/16/24/26/12/17 and gets the SAME 60.2fps/0-underrun numbers as an unrestricted box. **Restricting
is safe; isolating is not** — don't conflate the two when reasoning about this class of fix.

**Two independent acceptance-gate checks now guard this (`scripts/verify-imag.sh`, run at the
MANDATORY step-4 VERIFY phase of every provisioning — see the runbook at the top of this file):**
(d) hard-FAILs if `/proc/cmdline` carries either `isolcpus=` or `nohz_full=` (`imag_cmdline_free_
of_kernel_isolation`), and separately (s) hard-FAILs if OBS's live threads concentrate onto a
single CPU core (`imag_obs_thread_concentration_ok`, >60% on one core) — the DIRECT SYMPTOM check,
independent of (d), so a *different* future mechanism that produces the same thread pileup without
writing a cmdline token still cannot pass silently. `scripts/setup-imag.sh` also self-heals: if a
leftover `98-imag-isolation.cfg` is found on a box being (re)provisioned, it is removed and grub
regenerated, rather than merely not-writing a new one.

**If a future SCHED_FIFO-class realtime thread genuinely needs kernel-level tick support** (the
`nohz_full`/`irqaffinity` tokens existed to prepare for a genlock render-tick thread that, per
#483/#484, does not exist yet — it uses `sched_setscheduler(SCHED_FIFO)` + an rtprio ulimit grant,
neither of which needs a cmdline flag) — that is a NEW, explicit, tested, per-thread pinning
design of its own, never a blanket range-mask isolation reintroduced "because it was there before".
#784 said it plainly: isolation may return "LEN s explicitným per-thread pinningom" (only with
explicit per-thread pinning) — this is still the bar.
