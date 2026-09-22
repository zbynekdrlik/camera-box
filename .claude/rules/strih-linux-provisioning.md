---
paths:
  - "scripts/setup-strih.sh"
  - "scripts/verify-strih.sh"
  - "scripts/lib/strih-provision.sh"
  - "systemd/strih-obs.service"
  - "systemd/strih-bundle-state-server.service"
  - "tests/strih_provision_pure_functions.rs"
---

# strih-lx — the Linux notebook replacing the Windows strih PC (issue 1317)

The strih cutter/mix role is migrating from the Windows STRIH-SNV PC to a Linux notebook
(`strih-lx`). The owner ruling (16.9.2026): the notebook **runs IN PARALLEL** with the Windows PC
until everything is tuned on it — the Windows PC stays the production cutter meanwhile. This rule
covers the provisioning scaffolding built during that preparation.

## The parallel-run contract (why the namespacing + the client-clock matter)

While both boxes run, TWO strih senders coexist on the NDI wire and must never collide:

- **strih-lx's NDI OUTPUTS are namespaced `STRIH-LX (...)`** — `STRIH-LX (2ME PGM)` /
  `STRIH-LX (2ME PVW)` + the `STRIH-LX (interkom|MULTIVIEW|Grading)` republishes. The stream box +
  the receivers must **never** see a second `STRIH-SNV (...)` sender. `verify-strih.sh` fail-closes
  on any live output that is a `STRIH-SNV (...)` name (`strih_lx_no_second_strihsnv_sender`).
- **strih-lx joins the cluster clock as a dantesync CLIENT** (`--ntp-server strih.lan`), never a
  server/master — the Windows PC keeps the single NTP-master role. `setup-strih.sh` self-checks the
  invocation with `strih_lx_dantesync_is_client_not_master` (fail-closed: an empty/ambiguous mode
  refuses) so a mis-provision can never spawn a second master.

## Ubuntu 26.04 (owner ROZHODNUTÉ 18.9.2026)

The strih-lx notebook runs **Ubuntu 26.04 LTS (resolute)**, NOT 24.04 (owner: „chcem prejst na 26
to je nova lts verzia … mal by byt este lepsi support"). This is a deliberate divergence from
imag-nb (retired to the owner, noble) — strih-lx is the only Linux OBS box, so its bundle builds on
a different image than imag's.

Box facts (live on 10.77.9.187, 26.04.1 LTS `resolute`, i5-13450HX / RTX 5050 Mobile GB207M):

- `linux-generic-hwe-26.04` (7.0.0-31) EXISTS on 26.04, so the derived HWE kernel name works
  directly; `linux-generic` is the GA fallback.
- `nvidia-driver-595-open` (595.91.07-0ubuntu0.26.04.1) is recommended for the RTX 5050 — the SAME
  driver `setup-imag.sh` step 9 installs (the proprietary 595 does not init Blackwell).
- Runtime sonames differ from noble: `libavcodec62` (ffmpeg 8.0.1), `libqt6core6t64` 6.10.2 —
  noble's `libavcodec60` / `libqt6core6` do NOT exist there, so a **24.04-built bundle cannot load**.
  `libgles2-mesa-dev` / `libpipewire-0.3-dev` / `qt6-base-dev` exist under the same names.

What this decision changed (issue 1317, owner 18.9.):

1. **CI: the strih variant builds on the `ubuntu-26.04` hosted runner.** `linux-genlock.yml` job
   `linux-genlock-build-strih` is `runs-on: ubuntu-26.04` (ONLY that job — the imag-parity
   `linux-genlock-build` full build and `linux-distroav-compile-check` stay on `ubuntu-24.04`). The
   apt dependency list stays the shared `OBS_APT_PACKAGES` (the names above exist on 26.04); if a
   package name ever differs on resolute the job fails loud and the fix is a strih-scoped apt
   override, never dropping a dependency. The CEF pin is unchanged (CEF 6533 is glibc-portable).
2. **The release-parity marker + gate.** The Stage step writes `TARGET-RELEASE: ubuntu-26.04` into
   `STRIH_BUILD_FLAGS.txt`, single-sourced from the job env `STRIH_TARGET_RELEASE: 'ubuntu-26.04'`
   (`runs-on` cannot read job env, so `tests/linux_genlock_workflow_gate.rs` pins the two literals
   EQUAL). The pure predicate `strih_lx_release_parity_ok BUNDLE_FLAGS_TEXT BOX_VERSION_ID`
   (`scripts/lib/strih-provision.sh`) → 0 iff the flags text carries `TARGET-RELEASE:
   ubuntu-<BOX_VERSION_ID>`; a mismatch OR an absent marker OR an empty VERSION_ID → 1 (fail-closed).
   `setup-strih.sh` step 4 gates the STAGED bundle's marker vs the box `/etc/os-release` VERSION_ID
   BEFORE the `cp -a` install (`fail` on mismatch — never install a bundle built for another
   release), and `verify-strih.sh` item 15 asserts the SAME on the installed bundle. So a
   24.04-built bundle can never silently run on the 26.04 box (it would crash OBS at load).
3. **The installer kernel is release-derived.** `scripts/install-imag-nb.sh` gains a pure
   `imag_kernel_meta_package VERSION_ID AVAILABLE` (`linux-generic-hwe-<VERSION_ID>` when that exact
   name is in the target's `apt-cache pkgnames linux-generic` list, else `linux-generic`; empty
   VERSION_ID → non-zero). The chroot step reads `VERSION_ID` from the target's `/etc/os-release`
   and the available list inside the chroot (after its apt-get update), then installs the derived
   meta — so the ONE installer serves both a 24.04 and a 26.04 live stick, never a hardcoded noble
   literal. The `ls /boot/vmlinuz-*` fail-loud checks are unchanged.

Static IP **10.77.9.203** (free; .202 strih, .204 stream); `strih-lx.lan` DNS is a MikroTik static
entry = owner step (the router is read-only for me), scripts dial the IP / hostname.

## Module structure (the source-of-truth split)

- **`scripts/lib/strih-provision.sh`** — the ONE place the strih-lx role FACTS + pure decisions live
  (the `obs-fleet.sh` / `camera-set.sh` source-only convention: `# airuleset:script-ok`, no
  top-level statements, no `set -euo pipefail` leak). Sourced by `setup-strih.sh`, `verify-strih.sh`
  AND `tests/strih_provision_pure_functions.rs` (rustc-free Tier-0). Pure fns: the 10 NDI inputs,
  the namespaced outputs/republishes, the floor-3 latency, the strih CI artifact name, the
  dantesync-client args + client-not-master check, the profile facts, the MiniFuse-4/PipeWire audio
  route + fail-loud TODO predicate, the output-name + no-2nd-STRIH-SNV guards, and the verify
  predicates (render-tick / distroav-loaded / nvenc-available / single-timesync-authority).
- **`scripts/setup-strih.sh`** — numbered, enable-only, fail-loud, idempotent provisioning flow
  (13 steps) run ON the box as root; models `setup-imag.sh`. BASH_SOURCE-guarded so the tests can
  source it. Reuses `genlock_write_markers` (never re-implemented) and the canonical remoteos-mcp /
  bundle-state tooling.
- **`scripts/verify-strih.sh`** — the acceptance gate; feeds live reads into the lib predicates and
  exits non-zero on any failed item. Latency pins are REPORT-ONLY vs
  `scripts/latency-pins-baseline.json`'s `strih-lx` key (`_all_camera_ndi_inputs_ms` = 3).

## Audio — the one BLOCKER, fail-loud until wired

There is NO Dante on the strih PC (owner 15.9.): program audio is `MiniFuse 4` USB → VB-Matrix
(VASIO-8) ASIO on Windows. On Linux the class-compliant MiniFuse 4 is a native PipeWire node and
**PipeWire replaces VB-Matrix**. The graph that routes the mastered program mix into the MiniFuse 4
capture is not yet wired — `setup-strih.sh` step 12 **FAILS LOUD** (`strih_lx_audio_route_wired`)
until an operator wires it and re-runs with `STRIH_LX_AUDIO_WIRED=1`. Arena stays on the Windows PC
(Spout has no Linux); its cg feed reaches the notebook over NDI (`RESOLUME-SNV (cg-obs)`).

## CI — the strih FULL-build variant, with obs-browser + CEF (issue 1317)

`linux-genlock.yml` has a `linux-genlock-build-strih` job (artifact
`obs-genlock-linux-x86_64-strih`) copying the imag-parity full build byte-for-byte. The full build
already ships obs-websocket / obs-ffmpeg+NVENC / x264 / outputs / filters / v4l2 / DistroAV (the
ubuntu-ci preset defaults). The strih role additionally needs **obs-browser** (the 4 browser
sources: VDO.ninja interkom, Ableset, camera crew, 1pixel), which needs **CEF**.

**CEF is now WIRED** (issue 1317). The vendored ubuntu-ci preset does NOT auto-fetch CEF on Linux
(no `cmake/linux/buildspec.cmake` — only macos/windows call `_check_dependencies`), so the strih job
fetches it the same way upstream OBS's own Linux CI does:

- **The pin.** `STRIH_ENABLE_BROWSER: 'ON'` plus a CEF pin block in the job `env`: `CEF_VERSION`,
  `CEF_ARCHIVE`, `CEF_SHA256`, `CEF_BASE_URL`. These MUST match
  `vendor/obs-studio/CMakePresets.json` → `configurePresets[dependencies].vendor."obsproject.com/obs-studio".dependencies.cef`
  (the `ubuntu-x86_64` hash). For OBS 32.2.0 that is version `6533`, archive
  `cef_binary_6533_linux_x86_64_v6.tar.xz`, sha256 `7963335519a19ccdc5233f7334c5ab023026e2f3e9a0cc417007c09d86608146`,
  base `https://cdn-fastly.obsproject.com/downloads`. The `revision` (6) is the `_v6` archive
  suffix. `tests/python/test_linux_genlock_strih_cef_1317.py::test_workflow_cef_hash_matches_vendored_pin`
  guards against the workflow pin drifting from the vendored one.
- **The fetch step** (`if: env.STRIH_ENABLE_BROWSER == 'ON'`): download → `sha256sum -c` against the
  pin → `tar -xf` → locate the extracted `cef_binary_*` top dir by NAME (`find … -name 'cef_binary_*'`,
  never guessing the `_v<rev>` suffix) → export its ABSOLUTE path as `CEF_ROOT_DIR` to `$GITHUB_ENV`.
  The OBS configure passes `-DCEF_ROOT_DIR=${{ env.CEF_ROOT_DIR }}`.
- **Cache.** `actions/cache` keyed `cef-${CEF_VERSION}-${CEF_SHA256}`, so the ~325 MB download is
  paid once and a version bump busts it automatically.
- **Marker.** `STRIH_BUILD_FLAGS.txt` is env-conditional: `BROWSER-ON: obs-browser + CEF <version>`
  when ON, the original `BROWSER-OFF` text when OFF.
- **Verify.** `verify-strih.sh` reads the installed `/opt/obs-genlock/STRIH_BUILD_FLAGS.txt`; when it
  says `BROWSER-ON` it fails loud unless `obs-browser.so` AND the CEF runtime `libcef.so` are present
  under the bundle root (pure predicates `strih_lx_browser_bundle_required` /
  `strih_lx_browser_bundle_ok` in `scripts/lib/strih-provision.sh`).

**To flip browser OFF again:** set `STRIH_ENABLE_BROWSER: 'OFF'` in the job `env` — the ONLY change
needed (the CEF fetch step, the `CEF_ROOT_DIR` arg, the marker, and the verify item all follow it).

## chrome-sandbox setuid-root — the CEF sandbox helper (issue 1317 F6, DONE)

CEF ships a **SUID sandbox helper** `chrome-sandbox` alongside `obs-browser.so`/`libcef.so` (in the
strih bundle: `lib/x86_64-linux-gnu/obs-plugins/chrome-sandbox`). In the CI artifact it is mode `700`
owned by the runner uid; Chromium's SUID-sandbox contract requires it **owned root:root, mode 4755
(setuid root)** or the sandbox helper aborts at browser-source launch (`Running without the SUID
sandbox!` → the render process dies). `verify-strih.sh` item 13 only checks the *presence* of
`obs-browser.so`/`libcef.so`, not the sandbox's launchability — F6 closes that gap.

- **The rejected shortcut.** Starting OBS with the sandbox disabled at launch (`--no-sandbox`) would
  make the sources start with no chmod, but it disables the Chromium sandbox for EVERY browser source
  for the whole session — a session-wide security downgrade of all 4 web-rendering sources. The
  setuid helper is Chromium's sanctioned shape and is applied once at provisioning.
- **Pure helpers** (`scripts/lib/strih-provision.sh`): `strih_lx_chrome_sandbox_fix_cmd <bundle-root>`
  emits the idempotent `chown root:root` + `chmod 4755` statements (locates the helper by NAME under
  the install root, every statement `;`-terminated per the `v4l2-neutral.sh` `_cmd`-helper gotcha,
  runs as an `if` so a missing binary is a no-op / set-e safe). `strih_lx_chrome_sandbox_verdict
  <owner> <mode> <present>` → `ok` / `missing` / `wrong-owner` / `wrong-mode`.
- **`setup-strih.sh` step 4** (right after the bundle install, as root): when the installed
  `STRIH_BUILD_FLAGS.txt` says `BROWSER-ON`, it FAILS LOUD if `chrome-sandbox` is absent, applies the
  fix, then re-reads owner+mode through the verdict — so a chown/chmod that failed to record
  `root:root` mode `4755` in the inode (e.g. a `cp -a` that preserved the runner uid and a chown that
  did not run, or a fix that errored) is caught by name. (It does NOT detect a `nosuid` mount — that
  is an execve-time property, invisible to `stat`; the S_ISUID bit is still stored and read back.)
  A `BROWSER-OFF`/absent marker is a loud SKIP.
- **`verify-strih.sh` item 14** (read-only, on the live box): `stat -c '%U:%G'` / `-c '%a'` the same
  `find`-path and PASS only on the `ok` verdict; `BROWSER-OFF`/absent NOTE-skips. On a live box the
  PASS line reads `chrome-sandbox setuid-root (root:root 4755) -- CEF sandbox launchable`; a regressed
  helper prints e.g. `chrome-sandbox not setuid-root (wrong-owner: owner=newlevel:newlevel mode=700)`.

## NIC xhci IRQ placement — the NET_RX softirq off the OBS cores (issue 1317 item H, DONE)

The strih-lx notebook's NIC is a **USB 2.5GbE adapter** (RTL8156B), so its interrupt is an
**xhci host-controller** IRQ, not a native PCIe NIC IRQ. With `irqbalance` NOT installed the kernel
parks that IRQ on ONE core; on the live box (issue 1354, 22.9.2026) IRQ 125 (`xhci_hcd`, PCI
function `0000:00:14.0`, iface `enx6c1ff766154b` carrying 10.77.9.202) was serviced only by CPU 6,
where **~1.1 Gb/s of NDI NET_RX softirq** (28 % of the core) collided with OBS's
`ndir:video`/`libobs` threads (unpinned across all 16 cpus, so they land on cpu6 routinely).

**The gotcha — a NIC IRQ sharing an OBS core is receive-side jitter that folds straight into the
genlock ladder.** Moving IRQ 125 to CPU 15 (the last `cpu_atom` E-core) — `echo 8000 >
/proc/irq/125/smp_affinity_list` — cut `genlock #797 slow output_video` from **~35/min to 5–10/min**
and the idle cam6/cam7 HOLD rate from 1–2/min to ~0.15/min. The softirq that decodes incoming NDI
frames must live on a core OBS never renders on; the reverse (pinning OBS away from the NIC core)
was measured INERT (`taskset 0-11` on OBS changed nothing while the IRQ move did), because the
kernel scheduler still floats OBS threads onto whichever core the softirq is starving.

**A live `smp_affinity` write does NOT survive a reboot / re-flash** — so it is baked into
provisioning, fact-resolved (never a hard-coded IRQ number, which would break after a kernel/
firmware change or a different USB port):

- **`scripts/lib/strih-provision.sh`** pure resolvers, all Tier-0 testable over `/proc`-shaped
  fixtures (`SYS_ROOT` / `PROC_INTERRUPTS` / `CPU_ATOM_FILE` seams): `strih_nic_xhci_pci_function`
  (walk `readlink -f /sys/class/net/<iface>/device` up to the `usbN` root → the parent PCI
  function), `strih_nic_xhci_irqs` (the `/proc/interrupts` rows naming BOTH `xhci_hcd` AND that PCI
  function — the `IR-PCI-MSI-<pcifn>` chip column, which disambiguates a box with several xhci
  controllers), `strih_nic_irq_target_cpu` (the LAST cpu in `/sys/devices/cpu_atom/cpus`, an E-core;
  fallback = the highest online cpu on a non-hybrid box), `strih_nic_irq_affinity_verdict`
  (single-cpu AND ≥ first cpu_atom), `strih_irq_total_count` + `strih_counter_advanced` (the live
  advancing-counter half), `strih_cpulist_min`/`_max`.
- **`strih_nic_irq_affinity_script_text`** emits the self-contained `/usr/local/bin/strih-nic-irq-affinity.sh`
  boot script (iface via `STRIH_NIC_IFACE` override or the 10.77.9.202-address match → xhci PCI
  function → IRQ(s) → last cpu_atom E-core → write `smp_affinity_list` + read back; **fail LOUD**
  on any unresolved step or read-back mismatch). Its resolution is parity-locked to the lib helpers
  by a test (`emitted_irq_script_resolution_matches_the_lib_helpers`) so the boot-write and the
  verify-read never drift.
- **`strih_nic_irq_affinity_unit_text`** ↔ the committed **`systemd/strih-nic-irq-affinity.service`**
  (byte-parity test): a **system oneshot**, `Type=oneshot` / `RemainAfterExit=yes` /
  `After=network-pre.target` / `WantedBy=multi-user.target`. `setup-strih.sh` installs the script
  (0755) + unit as a **lettered sub-step `11b`** (TOTAL_STEPS stays 13 — the issue 1352/1353
  lettered-sub-step precedent) and `systemctl enable`s it ONLY (enable-only; the supervisor applies
  it live, the unit re-applies at every boot).
- **`verify-strih.sh` item 16** (read-only, on the box): resolves the same xhci IRQ the same way,
  asserts its `smp_affinity_list` is a SINGLE cpu ≥ the first `cpu_atom` cpu AND that its
  `/proc/interrupts` counter is **ADVANCING** over a live 2-s window — never a static file check
  (the "three ways a gate lies" discipline: a placement that reads right but on a dead IRQ is still
  a fault). FAIL loud, drain-safe.

**Boot-timing caveat (UNVERIFIED until the live apply):** `After=network-pre.target` orders the
oneshot early; the iface's 10.77.9.202 address may not be assigned yet at that sync point on a
slow-configuring boot. The script's primary path is the address match, so if the address is not up
it fails LOUD (a diagnosable journal line, never a silent wrong placement) and the next boot / the
supervisor's live run re-applies it. `STRIH_NIC_IFACE` pins the iface deterministically if the
boot race ever proves real. Re-confirm on the first live boot that the oneshot resolved cleanly.

## Follow-ups (not done in the preparation lane)

- ~~obs-browser / CEF in the strih CI variant~~ — **DONE (issue 1317, CEF now wired)**, see the CI
  section above.
- **A bespoke `strih_scenes.py`** WS seeder (sibling of `imag_scenes.py`) that seeds the 10 inputs
  (genlock_fifo + floor 3) + the `STRIH-LX (...)` outputs from `/opt/camera-box/strih-lx-seed.json`
  and enforces Studio Mode. `setup-strih.sh` installs the seed manifest + `obs_phase2.py` primitives;
  the full seeder is deferred.
- **`deploy-genlock-fleet.sh` strih-lx EXECUTE deploy** (scp the strih artifact + ssh-run the on-box
  program). The pure helpers + the PLAN arm exist; execute reuses the imag transport with
  `fleet_linux_bundle_artifact_for strih-lx` + `STRIH_LX_IP` once the box exists.
- **`strih-obs-start.sh` / `strih-obs-stop.sh`** launcher pair (sibling of `imag-obs-start.sh`) that
  `strih-obs.service` ExecStart references.
- **The 2ME self-feedback INPUT names.** issue 1317's spec lists the multiview-feedback inputs as
  `STRIH-SNV (2ME PGM/PVW)` (what the Windows box publishes). For a fully independent strih-lx the
  box's OWN multiview feedback should read `STRIH-LX (2ME PGM/PVW)` (self-loop); while parallel and
  tuning, either is fine and trivially re-seedable. Re-confirm the intended feedback source when the
  box goes live.

## What is NOT in scope

Recording retention, the NIC self-heal watcher, Companion Satellite re-pairing, WoL, and the 4K
multiview projector budget are all listed in the issue-1317 inventory as NEEDS-WORK but are not part
of the initial provisioning scaffolding — they follow once the hardware is in hand.
