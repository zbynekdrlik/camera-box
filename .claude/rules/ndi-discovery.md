---
paths:
  - "scripts/lib/ndi-discovery.sh"
  - "scripts/ndi-discovery/**"
  - "scripts/ndi-discovery-laptop.ps1"
  - "scripts/camera-set.sh"
  - "scripts/lib/obs-fleet.sh"
  - "scripts/setup-device.sh"
  - "scripts/verify-device.sh"
  - "scripts/setup-strih.sh"
  - "scripts/verify-strih.sh"
  - "tests/python/test_ndi_discovery_1342.py"
---

# NDI discovery = a receiver-side list of every managed sender IP + ONE output per cambox (issue 1342)

## What changed and why

- **Each cambox publishes ONE NDI source, `CAMn (usb)`** (60p, the certified `Cam N = CAMN (usb)`
  mapping).
  - The issue-792 `CAMn (30p)` blend stream is gone. The main's read-only OBS-WS read on 24.9.2026
    found 0 of the 16 NDI inputs on strih-lx + stream bound to a `(30p)` source.
  - Setup-device STEP 7 DELETES a leftover `camera-box.service.d/publish-30p.conf`, and the old
    verify check `(z)` is removed. Owner ruling 24.9.2026: "ak na nic tak prosim nech maju iba jeden
    spravny".
- **Every managed RECEIVER queries every managed SENDER by IP, in addition to mDNS.** mDNS multicast
  over the venue MikroTik chain missed the RESOLUME-SNV sources on the strih OBS, and fresh laptops
  listed only part of the sources.

## The SDK contract (why this shape, and why NOT a Discovery Server)

The receiver side: `vendor/distroav/lib/ndi/Processing.NDI.Find.h`, `p_extra_ips`:

> "The list of additional IP addresses that exist that we should query for sources on. For instance,
> if you want to find the sources on a remote machine that is not on your local sub-net then you can
> put a comma separated list of those IP addresses here and those sources will be available locally
> even though they are not mDNS discoverable. ... When none is specified the registry is used."

Find.h does not name the registry key. The documented mapping (NDI SDK docs, *Configuration Files*)
is `ndi.networks.ips` in `ndi-config.v1.json`, the same list NDI Access Manager's "Remote Sources"
tab edits. This lane could not exercise it live (the rig was under the E2E lease); the supervisor's
acceptance step below proves it. A finder with that key queries each listed IP by unicast AND keeps
using mDNS. **Senders never read it.** So every sender keeps announcing over mDNS, and the following
keep working exactly as before:
- stock TVs and building displays showing the strih program (they cannot carry a config);
- guest laptops that never ran the laptop script;
- the avahi-based port-map audit (`.claude/rules/ndi-portmap-watchdog.md`).

**Never configure `networks.discovery` on a managed box.** Part 1 shipped a gated Discovery-Server
client config, which was never enabled and is now removed. The lane's documentation finding on
issue 1342 is the reason. From the NDI SDK docs, *Configuration Files*:

> "When a Discovery server is used, receivers combine the list of sources found on the discovery
> server with those discovered via mDNS. Senders, however, will avoid using mDNS when a discovery
> server is configured ..."

Every managed OBS box is also a sender, so a discovery config would hide its outputs from every
unconfigured receiver. The writer therefore DELETES a stray `networks.discovery`, and both
verifiers FAIL on one.

## The list is GENERATED -- never hand-typed

`scripts/lib/ndi-discovery.sh` `ndi_discovery_sender_ips` is the ONE generator. It lists, in
order, each IP once:
1. **Every camera `camera_resolve` knows** (`scripts/camera-set.sh`), walked `cam1`, `cam2`, ... to
   the first unknown name.
   - This is deliberately NOT `CAMERA_ACTIVE_SET`: a camera retired from MEASUREMENT is still a
     powered sender.
   - Walking the resolver means a new `camN)` arm is picked up with no second roster (the
     camera-active-set rule).
   - The walk runs in a subshell, so setup-device's own `CAMERA_IP` / `CAMERA_NAME` stay intact.
2. **Every member of the obs-fleet `ndi-sender` facet** (`scripts/lib/obs-fleet.sh`: strih-lx,
   stream, resolume). `retired` rows are excluded by `obs_fleet_boxes`.

The two modes:
- **`pinned`**: the cameras plus every IPv4 fleet host. It is deterministic, and it is what the
  verifiers REQUIRE, what the checked-in file carries and what the `.ps1` default carries.
- **`resolve`** (the provisioners' default): additionally every HOSTNAME fleet host (resolume.lan,
  a traveling DHCP box) resolved to IPv4 with a bounded `getent ahostsv4`.
  - An unresolvable or non-IPv4 answer is skipped and named on stderr. Those sources stay
    mDNS-only, as before.
  - A traveling box's lease is never REQUIRED, so a verify never flaps on it.

**The traveling resolume gap (known, accepted by the design's provisioning-time model).** Resolume's
IP is resolved ONCE, when the receiver is provisioned:
- If resolume was away at that moment, the receiver has no entry for it until its next provisioner run.
- If resolume's DHCP lease moved afterwards, the entry is stale.
- In both cases its sources fall back to mDNS, exactly as before this change. They are never hidden.
- verify-strih item 34 prints a NOTE (never a FAIL) when resolume resolves NOW to an IP the strih-lx
  OBS config lacks. Re-running setup-strih.sh step 4b fixes it; every strih-lx genlock deploy does.
- The durable cures are both outside this lane:
  - (a) a DHCP reservation for RESOLUME-SNV, after which its row becomes a pinned IPv4 in `OBS_FLEET`
    and the verifiers require it;
  - (b) regenerate the list at OBS start (an `ExecStartPre=` in `strih-obs.service` running an
    installed copy of this lib).

**One list for every venue (the design's Shared-benefit line).** The receiver list is a FLEET fact,
not a per-strih-box fact: `scripts/strih-boxes/<box>.env` (issue 1361) holds a strih's OWN identity
(hostname, IP, NDI prefix, which cameras it takes as inputs), while the senders a receiver should
query come from the fleet lists.
- A second strih (strih-pp) joins `OBS_FLEET` + the `ndi-sender` facet at go-live
  (`.claude/rules/obs-fleet-list.md`), and every receiver picks it up on its next provisioner run.
- If the Poprad camboxes turn out to have different IPs from `camera_resolve`, they need their own
  roster (a venue dimension in `camera-set.sh`) before strih-pp's list can be venue-exact. Until
  then, strih-pp would list the SNV IPs: harmless extra unicast queries, never a hidden source.

A renumbered camera / strih-lx / stream FAILS `verify-device (an)` / `verify-strih` item 34 until the
box is re-provisioned. Re-provisioning is the fix; no hand edit is needed. Extra entries in a box's
list (an old lease, a retired box) are harmless and do not fail: a finder just queries one more
address.

The CLI, for boxes this repo does not provision:
- `bash scripts/lib/ndi-discovery.sh --ips [resolve|pinned]`
- `bash scripts/lib/ndi-discovery.sh --json [resolve|pinned]`

## Where the config is written, per receiver

| Receiver | Config path | Written by | Graded by |
|---|---|---|---|
| cambox camera-box.service (receives `STRIH-LX (interkom)` for the cameraman preview; root, `ProtectHome=yes`) | `/etc/ndi/ndi-config.v1.json` + `camera-box.service.d/ndi-discovery.conf` (`NDI_CONFIG_DIR=/etc/ndi`) | setup-device STEP 7 | verify-device `(an)` |
| strih-lx OBS + bkshading-service (User=newlevel) | `~newlevel/.ndi/ndi-config.v1.json` | setup-strih step 4b | verify-strih item 34 |
| strih-lx intercom-hub (`ProtectHome=true`) | `/etc/ndi/ndi-config.v1.json` + `intercom-hub.service.d/ndi-discovery.conf` | setup-strih step 4b | verify-strih item 34 |
| Windows stream / resolume / any Windows laptop | `%ProgramData%\NDI\ndi-config.v1.json` | `scripts/ndi-discovery-laptop.ps1` (supervisor / owner) | the script's own read-back |
| Linux laptop / dev1 probes | `~/.ndi/ndi-config.v1.json` | copy `scripts/ndi-discovery/ndi-config.v1.json` | -- |

- Nothing is gated. Receiver config cannot hide a sender, so it ships on the next provisioner run.
  Every strih-lx genlock deploy re-runs setup-strih, so strih-lx converges on its next deploy.
- The Linux SDK reads `$HOME/.ndi/ndi-config.v1.json`, or `$NDI_CONFIG_DIR/ndi-config.v1.json` when
  that env var is set. A root system service without `User=` has no guaranteed `$HOME`, and
  `ProtectHome` hides `/root` anyway, so root services get `/etc/ndi` plus the drop-in.
- The managed-box writer MERGES into an existing file. It sets `networks.ips` EXACTLY to the
  generated list (so it converges), drops `networks.discovery` and keeps every other key.
  - An unmergeable file (not JSON, or no python3 on the box) is backed up to `.bak-<stamp>` and
    replaced.
  - An EMPTY list is refused.
- The laptop `.ps1` MERGES too, but keeps the machine's own existing `networks.ips` entries and ADDS
  the rig IPs. It removes a `discovery` value only when it is the retired `10.77.9.200`.
- The checked-in `scripts/ndi-discovery/ndi-config.v1.json` and the `.ps1`'s `-Ips` default are
  both test-pinned to `--ips pinned` / `--json pinned`. After a fleet renumber, regenerate them:
  `bash scripts/lib/ndi-discovery.sh --json pinned > scripts/ndi-discovery/ndi-config.v1.json`,
  plus the `.ps1` default line.

The genlock skill's old "libndi ignores ndi-config.v1.json" finding (issue 797) is not evidence
against this. That test wrote `/root/.ndi/`, which is invisible under camera-box's
`ProtectHome=yes`, and used the non-SDK shape `"rudp":{"recv":false}`. Prove the config took effect
from the receiver's source list instead (below).

## Supervisor live steps (code-only lane -- nothing here was run live)

Run each step when the rig is free. Every step is a receiver-only change, so no ordering constraint
and no maintenance window exists: an unconfigured receiver simply keeps mDNS-only discovery.

1. **strih-lx**: the next genlock deploy re-runs `setup-strih.sh --box strih-lx` (step 4b). Standalone
   alternative: `sudo ./scripts/setup-strih.sh --box strih-lx`. Then:
   - `sudo ./scripts/verify-strih.sh --box strih-lx` -- item 34 prints 3 PASS lines;
   - `systemctl --user restart strih-obs.service`, `sudo systemctl restart bkshading-service intercom-hub`.
2. **Camboxes**: re-run `setup-device.sh <CAMn>` (the provisioning runbook), restart
   `camera-box.service` (never a reboot, the never-remote-reboot-a-cambox rule), then
   `verify-device.sh <CAMn>` `(an)`.
3. **stream and resolume** (Windows, via the win-* MCP; never ssh for a GUI step):
   - On dev1 with resolume home: `bash scripts/lib/ndi-discovery.sh --ips` prints the list including
     resolume's current IP.
   - scp `scripts/ndi-discovery-laptop.ps1` to the box, run
     `powershell -ExecutionPolicy Bypass -File <path>\ndi-discovery-laptop.ps1 -Ips "<that list>"`,
     then restart OBS the usual way (obs-ops).
4. **dev1 probes** (optional, they run as `newlevel`):
   `mkdir -p ~/.ndi && cp scripts/ndi-discovery/ndi-config.v1.json ~/.ndi/`.
   - Part 1's `ndi-discovery-server` user unit shipped DISABLED and was never installed by code.
     If it was hand-installed on dev1, remove it:
     `systemctl --user disable --now ndi-discovery-server.service`, then delete
     `~/.config/systemd/user/ndi-discovery-server.service` + `~/.local/bin/ndi-discovery-server` and
     run `systemctl --user daemon-reload`.
5. **Acceptance** (issue 1342):
   - Every managed receiver lists every managed sender within 5 s of OBS start: 10/10 cold starts
     on strih-lx and stream, plus one laptop that ran the `.ps1`.
     - Resolume counts only when it was resolvable at the receiver's last provisioning run and its
       lease has not moved since (the traveling gap above). Otherwise it is found by mDNS only.
   - A stock TV / unconfigured laptop still sees the strih program over mDNS.
   - `avahi-browse -rtp _ndi._tcp` from dev1 still lists every sender (senders are unchanged), and
     shows 0 `(30p)` sources.

## Rollback (back to mDNS-only discovery)

There is no gate to flip, because the config is receiver-only. To remove it from a box:

- cambox: remove `/etc/ndi/ndi-config.v1.json` and
  `/etc/systemd/system/camera-box.service.d/ndi-discovery.conf`, run `systemctl daemon-reload`,
  then restart `camera-box.service`. The root fs is read-only, so do it in setup-device's rw window
  or with a `mount -o remount,rw /` + remount ro.
- strih-lx: remove `~newlevel/.ndi/ndi-config.v1.json`, `/etc/ndi/ndi-config.v1.json` and
  `/etc/systemd/system/intercom-hub.service.d/ndi-discovery.conf`, run `systemctl daemon-reload`,
  then restart `strih-obs.service` (user unit), `bkshading-service` and `intercom-hub`.
- Windows boxes and laptops: restore the `ndi-config.v1.json.bak-<stamp>` the `.ps1` left next to
  the config (or delete the file), then restart OBS.
- A permanent rollback also removes the STEP 7 / step 4b writes, or the next provisioner run
  re-writes them.

## Laptop quick steps (owner-facing)

- **Windows:** as Administrator, run
  `powershell -ExecutionPolicy Bypass -File scripts\ndi-discovery-laptop.ps1`, then restart OBS.
  - `-DryRun` prints the result without writing.
  - The script merges into an existing config (the laptop's own listed IPs stay), backs it up, and
    writes UTF-8 without a BOM. A BOM makes a JSON reader fall back to defaults; see the dantesync
    incident in `.claude/skills/ops`. Never hand-write the file with a PowerShell cmdlet that adds
    a BOM.
  - NDI Access Manager (NDI Tools) -> "Remote Sources" is the same list through a GUI.
- **Linux:** `mkdir -p ~/.ndi && cp scripts/ndi-discovery/ndi-config.v1.json ~/.ndi/`, then restart OBS.
  If a config already exists, add the IPs to its `ndi.networks.ips` instead of overwriting it.
