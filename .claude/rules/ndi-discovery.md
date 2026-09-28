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
  - "tests/python/test_ndi_discovery_1389.py"
  - "tests/pwsh/run_ndi_discovery_laptop_1389.sh"
---

# NDI discovery = a receiver-side list of the OBS-box senders, never a cambox; the camboxes carry no list + ONE output per cambox (issues 1342, 1389)

## What changed and why

- **Each cambox publishes ONE NDI source, `CAMn (usb)`** (60p, the certified `Cam N = CAMN (usb)`
  mapping).
  - The issue-792 `CAMn (30p)` blend stream is gone. The main's read-only OBS-WS read on 24.9.2026
    found 0 of the 16 NDI inputs on strih-lx + stream bound to a `(30p)` source.
  - Setup-device STEP 7 DELETES a leftover `camera-box.service.d/publish-30p.conf`, and the old
    verify check `(z)` is removed. Owner ruling 24.9.2026: "ak na nic tak prosim nech maju iba jeden
    spravny".
- **The OBS boxes and laptops query the managed OBS-box SENDERS by IP, in addition to mDNS.** mDNS
  multicast over the venue MikroTik chain missed the RESOLUME-SNV sources on the strih OBS, and fresh
  laptops listed only part of the sources.
- **Issue 1389: the list NEVER names a cambox, and a cambox carries NO list at all.** Until 28.9.2026
  every receiver, the camboxes included, listed all 7 cambox IPs, and that list made camera-box abort
  (next section).

## Never a cambox (issue 1389)

**Never list a cambox IP in `networks.ips`** -- not on a receiver, not on a laptop, not on a cambox.

**A cambox carries NO `networks.ips` at all (ROZHODNUTÉ 5879261962, supervisor, 28.9.2026).** A
cambox is an mDNS-only NDI receiver: camera-box's finder needs only the strih preview source
`STRIH-LX (interkom)`, which mDNS finds in milliseconds. Any listed host would give it an OUTBOUND
discovery connection whose teardown, at every strih / stream / resolume restart, is the same
unmeasured abort risk. With no list there is no connection in either direction, so there is nothing
to measure.

**Why:**
- **An extra-IP finder opens a TCP discovery connection to each listed sender's `:5960` listener.**
  An mDNS-only finder opens none. The supervisor proved it from dev1 against cam4 on 28.9.2026: 0
  connections with mDNS only, 1 connection with `p_extra_ips` (the design comment on issue 1389).
- **camera-box serves each inbound connection on its own libndi `disc:recv` thread.** Every
  camera-box held 19–20 `disc:recv` + `disc:send` thread pairs. The peers were strih-lx, resolume,
  stream and the other camboxes.
- **When the remote NDI process exits or restarts, that thread's teardown can abort camera-box.**
  - The trigger is a strih OBS restart, a SongPlayer restart, or the E2E twin hold/restore.
  - libndi 6.3.2 intermittently throws `std::system_error` (EINVAL) in that thread, uncaught.
  - It links its own static C++ runtime, so `std::terminate` aborts the whole process
    (`status=6/ABRT`).
- **The cost:** a ~3 s camera outage and a V4L2 re-open, which re-draws the capture lag (the
  issue-1367 A/V jump). cam1/cam2/cam4 aborted 6–22 times a day.
- **The captured stack** (cam1, 28.9.2026 20:32 UTC, a RAM-only `abort()` interposer):
  `libndi.so.6+0x18bd56f` ← `+0x1873a7e` ← `+0x1873ab4` ← `+0x187ead9` ← `start_thread`, no
  camera-box frame on it.
- **camera-box cannot contain it**: the throw is on a libndi-internal thread with libndi's own C++
  runtime, and surviving an abort with torn libndi state is not safe (the design's rejected
  Approach 3).
- **The camboxes are found by mDNS alone**, as before 24.9.2026. They were never among the missed
  sources: on 28.9 an mDNS-only finder saw all 26 sources in 0.02 s on dev1 and < 1 s on strih-lx,
  8/8 cold rounds.

**How it is enforced (all in `scripts/lib/ndi-discovery.sh`):**
- **The receiver list** (strih-lx, stream, resolume, laptops): `ndi_discovery_sender_ips` lists only
  the obs-fleet `ndi-sender` hosts.
  - The `camera_resolve` walk (`ndi_discovery_camera_ips` / `ndi_discovery_cambox_ips`) stays as
    the FORBIDDEN set.
  - A fleet or resolved host that lands on a cambox IP is skipped and named on stderr.
  - The generator fails loud (no list) when it cannot derive the cambox set, or when no host is left.
  - `ndi_discovery_config_verdict` has a `cambox` facet: a strih-lx config that lists a cambox IP
    FAILs verify-strih item 34, and so does an underivable cambox set.
  - The Windows `.ps1` REMOVES every `-RemoveIps` entry (default: every cambox IP, test-pinned to
    `--cambox-ips`), refuses an `-Ips` list naming one, and fails its read-back if one survived.
- **The cambox itself** (setup-device STEP 7 and, on a live box, `--cambox-apply`): the plan/apply
  pair `ndi_discovery_cambox_plan` + `ndi_discovery_cambox_apply_plan` takes the issue-1342 config off.
  - It strips `networks.ips` and `networks.discovery`.
  - When nothing else is left in `/etc/ndi/ndi-config.v1.json`, it removes the file AND the
    camera-box `NDI_CONFIG_DIR` drop-in (then `daemon-reload`). camera-box then runs on libndi's
    defaults: root, `ProtectHome=yes`, no readable `$HOME/.ndi`, so mDNS only.
  - Any other key keeps the file and the drop-in: something else in it still matters.
  - A drop-in pointing `NDI_CONFIG_DIR` anywhere else is not this repo's: STEP 7 fails loud, the
    live program refuses and touches nothing.
  - verify-device `(an)` grades it with `ndi_discovery_cambox_verdict`: no config is ok. A config
    that lists ANY IP FAILs naming it, with or without the drop-in (a drop-in would load it again).
    So do a `networks.discovery`, a config that is not JSON, and a foreign drop-in.

## The SDK contract (why this shape, and why NOT a Discovery Server)

The receiver side: `vendor/distroav/lib/ndi/Processing.NDI.Find.h`, `p_extra_ips`:

> "The list of additional IP addresses that exist that we should query for sources on. For instance,
> if you want to find the sources on a remote machine that is not on your local sub-net then you can
> put a comma separated list of those IP addresses here and those sources will be available locally
> even though they are not mDNS discoverable. ... When none is specified the registry is used."

Find.h does not name the registry key. The documented mapping (NDI SDK docs, *Configuration Files*)
is `ndi.networks.ips` in `ndi-config.v1.json`, the same list NDI Access Manager's "Remote Sources"
tab edits. A finder with that key queries each listed IP by unicast (a TCP connection to the
sender's `:5960`, the issue-1389 finding) AND keeps using mDNS. **Senders never read it.** So every
sender keeps announcing over mDNS, and the following keep working exactly as before:
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

`scripts/lib/ndi-discovery.sh` `ndi_discovery_sender_ips` is the ONE generator. It lists, in fleet
order, each IP once, every member of the obs-fleet `ndi-sender` facet (`scripts/lib/obs-fleet.sh`:
strih-lx, stream, resolume). `retired` rows are excluded by `obs_fleet_boxes`. It never lists a
cambox (above), and no cambox carries its output.

The FORBIDDEN set walks every camera `camera_resolve` knows (`scripts/camera-set.sh`): `cam1`,
`cam2`, ... to the first unknown name.
- This is deliberately NOT `CAMERA_ACTIVE_SET`: a camera retired from MEASUREMENT is still a powered
  sender.
- Walking the resolver means a new `camN)` arm is excluded with no second roster (the
  camera-active-set rule).
- The walk runs in a subshell, so setup-device's own `CAMERA_IP` / `CAMERA_NAME` stay intact.

The two modes:
- **`pinned`**: every IPv4 fleet host (today `10.77.9.202,10.77.9.204`). It is deterministic, and it
  is what verify-strih REQUIRES, what the checked-in file carries and what the `.ps1` default carries.
- **`resolve`** (setup-strih's default): additionally every HOSTNAME fleet host (resolume.lan, a
  traveling DHCP box) resolved to IPv4 with a bounded `getent ahostsv4`.
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
- The durable cures are both outside this lane (returned to the supervisor as a follow-up candidate
  to file):
  - (a) a DHCP reservation for RESOLUME-SNV, after which its row becomes a pinned IPv4 in `OBS_FLEET`
    and the verifiers require it;
  - (b) regenerate the list at OBS start (an `ExecStartPre=` in `strih-obs.service` running an
    installed copy of this lib).

**One list for every venue (the design's Shared-benefit line).** The receiver list is a FLEET fact,
not a per-strih-box fact: `scripts/strih-boxes/<box>.env` (issue 1361) holds a strih's OWN identity
(hostname, IP, NDI prefix, which cameras it takes as inputs), while the senders a receiver should
query come from the fleet list.
- A second strih (strih-pp) joins `OBS_FLEET` + the `ndi-sender` facet at go-live
  (`.claude/rules/obs-fleet-list.md`), and every receiver picks it up on its next provisioner run.
- The camboxes are never listed, so the Poprad cambox IPs never need a roster for this list. Only
  the FORBIDDEN set reads `camera_resolve`. A Poprad cambox would need its own `camera_resolve` arm
  to be caught by the verdict's `cambox` facet, but the generator can never list one either way.

A renumbered strih-lx / stream FAILS `verify-strih` item 34 until the box is re-provisioned.
Re-provisioning is the fix; no hand edit is needed. Extra NON-cambox entries in a box's list (an old
lease, a retired box) are harmless and do not fail: a finder just queries one more address. A cambox
entry always FAILs.

The CLI, for boxes this repo does not provision and for the issue-1389 runbook:
- `bash scripts/lib/ndi-discovery.sh --ips [resolve|pinned]`
- `bash scripts/lib/ndi-discovery.sh --json [resolve|pinned]`
- `bash scripts/lib/ndi-discovery.sh --cambox-ips` (the FORBIDDEN set)
- `bash scripts/lib/ndi-discovery.sh --cambox-apply` (the on-box cambox program that strips the
  list, below; it needs no fleet list)

## Where the config is written, per receiver

| Receiver | Config path | Written by | Graded by |
|---|---|---|---|
| cambox camera-box.service (receives `STRIH-LX (interkom)` for the cameraman preview; root, `ProtectHome=yes`) | NONE: mDNS only. The issue-1342 `/etc/ndi/ndi-config.v1.json` + `camera-box.service.d/ndi-discovery.conf` are removed | setup-device STEP 7 strips them; on a live box `--cambox-apply` | verify-device `(an)`: no `networks.ips` |
| strih-lx OBS + bkshading-service (User=newlevel) | `~newlevel/.ndi/ndi-config.v1.json` | setup-strih step 4b | verify-strih item 34 |
| strih-lx intercom-hub (`ProtectHome=true`) | `/etc/ndi/ndi-config.v1.json` + `intercom-hub.service.d/ndi-discovery.conf` | setup-strih step 4b | verify-strih item 34 |
| Windows stream / resolume / any Windows laptop | `%ProgramData%\NDI\ndi-config.v1.json` | `scripts/ndi-discovery-laptop.ps1` (supervisor / owner) | the script's own read-back |
| Linux laptop / dev1 probes | `~/.ndi/ndi-config.v1.json` | copy `scripts/ndi-discovery/ndi-config.v1.json` | -- |

- Nothing is gated. Receiver config cannot hide a sender, so it ships on the next provisioner run.
  Every strih-lx genlock deploy re-runs setup-strih, so strih-lx converges on its next deploy.
- The Linux SDK reads `$HOME/.ndi/ndi-config.v1.json`, or `$NDI_CONFIG_DIR/ndi-config.v1.json` when
  that env var is set. A root system service without `User=` has no guaranteed `$HOME`, and
  `ProtectHome` hides `/root` anyway, so a root receiver that needs a list gets `/etc/ndi` plus a
  drop-in (intercom-hub). camera-box needs none: without the drop-in it reads no config at all.
- The managed-box writer (strih-lx) MERGES into an existing file. It sets `networks.ips` EXACTLY to
  the generated list (so it converges and drops the old cambox entries), drops `networks.discovery`
  and keeps every other key.
  - An unmergeable file (not JSON, or no python3 on the box) is backed up to `.bak-<stamp>` and
    replaced.
  - An EMPTY list is refused.
- The laptop `.ps1` MERGES too, but keeps the machine's own existing `networks.ips` entries and ADDS
  the rig IPs. It removes every `-RemoveIps` (cambox) entry, and a `discovery` value only when it is
  the retired `10.77.9.200`.
- The checked-in `scripts/ndi-discovery/ndi-config.v1.json` and the `.ps1`'s `-Ips` / `-RemoveIps`
  defaults are test-pinned to `--json pinned` / `--ips pinned` / `--cambox-ips`. After a fleet
  renumber, regenerate them: `bash scripts/lib/ndi-discovery.sh --json pinned >
  scripts/ndi-discovery/ndi-config.v1.json`, plus the two `.ps1` default lines.
- `tests/pwsh/run_ndi_discovery_laptop_1389.sh` RUNS the real `.ps1` against scratch `%ProgramData%`
  dirs. CI runs it through `LaptopScriptRun1389` in `tests/python/test_ndi_discovery_1389.py`:
  ubuntu-latest ships pwsh, and dev1 has a portable one at `~/.local/pwsh74/pwsh`. A missing pwsh
  FAILS the test, never skips it. pwsh 7 is not Windows PowerShell 5.1, so keep the `.ps1` to 5.1
  syntax: the run proves the logic.

The genlock skill's old "libndi ignores ndi-config.v1.json" finding (issue 797) is not evidence
against this. That test wrote `/root/.ndi/`, which is invisible under camera-box's
`ProtectHome=yes`, and used the non-SDK shape `"rudp":{"recv":false}`. Prove the config took effect
from the receiver's source list and its TCP connections instead (below).

## Taking the list off a live cambox: `--cambox-apply` (issue 1389)

Re-running setup-device.sh on a live cambox is a full re-provision. The smallest safe equivalent
touches ONLY `/etc/ndi/ndi-config.v1.json` and the camera-box `ndi-discovery.conf` drop-in:
- `apply="$(mktemp)"; bash scripts/lib/ndi-discovery.sh --cambox-apply > "$apply"` generates ONE
  program for every cambox. It embeds the SAME `ndi_discovery_cambox_plan` /
  `ndi_discovery_cambox_apply_plan` pair STEP 7 calls (via `declare -f`, never a copy) and needs no
  fleet list.
- `sshpass -p "$DEVICE_ROOT_PW" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
  -o ConnectTimeout=10 root@<cambox> bash -s < "$apply"` runs it on one box (the ssh options of
  verify-device's `ssh_box`: a reflashed box has a new host key). The plan decides:
  - **none**: no config, or one with no `networks.ips` / `networks.discovery`, and no stale drop-in.
    It prints `this cambox carries no networks.ips already (mDNS only) -- nothing written`: no
    remount, no write to the USB stick.
  - **remove**: the config holds nothing but the list (the issue-1342 rendering) or nothing at all,
    or only the drop-in is left. It deletes the config and the drop-in, then `systemctl
    daemon-reload`.
  - **remove-backup**: the config is not a JSON object, or python3 is missing on the box and the
    file is not the lib's own rendering, so other keys cannot be told apart. It copies the file to
    `ndi-config.v1.json.bak-<stamp>` first, then removes as above. That backup is inert (no drop-in
    points at it); never restore one that lists IPs.
  - **strip**: the config also holds other keys. It rewrites it without `networks.ips` /
    `networks.discovery` (temp file + atomic rename) and keeps the drop-in, because the other keys
    still matter.
  - **refuse**: the drop-in points `NDI_CONFIG_DIR` somewhere else. It exits non-zero, `REFUSED:`
    names the drop-in, and nothing is touched: a human looks at it.
  - For any change, a read-only root is remounted rw first and put back ro after `sync` (3 tries).
    A root it cannot put back is a loud `ERROR ... read-WRITE` and a non-zero exit, and an EXIT trap
    puts it back on any other failure.
- It removes one or two ~100-byte files in the same rw window setup-device.sh and the dantesync
  upgrader use. That is not a risky write to the stick, and a clean box is never touched.
- Tier-0 tests run the real program fed to `bash -s`, with `findmnt` / `mount` / `sync` /
  `systemctl` stubbed: `CamboxApply1389` and `CamboxVerdict1389` in
  `tests/python/test_ndi_discovery_1389.py`.

## Supervisor deploy runbook (issue 1389 -- code-only lane, nothing here was run live)

Receiver config only, no gate. Run it when no E2E / soak holds the rig lease
(`curl -s http://127.0.0.1:8890/rig-lease.json` on dev1) -- the no-cambox-touch-while-lease rule.

**1. Write the new state on every receiver.** Any order, no restart yet. On dev1:
`bash scripts/lib/ndi-discovery.sh --ips` prints the OBS-box list including resolume's current IP
(today `10.77.9.202,10.77.9.204,10.77.9.201`).
- **strih-lx** (OBS + bkshading-service read `~newlevel/.ndi`, intercom-hub reads `/etc/ndi`):
  `sudo ./scripts/setup-strih.sh --box strih-lx` (step 4b). The next genlock deploy runs it anyway.
  - Grade: `sudo ./scripts/verify-strih.sh --box strih-lx`, item 34 = 3 PASS, no `cambox` FAIL.
- **cam1 … cam7** (each `camera_resolve` camera) -- the list comes OFF:
  - `apply="$(mktemp)"; bash scripts/lib/ndi-discovery.sh --cambox-apply > "$apply"` once;
  - then per box `sshpass -p "$DEVICE_ROOT_PW" ssh -o StrictHostKeyChecking=no
    -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 root@10.77.9.6N bash -s < "$apply"`
    → `OK: remove done -- this cambox carries no networks.ips now (mDNS only)` (or `... already`);
  - `rm -f "$apply"` afterwards.
  - Never setup-device.sh on a live box for this, never a reboot (the never-remote-reboot-a-cambox rule).
- **stream + resolume** (Windows, via the win-* MCP; never ssh for a GUI step):
  - copy `scripts/ndi-discovery-laptop.ps1` to the box;
  - in an elevated session run `powershell -ExecutionPolicy Bypass -File <path>\ndi-discovery-laptop.ps1
    -Ips "<the dev1 --ips list>"`;
  - it prints `removed the cambox IP(s) ...` and `OK: ... networks.ips = ...`. The `-RemoveIps`
    default strips every cambox, and a backup is left next to the file.

**2. Restart every NDI process, the remote receivers FIRST and the camboxes LAST.**
- Each restart closes that process's old connections into the camboxes. That is the abort trigger,
  one last time per peer: a camera-box may abort more than once during this step (each peer still on
  the old list can trigger it once), and each abort restarts it in ~3 s, now without the list.
- **strih-lx:** `systemctl --user restart strih-obs.service` (as newlevel), then
  `sudo systemctl restart bkshading-service intercom-hub`.
- **stream:** restart OBS the usual way (`.claude/skills/obs-ops`).
- **resolume:**
  - the cg OBS relaunch per `.claude/rules/resolume-cg-obs.md` (AHK guard mode, never start/restart AHK);
  - SongPlayer through its own session;
  - Resolume Arena (the owner's CG app, its own bundled NDI 6.1.1) holds a finder too. If the step-3
    read-back names Arena, ask the owner to restart it; never restart Arena or its AHK loop yourself.
- **cam1 … cam7:** `systemctl restart camera-box` on each, one box at a time (the deploy's restart,
  never a reboot).
  - The camera-box that starts now reads no config (the drop-in is gone), so it opens no discovery
    connection to the OBS boxes or the other camboxes.
  - A peer that aborts once on that close comes back from systemd already mDNS-only.

**3. Read-back.** The acceptance is 0 discovery connections into OR out of any camera-box.
- **Per cambox** (over ssh):
  - `ss -Htnp state established '( sport = :5960 or dport = :5960 )'` must print 0 lines: no remote
    finder holds a discovery connection into camera-box, and camera-box holds none out.
  - The `disc:recv` threads: `pid=$(systemctl show -p MainPID --value camera-box)` then
    `grep -cx 'disc:recv' /proc/$pid/task/*/comm | awk -F: '{s+=$2} END {print s}'`. It must drop
    from 19–20, expected 0 with no connection. The `ss` line is the authoritative check; record the
    first box's thread count.
  - A remaining connection names its peer IP: that box or process still runs the old list.
  - `./scripts/verify-device.sh <CAMn>` `(an)` passes: `no networks.ips on this cambox -- mDNS only`.
- **strih-lx:** `ss -Htnp state established '( dport = :5960 )'` shows no peer from `--cambox-ips`.
- **stream / resolume** (win-* MCP Shell):
  - `$c = '<--cambox-ips output>' -split ','; Get-NetTCPConnection -State Established -RemotePort 5960 |
    Where-Object { $c -contains ($_.RemoteAddress -replace '^::ffff:', '') } |
    Select-Object RemoteAddress, OwningProcess, @{n='Name';e={(Get-Process -Id $_.OwningProcess).ProcessName}}`
    must print nothing (the `::ffff:` strip covers a dual-stack socket);
  - any row names the NDI app still holding a cambox connection: restart it (Arena: ask the owner).
- **Acceptance (the design):**
  - restart the strih-lx OBS, the stream OBS, SongPlayer and the cg OBS one at a time; each restart
    aborts no camera-box: `journalctl -u camera-box --since '<t>' | grep -c 'status=6/ABRT'` = 0 on
    every box;
  - 0 aborts on cam1/cam2/cam4 over 24 h;
  - the cameraman HDMI preview on each cambox still shows `STRIH-LX (interkom)` after its
    camera-box restart (the preview source now comes from mDNS alone);
  - after a strih OBS cold start, every `CAMn (usb)` source still lists within seconds (mDNS), and
    every other source too (the issue-1342 cold-start check below).

## Issue-1342 acceptance (still valid)

- Every OBS-box receiver lists every managed sender within 5 s of OBS start: 10/10 cold starts on
  strih-lx and stream, plus one laptop that ran the `.ps1`.
  - The OBS-box senders come from the list, the camboxes from mDNS (issue 1389).
  - Resolume counts only when it was resolvable at the receiver's last provisioning run and its lease
    has not moved since (the traveling gap above). Otherwise it is found by mDNS only.
- A stock TV / unconfigured laptop still sees the strih program over mDNS.
- `avahi-browse -rtp _ndi._tcp` from dev1 still lists every sender (senders are unchanged), and shows
  0 `(30p)` sources.
- dev1 probes (optional, they run as `newlevel`): `mkdir -p ~/.ndi && cp
  scripts/ndi-discovery/ndi-config.v1.json ~/.ndi/`. dev1 had no `~/.ndi` config on 28.9.2026, so its
  probes are mDNS-only.
  - Part 1's `ndi-discovery-server` user unit shipped DISABLED and was never installed by code.
    If it was hand-installed on dev1, remove it:
    `systemctl --user disable --now ndi-discovery-server.service`, then delete
    `~/.config/systemd/user/ndi-discovery-server.service` + `~/.local/bin/ndi-discovery-server` and
    run `systemctl --user daemon-reload`.

## Rollback (back to mDNS-only discovery)

There is no gate to flip, because the config is receiver-only. Never roll back to the issue-1342
list that named the camboxes, and never put a list back on a cambox: it brings the camera-box aborts
back. The camboxes are already mDNS-only; to remove the config from the other boxes:

- strih-lx: remove `~newlevel/.ndi/ndi-config.v1.json`, `/etc/ndi/ndi-config.v1.json` and
  `/etc/systemd/system/intercom-hub.service.d/ndi-discovery.conf`, run `systemctl daemon-reload`,
  then restart `strih-obs.service` (user unit), `bkshading-service` and `intercom-hub`.
- Windows boxes and laptops: restore the `ndi-config.v1.json.bak-<stamp>` the `.ps1` left next to
  the config (or delete the file), then restart OBS. A backup written before issue 1389 still lists
  the camboxes: delete the file instead of restoring that one.
- A permanent rollback also removes the step 4b write, or the next provisioner run re-writes it.

## Laptop quick steps (owner-facing)

- **Windows:** as Administrator, run
  `powershell -ExecutionPolicy Bypass -File scripts\ndi-discovery-laptop.ps1`, then restart OBS.
  - `-DryRun` prints the result without writing.
  - The script merges into an existing config (the laptop's own listed IPs stay), removes every
    cambox IP (issue 1389), backs it up, and writes UTF-8 without a BOM. A BOM makes a JSON reader
    fall back to defaults; see the dantesync incident in `.claude/skills/ops`. Never hand-write the
    file with a PowerShell cmdlet that adds a BOM.
  - NDI Access Manager (NDI Tools) -> "Remote Sources" is the same list through a GUI. Never add a
    cambox there.
- **Linux:** `mkdir -p ~/.ndi && cp scripts/ndi-discovery/ndi-config.v1.json ~/.ndi/`, then restart OBS.
  If a config already exists, add the IPs to its `ndi.networks.ips` instead of overwriting it, and
  remove every cambox IP (`bash scripts/lib/ndi-discovery.sh --cambox-ips`).
