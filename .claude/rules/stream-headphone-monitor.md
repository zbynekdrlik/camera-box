---
paths:
  - "scripts/strih_stream_monitor.py"
  - "scripts/lib/strih-stream-monitor.sh"
  - "systemd/strih-stream-monitor.service"
  - "scripts/obs_box_stream_profile.py"
  - "tests/python/test_strih_stream_monitor_1436.py"
  - "tests/python/test_stream_monitor_provision_1436.py"
  - "tests/python/test_strih_stream_monitor_vlc_1456.py"
  - "tests/python/test_strih_stream_monitor_1465.py"
---

# The stream box's headphones play the REAL outgoing stream (issue 1436)

> **Issue 1465 item 3 (owner 23.10.2026) moved the stream role onto a LOCKED PipeWire:** the unit runs
> `--output pulse` (VLC through pulse to the one sink `stream-operator-headphones`, the headphone PCM held by
> PipeWire, `--check-state` inverted). Every "never PipeWire" / "PipeWire on the PCM FAILs" below is the
> `--output alsa` player, now the rollback. See `.claude/rules/stream-operator-audio.md`.

> **Issue 1465 (owner 23.10.2026) changed two things in the VLC player:** the headphone box (a USB audio
> interface) wins over the notebook jack, and VLC gets `pl_play` only while OBS publishes. See "The
> headphone box and the blinking (issue 1465)" at the end.
>
> **Issue 1456 (owner 21.10.2026) replaced the player with VLC** ("tak chcem to mat rovnako cize to tiez z
> implementuj aby na stream-pp tiez bolo vlc, a neviem si v stream-pp nastavovat hlasitost vystuput
> sluchatkoveho a je to potichu"). The ffmpeg -> aplay relay described under "The player" below is
> HISTORY. The unit, the PCM-by-name resolve, busy / no-card, the state file, `--check-state` and the
> three verify rows stay; see "The VLC player (issue 1456)" at the end for what changed.

Owner, 15.10.2026, on stream-pp: "nestreamujem ziaden zvuk do vystupu a do sluchatiek nieco raje" ->
"a spomen si ako to mame riesene v snv ze sa pocuva cez vlc ktore ma nacitany stream rtmp z obs alebo z
restreamera" -> "pozadujem aby sa budovalo jednotne riesenie pp/snv". And (issue 1437): "pozadujem
hlbkove preverenie nutnosti vyuzivat pipewire". Main design: issue 1436 comment 6189806622.

stream-pp's jack used to play ONE OBS source: `Zvuk z mixu` on Monitor and Output to the default device,
so also with no stream running and also when something else went out. stream-snv's operators hear the
ENCODED stream that really leaves the box (`scripts/avsync-vlc-monitor.ps1`, VLC on the restreamer's
`rtmp://127.0.0.1:1234/live/obs-e2e-test`, `.claude/rules/avsync-monitoring.md`). Every Linux stream-role
box now does the same, through ALSA, never PipeWire.

## The three pieces

- **The key** (setup-strih step 3, `obs_box_stream_encoder_provision`): `obs_box_stream_profile.py
  --key <box>` writes the box name (`obs_box_stream_key` = `strih_lx_hostname`) into `service.json` when
  it carries NO key or a blank one. An existing key is the operator's and is kept (operator wins), and
  the file is written only with OBS stopped, like the rest of the profile. OBS then publishes
  `rtmp://127.0.0.1:1234/live/stream-pp`. Restreamer accepts any key for the INGEST, but a player must
  name the key OBS publishes (a wrong name gives an I/O error), so the monitor plays exactly the box name
  and verify FAILs any other key. An empty key also played, but the URL shape must match SNV and never
  depend on an empty path (comment 6189226451).
- **Monitor Off** (the seeder, every OBS launch): the stream manifest carries
  `"program_audio_monitor": "off"`; `strih_scenes.py --bootstrap` sets the program-audio input to
  `OBS_MONITORING_TYPE_NONE` when OBS monitors it (update-only, read back), and the seed line says
  `Zvuk z mixu=matched; monitor off (was ...)`. A strih manifest has no such key: monitoring untouched.
- **The player** `strih-stream-monitor.service` -> `scripts/strih_stream_monitor.py` (std-only), a
  session app on the STREAM role only (`obs_box_role_apply`: keeper, monitor, dantesync tray):
  - the card is resolved by its PLAYBACK PCM NAME (` Analog$`, never a Realtek `Alt Analog`; stream-pp
    `ALC257 Analog` on card
    `Generic_1`, read live 15.10.2026), addressed `plughw:CARD=<id>,DEV=<n>`, re-read before every
    connect. Never a card index, and never the card name: both stream-pp cards are called
    `HD-Audio Generic` and the id `Generic_1` is enumeration order;
  - a PCM held by another process (its `/proc/asound/.../sub0/status` `owner_pid`) = `busy`: it does
    not connect at all (no RTMP subscriber), names the holder, retries every 3 s;
  - `ffmpeg -rw_timeout 5 s -i rtmp://127.0.0.1:1234/live/%l -map 0:a:0 -vn ... -f s16le` (the unit's
    `%l` = the short hostname = the key), and on the FIRST audio: `amixer -c <card id> sset Speaker mute`
    (PipeWire's ACP keeps `Auto-Mute Mode` Disabled and routes the speaker itself, so a direct write
    could also sound on the laptop speaker; a card without the control plays anyway, named once), then
    `aplay -N -D <plughw> -t raw -f S16_LE -r 48000 -c 2 --buffer-time=100000` (the hub's AplaySink:
    `-N` fails a busy PCM at once). Every child runs in the C locale: the busy / underrun / no-stream
    text the player reads is English;
  - **never a paused subscriber** (restreamer's per-subscriber channel is unbounded): Python relays,
    always reads ffmpeg, writes the aplay pipe NON-blocking in whole 4-byte frames (a pipe read can end
    mid-frame: a dropped odd chunk would shift the samples), and drops a chunk that would push the pipe
    past 200 ms (FIONREAD before each write, the hub's pipe-fill measure) or that the full pipe refuses. ffmpeg's probe delivers its first ~1 s as one burst, so
    the start is trimmed to the ceiling; a card slower than the sender drops a 20 ms chunk now and then,
    a faster one underruns (aplay's own count). Latency ~ RTMP + probe + <= 300 ms, as on SNV;
  - a stream end (ffmpeg EOF) or no audio for 5 s (stall) stops ffmpeg first (the subscriber goes at
    once), aplay plays out its buffer; reconnect after 3 s. Nothing published = ffmpeg fails
    `Input/output error` / `Connection refused`: silent, logged once. A stream without an audio track is
    the fault state `no-audio` (ffmpeg prints five lines for it and `matches no streams` is not the last:
    the stderr reader keeps the first line holding each watched text, never only the last line); an
    aplay that fails (not busy) is `aplay-failing`, retried after a doubling pause (up to 20 x 3 s),
    and any other outcome resets the pause. A failed play stays a fault (`last_play_failed`) until a
    stream plays, so a later `waiting` never reads healthy; a signal-killed aplay (a shutdown) is not
    an aplay fault;
  - logs: `watching ...` at start, `playing <url> -> <plughw> (connect N)`, `stream ended after N s:
    <why>; N ms dropped over the 200 ms ceiling; N aplay underrun(s)`, and one line per idle-state
    change (`waiting:` / `busy:` / `no-card:`);
  - state file `%t/strih-stream-monitor.json` (atomic, a heartbeat every 5 s).

## Provisioning + verify (no line in setup-strih.sh / verify-strih.sh, both at 1000)

- Step 16d (`strih_session_apps_install`) installs the program + unit like every session app, enable-only,
  and `strih_session_apps_packages` adds `ffmpeg alsa-utils` when the entry is in the list. The kiosk
  autostart starts it at every login.
- verify-strih: the entry's unit row (items 37-40 shape: files, enabled, autostart, active, the running
  process) plus `STRIH_STREAM_MONITOR_ROWS` = 3 rows from `strih_stream_monitor_grade_rows`
  (`scripts/lib/strih-stream-monitor.sh`):
  - `(stream-monitor-pass)` = `--check-state`: a pass within 30 s, waiting / playing / ended (never
    `no-card` / `busy` / `aplay-failing` / `no-audio`), and the headphone PCM free or held by `aplay`.
    **PipeWire (or anything else) PLAYING on the headphone PCM FAILs**, read from
    `/proc/asound/card<N>/pcm<D>p/sub0/status` -- no PipeWire tool needed. A suspended PipeWire sink
    on the card reads `closed` and passes: denying the card to PipeWire is the issue-1437 migration;
  - `(stream-monitor-key)` = `--grade-key`: `rtmp_custom` to `rtmp://127.0.0.1:1234/live` with exactly
    the box name as the key, AND `hostname -s` = that name (the unit plays `/%l`);
  - the Speaker mute is the player's own act at every connect (logged once), not a verify row; it
    re-mutes a speaker someone turned on by hand while a stream plays to the headphones;
  - `(stream-monitor-obs)` = `strih_scenes.py --audio-monitor-type` (on `OBS_WS_HOST`, its LAST line):
    `OBS_MONITORING_TYPE_NONE`; rc 2 (`unreachable: ...`, OBS not running) is a NOTE, an `absent` input
    and a reader failure FAIL with their own remedy.
- Seams: `STRIH_STREAM_MONITOR_ASOUND` / `_PROC` (the player), `_HOSTNAME` / `_SCENES` / `_MANIFEST`
  (the rows).

## Tests (Tier-0 pytest)

`tests/python/test_strih_stream_monitor_1436.py` runs the REAL loop with fake `ffmpeg` / `aplay`
executables and a fake `/proc/asound` (stream-pp's two cards), and the rows through the real libs.
`tests/python/test_stream_monitor_provision_1436.py` pins the key, the manifest and the seeder. A local
smoke with the REAL ffmpeg against `ffmpeg -re -i clip -c copy -f flv -listen 1 rtmp://127.0.0.1:<port>/live/x`
(a one-client RTMP server) played the clip end to end and trimmed the probe burst to the ceiling.

## Live apply on stream-pp (supervisor runbook)

1. Read first (read-only): `cat ~/.config/obs-studio/basic/profiles/stream-pp/service.json` (no key
   today), `cat /proc/asound/card*/pcm*p/info | grep name`, `cat /proc/asound/card1/pcm0p/sub0/status`.
2. Stop OBS (`systemctl --user stop strih-obs.service` as newlevel; the profile is never written under a
   running OBS), then run `sudo ./scripts/setup-strih.sh --box stream-pp` from the deployed checkout.
   Step 3 logs `service.json: rtmp_custom rtmp://127.0.0.1:1234/live (stream key stream-pp written ...)`,
   step 16d `installed strih-stream-monitor.service ...` and `packages present ... ffmpeg alsa-utils`.
3. Start OBS (`systemctl --user start strih-obs.service`): the seed line in its journal reads
   `Zvuk z mixu=matched; monitor off` (it was already Monitor Off by hand on 15.10.2026).
4. `systemctl --user start strih-stream-monitor.service` (setup never starts a stopped unit; the next
   kiosk login does too). `journalctl --user -u strih-stream-monitor -n 20`: `watching ...` then
   `waiting: nothing published on rtmp://127.0.0.1:1234/live/stream-pp (...)`.
5. Start the stream in OBS: the journal reads `playing rtmp://127.0.0.1:1234/live/stream-pp ->
   plughw:CARD=Generic_1,DEV=0 (connect 1)`, `/proc/asound/card1/pcm0p/sub0/status` names an `aplay`
   owner, and the headphones carry the stream. The restreamer session re-verifies its ingest on the
   new `/live/stream-pp` path (comment 6189226451).
6. If it logs `busy: ... held by pipewire`: something plays to PipeWire's analog sink (the default
   sink). That is the issue-1437 migration's WirePlumber deny of the headphone card, not this code.
7. If the stream plays but nothing is heard: the ALSA mixer of the jack (`amixer -c Generic_1 sget
   Master` / `Headphone`) keeps what PipeWire last set (15.10.2026: Master 64 %, Headphone 100 % on,
   Speaker off, Auto-Mute Disabled); unmute / raise it with `alsamixer -c Generic_1`. The journal's
   `muted the Speaker switch of Generic_1` line is the player's own mute.
8. Stop the stream: `stream ended after ...`, then `waiting:` again. `sudo ./scripts/verify-strih.sh
   --box stream-pp`: the `(stream-monitor)` unit row and the three rows PASS.

## The VLC player (issue 1456)

- `strih_stream_monitor.py` supervises VLC (`vlc_argv`): `--aout=alsa --alsa-audio-device=<plughw>`
  (never PipeWire), `--zoom=0.25` (the picture in a small window, as on SNV the window shows it),
  `--network-caching=1000` (SNV's), `--no-qt-error-dialogs --no-qt-privacy-ask`, `--extraintf=http` on
  127.0.0.1:8090 with a per-run random password, the URL last. No `--repeat`: a failed open with repeat
  would loop VLC.
- Before EVERY VLC start, `mixer_unity_cmds`: the card's `Master` / `Headphone` 0 dB unmute, `PCM` 0 dB,
  `Speaker` mute (`amixer -q -c <card id> sset ...`, the C locale). stream-pp read Master -23.25 dB on
  21.10.2026, a value PipeWire left. A control the card lacks is logged once and VLC starts anyway. The
  operator's level is VLC's own slider (`--volume-save` is VLC's default, so it survives a restart).
- The http status drives the loop, polled every second:
  - `stopped` -> `pl_play` every 3 s (nothing published, or the stream ended);
  - `paused` for more than 10 s -> `pl_stop`. The restreamer's per-subscriber channel is unbounded, so
    a paused player must never hold a subscriber (the reason the issue-1436 relay never paused);
  - VLC exits (the operator closed it) -> it starts again after 3 s. An exit before its http answered
    within 15 s is `vlc-failing`. An http that never answers within 15 s is `vlc-failing` and VLC is
    restarted.
- `--check-state`: healthy states `starting` / `waiting` / `playing` / `paused`; faults `no-card` / `busy`
  / `vlc-failing`; the only allowed PCM owner is `vlc` (aplay is now a FAIL, PipeWire always was).
- Packages: `STRIH_STREAM_MONITOR_PACKAGES=(vlc alsa-utils)`.
- Tests: `tests/python/test_strih_stream_monitor_vlc_1456.py` runs the real supervisor against a fake
  `vlc` that serves the http status interface (Basic auth) and a fake `amixer`, on a free local port.
- **Live apply (supervisor, after a production):**
  1. setup-strih step 16d on the stream box installs `vlc` (plus Qt) and the new program, then
     try-restarts the running unit.
  2. Read back:
     - the journal shows `watching ... in VLC` and `playing rtmp://127.0.0.1:1234/live/<box> -> plughw:...`;
     - `/proc/asound/card<N>/pcm0p/sub0/status` names a `vlc` owner;
     - `amixer -c <card id> sget Master` reads 0 dB;
     - the VLC window sits on the panel's screen.

## The headphone box and the blinking (issue 1465)

Owner, 23.10.2026, after the stream-snv operator's report ("Nefungovala mi skrinka na slúchadla"; "na stream
snv vidim dokolecka nejako blikat vlc"): "operator si chce otovorit vlc aby videl obraz a chce si vediet
prepnut zvuk do sluchatiek aby isiel z vlc tak vidi a pocuje co je na vystupe z obs a teda vie skontrolovat
lipsync a audio vypadky!!!". The VLC window STAYS (the operator watches the outgoing picture and checks
lipsync + audio dropouts by ear); the headphones carry VLC's audio. Owner decision: issue comment
6362030709; design 6362045028 (Approach 1).

- **Why the headphone box got nothing.** The stream-snv operator's headphones hang on a Focusrite Scarlett
  Solo 4th Gen (USB, card id `Gen`, playback PCM `USB Audio`, device 0, factory routing: PCM 1/2 -> the
  headphone out). The default ` Analog$` match picked the notebook jack `ALC256 Analog` on card `PCH`.
- **The card rule (`resolve_headphone`), re-read before every VLC start:**
  - an explicit `--pcm-match` = a regex on the PCM name, exactly the old meaning (argparse default `None`
    tells "not given" apart);
  - else the playback PCM of the ONE USB audio interface: a `/proc/asound/cardN` dir with a `usbid` file
    (snd-usb-audio writes it; `list_playback_pcms` marks each PCM `usb`);
  - no USB playback PCM -> the jack (` Analog$`, stream-pp's `ALC257 Analog`: stream-pp has no USB card);
  - two or more USB playback PCMs (two interfaces, or one with two playback PCMs) -> one log line naming
    them, then the jack. Name one with `--pcm-match` in a drop-in if that ever happens.
  - The `watching ...` line names the rule (`the ONE USB audio interface's playback PCM, else the
    headphone jack matching ...` or `the playback PCM matching ... (--pcm-match)`).
  - Plugging a headphone box in switches at the next VLC start (the operator closing VLC, or a unit
    restart); no re-provision.
  - **A USB headphone box keeps its own mixer** (review round 1): no unity writes and no `Speaker` mute
    on a USB card, one `mixer: <card> is a USB audio interface -- its own mixer is left as it is` line.
    snd-usb-audio names many headsets' and dongles' main output `Speaker`, so the jack's mute would
    silence the box the operator listens on, and a 0 dB write on its own volume is full scale. The
    Scarlett has none of those controls anyway; its factory routing is unity. The jack keeps the 1456
    unity + speaker mute.
- **Why the window blinked.** With nothing published, VLC reads `stopped`, and the supervisor sent
  `pl_play` every 3 s. Each one opened and failed the RTMP input, so the window and its taskbar entry
  flickered all day.
- **`pl_play` only while OBS publishes (`obs_publisher`).**
  - True = a process whose `/proc/<pid>/comm` is `obs` (`--obs-process-name`) holds an ESTABLISHED
    (`st` 01) TCP connection whose REMOTE end is the URL's host:port (port 1935 when the URL names none).
  - Read std-only: `<proc_root>/net/tcp` + `tcp6` (the 32-bit address words in host order, an
    IPv4-mapped tcp6 address read as IPv4), then the socket inodes in `<proc_root>/<pid>/fd` of the `obs`
    pids only. A vanished pid, a foreign pid's fd dir and a non-UTF-8 comm are skipped.
  - The comm filter matters: the monitor's own VLC holds an ESTABLISHED connection to the same port
    while it plays. `--obs-process-name` longer than 15 characters (the kernel's comm) is refused.
  - A hostname URL is looked up at most once a minute (`RESOLVE_TTL_S`); the default literal
    `127.0.0.1` never is. The stream KEY is not checked: OBS publishing under another key reads as
    published (verify's `(stream-monitor-key)` row grades the key).
  - Nothing published -> state `waiting`, reason `OBS is not publishing to <url> -- VLC idles until it
    does`, no `pl_play` at all. The reason is CONSTANT, so it is one journal line however other clients
    come and go on the port; what holds it (`nothing holds ...` / `no obs process holds ...`) is the state
    file's `publisher` field. VLC stays open with its window (it is still launched with the
    URL, so its first open at launch fails once). The state is healthy for `--check-state` (unchanged).
  - OBS publishes and VLC is not playing yet -> `waiting: OBS publishes to <url>, VLC not playing yet`
    and `pl_play` every 3 s until `playing`.
- **The login ordering cycle.** The journal read `Found ordering cycle: strih-stream-monitor.service/start
  after strih-obs.service/start after graphical-session.target/start`. A target orders itself After= every
  unit it Wants (the `WantedBy=` link) unless that unit is already After= the target (systemd's
  `unit_add_default_target_dependency` loop guard). strih-obs is After= the target, so a unit that is only
  `After=strih-obs.service` closes the cycle. `strih-stream-monitor.service` and
  `strih-browser-keeper.service` now read `After=graphical-session.target strih-obs.service` (the
  `strih-bundle-state-server.service` shape); `WantedBy=graphical-session.target` stays, so `enable` and
  the session-apps grader are unchanged. Checked with the real `systemd-analyze --user verify` on dev1
  (a scratch unit dir with the `graphical-session.target.wants` links): the old units report both cycles,
  the new ones none.
- **Tests:** `tests/python/test_strih_stream_monitor_1465.py`. The card rule; the publisher check over a
  fake `/proc` (`net/tcp` rows + an obs pid fd symlink `socket:[N]`); the real loop (`Rig.publish()` in
  the 1456 file also writes that fake publisher); a python model of the unit ordering graph over every
  `systemd/` unit, which first finds the live cycle in a two-unit copy of the old shape.

### Live apply (supervisor)

1. On stream-snv the 23.10.2026 hand drop-in
   `~/.config/systemd/user/strih-stream-monitor.service.d/10-scarlett.conf` (`--pcm-match "^USB Audio$"`)
   stays until the new program is installed, then is removed: `rm` it, `systemctl --user daemon-reload`.
2. setup-strih step 16d on stream-snv and stream-pp installs the program + both units and try-restarts the
   running ones.
3. Read back, as the box user:
   - `journalctl --user -u strih-stream-monitor -n 20`: `watching ... the ONE USB audio interface's ...`,
     then `waiting: OBS is not publishing ...`;
   - with nothing streamed the VLC window stays still (no flicker), and the journal has no new lines;
   - `systemctl --user show -p After strih-stream-monitor.service` lists `graphical-session.target`;
     the next login's journal has no `Found ordering cycle` line;
   - stream-snv: start a stream -> `playing ... -> plughw:CARD=Gen,DEV=0`, and the Scarlett's headphones
     carry it. stream-pp -> `plughw:CARD=Generic_1,DEV=0` as before.
