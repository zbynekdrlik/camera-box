---
paths:
  - "scripts/lib/dantesync-gate.sh"
  - "scripts/dantesync-*.sh"
  - "scripts/drift-guard.sh"
  - "scripts/netcfg-audit.sh"
---

# A NIC swap silently poisons dantesync until its service is restarted (2026-08-27, strih)

Replacing a network card in a rig box does NOT just change the wire — it leaves **dantesync
running on a dead pcap handle**, which is invisible from every liveness check and only surfaces
as a DanteSync-gate abort or (worse) drifting A/V hours later.

## The failure shape

`dantesync` picks its PTP capture interface ONCE at service start
(`camera-box issue 1073: selected PTP capture interface \Device\NPF_{<guid>} (<ip>) — on the
trusted grandmaster subnet per gm_allowlist`). When that adapter is physically removed, libpcap
starts returning `ERROR_DEVICE_REMOVED` and dantesync **spams the error forever without ever
re-selecting an interface**:

```
[2026-08-27T10:50:50.972Z] Npcap recv error: libpcap error: The interface disappeared
  (error code ERROR_DEVICE_REMOVED/STATUS_DEVICE_REMOVED)
```

With PTP capture gone it silently degrades to plain userspace NTP against the configured internet
server and starts step-correcting the system clock roughly every burst:

```
[NTP] burst offset:+1535us ... [StepClock] Stepping by +1.686ms ... [NTP] Stepped +1686us
```

which the `[0/8]` DanteSync gate reports as:

```
strih  NTP STORM    (dantesync ntp_step_storm=true, 153 steps/hour past its 120/h alarm)
strih  PTP DEGRADED (NTP-only sawtooth — GM 10.77.9.184 down? latency meaningless)
```

**The "GM down?" hint in that message is a red herring here** — the grandmaster was reachable the
whole time (`Test-NetConnection 10.77.9.184` → True). The node lost its ability to *capture* PTP,
not its route to the master. Do not go hunting the grandmaster before checking the capture
interface.

## Diagnosis (in this order)

1. `curl -s http://<box>:8898/` — the live truth. A poisoned node shows `"ntp_step_storm": true`
   with a high `ntp_steps_last_hour` and is not in `"mode": "LOCK"`. A healthy one:
   `"is_locked": true, "mode": "LOCK", "gm_source_ip": "<gm>", "ntp_step_storm": false,
   "ntp_steps_last_hour": 0`.
2. Grep the service log (`C:\ProgramData\dantesync\dantesync.log` on Windows) for
   `ERROR_DEVICE_REMOVED` — its FIRST occurrence timestamps the moment the card was pulled.
3. Compare the adapter dantesync selected (`Found device: ... Some("<adapter name>")`) against
   what actually holds the rig IP now (`Get-NetIPAddress` / `Get-NetAdapter`). A name mismatch
   (e.g. `Marvell AQtion Felicity` in the log vs a live `Intel(R) Ethernet Server Adapter X520-2`)
   is the confirmation.

## Cure

`Restart-Service dantesync -Force` (Windows) / `systemctl restart dantesync` (Linux). Verify the
fresh init block names the NEW adapter and that multicast actually joined:

```
Initializing Npcap capture (default-interface hint: {<new-guid>})
Found device: \Device\NPF_{<new-guid>} (Some("<new adapter>"))
Using interface IP <rig-ip> for multicast join
Joined PTP multicast group 224.0.1.129 on ports 319 and 320
Npcap capture initialized with HIGH PRECISION synchronized timestamps
```

then re-read `:8898/` and confirm `mode: LOCK` + `ntp_step_storm: false`.

## Two traps that waste time here

- **`pcap_ntp_active: false` is NOT this bug.** That flag is the *NTP* kernel-timestamp transport,
  which is unavailable by design whenever the configured NTP server sits off the capturable subnet
  (dantesync#53 — `No Npcap-capturable interface can reach NTP server <ip> ... falling back to
  userspace rsntp`). It reads `false` on a perfectly healthy node; compare against an older log
  before treating it as a regression. The PTP-side proof is the `Joined PTP multicast group` line
  and `is_locked`/`mode` from `:8898/`.
- **Npcap binding is usually fine.** `Get-NetAdapterBinding -Name "<new adapter>"` showing
  `Npcap Packet Driver (NPCAP)  Enabled=True` does not exonerate the box — the driver binds the new
  adapter automatically, but the already-running dantesync process never looks again. Checking the
  binding first is the natural instinct and it answers the wrong question.

## Same class, different daemon: a NIC swap also wedges the strih NDI receivers

The same 2026-08-27 swap left strih's `NDI cam1` receiver holding one frozen frame indefinitely —
the barrier screenshot returned an IDENTICAL painter `frame_id` for 14 consecutive rounds over
~45 s while cam2/cam3/cam4 advanced normally, and `NDI cam1` was absent from the OBS log's
`recv-timing #797` lines entirely (the healthy inputs showed `n=300..301` per 5 s). The source box
was emitting 60 fps, the sender was in the finder, and `ndi_source_name` was correct — the receiver
THREAD was dead. `obs_phase2.py idle-receiver` + `--restore` did NOT revive it (the
`distroav-receiver-lifecycle.md` class: `break` never clears `s->running`); only an OBS restart
did. So after any NIC change on strih/stream, verify each camera's painter tick actually ADVANCES,
not merely that the input exists and renders something.

## Whose job

The manual restart is a rig step for the SUPERVISOR, never a worker. The durable fix belongs in
the dantesync repo: on `ERROR_DEVICE_REMOVED`, re-run the `gm_allowlist`-based interface selection
instead of spamming forever. Tracked from camera-box issue 1130's own thread.
