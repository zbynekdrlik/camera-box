---
paths:
  - "scripts/lib/strih-egress-pacing.sh"
  - "systemd/strih-egress-pacing.service"
  - "tests/python/test_strih_egress_pacing_1553.py"
---

# strih / stream box sender egress pacing: an fq per tx queue, from a box fact (issue 1553)

25.11.2026 at PP: strih-pp's 10 GbE rig NIC (`ixgbe`, 16 tx queues) sends the program / multiview
NDI and JPEG XS bursts at line rate to receivers on slower switch ports (stream-pp 2.5G, grading-pp
1G), and the MikroTik egress queue of the slower port tail-dropped them. A hand-applied `mq` root +
`fq maxrate 900mbit flow_limit 4000 limit 20000` per tx queue made the switch drops 0; a reboot lost
it. This is the strih-box twin of `cambox-egress-pacing.md` (same pieces), design
issuecomment-7172341290. Sender only.

## The pieces

- **The fact** `STRIH_EGRESS_PACING_RATE` (the last key): `none` | `<N>mbit` | `<N>gbit` (N 1..999999,
  no leading zero, lower case). All four boxes `900mbit`, one value at both venues. The loader's check
  rides the housekeeping-CPU line of `strih-box-facts.sh` (that lib is at its 1000-line budget); a test
  pins its shape equal to `strih_egress_pacing_rate_ok`. fq's maxrate is a u32 in bytes/s, so a rate
  above ~34 Gbit/s is refused by `strih_egress_pacing_rate_bytes` at setup.
- **`scripts/lib/strih-egress-pacing.sh`** (sourced by strih-provision.sh) is the ONE declaration of
  the fq limits (`flow_limit` 4000, `limit` 20000). A test fails when any other `scripts/` or
  `systemd/` file spells `flow_limit 4000`.
- **The generated boot script** `/usr/local/sbin/strih-egress-pacing`: the rig NIC (from
  `strih_lx_rig_nic`, rename-aware) and the rate are baked in at setup time; the tx queue count is read
  from `/sys/class/net/<nic>/queues/tx-*` at boot. More than one queue = `root handle 1: mq` + one fq per
  class; one queue = a root fq (`mq` refuses a single-queue device: "Operation not supported"). Every
  step is a `replace`: a re-run keeps the same qdisc handles (measured in a netns, 7.0 kernel). A NIC not
  present yet is retried 30 x 2 s, then a loud FAILED line, exit 1 (the unit restarts); a failed tc step
  exits 2 and is never retried (`RestartPreventExitStatus=2`).
- **`systemd/strih-egress-pacing.service`** = `strih_egress_pacing_unit_text` byte for byte: oneshot,
  RemainAfterExit, After/Wants network-online, Restart=on-failure.
- **The NM dispatcher hook** `/etc/NetworkManager/dispatcher.d/61-strih-egress-pacing` (0755, the rig
  NIC baked in): an `up` of the rig NIC runs `systemctl --no-block restart strih-egress-pacing`, so a
  re-added adapter (the stream boxes' USB r8152) is paced at once, not at the next boot. Any other
  interface or action exits 0. (The housekeeping lib's `60-` hook is the same pattern.)
- **setup-strih step 11b** (`strih_egress_pacing_install`, one line, every role): write-if-changed
  (temp + rename; a private writer because `obs_box_write_if_changed` calls `fail()` and always chowns),
  daemon-reload on a script / unit change, enable, then `systemctl restart` (= the same idempotent
  `replace` script) only when the script or unit changed or the live tree is not the declared one. `none`
  removes OUR script + unit + hook only (a file of that name without the lib + `issue 1553` in its text is
  named and left, and verify never FAILs it); the live tree stays until a reboot, the step prints the
  `tc qdisc del` that removes it now.
- **verify-strih** (`strih_egress_pacing_grade_report`, on item 36's line: verify is at its budget):
  `(egress-pacing-install)` = script + unit + hook equal to their render, enabled; `(egress-pacing)` =
  `tc -j qdisc show dev <nic>` graded by `strih_egress_pacing_tree_verdict`; `none` = one NOTE, our
  leftover files FAIL.

## Gotchas

- **`tc ... parent 1:N` reads N as HEX.** A decimal loop over 16 queues sets 1:1..1:9 and 1:10 (=
  queue 16) and fails `Specified class not found` on 1:11..1:16, so queues 10-15 (1:a..1:f) stay on
  fq_codel, UNPACED. That is what the 25.11.2026 hand loop did if it counted in decimal. The script
  names each class `printf '1:%x'`; the verdict names every class that is not fq.
- **A `replace` of an existing `mq 1:` keeps it AND its children** (27.11.2026, strih-pp: `ethtool -L
  enp5s0f1 combined 4` from 16, the restart logged "4 tx queue(s)" but the fq under 1:5..1:10 stayed and
  verify FAILed `an extra qdisc under 1:5`; a grow would fail `Specified class not found`, exit 2). The
  boot script reads `tc qdisc show` first: our mq whose children are not exactly 1:1..1:<queues hex> is
  deleted (`tc qdisc del dev <nic> root`, logged), then rebuilt; an unchanged tree takes no delete and
  keeps its handles. The test's fake tc models that kernel rule.
- **`tc -j` prints fq `maxrate` in BYTES/s** (900mbit = 112500000); the human `tc qdisc show` prints
  `900Mbit`. The verdict compares bytes.
- **JPEG XS is paced with NDI here** (no cambox-style prio + fw exemption): `mq` takes no filter, and the
  program strih -> stream is exactly the burst onto a slower port. A 2 MB frame takes ~18 ms at 900 Mb/s,
  inside the receiver's fixed grid age.
- **A box without NetworkManager on the rig NIC** gets no hook run: a re-added NIC there stays on the
  default qdisc until the next boot, and verify FAILs it naming `systemctl restart strih-egress-pacing`.

## Supervisor live steps (the lane was code-only)

1. On each box, after the merge, run setup-strih (or the next genlock deploy): step 11b writes the
   files, enables the unit, and replaces the live tree once (strih-pp's hand tree is replaced by the same
   values; a dropped queued packet at most).
2. `sudo ./scripts/verify-strih.sh --box <box>`: both `(egress-pacing*)` rows PASS. By hand:
   `tc qdisc show dev <nic>` lists one fq per tx queue, every one `maxrate 900Mbit`.
3. Read the switch egress drop counters of the slower ports (read-only, companion-pp's own account at
   PP) over a production window: they stay 0.
4. Rollback: set the fact `none` and re-run setup-strih, then `tc qdisc del dev <nic> root`.

## Router entry (moved from CLAUDE.md, issue 1552)

- strih / stream box sender egress PACING (issue 1553: box fact `STRIH_EGRESS_PACING_RATE`, `scripts/lib/strih-egress-pacing.sh` = an fq per tx queue under a root mq (a root fq on one queue), classes named in HEX, `systemd/strih-egress-pacing.service`, setup-strih step 11b, verify `(egress-pacing*)`, `tc -j` maxrate in bytes/s) → `.claude/rules/strih-egress-pacing.md` (auto-loads on its `paths:`)
