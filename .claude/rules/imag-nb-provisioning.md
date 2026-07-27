---
paths:
  - "scripts/install-imag-nb.sh"
  - "scripts/setup-imag.sh"
  - "tests/install_imag_nb_pure_functions.rs"
  - "tests/setup_imag_hardware_agnostic.rs"
  - "tests/setup_imag_guards.rs"
  - "tests/setup_imag_pure_functions.rs"
---

# Replacing the imag notebook — install the OS, then provision it (#791 / #815 / #816)

Two scripts, in this order. Neither is manual work; a notebook swap is repo tooling.

```
1. INSTALL OS   scripts/install-imag-nb.sh --target-disk /dev/nvme0n1 --ip <addr> --yes
                (run FROM the box's own Ubuntu desktop live-USB, as root)
2. REBOOT       into the installed system (NVRAM entry is written + set first)
3. PROVISION    IMAG_IP=<addr> sudo -E ./setup-imag.sh --yes        (on the box)
4. SCENES       scripts/imag_scenes.py --host <addr>                (from dev1)
```

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
  `imag_require_tools`, which names the missing tool.
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
