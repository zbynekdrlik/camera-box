---
paths:
  - "scripts/lib/strih-dante-input.sh"
  - "scripts/dante_input_continuity.py"
  - "scripts/lib/program-audio-continuity.sh"
  - "tests/python/test_strih_dante_input_1485.py"
  - "scripts/lib/strih-dante-rx-pin.sh"
  - "tests/python/test_strih_dante_rx_pin_1485.py"
---

# strih-snv's program audio over Dante: the obs release's `dante_input` source (issue 1485)

Owner 2.11.2026: each application is its own Dante device; strih-snv only, PP and the stream role keep
obs-vban. The FOH program (TX `master-asio-dante`, stereo, L then R) reaches the strih OBS through the
Dante device `strih-obs` that the obs release's `dante-input.so` runs (dstack's dlink).

- **Which box:** the fact `STRIH_DANTE_DEVICES` names `strih-obs=arc:cmc:flows:rxfirst-rxlast`
  (`strih_program_dante_declared`). The loader then requires the VBAN program facts `none` and keeps
  `STRIH_PROGRAM_AUDIO_INPUT` (`ASIO zvuk`) as the input's name; a box with both is refused. The leg's
  sender keeps only the hub's 6980.
- **The file:** `~/.camera-box/dante-input.toml` of the OBS user, read ONCE at OBS start (no file = no
  device and no source kind). setup-strih step 13 writes it after the leg (`strih_dante_input_install`):
  name `strih-obs`, iface = the leg's (`enp7s0.70`), `network_rate = 96000`, no `sample_rate`,
  `latency_ms = 10`, the fact's ports, one `[[subscribe]]` per RX channel. Another box removes a file
  carrying our header and leaves an operator's own.
- **Site data is never in git (the repo is public):** the per-box untracked
  `/etc/camera-box/dante-input.site.conf` (`STRIH_DANTE_INPUT_SITE_FILE`), placed by the supervisor:
  `RX<n>=<rx label>|<tx device>|<tx channel>`, 1-6 or 8 channels, contiguous. Missing or malformed on a
  Dante box = setup fails by name.
- **The seed:** the strih manifest carries `"program_audio_dante": {"device": "strih-obs"}`;
  `strih_scenes.py` switches `ASIO zvuk` to `dante_input` (no settings) keeping the operator's mixer
  state; an OBS without the kind registered keeps the input.
- **verify item 9** (`strih_program_audio_grade` dispatches): `(program-dante)` rows = the TOML, the OBS
  input kind, the `dante-input start: config=` line (`device_rate=96000` and `converts=true` from obs v0.4.1 on, dstack #90's `Rated` sinc
  converter; `halved=true` on v0.4.0 and older, graded by whichever key the line carries), any
  `dante-input error:` line, and two `dante-input status:` lines 12 s apart (clock=media, holdover 0, a
  grandmaster, delivered_frames advancing; any of the gate's loss counters GROWING between the two lines
  FAILs, a total since the start that holds still is only named). The `(vban-leg) vban` arrival
  row is a NOTE on a Dante box. PipeWire stays purged (`dante:strih-obs` counts as a direct program).
- **The E2E gate:** `program-audio-continuity.sh` also marks the strih OBS log (through `strih-log-read.sh`)
  when `$STRIH` is a Dante box, and folds `scripts/dante_input_continuity.py`'s window result into the
  `program_audio_continuity` block (`strih_dante`, the worse verdict, `strih Dante:` reasons, so the
  report needs no second key): any rx_loss / rx_holes / underruns (skipped blocks) / rx_gaps / socket_drops /
  rebootstraps / rx_written_late / re_anchors / obs_dropped (v0.4.0) increase (the obs project's meanings, 3.11.2026: audio other
  than FOH sent; rx_late, overruns, late_blocks are timing only and reported; a line before obs v0.3.0 has
  no rx_holes and reads UNKNOWN; in v0.2.0 `underruns` mixed late blocks with skips, so it proves nothing), clock=free or holdover=1, or a `dante-input error:` in the window = FAIL; no
  opening line, a restart, a counter backwards, no delivered frames = UNKNOWN = FAIL. The closing line is
  the second status line after the stop.
- **The Dante threads' own CPU (`scripts/lib/strih-dante-rx-pin.sh`, 3.11.2026, the obs project's ask
  beside their v0.4.0).** dlink's pump thread `dstack-engine` wakes every 0.33 ms; among ~10 runnable OBS
  threads per CPU at SCHED_OTHER nice 0 its blocks finished up to 6.7 ms late against a 5 ms margin and
  audio was skipped (underruns, rebootstraps). So on a Dante-program box only:
  - **The CPU is derived:** the highest CPU that is online AND in `/etc/strih-isolated-cpus.conf`
    (`strih_dante_rx_pin_plan`); strih-snv = CPU 15, OBS on 2-14. Its SMT sibling (14 on strih-snv,
    written to the conf as `smt_siblings`) stays OBS's: the two threads get their own run queue (the
    wake-up latency is the fault), not a whole physical core. Taking 14 too would cost OBS another CPU;
    re-measure before doing it.
  - **The mask file stays the full block.** The background slice (`rig-background.slice` = online minus
    that file, 0-1) and the baseline `affinity` row read it unchanged. setup-strih step 13
    (`strih_dante_input_install`) writes `/etc/strih-dante-rx-cpu.conf` (`reserved=`, `obs=`, `base=`,
    `smt_siblings=`); `strih-obs-start.sh` launches OBS under `obs=` only while `base=` equals the mask in
    use (a stale file is named and ignored; no file = every other box, unchanged).
  - **nice -10 by a root helper, never a limits.d grant.** `/usr/local/sbin/strih-dante-rx-pin`, allowed
    by ONE sudoers line with no arguments (`... NOPASSWD: /usr/local/sbin/strih-dante-rx-pin ""`, checked
    by `visudo -cf` before the write), the obs-box-background-run pattern (issue 1387). A limits.d nice
    grant is a session-wide rlimit like the retired `95-<box>-genlock-rtprio.conf` (obs-box-baseline.md)
    and would let every thread of every process of the user lower its nice. No realtime policy anywhere;
    rtprio stays off.
  - **The helper:** the newest `obs` of the sudo caller (`pgrep -n -x -u $SUDO_UID`), only its tasks whose
    `comm` is `dstack-engine` or `dstack-rx`; `taskset -p -c <cpu>` + `renice --priority -10` (absolute)
    per thread, read back from `/proc`; waits up to 120 s for the threads (they start when OBS loads the
    module); one log line (`obs pid P: dstack-engine tid T, dstack-rx tid U -> CPU 15 nice -10`, also to
    the journal via `logger -t strih-dante-rx-pin`). Idempotent. No env seam (a root program takes no path
    from its caller); its tests rewrite its `PATH=` / `PROC=` / `BUDGET_S=` / `CONF=` lines.
  - **Every OBS start:** the launcher runs it in the background right after the launch (`sudo -n`), so an
    OBS restart by the unit re-pins. A failure is a WARN in `/tmp/strih-obs-start.log`, never fatal.
  - **Exclusive since the A/B of 3.11.2026 (`scripts/lib/strih-dante-reserved-cpu.sh`).** The rx pin
    alone kept only OBS's MAIN mask off CPU 15: PID 1 and everything it forked had 0-15 (bkshading, the
    hub, Xorg, dsync, the session apps), and the program-audio sampler on 0-15 cost the engine a
    rebootstrap in every 10-15 min window (moved live to 0-1: a 15.5-min window clean). Now:
    - **ONE manager-level default, a Dante box only:** step 13 (`strih_dante_reserved_cpu_install`,
      right after the rx pin) writes `/etc/systemd/system.conf.d/` AND `/etc/systemd/user.conf.d/`
      `50-strih-dante-reserved-cpu.conf` = `[Manager] CPUAffinity=<online minus reserved>` (0-14), so
      every unit and every forked process of both managers starts off CPU 15, units we do not own
      included (the camboxes' `cambox-housekeeping-cpus.sh` precedent, plus the user manager). Rejected: a per-unit `CPUAffinity=` drop-in (every new unit must remember it, foreign
      units stay on 15) and a cpuset `AllowedCPUs=` (a cpuset BOUNDS sched_setaffinity, so the helper
      could no longer put the dstack threads there).
    - **Affinity, not a bound:** `sched_setaffinity` is limited by the cgroup cpuset and the online CPUs
      only, never by a parent's or the manager's mask, so the helper's `taskset -p -c 15` still works
      (pinned by a test that widens a narrowed mask) and OBS's `taskset -c 2-14` is a subset.
    - **A manager takes it when it re-reads its configuration, at the latest at the next boot** (PID 1
      may already on a daemon-reload; not verified on a box, so nothing relies on it). Every process
      started after that inherits it; running processes keep their masks until they restart. Setup never
      daemon-reexecs and never moves a live process; after a deploy the supervisor reboots strih-snv (or
      moves the running ones: `taskset -a -p -c 0-14 <pid>`), then reads `grep Cpus_allowed_list
      /proc/1/status` and verify item 9.
    - **The sampler is placed off it too** (`strih_program_audio_cpus`, `program-audio-guard.md`).
    - **verify item 9's `(program-dante) reserved-cpu` rows** (`strih_dante_reserved_cpu_grade`, before
      the rx-pin rows): the two drop-ins byte-identical to the render, and a /proc scan (bash builtins,
      no fork per task) that FAILs every task allowed on the reserved CPU except OBS's two dstack threads
      and kernel threads (PF_KTHREAD), named by comm with a count (first 8, then `(+N more)`). Another
      process's own `dstack-*` threads (the hub's) are offenders. While the drop-ins are NEWER than the
      boot (`btime` in /proc/stat vs the system drop-in's mtime: a deploy before its reboot) the same
      list is a NOTE `reboot pending`, so step 17 and the genlock deploy's acceptance verify do not fail
      on it; after any later boot it is a FAIL. Missing or different drop-ins are never pending. A task
      that exits between its `status` and its `stat` read is skipped; a process name is counted as text
      (any user sets it, verify runs as root), never inside an arithmetic subscript.
    - Another box: step 13 removes our two drop-ins (header-checked), never a foreign file.
    - A thread the two dstack threads create later inherits their mask and nice.
  - **verify item 9's `(program-dante) rx-pin` rows** (`strih_dante_rx_pin_grade`, in the lib; verify-strih
    is at its line budget): FAIL when OBS's main `Cpus_allowed_list` still holds the reserved CPU, when a
    thread is missing, runs on anything but exactly that CPU, or has nice > -10; no conf / no running OBS
    FAIL too.
  - Another box: step 13 removes our conf, helper and sudoers line if present, silent when absent.
  - **The strih JPEG XS program encoder too (3.11.2026):** `jxs_cores` empty = every ONLINE CPU (vstack's
    `affinity::online_cores`, an explicit sched_setaffinity, so the OBS mask does not bound it): its
    `jxs-feed` / `jxs-send` threads ran on CPU 15. Setup step 6 now writes the OBS mask without the reserved
    CPU into an EMPTY `jxs_cores` of `jxs-output.json` (`strih_jxs_output_default_cores`, the rx-pin plan);
    an operator value is kept. The output reads the file at its next start (or Stop/StartOutput over WS).
- Clocks + latency: `.claude/rules/audio-clock-map.md` (Dante on strih-snv).
