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
  mapping). The issue-792 `CAMn (30p)` blend stream is gone: live on 24.9.2026, 0 of 16 OBS NDI
  inputs on strih-lx + stream were bound to a `(30p)` source, and no mapping table, latency-pin
  baseline or scene script names one. Setup-device STEP 7 now DELETES a leftover
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
2. **Rollout order is receivers first, senders last.** Configuring a receiver is harmless, because
   it keeps mDNS and only adds the server's list. The cambox re-provision turns the camboxes into
   discovery-only senders, so it runs only after every receiver below is configured.

A redundant second server (`NDI_DISCOVERY_SERVERS="10.77.9.200,10.77.9.202"`, NDI's documented
redundancy form) removes dev1 as a single point of failure. That call belongs to the main/owner
(open question on issue 1342); the code takes either value.

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

This also explains the genlock skill's "libndi ignores ndi-config.v1.json" finding (issue 797):
- That test wrote `/root/.ndi/`, which is invisible under camera-box's `ProtectHome=yes`.
- It used the non-SDK shape `"rudp":{"recv":false}`; the SDK schema is `"rudp":{"recv":{"enable":false}}`.

So that finding does not prove the SDK ignores the file. Prove the discovery config took effect
from the SERVER side instead: the server log lists every registered source.

## Supervisor rollout (after production -- code-only lane, nothing here was run live)

1. **Install on dev1.** Fetch the Linux standalone installer
   `Install_NDI_Discovery_Server_v6.sh` from ndi.video. It is NOT in the libndi runtime the
   camboxes copy, and the fleet NDI pin is 6.3.2. Run it in a scratch dir; it extracts to
   `Install_NDI_Discovery_Server_v6/bin/`. Copy the x86_64 binary to
   `~/.local/bin/ndi-discovery-server` (`chmod +x`), then:
   `cp systemd/ndi-discovery-server.service ~/.config/systemd/user/ && systemctl --user daemon-reload`
   `systemctl --user enable --now ndi-discovery-server.service`
   - Linger must be on (`loginctl show-user newlevel -p Linger` = yes, the rig-lease-server precedent).
   - Verify it listens: `ss -ltnp | grep 5959`.
2. **Receivers first**, each followed by an OBS/app restart:
   - strih-lx: `setup-strih.sh --box strih-lx` (step 4b), or copy the config by hand, then run
     `verify-strih.sh` item 34.
   - stream and resolume: run `scripts/ndi-discovery-laptop.ps1` via the win-* MCP (scp the `.ps1`,
     then `powershell -File`; never ssh for a GUI step), then restart OBS the usual way (obs-ops).
   - The owner's laptops: the `.ps1` (Windows), NDI Access Manager -> Advanced -> Discovery Server,
     or the JSON copy (Linux).
   - Other NDI receivers: any dev1-side NDI probe/finder that still needs to see the camboxes.
     Inventory it before step 3.
3. **Senders last:** re-provision the camboxes (setup-device.sh). This removes publish-30p.conf and
   writes the config + drop-in. Restart `camera-box.service`, never a reboot (the
   never-remote-reboot-a-cambox rule). Then run `verify-device.sh` `(an)`.
4. **Acceptance** (issue 1342):
   - `avahi-browse -rtp _ndi._tcp` from dev1 shows 0 `(30p)` sources.
   - The server log lists every managed sender.
   - Every managed receiver lists every managed sender within 5 s of OBS start, 10/10 cold starts
     on strih-lx and stream, plus one laptop.

## Laptop quick steps (owner-facing)

- **Windows:** as Administrator, run
  `powershell -ExecutionPolicy Bypass -File scripts\ndi-discovery-laptop.ps1`, then restart OBS.
  - `-DryRun` prints the result without writing.
  - The script merges into an existing config, backs it up, and writes UTF-8 without a BOM. A BOM
    makes a JSON reader fall back to defaults; see the dantesync incident in `.claude/skills/ops`.
  - Never hand-write the file with a PowerShell cmdlet that adds a BOM.
- **Linux:** `mkdir -p ~/.ndi && cp scripts/ndi-discovery/ndi-config.v1.json ~/.ndi/`, then restart OBS.
  If a config already exists, set `ndi.networks.discovery` in it instead of overwriting it.
