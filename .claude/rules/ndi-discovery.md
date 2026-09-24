---
paths:
  - "scripts/lib/ndi-discovery.sh"
  - "scripts/ndi-discovery/**"
  - "scripts/ndi-discovery-laptop.ps1"
  - "systemd/ndi-discovery-server.service"
  - "scripts/setup-device.sh"
  - "scripts/verify-device.sh"
  - "scripts/setup-strih.sh"
  - "scripts/verify-strih.sh"
  - "tests/python/test_ndi_discovery_1342.py"
---

# NDI discovery = the dev1 NDI Discovery Server + ONE output per cambox (issue 1342)

## What changed and why

- **Each cambox publishes ONE NDI source, `CAMn (usb)`** (60p, the certified `Cam N = CAMN (usb)`
  mapping). The issue-792 `CAMn (30p)` blend stream is gone. The main's read-only OBS-WS read on
  24.9.2026 found 0 of the 16 NDI inputs on strih-lx + stream bound to a `(30p)` source (resolume
  and laptops were not read), and no mapping table, latency-pin baseline or scene script in this
  repo names one. Setup-device STEP 7 now DELETES a leftover
  `camera-box.service.d/publish-30p.conf`, so a re-provisioned box converges. The old verify
  check `(z)` is removed. Owner ruling 24.9.2026: "ak na nic tak prosim nech maju iba jeden spravny".
- **Discovery goes through the NDI SDK's own Discovery Server on dev1**, instead of relying only
  on mDNS multicast over the venue MikroTik chain. That chain missed the RESOLUME-SNV sources on
  the strih OBS, and fresh laptops listed only part of the sources.
  - The server: `systemd/ndi-discovery-server.service`, a dev1 `--user` unit on :5959. It ships DISABLED.
  - The client config: ONE source of truth, `scripts/lib/ndi-discovery.sh`. It renders
    `ndi-config.v1.json` with `networks.discovery` = `NDI_DISCOVERY_SERVERS` (default
    `10.77.9.200`) and `networks.ips` = `""`. The empty list is deliberate: a hand-kept static
    list is what went stale on the old Windows strih.

## The SENDER caveat -- read before rolling anything out

The NDI SDK docs, *Configuration Files*:

> "When a Discovery server is used, receivers combine the list of sources found on the discovery
> server with those discovered via mDNS. Senders, however, will avoid using mDNS when a discovery
> server is configured, allowing you to run entirely without network multicast if you desire."

The docs name no key that keeps sender mDNS on, and no automatic fallback for senders. Two
consequences:

1. **mDNS stays a fallback for RECEIVERS only.** Once a sender (a cambox, the strih-lx OBS
   outputs, stream, resolume) is configured, it is visible only to receivers that are configured
   too. And only while at least one listed server is up.
2. **Rollout order is receivers first, senders last.**
   - Configuring a PURE receiver is harmless: it keeps mDNS and only adds the server's list.
   - Every managed OBS box is BOTH: strih-lx, stream and resolume publish outputs. Configuring
     one hides its outputs from every unconfigured receiver, so those boxes belong in the
     SENDER wave, not the receiver wave.

### THE ROLLOUT GATE -- `NDI_DISCOVERY_ENABLED` (scripts/lib/ndi-discovery.sh)

Every strih-lx genlock deploy re-runs `setup-strih.sh` (`scripts/lib/strih-lx-deploy.sh`), and a
cambox re-provision re-runs `setup-device.sh`. The client config must therefore not ride along on a
routine deploy before the server and every receiver are ready.

- **Gate OFF (the default, `0`):**
  - The provisioners write NO client config; they only print that the gate is off.
  - `verify-device (an)` and `verify-strih` item 34 pass a box that has neither the config nor the
    drop-in: that is the correct pre-rollout state.
  - A half-written box is still graded in full.
- **Gate ON (`1`):** the provisioners write the config, and the verifiers HARD-FAIL any box
  without it.
- **Per-class gates:** `NDI_DISCOVERY_ENABLED_STRIH` (setup-strih / verify-strih) and
  `NDI_DISCOVERY_ENABLED_CAMBOX` (setup-device / verify-device). Each defaults to the fleet switch
  `NDI_DISCOVERY_ENABLED`. They let the rollout configure strih-lx first and the camboxes after,
  or keep strih-lx off the gate entirely (the stock-display option).
- **Flipping a gate:** the supervisor flips the checked-in default in the lib, as one step of the
  rollout below. It is a reviewed code change, never a per-box env tweak.
  - For a single manual run, pass the variable INSIDE sudo:
    `sudo NDI_DISCOVERY_ENABLED_STRIH=1 ./scripts/setup-strih.sh --box strih-lx`.
  - `NDI_DISCOVERY_ENABLED=1 sudo …` does not work: sudo's env_reset drops the variable.
- **A gate flip configures nothing by itself.** A box is configured on its NEXT provisioner run.

### Consumers that must have a plan BEFORE the gate flips

Any receiver that cannot or will not carry the config loses every configured sender:

| Consumer | What breaks once the senders are configured | Plan needed |
|---|---|---|
| The stock NDI displays / building TVs showing the strih program (`.claude/rules/ndi-portmap-watchdog.md`) | they find sources only via mDNS | owner decision: keep strih-lx unconfigured, or accept the loss |
| Guest laptops that never ran the `.ps1` | no cambox / strih-lx sources at all | owner decision: every OBS laptop gets the one-line config (owner ruling 24.9.2026 wants every laptop to see every source) |
| `scripts/ndi-portmap-audit.sh` + its alert watchdog | reads `avahi-browse`; a configured strih-lx drops out of mDNS, so the audit logs an empty map as a gather error and never pages again | move the audit to the server's listing, or keep strih-lx off the gate |
| dev1 NDI probes (`src/bin/ndi-recv-probe.rs`, `src/probe/reader.rs`, `multi_reader.rs`, `liveness.rs`, run as `newlevel`) | they no longer find the camboxes | copy the config to `~newlevel/.ndi/ndi-config.v1.json` on dev1 before the cambox wave |
| The cameraman HDMI preview on each cambox (receives `STRIH-LX (interkom)`) | from the moment strih-lx is configured until each cambox is, that cambox (an mDNS-only receiver) loses `STRIH-LX (interkom)`. There is ALWAYS a window, and the reverse order is worse: configured camboxes vanish from the still-unconfigured strih-lx OBS program inputs | do both in ONE maintenance slot, strih-lx first (it keeps receiving the camboxes over mDNS), then every cambox right after; the window = the cameraman preview only, for the minutes the cambox pass takes |

### Open questions for the main / owner

These are posted on issue 1342 as a `Design-question`; the code accepts either answer.

1. **Server location.** At external events the rig runs at the venue on mobile data, while dev1
   stays at the church behind tailscale (the event-rig network memory). The venue boxes would then
   depend on a metered off-site link for discovery.
   - Option: run a server that travels with the rig on strih-lx (`10.77.9.202`), with dev1 as the
     second entry: `NDI_DISCOVERY_SERVERS="10.77.9.202,10.77.9.200"` (NDI's redundancy form). That
     would need a strih-lx unit, which is not written yet.
2. **Stock displays and guest laptops** (the table above).

## Where the config lives, per box class

| Box | Config path | Written by | Graded by |
|---|---|---|---|
| cambox (camera-box.service, root, `ProtectHome=yes`) | `/etc/ndi/ndi-config.v1.json` + `camera-box.service.d/ndi-discovery.conf` (`NDI_CONFIG_DIR=/etc/ndi`) | setup-device STEP 7 | verify-device `(an)` |
| strih-lx OBS + bkshading-service (User=newlevel) | `~newlevel/.ndi/ndi-config.v1.json` | setup-strih step 4b | verify-strih item 34 |
| strih-lx intercom-hub (`ProtectHome=true`) | `/etc/ndi/ndi-config.v1.json` + `intercom-hub.service.d/ndi-discovery.conf` | setup-strih step 4b | verify-strih item 34 |
| Windows stream / resolume / any Windows laptop | `%ProgramData%\NDI\ndi-config.v1.json` | `scripts/ndi-discovery-laptop.ps1` (supervisor / owner) | the script's own read-back |
| Linux laptop | `~/.ndi/ndi-config.v1.json` | copy `scripts/ndi-discovery/ndi-config.v1.json` | -- |

The Linux SDK reads `$HOME/.ndi/ndi-config.v1.json`, or `$NDI_CONFIG_DIR/ndi-config.v1.json` when
that env var is set. A root system service without `User=` has no guaranteed `$HOME`, and
`ProtectHome` hides `/root` anyway, so root services get `/etc/ndi` plus the drop-in.

This plausibly explains the genlock skill's "libndi ignores ndi-config.v1.json" finding (issue 797):
- That test wrote `/root/.ndi/`, which is invisible under camera-box's `ProtectHome=yes`.
- It used the non-SDK shape `"rudp":{"recv":false}`; the SDK schema is `"rudp":{"recv":{"enable":false}}`.

So that finding does not prove the SDK ignores the file. Prove the discovery config took effect
from the SERVER side instead: the server log lists every registered source.

## Supervisor rollout (after production and after the open questions are answered -- code-only lane, nothing here was run live)

1. **Install on dev1.** Fetch the Linux standalone installer
   `Install_NDI_Discovery_Server_v6.sh` from ndi.video. It is NOT in the libndi runtime the
   camboxes copy, and the fleet NDI pin is 6.3.2. Run it in a scratch dir; it extracts to
   `Install_NDI_Discovery_Server_v6/bin/`. Copy the x86_64 binary to
   `~/.local/bin/ndi-discovery-server` (`chmod +x`), then:
   `cp systemd/ndi-discovery-server.service ~/.config/systemd/user/ && systemctl --user daemon-reload`
   `systemctl --user enable --now ndi-discovery-server.service`
   - Linger must be on (`loginctl show-user newlevel -p Linger` = yes, the rig-lease-server precedent).
   - Verify it listens: `ss -ltnp | grep 5959`.
2. **Drop the 30p stream (independent of the gate).** Deploying the new camera-box binary alone
   stops the 30p stream: the new binary has no 30p code and ignores `CAMERA_BOX_PUBLISH_30P`.
   Restart `camera-box.service`, never a reboot (the never-remote-reboot-a-cambox rule).
   - The leftover `publish-30p.conf` drop-in is inert after that. The next routine
     `setup-device.sh` run deletes it, so there is no need to re-provision just for this.
   - Check: `avahi-browse -rtp _ndi._tcp` from dev1 shows 0 `(30p)` sources.
   - Run this check NOW: once the camboxes are configured senders they drop out of mDNS entirely,
     and an empty avahi list proves nothing.
3. **Pure receivers** (safe, they keep mDNS):
   - The owner's laptops: the `.ps1` (Windows), NDI Access Manager -> Advanced -> Discovery Server,
     or the JSON copy (Linux).
   - The dev1 probes: copy `scripts/ndi-discovery/ndi-config.v1.json` to `~newlevel/.ndi/`.
   - The consumer table above: each row has its plan.
4. **Sender wave, in ONE maintenance slot: flip, then configure in this order**, each followed
   by an OBS/app/service restart.
   - Flip: commit the gate defaults to `1` in the lib, either the fleet switch or only
     `NDI_DISCOVERY_ENABLED_CAMBOX` when strih-lx stays off.
   - strih-lx first: `setup-strih.sh --box strih-lx` (step 4b), then `verify-strih.sh` item 34.
     It keeps receiving the not-yet-configured camboxes over mDNS.
   - stream and resolume: `scripts/ndi-discovery-laptop.ps1` via the win-* MCP (scp the `.ps1`,
     then `powershell -File`; never ssh for a GUI step), then restart OBS the usual way (obs-ops).
   - Camboxes right after: `setup-device.sh`, restart `camera-box.service`, then
     `verify-device.sh` `(an)`. The cameraman-preview window closes here.
5. **Acceptance** (issue 1342):
   - The server log lists every managed sender.
   - Every managed receiver lists every managed sender within 5 s of OBS start: 10/10 cold starts
     on strih-lx and stream, plus one laptop.

## Rollback (back to mDNS-only)

Setting a gate back to 0 does not un-configure a box that already has the config, and the
verifiers keep passing it. To really go back, on each configured box:

- cambox: remove `/etc/ndi/ndi-config.v1.json` and
  `/etc/systemd/system/camera-box.service.d/ndi-discovery.conf`, run `systemctl daemon-reload`,
  then restart `camera-box.service`. The root fs is read-only, so do it in setup-device's rw
  window or with a `mount -o remount,rw /` + remount ro.
- strih-lx: remove `~newlevel/.ndi/ndi-config.v1.json`, `/etc/ndi/ndi-config.v1.json` and
  `/etc/systemd/system/intercom-hub.service.d/ndi-discovery.conf`, run `systemctl daemon-reload`,
  then restart `strih-obs.service` (user unit), `bkshading-service` and `intercom-hub`.
- Windows boxes and laptops: restore the `ndi-config.v1.json.bak-<stamp>` the `.ps1` left next to
  the config (or delete the file), then restart OBS.
- Set the gate back to 0 in the lib, so the next provisioner run does not re-write the config.

## Laptop quick steps (owner-facing)

- **Windows:** as Administrator, run
  `powershell -ExecutionPolicy Bypass -File scripts\ndi-discovery-laptop.ps1`, then restart OBS.
  - `-DryRun` prints the result without writing.
  - The script merges into an existing config, backs it up, and writes UTF-8 without a BOM. A BOM
    makes a JSON reader fall back to defaults; see the dantesync incident in `.claude/skills/ops`.
  - Never hand-write the file with a PowerShell cmdlet that adds a BOM.
- **Linux:** `mkdir -p ~/.ndi && cp scripts/ndi-discovery/ndi-config.v1.json ~/.ndi/`, then restart OBS.
  If a config already exists, set `ndi.networks.discovery` in it instead of overwriting it.
