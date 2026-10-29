---
paths:
  - "scripts/lib/cambox-egress-pacing.sh"
  - "systemd/cambox-egress-pacing.service"
  - "tests/python/test_cambox_egress_pacing_1242.py"
  - "tests/python/test_cambox_egress_pacing_apply_1387.py"
---

# cambox NDI egress pacing — fq maxrate on the default-route interface, made permanent (issue 1242)

## What and why

All seven cameras hand their frame to NDI on the same genlock grid instant. About 7 x 300 KB then
reach strih-snv at the 5 Gb/s line rate in ~3-4 ms, and its RTL8157 USB NIC answers with PAUSE
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

## The JPEG XS flow is exempt: a classful root (issue 1387)

The fq cap applied to EVERY flow, the JPEG XS UDP flow included. Live 29.10.2026 on cam5:
- its slices spread 23 ms at 4 bpp and 33 ms at 6 bpp, instead of 8-12 ms at line rate;
- the first arrival wandered 18-45 ms, the queue draining the previous frame;
- at 8 bpp it built a 162 ms backlog.

**No socket option can lift the cap.** fq paces a flow at the SMALLER of the socket's pacing rate
and `maxrate`, so a socket can only go slower. The JPEG XS flow has to leave the fq. The tree, built
by the lib's apply command (four `replace` steps chained with `&&`, all idempotent):

```
tc qdisc replace dev <if> root handle 1: prio bands 2 priomap 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
tc qdisc replace dev <if> parent 1:1 handle 10: fq maxrate 400mbit flow_limit 2000 limit 20000
tc qdisc replace dev <if> parent 1:2 handle 20: pfifo limit 4000
tc filter replace dev <if> parent 1: protocol all prio 1 handle 0x1387 fw classid 1:2
```

- **Every priority maps to band 0.** The classic prio priomap would split ordinary traffic across
  bands. With this one, only the `fw` filter moves a packet, so NDI, PTP/NTP and the intercom reach
  the SAME fq as before.
- **The JPEG XS socket carries `SO_MARK 0x1387`.** camera-box sets it in `JxsSocket::connect`
  (`src/jxs_send.rs` `JXS_SOCKET_MARK`, pinned equal to `CAMBOX_EGRESS_PACING_JXS_MARK` by pytest).
  It needs CAP_NET_ADMIN or CAP_NET_RAW; camera-box.service runs as root. A refused mark logs one
  WARN with the errno, and the "link ON" line says `egress mark NONE: paced with NDI`.
- **A mark, not a port.** The boot script cannot know the runtime `CAMERA_BOX_JXS_TARGET` port, and
  the camboxes have no `ip rule` with a fwmark (read on cam3).
- **Strict priority, band 0 first.** A ready NDI or PTP packet always leaves the QDISC before JPEG
  XS. While fq holds a paced packet back, its dequeue returns nothing, so prio sends JPEG XS and it
  fills the idle wire.
  - **The residual is the driver's TX ring, not the qdisc.** The qdisc hands packets to the ring up
    to the BQL limit. One JPEG XS GSO skb is up to ~59 KB, ~0.5 ms at 1 GbE. So an NDI or PTP packet
    can wait behind up to a BQL's worth of JPEG XS bytes. A PTP request is software-timestamped
    before that wait. The live acceptance below measures it.
  - The rejected opposite, a JPEG XS band ABOVE the fq, would put PTP behind a whole 8-12 ms JPEG XS
    burst.
- **The pfifo limit 4000** is above the ~1800 datagrams a 4 MB socket send buffer holds in the
  one-send-per-datagram fallback, so the socket buffer, never the FIFO, bounds the queue.
- **Measured on a real 7.0 kernel** (dev1, `sudo -n unshare -n`, a dummy device; the camboxes run
  7.0.0-38, `sch_prio` + `cls_fw` ship with it):
  - 400 unmarked 1400 B datagrams take 9.3 ms (paced) and land in `1:1`;
  - 400 marked ones take 0.9 ms and land in `1:2`;
  - a 3000-packet paced flow keeps its 84.8 ms with a 700-packet marked burst sent in 1.7 ms during it;
  - `SO_PRIORITY 1` without the mark stays in `1:1`;
  - the generated boot script turns the old root fq into the tree, a re-run changes nothing, and
    the verdict grades that output OK;
  - a root `JxsSocket` carries the mark.
- **Exactly one classifying filter on the root** (review round 1): the verdict lists every filter
  entry on parent `1:` that carries a `classid` / `flowid` (u32 prints `*flowid`) and allows only
  the fw one on the mark to `1:2`. Any other (an extra u32 / matchall, a second fw handle) moves
  traffic past the fq: DRIFT `... moves traffic past the fq -- that traffic is UNPACED`. The fixtures
  are real kernel output. A packet whose skb priority is a class id of `1:` itself (the prio direct
  path) cannot be seen from tc output; nothing on a cambox sets one.
- **A failed chain falls back to the old root fq, and is never retried** (review rounds 1-2). A
  half-built tree (prio over default pfifos) would leave NDI unpaced, so the apply command puts
  `root fq maxrate ...` back at once. Its status says what happened:
  - 1 = no default route yet, the only failure a retry can cure;
  - 2 = a tc step failed, and the root-fq fallback is in place;
  - 3 = the fallback failed too: `the NDI egress may be UNPACED`.
  A failed tc step fails the same way every time. Each retry would replace the root twice (the
  tree, then the fallback), dropping what the old root had queued, every 2 s, forever. So the boot
  script makes ONE attempt for 2 / 3 and always puts that step's line in the journal:
  - 2: exit 0, staying on the fallback (the unit stays `active`, no restart loop; `(ap)` reads
    `DRIFT root qdisc is fq, the pre-issue-1387 tree`);
  - 3: exit 1 at once.
- **Every non-fw classifier on the root is DRIFT**, once per pref, whatever it prints (review round
  2): a classic bpf or a cgroup classifier picks its class itself and prints no classid
  (real-kernel fixture `EXTRA_BPF`).
- **Rejected alternatives:** a u32 filter on the UDP destination port (a second declaration the boot
  script cannot see) and `SO_PRIORITY` through the priomap alone (implicit: any socket that sets that
  priority goes unpaced, and the tree does not show it).
- The test fixtures `TREE_QDISC` / `TREE_FILTER` are that real kernel output; re-capture them the
  way their comment says when the declaration changes.

## The pieces (main design 5905959484, Approach 1)

- **`scripts/lib/cambox-egress-pacing.sh` is the ONE declaration** of the rate, the two limits, and
  (issue 1387) the JPEG XS mark, the FIFO limit and the priomap.
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
    - the tree (`cambox_egress_pacing_verdict QDISC FILTER`, the gather reads `tc qdisc show` AND
      `tc filter show`):
      - the root is `prio 1:` with 2 bands and the all-zero priomap;
      - `1:1` is an `fq` with the declared maxrate (compared in bit/s: `400mbit` == tc's `400Mbit`),
        flow_limit and limit;
      - `1:2` is a `pfifo` with the declared limit;
      - a `fw` filter on parent `1:` whose handle (compared as a number) is the mark, to `1:2`.
    - DRIFT names every fault at once: the NDI side ("the NDI egress is UNPACED", or a wrong value)
      and the JPEG XS side ("JPEG XS ..."). The pre-1387 root fq is DRIFT: NDI paced, but the JPEG
      XS class is missing.
  - When the qdisc facet fails, its `fix:` follows the state:
    - no default route: a network problem, not provisioning;
    - tc missing: iproute2;
    - install fine + unit `inactive`: it never ran since the install, `systemctl start` (or the next
      reboot);
    - install fine + unit `active`: the qdisc changed after it ran, `systemctl restart`;
    - install stale: re-provision, then `systemctl restart` (never `start`: a `start` of an `active`
      RemainAfterExit oneshot re-applies nothing);
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
   <if> prio 1: -> 1:1 fq maxrate 400Mbit flow_limit 2000p limit 20000p (NDI paced) + 1:2 pfifo
   limit 4000p via fw mark 0x1387 (JPEG XS at line rate) + cambox-egress-pacing.service installed +
   enabled`. The next release E2E `[0/8]` prints one `ok:` line per cambox.
   - A box provisioned but not yet started whose hand-applied qdisc was lost FAILs with
     `fix: ... systemctl start cambox-egress-pacing`: start it (step 2), never re-provision for that.
   - **Changing the rate or a limit** later: edit the lib, then re-run setup-device on every box.
     Until then `(ap)` names each box's boot script as stale.
4. **Issue 1387: bring every provisioned box to the classful tree, without a setup-device re-run.**
   The boot script changed, so until then `(ap)` FAILs every box: `the boot script ... is stale` and
   `root qdisc is fq, the pre-issue-1387 tree -- JPEG XS class 1:2 missing`. Run it when no E2E /
   soak holds the rig lease (the no-cambox-touch-while-lease rule), one box at a time:
   - on dev1, generate the one program for every box:
     `apply="$(mktemp)"; bash -c '. scripts/lib/cambox-egress-pacing.sh; cambox_egress_pacing_apply_program' > "$apply"`;
   - per box: `sshpass -p "$DEVICE_ROOT_PW" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
     -o ConnectTimeout=10 root@<box> bash -s < "$apply"`;
   - what it does (`cambox_egress_pacing_apply_program`, `tests/python/test_cambox_egress_pacing_apply_1387.py`):
     - `PLAN (<box>): root ro; boot script write; unit in-place; enabled`: reads first, writes only
       what differs;
     - the boot script (and the unit, the enable when missing) inside ONE rw window, closed by the
       verified `ro_window_close_cmds` (`.claude/rules/ro-window.md`, the sites table). A busy root
       FAILs naming the writer and restarts nothing; never reboot a cambox for it;
     - after the close: `daemon-reload`, `systemctl restart cambox-egress-pacing` (the unit runs the
       new boot script, the `replace` chain). Replacing the root qdisc drops what the OLD root still
       had queued, once: at most one NDI frame's tail;
     - the read-back: `TREE (<box>, <if>):` with the live `tc qdisc show` + `tc filter show`, then
       `READBACK (<box>): OK prio 1: -> 1:1 fq maxrate 400Mbit ... + 1:2 pfifo limit 4000p via fw
       mark 0x1387`, graded by THIS lib's verdict (embedded). Exit 0 only on OK;
   - `rm -f "$apply"` afterwards;
   - grade: `./scripts/verify-device.sh <CAMn>` `(ap)` passes; by hand on the box:
     `tc -s qdisc show dev <if>` (the `1:2` pfifo's `Sent` grows only while the JPEG XS link is on;
     the `10:` fq's `throttled` keeps counting) and `tc filter show dev <if>` (`handle 0x1387
     classid 1:2`);
   - then the JPEG XS rerun at 4 and 6 bpp (`.claude/rules/jxs-link.md`): the receiver's `spread_ms`
     should drop from 23 / 33 ms to ~8-12 ms, `first_arrival` stop wandering;
   - **live acceptance that NDI and the clock are unchanged** (the netns run proves the tc
     semantics, not the rig; JPEG XS at line rate is a new burst source into strih-snv). Read each
     twice on the box under test, ~10 min with the JPEG XS link OFF and ~10 min with it ON:
     - dantesync `curl -s http://<box>:8898/status`: the offset and jitter must not grow with the link
       ON (the ring-wait residual above);
     - the NDI receive side on strih-snv: the `genlock-fifo audit` line for that camera's input (the
       `received=` rate, `relock` / `underrun` deltas) and the strih NIC PAUSE / receive-gap counters,
       compared OFF vs ON;
     - the box's `Streaming:` line stays 60.0 sent with no capture drop;
     - then one release E2E: its continuity read (copies / gaps per window) must stay clean.
     Record the numbers on the ticket. The fleet rollout counts as done only when every box passes
     `(ap)` (`verify-fleet.sh` + `--venue pp`) and this acceptance is clean on the first box.
   - rollback of this step only: put the previous lib's boot script back the same way (a
     re-provision from the previous commit), or by hand `tc qdisc replace dev <if> root fq maxrate
     400mbit flow_limit 2000 limit 20000` (that runtime state lasts until the unit runs again).
5. **Rollback** of the whole pacing, on the box, in this order:
   - `systemctl stop cambox-egress-pacing` FIRST: a unit inside its restart loop would otherwise
     re-apply the qdisc;
   - `tc qdisc del dev <if> root`: back to the default qdisc at once, as before 30.9.2026;
   - `systemctl disable cambox-egress-pacing` inside a `mount -o remount,rw /` ...
     `mount -o remount,ro /` window: the root is read-only and `disable` removes a symlink under `/etc`.

## Test notes

- `test_cambox_egress_pacing_apply_1387.py` runs the live apply program on the issue-1407 fake
  read-only-root box, with a fake `tc` that prints the tree only after the fake unit restart. A lib
  mutation run killed 7 of 8 mutants; the survivor (the early "root is fq" branch disabled) is
  equivalent: the next branch grades a non-prio root DRIFT too.

- The tests run fully offline: fake `ip` / `tc` / `systemctl` / `sshpass` on PATH, and the real cam7
  `tc qdisc show` output as the fixture.
- The setup-device sub-step and verify-device `(ap)` are SLICED out of the real scripts and RUN, not
  only grepped. The (ap) slice ends at the next check marker `# (af) `.
- **Never put a new verify-device check between `(ao)` and `(q)`.** pytest executes the
  `(ao)..(an)` and `(an)..(q)` slices of verify-device.sh with only their own lib sourced
  (`tests/python/test_bkshading_relay_gaps_808.py`, `tests/python/test_ndi_discovery_1342.py`).
  A new block there calls an undefined function under `set -e`, and its `warn` would trip their
  `'warn "' not in block` assertions. `(ap)` therefore sits right after `(ae)`.
