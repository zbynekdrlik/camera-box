---
paths:
  - "scripts/lib/strih-dante-input.sh"
  - "scripts/dante_input_continuity.py"
  - "scripts/lib/program-audio-continuity.sh"
  - "tests/python/test_strih_dante_input_1485.py"
  - "scripts/lib/strih-dante-rx-pin.sh"
  - "tests/python/test_strih_dante_rx_pin_1485.py"
  - "scripts/lib/strih-dante-ptp.sh"
  - "tests/python/test_strih_dante_ptp_1485.py"
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
  converter; `halved=true` on v0.4.0 and older, graded by whichever key the line carries: `converts=`
  PRESENT decides, also with an empty value), any
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
    (`strih_dante_rx_pin_plan`); a one-CPU box = that CPU, OBS on the rest. Its SMT sibling (written to
    the conf as `smt_siblings`) stays OBS's: the two threads get their own run queue (the wake-up latency
    is the fault), not a whole physical core. strih-snv has TWO reserved CPUs since 4.11.2026 (15 for OBS,
    14 for the hub's own Dante device, OBS on 2-13, the managers on 0-13): the two-CPU bullet below.
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
      `50-strih-dante-reserved-cpu.conf` = `[Manager] CPUAffinity=<online minus every reserved CPU>` (0-14
      on a one-CPU box, 0-13 on strih-snv), so every unit and every forked process of both managers starts
      off the reserved CPU(s), units we do not own
      included (the camboxes' `cambox-housekeeping-cpus.sh` precedent, plus the user manager). Rejected: a per-unit `CPUAffinity=` drop-in (every new unit must remember it, foreign
      units stay on 15) and a cpuset `AllowedCPUs=` (a cpuset BOUNDS sched_setaffinity, so the helper
      could no longer put the dstack threads there).
    - **Affinity, not a bound:** `sched_setaffinity` is limited by the cgroup cpuset and the online CPUs
      only, never by a parent's or the manager's mask, so the helper's `taskset -p -c 15` still works
      (pinned by a test that widens a narrowed mask) and OBS's `taskset -c 2-14` is a subset.
    - **A manager takes it when it re-reads its configuration, at the latest at the next boot** (PID 1
      may already on a daemon-reload; not verified on a box, so nothing relies on it). Every process
      started after that inherits it; running processes keep their masks until they restart. When a
      drop-in was written (install) or removed (another box), step 13 re-executes PID 1 and the
      operator's user manager (`daemon-reexec`, no unit restarts; each under `timeout 30`; a failure is
      a WARNING naming THAT manager, the next boot applies it; no user = PID 1 only). Setup never moves
      a live process; after a deploy the supervisor reboots strih-snv (or moves the running ones:
      `taskset -a -p -c <the managers' set> <pid>`, 0-13 on strih-snv), then reads `grep Cpus_allowed_list /proc/1/status` and verify
      item 9.
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
  - **A SECOND reserved CPU for the intercom hub's own Dante device (4.11.2026, comment 6634395906;
    design 6634500064).** Live on strih-snv, pump WARNs per 3 min: the hub's dstack-engine / dstack-rx
    unpinned on 0-14 = hub ~30; beside OBS's on CPU 15 = OBS 25 (two engines on one CPU collide); on CPU 14
    among the box's other threads (the hub's SCHED_FIFO `hub-mix`, NDI receivers, WebKit) = hub 24; alone
    on a cleared CPU 14 = 0 / 0. Each engine needs its OWN CPU with no other thread on it:
    - **By facts:** `strih_dante_hub_declared` (strih-dante-input.sh) = the Dante program AND a
      `<box>-intercom=` entry in STRIH_DANTE_DEVICES; `_strih_dante_hub_flag` hands the rx-pin lib its
      HUB argument (install + grade). Only then the plan (`strih_dante_rx_pin_plan ONLINE OBS 1`) prints
      `<obs cpu> <obs mask> <hub cpu>`: the highest = OBS's (15), the next one down that is online and in
      OBS's mask = the hub's (14), OBS on 2-13. A box without the hub device keeps ONE CPU, byte-identical
      (no `hub=`, no hub drop-in; a drop-in of ours left there is removed). The SMT sibling of 15 is now
      the hub's CPU 14 (`thread_siblings_list` 14-15 read on strih-snv 4.11.2026: one physical core for
      both engines; the clean live case 4 of comment 6634395906 was exactly that layout).
    - **The ONE source:** the plan, persisted as `hub=` in `/etc/strih-dante-rx-cpu.conf`, read by
      `strih_dante_reserved_hub_cpu` and `strih_dante_reserved_cpus` (`15 14`). Everything else reads
      those: the managers' drop-ins (`CPUAffinity=` online minus both = 0-13, a third comment line), the
      JPEG XS cores (`strih_jxs_output_default_cores` takes the plan's OBS mask = 2-13; the operator
      WARNING and the `(jxs-cores)` FAIL name every reserved CPU a value holds), the sampler
      (`strih_program_audio_cpus` subtracts both; strih-snv stays 0-1). dstack v0.6.26's
      `dlink::Config::pump_cpu` / `rx_cpu` (obs v0.4.11 reads them from dante-input.toml, the hub TOML
      to follow) get written from these readers when the pins carry them, never a second derivation; the
      pinner then keeps only the nice -10 (dstack sets none). Never an `AllowedCPUs=` cpuset that leaves a
      reserved CPU out: the in-process pin (and our taskset) would be refused.
    - **After every hub start:** `/etc/systemd/system/intercom-hub.service.d/60-strih-dante-rx-pin.conf`
      (`strih_dante_rx_pin_hub_dropin_text`, header-checked) = `ExecStartPost=-+/usr/local/sbin/strih-dante-rx-pin hub`
      and nothing else: `+` runs it as root outside the unit's `User=` / sandbox, `-` keeps a failed pin from
      failing the hub; it sets no CapabilityBoundingSet / AmbientCapabilities / SecureBits (the released
      unit's grant stays the only one, pinned by `tests/python/test_strih_dante_ptp_1485.py`). Written by
      step 13 (`strih_dante_rx_pin_install USER 1`, from `strih_dante_input_install`, after step 13's own
      daemon-reload), so a written or removed drop-in is followed by `timeout 30 systemctl daemon-reload`
      (a failure fails the install by name). A crash restart re-pins too: no timer, no path unit.
    - **The helper's hub mode** (`strih-dante-rx-pin hub`, root only, no sudo caller needed; the sudoers line
      stays the no-argument OBS form, so the operator cannot run it): the process = the unit's MainPID
      (`systemctl show -p MainPID --value intercom-hub`, never a name search), its `dstack-engine` +
      `dstack-rx` threads to `hub=` at nice -10, read back, one `intercom-hub pid P: ... -> CPU 14 nice -10`
      line. `HUB_BUDGET_S=30`: the threads appear ~0.2 s after the hub's start (measured on strih-snv
      4.11.2026), and ExecStartPost holds the unit in `activating` until it returns, so the budget stays
      far under the unit's 90 s start timeout. Every other hub thread (`hub-mix` SCHED_FIFO 10, tokio,
      the dstack tx / mdns threads) stays on the managers' default. MAINPID comes from systemd's own
      environment of ExecStartPost (a hand run as root asks `systemctl show`). Cost: a hub whose Dante
      device never starts (facts that declare it beside a routing file that does not) holds every start
      30 s in `activating`, then logs the FAIL and runs on; the facts and the pinned release's TOML are
      kept in step by the hub's own `(intercom-dante)` rows of item 16. Limit: a pump thread dstack
      recreated later from another thread inherits that thread's mask (0-13) and stays unpinned until the
      next hub start (OBS the same); none is recreated today (the threads are the device's lifetime).
    - **verify item 9 (no verify-strih line changed, the rows come from the libs):** `(intercom-dante)
      rx-pin` = the drop-in as rendered, the hub's main affinity off its CPU, both threads exactly on
      `hub=` at nice -10 or lower; no running hub / no `hub=` FAIL; a hub that started before its drop-in
      was written (the drop-in's mtime newer than btime + the process's start time; setup never restarts
      a running hub) turns its thread rows into `restart pending` NOTEs, so a deploy's acceptance verify
      does not fail on it, until the hub restarts; facts without the hub device but a
      `hub=` or our drop-in on the box FAIL. `(intercom-dante) reserved-cpu` = the same /proc scan as CPU
      15's, on CPU 14, allowing only the hub's two threads (a `hub-mix` or a crossed thread there is named);
      the scan of CPU 15 now also names the hub's threads if they sit there. Both scans share the
      reboot-pending NOTE. The emitter reads the verdict through `< <(...)`, never a pipe: a subshell's
      `bad` would not count (the ALL CLEAR bug class).
    - **Deploy (supervisor):** setup-strih on strih-snv writes `hub=14`, `obs=2-13`, the managers' 0-13 and
      the hub drop-in (a daemon-reload; the managers re-exec). Then restart the hub (ExecStartPost pins its
      threads: read the journal's `strih-dante-rx-pin: intercom-hub pid ...` line) and OBS (the launcher
      takes 2-13), move the long-running processes off 14-15 or reboot, and read item 9's `rx-pin` and
      `reserved-cpu` rows for both CPUs; then 3 min of pump WARNs = 0 on both.
  - **verify item 9's `(program-dante) rx-pin` rows** (`strih_dante_rx_pin_grade`, in the lib; verify-strih
    is at its line budget): FAIL when OBS's main `Cpus_allowed_list` still holds the reserved CPU, when a
    thread is missing, runs on anything but exactly that CPU, or has nice > -10; no conf / no running OBS
    FAIL too.
  - Another box: step 13 removes our conf, helper and sudoers line if present, silent when absent.
  - **The strih JPEG XS program encoder too (3.11.2026):** `jxs_cores` empty = every ONLINE CPU (vstack's
    `affinity::online_cores`, an explicit sched_setaffinity, so the OBS mask does not bound it): its
    `jxs-feed` / `jxs-send` threads ran on CPU 15. The OBS mask without the reserved CPU
    (`strih_jxs_output_default_cores`, the rx-pin plan) goes into `jxs_cores` of `jxs-output.json`:
    - **step 13 derives it** (`strih_jxs_output_cores_reconcile`, called by `strih_dante_input_install`
      after the managers' default; render mode `cores` changes only the two keys below). Step 6 runs
      before baseline step 11 writes `/etc/strih-isolated-cpus.conf`, so on a FRESH box it writes `""`
      (review of the 1485 commits). A Dante box whose set cannot be derived at step 13 fails by name;
    - **the derived value is recorded** as `jxs_cores_derived` and re-derived while `jxs_cores` is empty
      or still equals it (the reserved CPU moved = a new set). An operator's own value is kept, a
      WARNING when it holds the reserved CPU. A box that stops taking the Dante program gets the derived
      value cleared (`""`, the record dropped);
    - **verify item 49 `(jxs-cores)`** (strih-jxs-provision.sh, a Dante box with a program target):
      an empty `jxs_cores` or one holding the reserved CPU FAILs;
    - the Dante libs not sourced is an error by name, never "not a Dante box";
    - the output reads the file at its next start (or Stop/StartOutput over WS).
- **PTP in process (ROZHODNUTÉ 6628785864, owner 4.11.2026; design 6629112890; `scripts/lib/strih-dante-ptp.sh`).**
  Every dstack device here (the hub's `strih-snv-intercom`, OBS's `strih-obs`) follows Dante PTP itself: dstack
  v0.6.23's `ClockSource::Linked` starts dsync's follower (UDP 319 + 320 on INADDR_ANY, joined to 224.0.1.129 on
  the leg, receive only) and falls back to the dsync service's clock. The dsync service stays the ONLY writer
  of the system clock and the clock of the rest of the box; "dsync is the only PTP owner" is superseded for
  these devices.
  - **The leg:** one input rule for the group to 319/320 (`obs-vban-receive.md`, the Dante variant).
  - **OBS's right to bind 319 = the box-wide sysctl** `net.ipv4.ip_unprivileged_port_start = 319` in
    `/etc/sysctl.d/60-strih-dante-ptp-port.conf`, step 13 on a Dante-program box (`strih_dante_ptp_port_install`
    from `strih_dante_input_install`, header-checked file, the running value written to
    `/proc/sys/net/ipv4/ip_unprivileged_port_start`, no restart: the follower binds at OBS's next start);
    another box removes OUR file and puts a running 319 back to 1024 (a running 319 with no file of ours is
    left: nothing records who set it). **Only once BOTH pins are READY** (`strih_dante_ptp_all_ready`):
    until then step 13 takes the path of another box, since an older dlink that could bind 319 would start
    its follower beside dsync (the dantesync#109 collision; review round 1), and the right is box-wide, so
    the hub (an operator process, `DynamicUser=no`) gets it too (review round 2). Cost: every unprivileged process may bind 319-1023 (live
    `ss -lntup` read on strih-snv 4.11.2026, validation comment 6629102078: only dsync 319/320, rsyslog 514,
    nginx 443 there; one operator account). Rejected: `setcap` on /usr/bin/obs (a secure exec: libdbus /
    GLib refuse the session bus, no core dumps, the `/proc/<obs>` files root-owned, re-applied after every
    bundle install) and an ambient capability through the user manager (pam_systemd v259's
    `apply_user_record_settings` -> `capability_ambient_set_apply` REPLACES the manager's ambient set with
    CAP_WAKE_ALARM, read in systemd's source and matched by the live manager's CapPrm; a PAM / userdb
    change + a restart of the whole user session). Considered, not done: narrowing it with a cgroup bind
    filter (`SocketBindAllow=udp:319 udp:320` + `SocketBindDeny=` below 1024 on `user-.slice`), an option
    if the 320-1023 exposure ever matters. Verify item 9 `(program-dante) ptp-port` (graded before the
    log check, it reads no log): the file as rendered + the running value 319; below the threshold a NOTE
    when absent, a FAIL when our file is still there.
  - **The hub's right** comes from its released unit (item 16, `intercom-pinned-release.md`).
  - **The pin threshold** `STRIH_DANTE_PTP_FIRST_DSTACK=v0.6.23`: a component is READY when its pin's
    `*_RELEASE_DSTACK_VERSION` is at or above it (`strih_dante_ptp_pin_ready intercom|obs`, the repo's one
    version rule `release_version_cmp`, which `dtray_version_ge` uses too). Below it nothing opens 319: the
    leg rule needs either pin READY (`strih_dante_ptp_leg_open`: it only admits packets), the box-wide sysctl
    both (`strih_dante_ptp_all_ready`: the bind right is what starts an old follower); the capability
    and media clock rows are NOTEs. The threshold follows the pins, never a date.
  - **The hub's journal** is read for the current invocation only, filtered to the clock lines as it
    streams (a weeks-long invocation is never held whole), bounded 20 s; a hub that is not running is a
    NOTE (item 16's active row already names it), a journalctl that fails, or a read as non-root (journalctl
    then exits 0 with only a hint), is its own row, never "no line".
  - **The `media clock` rows** (`strih_dante_media_clock_grade LABEL COMPONENT < log`): the LAST dstack
    `media clock source source=<X>` line (target `dstack::clock::linked`); the OBS log carries it as
    `HH:MM:SS.mmm: dante-input dlink:  INFO dstack::clock::linked: media clock source source=Internal
    from=Service` (two spaces before the level, no ANSI); the hub's journal of its CURRENT invocation
    (`_SYSTEMD_INVOCATION_ID`) has ANSI codes on, stripped before the parse. `Internal` = OK; `Service`,
    `None` (`media clock: no source ready`), no line or another value = FAIL once READY, a NOTE below;
    the last `in-process follower did not start ... reason="..."` warning is named. Item 9 `(program-dante)
    media clock` (OBS log), item 16 `(intercom-dante) media clock` (only on a box whose facts declare a
    `*-intercom=` device). Seams: `STRIH_DANTE_PTP_INTERCOM_PIN` / `_OBS_PIN`, `STRIH_HUB_JOURNAL_FILE`,
    `STRIH_HUB_AMBIENT_CAPS` / `STRIH_HUB_BOUNDING_CAPS`, and the Dante libs' `STRIH_DANTE_RX_PIN_ROOT` prefix
    for `/etc` + `/proc`.
  - **Deploy (supervisor):** with the intercom + obs pins on dstack v0.6.23: setup-strih (the leg filter
    unit is restarted by step 13, the sysctl set live), restart the hub and OBS, then read both `media clock`
    rows `source=Internal` and measure the Dante path (rx_loss, holes, re_anchors) before and after.
- Clocks + latency: `.claude/rules/audio-clock-map.md` (Dante on strih-snv).
