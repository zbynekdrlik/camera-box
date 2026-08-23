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

**Driving step 3 from dev1 over a plain (no-tty) ssh call:** `sudo` cannot prompt, so a bare
`ssh newlevel@imag "sudo env … bash /tmp/setup-imag.sh --yes"` fails with `a terminal is
required to read the password`. Use `echo <pw> | sudo -S env GENLOCK_RUN_ID=<run> CAM_PW=…
GH_TOKEN=$(gh auth token) bash /tmp/setup-imag.sh --yes` (password via stdin) — confirmed live
on the 2026-08-01 hot-swap passes. After ANY hot-swap, still verify the obs process START TIME
is newer than the swap (`ps -o pid,lstart -C obs`) per the #912 restart-race gotcha.

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

## The six things a FRESH box exposes that the incumbent never did (live, .187, 2026-07-27)

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
  genlock build is 32.2.0; libobs then refuses every stock plugin (`compiled with newer libobs
  32.2` × 41) and OBS comes up with ONLY `distroav.so` — no obs-websocket (so `imag_scenes.py`
  gets `ConnectionRefused` on :4455) and no encoders. `IMAG_OBS_BASE_VERSION` pins it; a superseded
  PPA binary is gone from the pool but still served by
  `launchpad.net/~obsproject/+archive/ubuntu/obs-studio/+files/` (live: pool 404, +files 200).
  `apt-mark hold obs-studio` keeps it there. The durable answer is bumping the vendored genlock
  build to the current OBS release (#825).
- **`systemctl --user` needs a user bus the fresh box does not have yet (#1182).** Steps 21/27 run
  `sudo -u "$DESKTOP_USER" … systemctl --user daemon-reload`/`enable --now …`/`disable …`, which
  need the desktop user's systemd USER MANAGER bus (`/run/user/<uid>/bus`). That socket exists only
  once the user has a live login session (the kiosk lightdm autologin) or lingering — on a
  from-scratch box provisioned detached BEFORE the first kiosk boot it does not exist, so those
  calls die `Failed to connect to bus: Connection refused` and their `|| fail` aborted the whole run
  at step 21 (never reaching steps 22-27; live 2026-08-23, `/tmp/setup-1162.attempt2.log`). This is
  the SAME missing-session class steps 17/18 already degrade on (`[ -S /tmp/.X11-unix/X0 ]`). The
  fix mirrors that degrade: a `user_bus_alive()` guard (`[ -S "/run/user/$(id -u "$DESKTOP_USER")/bus" ]`,
  defined next to `UBUS`) gates each step's `systemctl --user` half. On the no-bus path step 21
  DEFERS the `--now` START (it needs X, which the openbox autostart provides on the first kiosk boot)
  but completes the ENABLE BUS-FREE — it creates the units' `*.target.wants` symlinks by hand
  (`ln -sf`, byte-identical to what `systemctl --user enable` writes and to the wants-symlink the
  incumbent already carries), so `verify-imag.sh` check (t) reads is-enabled=enabled after ONE reboot
  with no re-run; step 27 just defers picom's daemon-reload/disable (picom must stay DORMANT, so NO
  wants-symlink is created for it). The naive `loginctl enable-linger` fix alone is NOT enough — it
  brings the bus up mid-run, and a bare `enable --now imag-obs.service` would then start OBS with no
  `:0` (step 15 tore it down) and fight `Restart=on-failure`; deferring the START avoids that.

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

## `printf … | consumer` where the consumer EXITS EARLY is a latent SIGPIPE-under-pipefail footgun — feed from a here-string instead (#1047)

`imag_resolve_ndi_peer` fed `imag_pick_ndi_peer` (returns on the FIRST "up" line — an early-exit
consumer) through `printf '%s' "$probe" | imag_pick_ndi_peer`. The #816 "buffer first" fix removed
the loop-into-an-early-closing-pipe form but LEFT this hazard: a concurrent writer PROCESS feeding
an early-exit consumer through a real pipe. It is safe ONLY while the buffer fits one atomic
`write(2)` into the 64 KiB kernel pipe buffer (7 candidates ≈ 98 B → verified 1300 stress
iterations, 0 fails). The moment `$probe` exceeds ~64 KiB (fleet growth, or the function's own
supported override candidate args), `printf`'s single write BLOCKS with the tail unwritten, the
consumer reads line 1 and closes the read-end, and the blocked write gets EPIPE → SIGPIPE → exit
141 → `pipefail` aborts provisioning. CI run 31757820465 flaked exactly this once at the small size.

- **Fix: `imag_pick_ndi_peer <<<"$probe"`** — a here-string has NO concurrent writer process, so the
  consumer's early exit can never SIGPIPE anything, at ANY buffer size. Structurally immune, minimal.
  This is stronger than the sibling ldconfig-site fix (`ldconfig -p | grep libndi >/dev/null`, which
  makes the CONSUMER drain fully): removing the writer beats forcing the reader to over-read.
- **General rule for these scripts:** any `producer | consumer` under `set -euo pipefail` where the
  consumer can return/exit before draining stdin is a SIGPIPE risk. If the producer's output is
  already in a variable, feed the consumer with `cmd <<<"$var"` (no writer process). Reserve
  `| grep -q`/early-`head` pipes for cases where you genuinely stream and the producer is
  SIGPIPE-tolerant.

### Deterministically testing a SIGPIPE-under-pipefail race against these sourced scripts

The race is only STATISTICAL at the production buffer size (rare timing). To get a DETERMINISTIC
RED→GREEN test, force the buffer OVER the pipe capacity (~64 KiB default) with the first candidate
up: old pipe form → 141 every run, here-string → 0. Two gotchas learned building it:

- **Do NOT pass the huge candidate list as one big argv string** to `bash -c "<harness>"` — a single
  argv/env string over `MAX_ARG_STRLEN` (128 KiB) fails execve with `E2BIG`
  (`ArgumentListTooLong`), which looks like a test bug, not a repro. BUILD the list INSIDE the bash
  harness (`for i in $(seq 1 256); do cands+=("h$i-$filler"); done`) — an in-process function call
  never crosses execve, so no size limit. The per-candidate `ping` exec still gets one ~1 KiB arg,
  well under the limit.
- **256 hosts × ~1 KiB ≈ 262 KiB (~4× the 64 KiB default capacity)** is robust, not brittle: Linux
  default pipe capacity is a stable 65536 B; `/proc/sys/fs/pipe-max-size` only caps unprivileged
  RESIZE, never the default. First-candidate-up is the canonical worst case (reader closes after
  ~15 B while the producer still has 262 KiB queued). ~256 sequential stub-ping forks ≈ 1–2 s.
  See `ndi_peer_resolution_survives_pipefail_with_an_over_pipe_capacity_candidate_buffer`.

## Editing these scripts: the anchor-collision rule applies here too

`tests/setup_imag_guards.rs` pins ~113 literal strings and adjacencies in `setup-imag.sh`. After ANY
edit run the **full** `cargo test` (`# airuleset:build-ok`), not just the file you added — a failure
elsewhere right after touching this script is far more likely a textual collision than a real
regression. Same trap as `scripts/recording-e2e.sh` (see the project CLAUDE.md GOTCHA).

**Adding a new `step N`/bumping `TOTAL_STEPS` touches THREE test files, not one (#791, 2026-08-18).**
`TOTAL_STEPS=<N>` is pinned in `tests/setup_imag_guards.rs` (the `declared == N && declared ==
actual-step-count` invariant) AND independently in `tests/setup_imag_obs_watchdog_764.rs`
(`body.contains("TOTAL_STEPS=25")`) AND `tests/setup_imag_remoteos_mcp_858.rs` (same literal) —
those two hardcode the count to prove THEIR OWN step is still counted. Bumping only the guards test
leaves the other two RED, and because they read the script text they surface only at RUN time (not
`cargo test --no-run`), so a Tier-0 worker sees them only when running each compiled binary. Grep
`grep -rn 'TOTAL_STEPS=' tests/` before finishing and bump every literal in lock-step. #791 added
step 26 (imag-maxperf max-performance persistence) and hit exactly this — two GREEN-phase failures
after the guards test was already updated.

**The `imag-maxperf` trio (`imag-maxperf.service` + `/usr/local/sbin/imag-maxperf.sh` +
`99-imag-maxperf-pm.rules`, issue 756) IS now provisioned by `setup-imag.sh` step 26 (#791).** It
persists the FULL performance profile beyond step 4's governor: EPP=performance, intel_pstate
`no_turbo=0`, `platform_profile=performance`, `powerprofilesctl set performance`, usbcore
autosuspend, all-PCI runtime-PM off, plus the hotplug udev rule so a re-plugged USB/PCI device keeps
runtime-PM off. It was the last "hand-placed hidden by a hand patch" gap on the live box (same class
as imag-obs-start.sh #840 / NVIDIA tuning #841 / remoteos-mcp #858) — the 2026-07-18 EPP-persistence
audit demand. `verify-imag.sh` check (y) gates it (`imag_maxperf_state_ok`, `absent`-tolerant per
#816). The governor is set redundantly with `cpu-performance.service` — that redundancy exists on the
live box and is reproduced for parity, NOT a defect to "fix" (consolidation would be a separate,
box-touching refactor).

**A NEGATIVE anchor (`!body.contains(...)` / "must NOT") is tripped by ADDING a matching string, not
by duplicating one — and a Tier-0 worker who cannot run the full suite MUST grep for it explicitly
(#779, 2026-08-17).** The usual anchor sweep (grep your new literal + the `.find()`/`.split()`
anchors for a POSITIVE collision) does not catch this. Live incident: adding a new provisioning step
that does `cat > /etc/X11/xorg.conf.d/30-touchpad-tap.conf` (the #779 touchpad-input config) flipped
`setup_imag_does_not_ship_the_dead_tearfree_option_841` in `tests/harness_imag_intel_display_841.rs`
to FAIL — it asserts `!body.contains("cat > /etc/X11/xorg.conf.d/")`, an over-broad ban of ANY
xorg.conf.d WRITE whose real intent is only the dead `Option "TearFree"` DISPLAY snippet. It is
**invisible to `cargo test --no-run`** (which only compiles; a `!contains` assertion fails at RUN
time), so it surfaces only on CI's real `cargo test`. **Before adding ANY new `cat > <path>` /
`install <path>` / new content block to `setup-imag.sh` or `verify-imag.sh`, ALSO grep the tests for
a NEGATIVE assertion your addition would newly match** — e.g.
`grep -rn 'contains\|must NOT\|assert!(!' tests/ | grep -iE '<the path or token you are adding>'`.
A `!body.contains("<prefix your new write matches>")` is a coupled fixture: narrow it to its true
intent (never weaken it) in the SAME PR, exactly like the `TOTAL_STEPS` bumps — the #779 fix scoped
the ban to "any xorg.conf.d write OTHER than the touchpad input config" so a real display cargo-cult
still trips it.

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

A DIFFERENT, older OBS version (30.0.2 stock vs the rig's 32.2.0 genlock build) is fine for this --
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

## OBS-log grep misses are locale + input-source + byte-position sensitive — `LC_ALL=C grep -a` via a here-string (#1183/#1184)

An OBS-log matcher (`verify-imag.sh` (h), `setup-imag.sh` step-18, `drift-guard.sh`'s
`genlock_*_from_log` family) that greps for an ASCII marker can MISS a marker that IS present, for
two compounding reasons — fix BOTH:

- **Locale + input source:** OBS/DistroAV logs carry raw invalid-UTF-8 bytes (mojibake). GNU grep
  in a UTF-8 locale (dev1, imag) then fails to match an ASCII pattern on a line that contains an
  invalid byte sequence — even a byte at the LINE START or END, not just inside the matched span
  (confirmed #1184: a line-start `\xe2\x82` suppressed the fixed-literal rt-pin match too). A
  remote grep run over ssh in the box's C locale never sees this, which is why drift-guard's
  ssh-SIDE greps pass while its LOCAL `genlock_*_from_log` (grepping remote-fetched text on dev1)
  missed. **Fix: `LC_ALL=C grep -a`** (byte-literal, single-byte locale). A `sed`/`awk` DOWNSTREAM
  of the grep needs `LC_ALL=C` too — grep -a passes the raw bytes through and the next stage chokes
  on them in a UTF-8 locale (the #1184 latency extractor returned a mangled line until its `sed`
  also got `LC_ALL=C`).
- **Byte position / SIGPIPE (the #1047 residual, same story):** a matcher fed `printf … | grep -q`
  SIGPIPEs the writer when grep -q exits early on a match in a >64 KiB log (live OBS logs are
  173 KB–40 MB; markers are startup lines at the TOP) → rc=141 → pipefail false-FAILs a healthy
  box. **Fix: a here-string (`grep -a … <<<"$1"`)** — no writer process, SIGPIPE-immune at any
  size. The SAME here-string remedy the #1047 `imag_pick_ndi_peer` section above documents.

Test both deterministically with a >64 KiB body carrying the marker at the TOP + invalid bytes on
the marker lines (see `tests/verify_imag_pure_functions.rs` + `tests/drift_guard.rs`).

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
`scripts/imag-gpu-contention-sampler.sh` (#674 — since RETIRED via #846), with the identical unconditional
`command -v nvidia-smi || FATAL` shape. It was NOT wired into any automated gate (a standalone
manual diagnostic, zero callers anywhere else in the repo), so it was out of #845's own scope —
filed as its own follow-up issue (#846), which RESOLVED it by RETIREMENT (the tool was a one-shot
diagnostic for a hypothesis already disproven live, sampling NVENC/dGPU-VRAM state that has no
iGPU equivalent; the real cause of imag render degradation was later found + is continuously
monitored — #1040 power-envelope guard). So a repo-wide `nvidia` sweep after a fix like this may
find a sibling that is best DELETED rather than ported — decide from what the script is FOR. **#847's own sweep found ONE more**:
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

## New `verify-imag.sh` checks MUST run BEFORE any check that restarts/replaces OBS (#884)

`verify-imag.sh`'s check (o) (#840) RESTARTS OBS, which REPLACES the tracked obs process with a
fresh one. **Any check added AFTER check (o) in the file observes that post-restart process, not
the one the box booted with.** Confirmed live (#884, 2026-07-30): the #884 supervision checks
(unit enabled+active, `Restart=` value, autostart wiring, core-dump enablement) were first appended
at the very end of the file (after check (r)) and ALL four false-FAILED on an otherwise perfectly
healthy, correctly-provisioned box for exactly this reason. Fixed by moving the whole block to run
immediately BEFORE check (o) — pinned by `verify_imag_reads_884_service_state_before_the_840_restart_wipes_it`
in `tests/verify_imag_pure_functions.rs` (and the sibling #1015/#1040 ordering tests). **The general
rule: before appending a new acceptance check to this file, check whether check (o)'s restart call
sits ABOVE your intended insertion point — if so, your check reads post-restart state, a DIFFERENT
process than the box's normal boot-time state.** (Pre-#890 the restart was a DIRECT
`imag-obs-stop.sh && imag-obs-start.sh` ssh call, which additionally left the post-restart obs
UNTRACKED — outside the unit's cgroup, `Max core file size = 0`; #890 changed it to a bounded
`systemctl --user restart imag-obs.service`, so the new obs is now supervised, but the
reads-before-restart ordering rule is unchanged.)

## `verify-imag.sh` used to HANG FOREVER on every run — FIXED by #890 (bounded service restart)

**RESOLVED (#890, commits `eb61d544e` RED / `bf11ee2e5` GREEN / `6796e14d4` review-fix).** For the
record: check (o)'s persistence-proof used to restart OBS with a DIRECT ssh call
`ssh_box "/usr/local/bin/imag-obs-stop.sh && /usr/local/bin/imag-obs-start.sh"`. Since #882
(`83900d990`) `imag-obs-start.sh` ends in `wait "$OBS_PID"; exit "$OBS_EXIT"` — correct for the
Type=simple `imag-obs.service` (systemd needs obs to be the tracked main process), but when the
SAME script is invoked DIRECTLY over ssh (and `ssh_box` bounds only the SSH *connect* phase, never
remote *runtime*) that blocking `wait` never returns, so the whole gate hung indefinitely and never
reached ALL CLEAR / VERIFY FAILED.

**The fix + the reusable lesson (a REPEATABLE pattern for any verify/probe script):**

- **A verify/probe CHECK must be BOUNDED — never a blocking process supervisor.** #890's engineering
  fork (Scope-gate `needs-user-decision`) resolved to the obvious default: a gate CHECK gets a hard
  timeout + fail-loud, never an indefinite wait. Restart via the SERVICE
  (`imag_obs_service_restart_cmd` → `systemctl --user restart imag-obs.service`, `XDG_RUNTIME_DIR`
  exported for the non-graphical ssh `--user` bus), NOT a direct `imag-obs-start.sh` call: systemd
  (Type=simple) OWNS the blocking wait, so the ssh restart returns promptly AND the new obs stays
  supervised. Since #884 the box's OWN boot path IS the unit, so the service restart is also the
  operator-faithful "real restart" the persistence-proof wants.
- **`ssh_box` bounds only CONNECT, not remote command RUNTIME.** A remote command that blocks after
  connect (a `wait`, a wedged-X `wmctrl -l`, a stuck `journalctl`) hangs `ssh_box` forever. #890
  added `ssh_box_timeout SECONDS CMD` (a `timeout`-wrapped `ssh_box`); a 124 timeout is a loud FAIL,
  never a hang. Check (o)'s restart AND its before/after wmctrl reads now all go through it, with a
  bounded poll (`IMAG_OBS_PROJECTOR_POLL_S`, default 120) waiting for the projectors to reappear.
  The wider "bound EVERY ssh read in the file" hardening (needs per-call budgets, not a blanket
  10s wrap that would false-FAIL slow-but-healthy dantesync/apt reads) is filed as #1058.
- **When you make a script's tail systemd-blocking (`wait $PID`), audit every OTHER caller of it.**
  #882 made `imag-obs-start.sh` correct for the unit's `ExecStart=` but broke the pre-existing #840
  caller in verify-imag.sh that invoked it directly — a "correct for one caller, fatal for another"
  interaction. Grep for direct callers before adding a blocking tail.

## A reading taken RIGHT AFTER rapid consecutive OBS restarts can be transiently wrong — reread once settled before trusting it or filing it

Two DIFFERENT checks (OBS thread-core-concentration #842/#784, and the scenes/Multiview
`imag_scenes.py` bare-mode output parse) both showed a FAIL during #884's live verification, on a
box that had just been restarted several times in quick succession (by hand, chasing the #890 hang
above). Both checks came back clean moments later on the SAME box once it had settled for a couple
of minutes with no further restarts — `ps -L -o psr= -C obs` redistributed from a transient pileup
to a healthy spread, and a fresh `imag_scenes.py --host` call printed the expected clean 4-line
output that the check's own `^scenes: N/N OK` / `^MV scenes: N/N ... OK` line-anchored greps
correctly matched. **Before filing a live acceptance-check failure as a genuine regression, take a
SECOND reading after the box has been left alone for a minute or two** — a reading captured
mid-flight of your OWN repeated manual restarts is not evidence of steady-state behavior, and
filing it as a bug wastes a future session re-diagnosing noise.


## #858 — the RemoteOS MCP control-channel agent is now PROVISIONED by setup-imag.sh (step 23)

A fresh imag box used to come up with no `linux-imag-nb` MCP surface — the agent
(`remoteos-mcp.service` on :8092) survived only as a hand-install on the one original box. Since
#858, `setup-imag.sh` **step 23** provisions it by INVOKING the canonical installer of the SEPARATE
`zbynekdrlik/remoteos-mcp` project (`install-linux.sh`, fetched from its `master` raw URL; override
`REMOTEOS_MCP_INSTALLER_URL`). camera-box does NOT re-implement or re-pin the agent — the ops-skill
#555 discipline (use the installer, never a bare pip command; the ~40 transitive deps stay pinned in
remoteos-mcp's own `pyproject.toml`). The `!body.contains("remoteos-mcp.git")` guard in
`tests/setup_imag_remoteos_mcp_858.rs` enforces "no inline pip-git URL here".

Auth-key (a full-shell-RCE bearer token on `0.0.0.0:8092`) is NEVER committed: set
`REMOTEOS_MCP_AUTH_KEY` (env-secret convention, exactly like `CAM_PW`/`GH_TOKEN`) and step 23
pre-seeds `/etc/remoteos-mcp/config.json` (chmod 600, `install -d -m 700` + `umask 077`) BEFORE
running the installer, so the installer REUSES that known key and dev1's gitignored `.mcp.json`
keeps matching a freshly-hardware'd box (a fresh random key would leave the MCP surface dead at
dev1's end — the gap #858 closes). Unset → the installer generates a fresh on-box key and you must
update dev1's `.mcp.json` `linux-imag-nb` entry. The key is charset-guarded (`case … *[!A-Za-z0-9]*
) fail`) before the unquoted-heredoc JSON write, so a special char can never break the JSON (which
would make the installer silently discard the pre-seed and generate a DIFFERENT key while the
`systemctl is-active` gate still passes) or run command substitution. cam1-4 remain hand-installed
until `setup-device.sh` gains the same step (a candidate follow-up).

## Projector openers must be COUNT-FIRST — the SEEDER dedups, not just the gate (#769)

`OpenVideoMixProjector` (obs-websocket) ALWAYS opens a NEW window — the protocol has no "is a
projector open" query. OBS's own `CloseExistingProjectors=true` replace-loop (seeded by
`setup-imag.sh`, #756) only closes projectors whose INTERNAL `GetMonitor() == the target monitor`,
so a launch-restore stray (recreated WINDOWED, internal monitor = -1) is invisible to it. So a BLIND
opener stacks one more window every call while any stray survives → the live "3× Multiview, gate
refuse" incident (2026-07-15).

- **`imag_scenes.py::projector(obs, host)` is count-first (#769):** after opening each kind it
  enumerates `Projector - <kind>` windows with `wmctrl -l` and closes the OLDER strays, KEEPING THE
  NEWEST (highest numeric X window id == the one it just opened on the correct monitor). This runs on
  every boot (`imag-obs-start.sh`) and every watchdog tier-a relaunch (`imag-obs-watchdog.py`), which
  both call `--host 127.0.0.1` — so the stack never forms on the LIVE box between gate runs, and the
  watchdog inherits the fix for free (it calls the same seed).
- **KEEP NEWEST, never keep-oldest + re-fullscreen** (the ticket's original prescription was WRONG):
  `wmctrl -b add,fullscreen` changes the X window state, NOT OBS's internal monitor index, so the
  replace loop still can't see a windowed stray — see `scripts/lib/imag-projector-heal.sh`'s own
  header. The fresh open always has the highest id, so keep-newest keeps the correctly-placed window.
- **wmctrl local vs remote, mirroring `_lspci_query_local`/`_lspci_query_remote` + `_is_local_host`:**
  local subprocess for the loopback boot/watchdog path, `sshpass` (`IMAG_USER`/`IMAG_PW`, via the
  shared `_ssh_base()`) for a dev1-manual `--host <ip>`. A missing OR failing (`rc != 0`) wmctrl warns
  LOUD BY NAME and SKIPS dedup — NEVER read as "0 windows" (imag-ssh-remote-tool-preflight), NEVER
  raises (it runs under `imag-obs-start.sh`'s `set -euo pipefail`, same discipline as
  `clear_measurement_burns`, imag-obs-supervision.md).
- **The GATE keeps its OWN post-hoc heal:** `recording-e2e.sh` `[0/8]` still opens via
  `obs_phase2.py open-projectors` (blind) then sources `imag-projector-heal.sh` (same keep-newest) +
  the hard 1+1 count check. `verify-imag.sh` check (o) is count-ONLY (never opens, #840); its
  restart-repopulate step now flows through the idempotent seeder. So no path stacks windows.
- The pure decision (`projector_window_ids` / `projector_strays_to_close`) is extracted for an offline
  `wmctrl`-fixture pytest (`tests/python/test_imag_scenes_projector_idempotent_769.py`).
## Stopping the SUPERVISED OBS gracefully FROM setup-imag.sh's ROOT context (step 12, #785)

`imag-obs-supervision.md` already states the general rule: a deliberate stop of a supervised OBS
MUST go through `systemctl --user stop imag-obs.service` (a raw `pkill`/`kill` of the tracked
process looks like a crash and refights `Restart=on-failure`). The setup-imag.sh-specific wrinkle
is that **step 12 (the genlock hot-swap) runs as ROOT, and `systemctl --user` from root talks to
root's own (nonexistent) `newlevel` user bus — `systemctl --user is-active` from root returns
FALSE even when the unit is genuinely active on the desktop user's session.** So step 12 cannot
call `imag-obs-stop.sh` directly (from root it would misjudge the unit inactive and fall through to
the raw signal ladder, refighting Restart). The correct graceful-first ladder from root:

1. `sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR=/run/user/$(id -u "$DESKTOP_USER") DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/<uid>/bus systemctl --user is-active --quiet imag-obs.service` → if active, `... systemctl --user stop imag-obs.service` (same env). This is the ONE path that lets systemd own the stop and suppress Restart.
2. else `sudo -u "$DESKTOP_USER" DISPLAY=:0 XDG_RUNTIME_DIR=... DBUS_SESSION_BUS_ADDRESS=... /usr/local/bin/imag-obs-stop.sh` (the installed helper — its wmctrl-c→SIGTERM ladder saves the collection).
3. else inline `pkill -TERM -x obs` (OBS saves on its own signal handler).
4. bounded wait (`for _ in $(seq 1 25); do ... sleep 1`), and ONLY THEN `pkill -9 -x obs` as the last resort on a wedged process, keeping the `would not die after SIGKILL` fail-loud.

The mirror of the same `sudo -u … systemctl --user …` invoke already exists as the `u_systemctl`
helper elsewhere in setup-imag.sh — reuse that env shape, don't reinvent it.

## Anchor trick: adding a SECOND wait-for-death loop near the swap-kill loop (#785, #784/#842 anchor)

The existing test `setup_imag_swap_kill_uses_sigkill_and_waits_for_death` (tests/setup_imag_guards.rs)
anchors on `body.find("pkill -9 -x obs")` < `body.find("pgrep -x obs >/dev/null 2>&1 || break")` <
`"would not die after SIGKILL"`. If you add a NEW wait loop (e.g. a graceful-stop wait BEFORE the
SIGKILL last resort) using the SAME `pgrep -x obs >/dev/null 2>&1 || break` literal, that test's
middle `.find()` latches onto YOUR new loop instead of the SIGKILL one and the ordering assertion
breaks. **Write the new wait loop in a DIFFERENT form** — `if ! pgrep -x obs >/dev/null 2>&1; then
break; fi` — so the `... || break` literal stays unique to the SIGKILL loop. Keep the SIGKILL
last-resort block's `pkill -9 -x obs` → `pgrep … || break` → `would not die after SIGKILL` bytes
exactly as they were.

## `cat > "$USER_HOME/.config/openbox/menu.xml"` is NOT caught by the #841 xorg.conf.d ban (#785)

Adding a `cat > …/menu.xml` (or any new `cat > …` heredoc write) to setup-imag.sh is safe against
`setup_imag_does_not_ship_the_dead_tearfree_option_841` — that negative anchor filters lines that
`starts_with("cat > /etc/X11/xorg.conf.d/")` specifically, so an openbox-config write under
`$USER_HOME` never matches it. Still run the full NEGATIVE-anchor grep (`grep -rniE 'assert!\(!|
must NOT|!.*contains' tests/`) for any OTHER token your addition introduces — the openbox menu adds
`systemctl reboot`/`systemctl poweroff`, and the only bans on those live in
`tests/imag_obs_watchdog_unit_778.rs`, which reads the `systemd/imag-obs-watchdog.service` FILE, not
setup-imag.sh, so a menu.xml poweroff/reboot entry never trips them.

## The #785 menu is only REACHABLE if rc.xml binds the desktop right-click to it — verify ASSERTS it, never rewrites operator rc.xml (#1095)

`verify-imag.sh` asserts BOTH that `~/.config/openbox/menu.xml` is present (#791) AND — since
#1095 — that the openbox rc.xml actually BINDS the desktop right-click to it
(`imag_openbox_root_menu_bound`). The #785 `menu.xml` (`<menu id="root-menu">`) is only reachable
because the rc.xml's Root mouse-context Right-button binds `ShowMenu root-menu`; a stale hand-placed
`~/.config/openbox/rc.xml` (the "hand-placed, not provisioned" class #785 exists to close) could
bind the desktop click elsewhere and silently orphan the menu, with no gate catching it.

- **Design (b) — ASSERT-ONLY, never provision/rewrite rc.xml.** Provisioning a full stock rc.xml
  (option a) or patching just the binding into it (option c) would clobber an operator's hand-tuned
  openbox config (keybindings, etc.) — the same operator-state concern #785 is about. The gate
  fails loud and names the offending file; the operator fixes it by hand. This is the
  `minimal-fix-inform-dont-force` model — the right default for ANY "the box has an operator-owned
  config file" reachability check here (weigh it before reaching for a provision/overwrite fix).
- **Read the EFFECTIVE rc.xml, not a fixed path.** openbox loads `~/.config/openbox/rc.xml` when
  present, else the stock `/etc/xdg/openbox/rc.xml`. The check does a remote
  `[ -f <user> ] && echo user || echo stock` to pick whichever openbox will ACTUALLY load, asserts
  on THAT file, and names it in the failure. A fresh box (no user rc.xml) passes on the stock
  default; only a stale USER rc.xml binding elsewhere fails — which is exactly the target case.
- **`grep -P` is fine in a verify-imag.sh helper — it runs LOCALLY on dev1, not on the box.** The
  helper flattens the ssh-read rc.xml text and uses `grep -oP`/`-qP` (PCRE2) to scope the match to
  the `<context name="Root">` block (so a `root-menu` named only in a keybind, or a Root right-click
  bound to a different menu, does NOT falsely pass; `[\x22\x27]` tolerates both attribute quote
  styles). GOTCHA when TESTING such a helper: on dev1 the interactive `grep` is a **ugrep wrapper
  FUNCTION** (Claude Code shell integration), but `/usr/bin/grep` is GNU grep 3.11 and CI runs GNU
  grep — verify your exact patterns under `/usr/bin/grep` (GNU), not only the interactive wrapper.
  Both provide -P/PCRE2 and agreed here, but the wrapper is not what the deployed script or CI runs.
