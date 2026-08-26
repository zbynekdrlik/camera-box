---
paths:
  - "scripts/network-reach-alert-watchdog.sh"
  - "scripts/lib/network-reach-health.sh"
  - "systemd/network-reach-alert-watchdog.*"
---

# strih/stream network-UNREACHABLE alert watchdog (#1001)

Closes the "a box fell fully off the network and nothing alerted" gap (strih NIC died 2026-08-06,
~50 min silent; recurred 2026-08-13). Every OTHER watchdog probes a box it assumes is UP (OBS-WS
`GetStats`, ssh INTO the box) and treats a total outage as "no probe output = nothing to decide",
so a dead NIC / powered-off box / unplugged cable is nobody's job.

## The one structural difference from every sibling watchdog: probe FROM dev1, do NOT ssh IN

`imag-obs` / `imag-power-envelope` / `optical-chain` all `sshpass ... ssh INTO` the target to gather
state — impossible when the target is off the network (the exact failure mode). This watchdog
instead probes the two boxes FROM dev1 (which is up when the target is down and sits on the same rig
LAN). That is the whole point — never "fix" it into an ssh-based probe.

## Multi-signal reachability — REACHABLE iff ANY of ping / :4455 / :8899

A box is UNREACHABLE only when ALL THREE dev1-local probes fail: ICMP `ping`, a TCP connect to
`:4455` (OBS WebSocket), and a TCP connect to `:8899` (bundle-state HTTP — both ports are live on
strih AND stream). Rationale: a Windows box frequently firewalls ICMP while fully up and answering
TCP, so ping-alone would false-page a healthy box; the real incident had all three dead ("No route
to host" everywhere). The pure classifier is `net_reach_classify_box <ping> <ws> <bundle>` in
`scripts/lib/network-reach-health.sh` (any value other than `1` = a failed signal, defensively).

## Reuse the shared dev1-side alert framework — never invent a second mechanism

Same shape as the three siblings: a `set -uo pipefail` (NOT `-e`) systemd `--user` timer, a PURE
decision lib, `airuleset.py notify` from dev1. Reuse `scripts/lib/obs-watchdog-decision.sh`:
`obs_watchdog_confirm` (2-pass confirm — a single full-LAN blip must never page) +
`obs_watchdog_alert_throttle` (~1h re-alert cadence). State is PER-BOX
(`confirm_<box>`/`alert_sig_<box>`/`alert_passes_<box>`/`alerted_<box>`) so strih and stream page
independently; a REACHABLE pass clears that box's confirm+throttle (a NEW outage pages fresh).

## The dev1-side-outage guard (reference anchor) — event-safe

Before deciding, ping the REFERENCE rig nodes (cam1/cam2/imag-nb — nodes sharing the rig's network
fate; `net_reach_any_reachable`). If NONE answer, dev1's own path to the rig subnet is down (or the
whole rig link stalled — e.g. an event-day tailscale-over-mobile uplink), so the pass is "nothing to
decide" and per-box state is left untouched — never a false "both OBS boxes down". This encodes the
ticket's own discriminator ("every OTHER rig node is up"). Timeouts are deliberately generous
(`ping -W2`, TCP 4s) so a slow-but-up event-day mobile link reads REACHABLE, not down.

## Recovery ping + install

`net_reach_recovery_decision <was_alerted> <now_reachable>` fires ONE "reachable again" ping when a
box we actually paged for returns (the `alerted_<box>` latch). No OBS-WS password is needed (the
:4455 check is a bare TCP connect, no handshake), so the `.service` has no `EnvironmentFile`. Install
on dev1 like the siblings: `systemctl --user enable --now network-reach-alert-watchdog.timer` (units
in `systemd/`). Runs entirely dev1-side; nothing is deployed to strih/stream.

## resolume — a REPORT-ONLY node (a traveling box, #811)

resolume-snv (RESOLUME-SNV, the CG box) is watched too, but it is a **traveling box normally
powered off/away between events** — paging on its (normal) absence would be pure false-alarm noise.
So it is a REPORT-ONLY node: `BOXES` default carries `resolume|10.77.9.201`, and
`NETWORK_REACH_REPORT_ONLY_BOXES` (default `resolume`) names it. A report-only box is probed,
classified, logged and per-box state-tracked exactly like strih/stream, but it **NEVER pages** (no
alert, no recovery ping) — `handle_box` gates both POSTs on `net_reach_box_is_report_only` (the pure
lib fn) and logs `[report-only] … NOT paging` instead. It is probed on **ping OR :4455 only** (the
:8899 bundle-state probe is skipped — resolume runs no bundle-state server). Its IP is **not
pinned** (`.201` is the current event-LAN lease and collides with `bridge`, an ACTIVE box, in
`targets.md`) — a wrong/colliding IP may log a FALSE `reachable` (e.g. `bridge` answering at `.201`
while resolume is off) or a false unreachable, but **never pages**; always confirm box identity with
`getent hosts resolume.lan` + its OBS profile (`.claude/rules/rig-state-inspection.md` §2) before
flipping it required.

**Flip it to a paging node** (once it becomes a permanent fixture, not before): remove `resolume`
from `NETWORK_REACH_REPORT_ONLY_BOXES` (it stays in `BOXES`) — it then pages like strih/stream, with
its `confirm_<box>` counter already warm (only the confirm counter advances while report-only; the
`alert_sig`/`alert_passes`/`alerted` latches are untouched until it actually pages, so the first
confirmed pass after the flip pages with no extra 2-pass grace). resolume's dantesync clock-discipline +
version-parity are a SEPARATE, standalone maintenance step (`.claude/skills/ops` §resolume-snv), not
this reachability watchdog.

## Out of scope (follow-up)

The 2026-08-13 comment also floats enabling Wake-on-LAN in strih/stream BIOS + Windows NIC power
settings for remote recovery — a separate hardware/BIOS task, not this software watchdog.
