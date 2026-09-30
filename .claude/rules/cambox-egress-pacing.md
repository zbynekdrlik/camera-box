---
paths:
  - "scripts/lib/cambox-egress-pacing.sh"
  - "systemd/cambox-egress-pacing.service"
  - "tests/python/test_cambox_egress_pacing_1242.py"
---

# cambox NDI egress pacing — fq maxrate on the default-route interface, made permanent (issue 1242)

## What and why

All seven cameras hand their frame to NDI on the same genlock grid instant. About 7 x 300 KB then
reach strih-lx at the 5 Gb/s line rate in ~3-4 ms, and its RTL8157 USB NIC answers with PAUSE
storms (issue 1242 finding 5902816575).

- The owner approved staggered sending (5904641502). Delaying the frame hand-off failed live twice,
  because the NDI SDK encodes inside the send.
- So each cambox paces its KERNEL egress instead (decision 5904673375):
  `tc qdisc replace dev <default-route if> root fq maxrate 400mbit flow_limit 2000 limit 20000`.
- `fq` paces per FLOW: dantesync's PTP/NTP and the intercom are their own flows, never queued behind
  a video frame. A single `tbf` bucket would delay them and bias the clock. `flow_limit` 2000 holds a
  whole frame (~240 packets).
- Measured with it live on cam1-cam7 (result 5905948208): PAUSE 39-110/min -> 28-36/min, receive-gap
  storms from ~1 per 5 min to ~1 per 30 min. It helps, it is not the cure: 10 GbE through a PCIe NIC
  (issue 1387) is.

## The pieces (main design 5905959484, Approach 1)

- **`scripts/lib/cambox-egress-pacing.sh` is the ONE declaration** of the rate and the two limits.
  `tests/python/test_cambox_egress_pacing_1242.py` fails when any other file under `scripts/` or
  `systemd/` spells a `maxrate <number>` or `flow_limit 2000`. Change the value there, nowhere else.
- **The appliance has no checkout of this repo**, so the boot unit cannot source the lib.
  setup-device.sh writes the GENERATED script `/usr/local/sbin/cambox-egress-pacing`
  (`cambox_egress_pacing_boot_script`), which embeds the lib's apply command verbatim.
- **`systemd/cambox-egress-pacing.service`** (checked in) is a oneshot with RemainAfterExit,
  After/Wants `network-online.target`. It runs that script at every boot.
  - The interface is resolved at run time: the `dev` of the FIRST `ip route show default` line (the
    lowest-metric route). An enp name, or an enx after a rename, both work.
  - The retry is 30 attempts x 2 s for a late default route. It is an attempt COUNT, never a
    wall-clock deadline: the rig's dantesync date master can step the date while a box boots.
  - After the last attempt the script prints a loud `FAILED ... UNPACED` line and exits 1. Only the
    first and the last attempt of a round log their reason (`no default route yet`, a tc failure),
    so a route-less box does not flood the journal.
  - `Restart=on-failure` + `RestartSec=30` + `StartLimitIntervalSec=0` then start a new round every
    ~90 s, forever. A box that boots before its switch port has carrier (a rig cold start, a box
    moved while running) is paced as soon as the default route appears, never left unpaced for its
    whole uptime. systemd 255 (the camboxes' version) accepts `Restart=` on a oneshot.
  - `TimeoutStartSec=120` sits above one round, so the FAILED line is the script's own, never a kill.
  - `WantedBy=multi-user.target` makes the target wait for the FIRST round, so a route-less boot
    holds `multi-user.target` (and `cpu-performance.service`, ordered after it) for ~60 s. Accepted:
    such a box is not streaming anyway.
- **setup-device.sh `[egress-pacing]`** (after the DSCP oneshot, in the rw window before STEP 18):
  writes the script, installs the unit from `../systemd/`, `daemon-reload`, `enable`, and a literal
  `is-enabled == enabled` check. It is ENABLE-ONLY: never a live start, never a live `tc` apply.
  - A pre-flight right after the confirm prompt, BEFORE `ensure_root_writable`, refuses the run when
    only `scripts/` was staged on the box. Stage `scripts/` AND `systemd/` together (the provision
    skill's `git archive HEAD scripts systemd` recipe does).
- **verify-device.sh `(ap)`** is a HARD gate. One read-only ssh round trip reads the live root qdisc,
  the unit state, and what setup-device installed. `cambox_egress_pacing_provision_verdict` grades:
  - INSTALL facets (the fix is a re-provision):
    - the boot script is executable, starts with `#!/bin/bash`, and equals what the lib generates. A
      stale copy (the rate changed in the lib, the box was never re-provisioned) FAILs now, not at
      the next reboot;
    - the installed unit equals the checked-in `systemd/` one;
    - both are compared as a sha256 of their FUNCTIONAL lines (`cambox_egress_pacing_functional_lines`
      drops comment and blank lines; the gather embeds the same function with `declare -f`). A
      comment-only edit in this repo therefore keeps every box current, while a functional edit (a
      rate, an ExecStart, RestartSec) makes every box stale until it is re-provisioned;
    - the unit is `enabled`. A hand-applied runtime qdisc alone FAILs: that is exactly the
      non-permanent state this ticket fixes.
  - RUNTIME facets (the fix matches the state):
    - the unit is not `failed` (read its journal);
    - the root qdisc is `fq` with the declared maxrate (compared in bit/s: `400mbit` == tc's
      `400Mbit`), flow_limit and limit.
  - When the qdisc facet fails, its `fix:` follows the state:
    - no default route: a network problem, not provisioning;
    - tc missing: iproute2;
    - install fine + unit `inactive`: it never ran since the install, `systemctl start` (or the next
      reboot);
    - install fine + unit `active`: the qdisc changed after it ran, `systemctl restart`;
    - unit `activating`: a DRIFT read proves a default route exists, so the unit is failing to apply
      the qdisc (tc refusing it, e.g. a kernel without `sch_fq`) or is between rounds; the hint
      names the failed-round count (`NRestarts`) and the unit journal. With restart-forever the unit
      is `activating`, not `failed`, while this goes on.
  - `active` is NOT required. setup-device is enable-only and a cambox is never rebooted remotely,
    so a freshly provisioned box stays `inactive` until a physical reboot while its runtime qdisc is
    live.
  - Each failed facet is one `FAIL: <what> -- <fix>` line. `cambox_egress_pacing_verdict_oneline`
    joins them for the `(ap)` FAIL line and the E2E WARNING line.
- **E2E `[0/8]`**: `cambox_egress_pacing_e2e_report "$LEG_HEALTH_TARGETS" "$CAM_PW"`, one call line
  after the capture-leg loop. It is REPORT-ONLY:
  - one bounded read-only ssh gather per vetted cambox (`LEG_HEALTH_TARGETS` = the source box plus
    every box the fleet preflight vetted, derived from `CAMERA_ACTIVE_SET`);
  - an acked box is skipped;
  - `ok:` or a named `WARNING:` line per box, then a summary line;
  - it always returns 0. It can be made blocking once the fleet is provisioned.

## Supervisor runbook (this lane was code-only: no box was touched)

Run it when no E2E / soak holds the rig lease (`curl -s http://127.0.0.1:8890/rig-lease.json` on dev1).

1. **Provision each cambox** (cam1-cam7) with `setup-device.sh`, per `.claude/skills/provision`.
   It installs and ENABLES the unit, and never starts it.
2. **Make the unit read `active` without a reboot (optional).** On a box, `systemctl start
   cambox-egress-pacing` re-applies the SAME qdisc the box already runs by hand. `tc qdisc replace`
   is idempotent and was applied live on 30.9.2026 with no side effect.
   - Never reboot a cambox remotely for this (the never-remote-reboot-a-cambox rule).
   - The next physical reboot runs it anyway.
3. **Read back:** `./scripts/verify-device.sh <CAMn>`. `(ap)` must pass: `NDI egress pacing live:
   <if> root fq maxrate 400Mbit flow_limit 2000p limit 20000p + cambox-egress-pacing.service
   installed + enabled`. The next release E2E `[0/8]` prints one `ok:` line per cambox.
   - A box provisioned but not yet started whose hand-applied qdisc was lost FAILs with
     `fix: ... systemctl start cambox-egress-pacing`: start it (step 2), never re-provision for that.
   - **Changing the rate or a limit** later: edit the lib, then re-run setup-device on every box.
     Until then `(ap)` names each box's boot script as stale.
4. **Rollback**, on the box, in this order:
   - `systemctl stop cambox-egress-pacing` FIRST: a unit inside its restart loop would otherwise
     re-apply the qdisc;
   - `tc qdisc del dev <if> root`: back to the default qdisc at once, as before 30.9.2026;
   - `systemctl disable cambox-egress-pacing` inside a `mount -o remount,rw /` ...
     `mount -o remount,ro /` window: the root is read-only and `disable` removes a symlink under `/etc`.

## Test notes

- The tests run fully offline: fake `ip` / `tc` / `systemctl` / `sshpass` on PATH, and the real cam7
  `tc qdisc show` output as the fixture.
- The setup-device sub-step and verify-device `(ap)` are SLICED out of the real scripts and RUN, not
  only grepped. The (ap) slice ends at the next check marker `# (af) `.
- **Never put a new verify-device check between `(ao)` and `(q)`.** pytest executes the
  `(ao)..(an)` and `(an)..(q)` slices of verify-device.sh with only their own lib sourced
  (`tests/python/test_bkshading_relay_gaps_808.py`, `tests/python/test_ndi_discovery_1342.py`).
  A new block there calls an undefined function under `set -e`, and its `warn` would trip their
  `'warn "' not in block` assertions. `(ap)` therefore sits right after `(ae)`.
