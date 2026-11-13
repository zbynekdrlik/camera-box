---
paths:
  - "scripts/lib/strih-dante-input.sh"
  - "scripts/dante_input_continuity.py"
  - "scripts/lib/program-audio-continuity.sh"
  - "tests/python/test_strih_dante_input_1485.py"
  - "scripts/lib/strih-dante-cpu-leftovers.sh"
  - "tests/python/test_strih_dante_cpu_leftovers_1485.py"
  - "scripts/lib/strih-dante-ptp.sh"
  - "tests/python/test_strih_dante_ptp_1485.py"
  - "tests/python/test_stream_snv_dante_1495.py"
  - "scripts/strih-boxes/stream-snv.env"
  - "tests/python/test_strih_dante_terms_1513.py"
---

# strih-snv's program audio over Dante: the obs release's `dante_input` source (issue 1485)

Owner 2.11.2026: each application is its own Dante device; strih-snv only, PP and the stream role keep
obs-vban. The FOH program (TX `master-asio-dante`, stereo, L then R) reaches the strih OBS through the
Dante device `strih-obs` that the obs release's `dante-input.so` runs (dstack's dlink).

- **The stream role too (issue 1495, owner 9.11.2026: "prechod stream na dlink v obs"):** the program device
  is `<STRIH_ROLE>-obs` (`strih_dante_input_device`): `strih-obs` on a strih box, `stream-obs` on a stream
  box (stream-snv; dstack: distinct from mbc's own device `mbc-snv`). The loader takes it on either role.
  stream-snv keeps its input NAME `mbc` (the measurement watchdog's meter, the av-sync dock, rig-mode's
  `MBC_INPUT_NAME` key on it); the stream manifest carries `program_audio_dante` instead of
  `program_audio_vban` and the role-free seeder switches the kind. Its own consumers:
  - rig-mode TEST #901 item 2: `stream_program_dante_input HOST` (`stream-box.sh`, facts in a subshell,
    `<device> <input>`) -> `obs_phase2.py mbc-input-check --dante-device stream-obs`: kind `dante_input` +
    unmuted (pure `program_audio_vban.mbc_dante_transport_problem`);
  - the E2E continuity gate: the stream log graded `--no-vban` (guard + ASRC + mixed_late on `mbc`) and the
    SAME slice folded through `dante_input_continuity.py --box stream --device stream-obs` (`stream_dante`,
    `stream Dante:`; the strih defaults byte-identical);
  - the site file names the stream mix's transmitter and channels (never in the repo); verify item 9 and
    the TOML follow the device name. Tests: `tests/python/test_stream_snv_dante_1495.py`.
  - verify item 9's per-channel meter row (`strih_program_vban_channels_row`, OBS's own meter of the
    input) runs in the Dante grade too, tagged `(program-dante)` (strih-snv gets it as well). Its verdict
    assumes a stereo input: both site files subscribe 2 channels today; a site file with more RX channels
    needs the row re-checked on the box first (an unused subscription would read as an empty channel).
  - The per-point E2E report names the source `dante_input (stream-obs)` when the block carries
    `stream_dante` (`e2e_report_youtube.py`).
  - Before the first deploy, read `ss -lntup` on stream-snv: the box-wide `ip_unprivileged_port_start =
    319` lets every unprivileged process bind 319-1023 there too (strih-snv's read was 6629102078).
- **Which box:** the fact `STRIH_DANTE_DEVICES` names `strih-obs=arc:cmc:flows:rxfirst-rxlast`
  (`strih_program_dante_declared`). The loader then requires the VBAN program facts `none` and keeps
  `STRIH_PROGRAM_AUDIO_INPUT` (`ASIO zvuk`) as the input's name; a box with both is refused. The leg's
  sender keeps only the hub's 6980.
- **The file:** `~/.camera-box/dante-input.toml` of the OBS user, read ONCE at OBS start (no file = no
  device and no source kind). setup-strih step 13 writes it after the leg (`strih_dante_input_install`):
  name `strih-obs`, iface = the leg's (`enp7s0.70`), `network_rate = 96000`, no `sample_rate`,
  never `latency_ms` (obs v0.4.27 refuses it), the named latency terms + the program TX from the box facts
  (the bullet below), the fact's ports, one `[[subscribe]]` per RX channel. Another box removes a file
  carrying our header and leaves an operator's own.
- **The named latency terms + the program TX (issues 1510 + 1513, obs v0.4.27 / dstack 244).** Four
  required facts, the last of the key list: `STRIH_DANTE_NETWORK_LATENCY_MS` (a, one of Dante Controller's
  0.25 / 0.5 / 1 / 2 / 5) -> `network_latency_ms`, `STRIH_DANTE_RECEIVE_BUFFER_MS` (b, 0 < ms <= 50, up to 6
  decimals) -> `receive_buffer_ms` (both written as TOML floats), `STRIH_DANTE_TX` (`none` or the two labels
  `L,R`, 1..31 characters each) + `STRIH_DANTE_TX_TRACK` (1..6) -> `tx = ["L", "R"]` + `tx_track`
  (`strih_dante_input_terms_toml`, before the `[[subscribe]]` tables). The loader
  (`_strih_box_dante_terms_ok`): a program device needs (a) and (b) explicit; a TX needs both labels, a
  track and an explicit (b) (without it dlink lowers P to 8.365 ms); a box without a program device sets all
  four `none` (so a VBAN rollback of stream-snv is the three VBAN facts + `STRIH_DANTE_DEVICES=none` + these
  four `none`). strih-snv + stream-snv: (a) 1 + (b) 12.041666 = P 13.041666 ms, exactly the earlier default
  (no latency change, owner rule); only stream-snv sends `OBS program L,OBS program R` track 1 (mbc Live RX
  5/6, owner yes on issue 1513). A change of (a) or (b) is an owner decision. Clocks + latencies:
  `.claude/rules/audio-clock-map.md` ("The named Dante latency terms"). Tests:
  `tests/python/test_strih_dante_terms_1513.py`.
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
  FAILs, a total since the start that holds still is only named). Since obs v0.4.27 (issues 1510 +
  1513, `strih_dante_input_terms_toml_grade` + `strih_dante_input_tx_grade`): `latency terms` (the TOML
  carries exactly the facts' term + tx lines), `dante latency` (the last `dante-input start: latency split
  dante_latency_ns=` within 1 us of (a) + (b); a missing line FAILs), on a TX device the `dante-input start:
  TX of OBS track N` line and the status TX tokens over the same window (`tx_track` = the fact's,
  `tx_priming=0`, `tx_played` advancing, `tx_underruns` / `tx_recentres` / `tx_overflows` not growing; a
  rate off 48000/s +-10 % is a NOTE), on an RX-only device `tx_track=0` (absent on an older line = NOTE),
  and a report-only NOTE with the newest `network_latency_min_us` / `_p99_us` / `_max_us`. The
  `(vban-leg) vban` arrival row is a NOTE on a Dante box. PipeWire stays purged (`dante:strih-obs` counts as a direct program).
- **The E2E gate:** `program-audio-continuity.sh` also marks the strih OBS log (through `strih-log-read.sh`)
  when `$STRIH` is a Dante box, and folds `scripts/dante_input_continuity.py`'s window result into the
  `program_audio_continuity` block (`strih_dante`, the worse verdict, `strih Dante:` reasons, so the
  report needs no second key): any rx_loss / rx_holes / underruns (skipped blocks) / rx_gaps / socket_drops /
  rebootstraps / rx_written_late / re_anchors / obs_dropped (v0.4.0) increase (the obs project's meanings, 3.11.2026: audio other
  than FOH sent; rx_late, overruns, late_blocks are timing only and reported; a line before obs v0.3.0 has
  no rx_holes and reads UNKNOWN; in v0.2.0 `underruns` mixed late blocks with skips, so it proves nothing), clock=free or holdover=1, or a `dante-input error:` in the window = FAIL; no
  opening line, a restart, a counter backwards, no delivered frames = UNKNOWN = FAIL. The closing line is
  the second status line after the stop.
- **Timing signals, report-only (issue 1485, 12.11.2026):** the window grade counts `dante-input timing:`
  lines and dlink's `engine pump lost or overran blocks` WARN inside the same window as the counters (it
  opens at the last status line BEFORE the start mark, so a line between that and the mark is inside):
  `report_only: {timing_lines, pump_warn_lines}` + one `timing (report-only):` summary line; verify item 9
  adds one NOTE with both counts over the OBS log. Never a gate: a WARN that lost blocks also raises the
  gated `rebootstraps`; a missed=0 WARN loses nothing. Tests: `tests/python/test_dante_timing_report_only_1485.py`.
- **No CPU pinning for the Dante threads (dstack v0.6.28; owner ruling relayed by dstack issue 1,
  4.11.2026: nobody can guard cores on every PC).** On 3.-4.11.2026 camera-box reserved CPU 15 for OBS's
  `dstack-engine` + `dstack-rx` and CPU 14 for the hub's (a root helper behind a sudoers line, OBS
  launched on the reduced mask, systemd + user-manager CPUAffinity drop-ins, a hub ExecStartPost drop-in,
  the JPEG XS encoder's cores derived from it) after pump WARNs at a 5 ms margin. dstack v0.6.28 raised
  dlink's default budget to 15 ms with a 6 ms pump margin instead and removed `pump_cpu` / `io_cpu` /
  `rx_cpu`. That machinery is GONE; never bring a reservation back, and never write `latency_ms`
  for the OBS device (the latency is the named terms of the box facts since obs v0.4.27). The hub's TOML is the intercom project's.
  - **The leftovers (`scripts/lib/strih-dante-cpu-leftovers.sh`, `strih_dante_cpu_leftover_paths`):**
    `/etc/strih-dante-rx-cpu.conf`, `/usr/local/sbin/strih-dante-rx-pin`, `/etc/sudoers.d/strih-dante-rx-pin`
    (always ours by name), `/etc/systemd/{system,user}.conf.d/50-strih-dante-reserved-cpu.conf` and
    `/etc/systemd/system/intercom-hub.service.d/60-strih-dante-rx-pin.conf` (ours only with the camera-box
    header line; a file of that name without it is left and named).
  - **setup-strih step 13** (`strih_dante_input_install`, EVERY box, first thing) runs
    `strih_dante_cpu_leftovers_remove USER`: removes ours, then `daemon-reexec` of PID 1 + the operator's
    user manager ONLY when a manager drop-in went (each `timeout 30`, a failure a WARNING naming that
    manager), and `daemon-reload` ONLY when the hub drop-in went (a failure fails the step; the removed
    drop-in changes the hub's install fingerprint, so the running hub restarts after step 16i). A clean
    box prints nothing and calls no systemctl. **Only a REBOOT clears the old mask** (0-13 / 0-14):
    systemd sets its own affinity only when `CPUAffinity=` is set, and the mask survives the
    daemon-reexec, so PID 1 and the user manager keep it and every unit or OBS restarted later inherits
    it. After the deploy reboot strih-snv, then read `grep Cpus_allowed_list /proc/1/status` = every
    online CPU. The remove WARNs while an online CPU is missing from PID 1's mask (a wider mask, every POSSIBLE CPU at boot, is healthy)
    (`strih_dante_cpu_pid1_narrowed`, review round 1).
  - **verify item 9 `(dante-cpu)`** (`strih_dante_cpu_leftovers_grade`, called by
    `strih_program_audio_grade` before the Dante / obs-vban dispatch): one FAIL row per leftover (a
    foreign file at the name too), else one OK row; a NOTE while PID 1 still runs on the narrowed mask
    (reboot pending), and a NOTE when a non-root verify cannot list `/etc/sudoers.d` (root 0750: the
    sudoers leftover is graded only as root). Limit: a hub-fed strih (verify item 9's third branch, no
    program declared) runs no `(dante-cpu)` rows; only strih-snv ever had the reservation, and setup
    removes it on every box anyway.
  - **The JPEG XS program encoder** is back to the pre-1485 rule: `jxs_cores` is the operator's value,
    empty = every online CPU. setup-strih step 6 resets a value that still equals its
    `jxs_cores_derived` record to `""` and always drops the record (an operator's value is kept);
    verify item 49's `(jxs-cores)` FAILs a record left in the file. The running output takes it at its
    next Stop/StartOutput.
  - **The program-audio sampler** keeps its own set: the E-cores, else the background set (online minus
    the OBS mask, strih-snv 0-1), `strih_program_audio_cpus`.
  - Tests: `tests/python/test_strih_dante_cpu_leftovers_1485.py` (the remove against a temp root with a
    logging `systemctl` / `timeout`, every branch; step 13 on a Dante and a non-Dante box; the grade; the
    removed machinery; the TOML; the JPEG XS render and row).
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
