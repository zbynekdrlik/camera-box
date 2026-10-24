---
paths:
  - "scripts/stream_operator_audio.py"
  - "scripts/stream_operator_volume.py"
  - "scripts/lib/stream-operator-audio.sh"
  - "scripts/strih_stream_monitor.py"
  - "systemd/strih-stream-monitor.service"
  - "tests/python/test_stream_operator_audio_1465.py"
  - "tests/python/test_stream_operator_monitor_seed_1465.py"
  - "tests/python/test_stream_operator_jack_volume_1465.py"
---

# The stream operator's headphones through a LOCKED PipeWire (issue 1465 item 3)

Owner 23.10.2026, ROZHODNUTE (verbatim): "na toto mozme pipewire vyskusat ale nesmie sa babrat do inych veci
nez tych ktore mu jasne povolime a definujeme! A znova len chcem pripomenut ze vlc ma na sebe ovladac
hlasitosti a aj chrome youtube ma v prehravani hlasitost a mute. cize mat dalsi ovladac na dalsiu hlasitost
nie je uplne potrebne". Requirements (comment 6363676384): ONE fixed headphone device, everything the operator
hears there and never on the notebook speakers, the apps' OWN volume controls (VLC slider, YouTube volume /
mute; no mixer UI), nothing switches by itself, a fixed output latency so lipsync in VLC and Chrome is true.
Design: comment 6363778378 (Approach 1). **Stream role only** (issue 1440 purges PipeWire on a strih box whose facts need none and never touches
this one, `.claude/rules/strih-pipewire-purge.md`); the strih role's PipeWire (the hub's program
sink + loopback, the MiniFuse rules, the quantum-1024 floor) is never touched.

**REVISED 24.10.2026 (ROZHODNUTE 6383739315, verbatim):** "na snv stream pc chcem aby sa uz nepouzivala
focusrite a isli sluchatka priamo z jack notebooku ako to je aj v stream-pp a tiez musi byt zrozumitelne ako si
ma operator regulovat hlasitost sluchatiek, nie len tu localnu v vlc/chrome yt ale aj tu globalnu". Design
6383742509 (Approach 1). Two changes, see "The jack + the global headphone volume" below: the device is the
notebook jack on EVERY stream box (never a USB interface), and the sink volume is the operator's GLOBAL level
(a panel item + the volume keys), no longer pinned at 100 %.

## What is allowed (everything else is denied)

`scripts/stream_operator_audio.py` declares it, `scripts/lib/stream-operator-audio.sh` installs and grades it.

- **The device** = the stream monitor's own rule (`strih_stream_monitor.resolve_headphone`, imported, never
  copied): the notebook jack, ` Analog$` on a card that is NOT a USB interface (stream-snv: `ALC256 Analog` on
  card `PCH`; stream-pp: `ALC257 Analog` on `Generic_1`). A USB interface (stream-snv's Focusrite `Gen`) is
  never chosen, whatever its PCM is called; it is named once and stays disabled like every other card.
- **Its WirePlumber device name** = `alsa_card.` + udev `ID_ID` (USB: `usb-<serial>-<if>`), else `ID_PATH`
  (PCI: `pci-<address>`), sanitised like WirePlumber 0.5's alsa.lua (`[^A-Za-z0-9_.-]` -> `_`). When the
  operator session is running, its own `pw-dump` name for that card (`api.alsa.card`) wins (setup reads it as
  the operator).
- **`~/.config/wireplumber/wireplumber.conf.d/60-stream-operator-headphones.conf`**:
  - `monitor.bluez = disabled` (+ bluez-midi);
  - `device.restore-profile`, `device.restore-routes`, `node.stream.restore-props`,
    `node.stream.restore-target` = false;
  - every ALSA card `device.disabled = true`, then the headphone card back on with `device.profile =
    "pro-audio"` and `api.acp.auto-profile/auto-port = false`. Pro-audio = one node per PCM, no ports to switch,
    and ACP never touches the ALSA mixer. WirePlumber 0.5 itself picks the profile by priority
    (`find-preferred-profile`; `find-stored-profile` is off with the restore), and pro-audio has the lowest
    priority, so the drop-in also lists `priorities = [ "pro-audio" ]` in `device.profile.priority.rules`
    for that device (review round 1: without it the card could come up on an analog profile whose nodes the
    next rule disables -- zero sinks);
  - every node of that card `node.disabled`, then its headphone playback PCM (`alsa_output.<stem>.pro-output-<dev>`)
    back on and RENAMED `stream-operator-headphones`: 2 ch `FL,FR`, 48 kHz, `api.alsa.period-size` 256 +
    `api.alsa.headroom` 256, `session.suspend-timeout-seconds = 0`. That only keeps a RUNNING node from
    suspending: until the sink's first client plays, the PCM reads `closed` (live stream-snv 23.10.2026).
- **`~/.config/pipewire/pipewire.conf.d/60-stream-operator-clock.conf`**: `default.clock.rate` 48000,
  `allowed-rates [ 48000 ]`, `quantum = min-quantum = max-quantum = 256` (5.3 ms). pipewire-pulse reports
  that latency to VLC and Chrome, which compensate for it.
- **The jack's HDA mixer is DECLARED** (`MIXER_LEVELS` / `MIXER_SPEAKER` / `MIXER_ENUMS`): Speaker off,
  `Auto-Mute Mode` Enabled, Master / Headphone / PCM 0 dB unmuted (a fixed hardware level). Step 12 writes it
  (`stream_operator_audio.py apply-mixer`, as root) and `alsactl store <card>`s it; `alsa-restore.service`
  (active on both stream boxes) restores it at every boot, and ACP never touches the mixer in pro-audio. A
  control the codec lacks is named and skipped; any other amixer error stores NOTHING (rc 1). The monitor's
  `mixer_unity_cmds` before every VLC start writes the same values (a re-assert).
- **Plugging a headphone box in or out needs setup step 12 again** (the drop-in names ONE device; the
  player alone would switch at its next VLC start, the locked PipeWire does not).
- **The sink volume is the operator's GLOBAL level** (below), next to VLC's slider and the YouTube player's.
  Verify READS it and never pins it.

## setup-strih step 12 / verify-strih item 9b (both at 1000 lines)

- `obs_box_step_applies 12-strih-audio` (strih role only) branches step 12: the stream role runs
  `stream_operator_audio_install` (both drop-ins through `obs_box_write_if_changed`, then removes the five
  strih-role leftovers `STREAM_OPERATOR_AUDIO_LEFTOVERS`); a box where no headphone device resolves fails
  step 12 with nothing written or removed. Setup NEVER restarts the operator's audio (the runbook below).
- **A monitor drop-in with `--pcm-match`** (the 23.10.2026 stream-snv hand drop-in `10-scarlett.conf`) is
  refused by the install and FAILs the files row: the player and the locked PipeWire must pick the SAME card
  (review round 1). Remove it before setup.
- Item 9b on the stream role: `stream_operator_audio_grade_report` = 10 rows: `(operator-audio-files)` (both
  files as rendered, no leftover), the module's six read-back rows from `pw-dump` + `pw-metadata -n settings`
  / `-n sm-settings` run in the operator session (`-sink` exactly one Audio/Sink = the operator sink on the
  headphone device; `-cards` no other ALSA device, no bluez; `-capture` no Audio/Source; `-volume` the level
  readable and at most 100 % (OK names it on wpctl's cubic scale, muted = NOTE, above 100 % = FAIL); `-clock`;
  `-restore`), `(operator-audio-mixer)` (the jack's declared mixer read live with `amixer -c <id> sget` AND
  stored in `/var/lib/alsa/asound.state`: Speaker Playback Switch false, Headphone Playback Switch true,
  Auto-Mute Enabled; a control the card lacks is named; a USB device = FAIL),
  `(operator-audio-obs)` = `strih_scenes.py --pulse-capture-inputs` (none; a NOTE while OBS does not answer),
  and `(operator-audio-control)` (the helper = the checkout, the three XF86Audio keybinds in the user's
  rc.xml, every line of the panel item in the tint2 config). An unreadable session FAILs every read-back row.
- The strih role keeps item 9b's MiniFuse/quantum grade unchanged.

## The jack + the global headphone volume (item 3 revision, 24.10.2026)

- **ONE control for the ONE output:** `scripts/stream_operator_volume.py`, installed by step 12 as
  `/usr/local/bin/obs-box-headphones` (`OBS_BOX_PANEL_HEADPHONES_HELPER`, declared in `obs-box-panel.sh`).
  - It acts on the sink `stream-operator-headphones` ONLY, found BY NAME in `pw-dump` on every call
    (`wpctl` takes only a numeric id, read live 24.10.2026, and the id changes with every PipeWire restart).
  - Never the default-sink alias, never another node; every change `wpctl set-volume -l 1.0` (no boost).
  - Rejected: pavucontrol / pasystray, which expose every card and every per-app stream ("nesmie sa babrat").
- **The panel item** (step 16h, stream role only, `_obs_box_panel_tint2rc_headphones`): a continuous tint2
  executor before the keyboard item (`panel_items = TSEEC` / `LTSEEC`). It shows `Slúchadlá NN %` /
  `Slúchadlá stlmené` / `Slúchadlá nedostupné`; wheel up/down = +-5 % (`up` also unmutes), left click = mute.
- **The keys:** `XF86AudioRaiseVolume` / `LowerVolume` / `Mute` (`obs_box_panel_headphones_keybinds_xml`)
  merged by step 12 into `~/.config/openbox/rc.xml` (seeded from `/etc/xdg/openbox/rc.xml` when absent)
  through the kiosk's ONE rc.xml filter, now `obs_box_openbox_rc_with_keys KEYS_FN` (the brightness merge is
  `obs_box_openbox_rc_with_keys obs_box_brightness_keybinds_xml`). The running openbox reads them at the next
  login or `openbox --reconfigure`.
- **Kept across restarts:** the helper keeps `{volume, restored}` in
  `~/.local/state/camera-box/operator-headphones.json` (every level change, also one made elsewhere and seen by
  `watch`). A sink is identified by `<core cookie>:<node id>`: PipeWire hands node ids out again after a restart,
  and the cookie is random per daemon start (review round 1: a restart between two looks reused the id, the
  watcher skipped the restore and KEPT the 100 %). A sink this box never restored (a login after a reboot, an
  operator-audio restart) gets the kept level back (at least 10 %, so it is audible) and is UNMUTED; the first
  session ever adopts the sink's current level as the kept one. A sink already restored (a restarted watcher
  after a panel restart) is adopted as it is, the operator's own mute included. The sink counts as gone only
  after `GONE_AFTER` (3) missed looks, so a slow pw-dump never flashes the label or re-runs the restore. Only
  this one sink's volume is restored; WirePlumber's own restore stays off.
- **Cost:** one `pw-dump` per second (~11 ms on stream-pp, 24.10.2026), a client that creates no node.
- **The stored mixer also covers the levels:** `alsactl store` writes each level control's dB as
  `dbvalue.N` (0.01 dB) in its comment; the mixer row needs Master / Headphone / PCM Playback Volume at 0 there
  too (stream-pp's file of 20.10.2026 held Master -23.25 dB: a reboot would have brought that back).
- **Tests:** `tests/python/test_stream_operator_jack_volume_1465.py` (fake `pw-dump` / `wpctl` that applies
  its calls to the dump, fake `amixer` / `alsactl`, an `asound.state` fixture with a second card's Speaker on
  first, so the reader must keep to the headphone card's own block).

## The monitor (`--output pulse`)

`systemd/strih-stream-monitor.service` runs `--output pulse`: VLC `--aout=pulse` with `PULSE_SINK=stream-operator-headphones`
(libpulse's default sink for that client; it is also the box's only sink). Before every VLC start the headphone
PCM must be held by PipeWire (`pcm_owner`, the thread's process by `Tgid`, comm `pipewire`) or closed (the
operator sink suspended until its first sound; the first live apply deadlocked on "closed = no-sink"), any other
holder = `busy`; and `pw-dump` (`--pw-dump`) must list the Audio/Sink
`stream-operator-headphones`, else `no-sink` too (review round 1: an UNLOCKED PipeWire also holds the card
while something plays, and a PULSE_SINK naming a missing sink falls back to the default sink -- maybe the
notebook speakers). `--check-state` reads the state file's `output` and inverts the
owner rule the same way. `--output alsa` (the default) is unchanged: the rollback of this trial.

## The stream OBS: no pulse capture input

The stream seed manifest carries `"pulse_capture": "forbid"`; `strih_scenes.py --bootstrap` then runs
`program_audio_vban.drop_pulse_capture_inputs` (status key `pulse capture`): every SCENE `pulse_input_capture`
/ `pulse_output_capture` input except the program-audio input is removed, read back, and named with its device
(stream-snv's `Zvuk z mixu` on strih-program-source). This includes one the operator made by hand: on the stream
role a pulse capture is never the operator's to keep (it could put his headphone mix on air); the seed line
names each removal with its device, so it can be rebuilt as another kind. A global channel (Settings > Audio) is only named --
obs-websocket's RemoveInput would not release the frontend's channel -- and verify FAILs it (set it Disabled).
The strih manifest has no such key (its `ASIO zvuk` IS the pulse program capture).

**Every scene item of the input is removed FIRST, then `RemoveInput` (issue 1462).** libobs drops a removed
source's items only while their scene RENDERS (`update_transforms_and_prune_sources` in `scene_video_render`),
and in Studio Mode the program renders a private duplicate. A scene that is never shown on its own (stream-snv's
`Development`, on the program) kept the item, the item kept the input, `RemoveInput` returned success and changed
nothing, and a second call was a no-op (`removed` was already set). Live 24.10.2026: `pulse capture=remove
FAILED: 'Zvuk z mixu'`. Any other OBS-WS removal of an input that scenes may reference needs the same order.

## Tests (Tier-0)

`tests/python/test_stream_operator_audio_1465.py` (fake udevadm / pw-dump / pw-metadata, the real lib under
`set -euo pipefail`) and `tests/python/test_stream_operator_monitor_seed_1465.py` (the monitor through the
1456 VLC fake, which logs its `PULSE_SINK`; a FakeObs for the seed). Seams: `STREAM_OPERATOR_AUDIO_SYS`,
`STREAM_OPERATOR_AUDIO_UDEVADM`, `STREAM_OPERATOR_AUDIO_SCENES`, the monitor's `STRIH_STREAM_MONITOR_ASOUND`,
`STRIH_SESSION_APPS_EUID`.

## UNVERIFIED until the live apply (nothing here ran on a box)

Item 3 revision (24.10.2026):
- that `alsactl store PCH` + alsa-restore bring the declared mixer back after a reboot (read `amixer -c PCH
  sget Speaker` / `'Auto-Mute Mode'` after the next boot);
- that tint2 17.0.1 runs `execp_uwheel_command` / `execp_dwheel_command` (read the item while turning the wheel);
- that wpctl's `get-volume` percentage equals the item's cube-root of the mean `channelVolumes`;
- that the notebook's Fn volume keys emit `XF86AudioRaiseVolume` / `LowerVolume` / `Mute` under openbox
  (`xev` on :0 if a key does nothing).

Item 3 (23.10.2026):

- the WirePlumber device name of each box's headphone card (step 1 reads it before anything changes);
- that the headphone card comes up on the `pro-audio` profile (the priority rule + `device.profile`): read
  FIRST after the restart (`wpctl status`: the sink exists; `pw-dump` the device's `Profile` param);
- the node rename through `monitor.alsa.rules` and the pro-audio node's 2 channels on the Scarlett;
- that WirePlumber 0.5.13 publishes the four restore keys in `sm-settings`;
- on stream-pp, the jack's Speaker mute holds from the monitor's first VLC start after login (the monitor
  starts VLC at once, idle until OBS publishes); the `(operator-audio-mixer)` row reads it.
Each of them shows as a FAIL row of item 9b, never a silent pass. The Tier-0 tests prove the rendered text and
the rule order, not how WirePlumber reads it (dev1 has no PipeWire): the live read-back is the gate before the
25.10.2026 event.

## Live apply (supervisor; stream-snv and stream-pp; nothing here restarts OBS)

1. **Read first, as the box user** (`newlevel`, `XDG_RUNTIME_DIR=/run/user/$(id -u)`): `wpctl status`;
   `cat /proc/asound/cards`; `pw-dump | python3 -c 'import json,sys; [print(o["info"]["props"].get("api.alsa.card"), o["info"]["props"].get("device.name")) for o in json.load(sys.stdin) if o.get("type")=="PipeWire:Interface:Device"]'`;
   `ls ~/.config/pipewire/pipewire.conf.d ~/.config/wireplumber/wireplumber.conf.d ~/.config/systemd/user/strih-stream-monitor.service.d 2>/dev/null`.
2. On stream-snv FIRST remove the 23.10.2026 hand drop-in
   `~/.config/systemd/user/strih-stream-monitor.service.d/10-scarlett.conf` (`--pcm-match "^USB Audio$"`; the
   install refuses while it exists), then `systemctl --user daemon-reload`.
3. **Install** from the deployed checkout: the full `sudo ./scripts/setup-strih.sh --box <box>`, or only the
   two steps with the "A few steps on a live box" driver of `strih-linux-provisioning.md`
   (`set -- --box <box>; . scripts/setup-strih.sh`, then `stream_operator_audio_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"`
   and `strih_session_apps_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"`). The step-12 log names the
   resolved device (`headphone Gen:0 USB Audio -> alsa_card.usb-... (from pw-dump|udev)`), the two files and
   each removed leftover. Step 16d installs the new monitor + unit and try-restarts the running monitor.
4. **Restart the operator's audio, as the box user:** `systemctl --user restart pipewire pipewire-pulse wireplumber`,
   then `systemctl --user restart strih-stream-monitor`. Chrome / the restreamer window reconnect by
   themselves; the stream OBS's program audio is the obs-vban source (not PipeWire).
5. **Read back:**
   - FIRST `wpctl status`: Audio > Devices = only the headphone card, Sinks = only `Operator headphones`,
     Sources = none. Zero sinks = the card did not take `pro-audio` (or the device name differs): roll back
     (step 6) at once, the operator hears nothing until then;
   - `pw-metadata -n settings | grep clock` = rate 48000, allowed `[ 48000 ]`, quantum / min / max 256;
   - `pw-metadata -n sm-settings | grep restore` = the four keys `false`;
   - `wpctl get-volume @DEFAULT_AUDIO_SINK@` = `Volume: 1.00` (HISTORY: since the item 3 revision the level is
     the operator's, see the next section);
   - `cat /proc/asound/card<N>/pcm0p/sub0/status` reads `closed` until something plays, then names the `pipewire` owner;
   - `journalctl --user -u strih-stream-monitor -n 20`: `watching ... pulse output to the sink 'stream-operator-headphones' ...`;
   - play a YouTube video in Chrome and start a stream: both in the headphones, nothing on the notebook
     speakers, VLC's slider and the YouTube volume move only their own app;
   - restart OBS once at a convenient moment (or wait for the next launch) so the seed removes `Zvuk z mixu`
     on stream-snv (its seed line names it);
   - `sudo ./scripts/verify-strih.sh --box <box>`: item 9b's `(operator-audio-*)` rows PASS (ten since the item 3
     revision, see the next section).
6. **Rollback** (the owner's "vyskusat"): `rm` the two `60-stream-operator-*.conf`, `systemctl --user restart
   pipewire pipewire-pulse wireplumber`; for the monitor a drop-in `[Service]` + `ExecStart=` + the unit's
   ExecStart without `--output pulse`, `systemctl --user daemon-reload`, restart the monitor. The strih-role
   leftovers are not put back (the stream role never needed them).

## Live apply of the item 3 revision (supervisor; stream-snv, then stream-pp; nothing here restarts OBS)

The rollout gate is cleared (restreamer 0.29.55 live on both boxes, issue comment 6384414395): a room open after a
`pipewire-pulse` restart ends the stale room-Chrome audio service by itself.

1. **Read first, as `newlevel`** (`XDG_RUNTIME_DIR=/run/user/$(id -u)`): `wpctl status` (stream-snv today: the
   only device the Scarlett), `cat /proc/asound/cards`, `amixer -c PCH sget Speaker; amixer -c PCH sget 'Auto-Mute Mode'`
   (stream-pp: `-c Generic_1`), `ls ~/.config/systemd/user/strih-stream-monitor.service.d`. A `--pcm-match`
   drop-in there makes step 12 refuse: remove it, `systemctl --user daemon-reload`.
2. **Install** from the deployed checkout (root): the full `sudo ./scripts/setup-strih.sh --box <box>`, or the
   live-box driver of `strih-linux-provisioning.md` running `stream_operator_audio_install` then
   `obs_box_panel_install` (step 16h writes the panel item; it try-restarts a running panel). Step 12 logs the
   resolved device (`headphone PCH:0 ALC256 Analog -> alsa_card.pci-0000_00_1f.3 ...` on stream-snv, plus the
   line naming the unused Focusrite), `mixer PCH: Speaker off, Auto-Mute Mode Enabled, Master / Headphone / PCM
   0 dB; stored with alsactl`, the helper and the rc.xml keys.
3. **Restart the operator's audio, as `newlevel`:** `systemctl --user restart pipewire pipewire-pulse
   wireplumber`, then `systemctl --user restart strih-stream-monitor`, then `DISPLAY=:0 openbox --reconfigure`
   (the keys). The panel item restores nothing yet (no level kept on a fresh box: the sink starts at 100 %).
4. **Acceptance (read back):**
   - `wpctl status`: Devices = only the notebook's HDA card (stream-snv: not the Scarlett), Sinks = only
     `Operator headphones`, Sources = none;
   - `amixer -c PCH sget Speaker` = `[off]`, `'Auto-Mute Mode'` = `Enabled`, Master / Headphone 0 dB `[on]`;
     `grep -A3 "Speaker Playback Switch" /var/lib/alsa/asound.state` under `state.PCH` = `false`;
   - the panel shows `Slúchadlá 100 %`; turn the wheel down 6 notches -> `Slúchadlá 70 %`, `wpctl get-volume
     <id>` = `0.70`; click -> `Slúchadlá stlmené`, click -> back; press the notebook's volume keys -> the item
     follows;
   - play a YouTube video in Chrome and start a stream: both in the headphones on the notebook jack, NOTHING
     on the notebook speakers (unplug the headphones: silence), VLC's slider and the YouTube volume move only
     their own app, the item moves both;
   - keep: `systemctl --user restart pipewire pipewire-pulse wireplumber` -> the item reads `Slúchadlá 70 %`
     again within a few seconds (restored, unmuted); a reboot gives the same and the Speaker stays off;
   - open a restreamer room: a `Google Chrome` stream on `Operator headphones` (`wpctl status`);
   - `sudo ./scripts/verify-strih.sh --box <box>`: item 9b's ten `(operator-audio-*)` rows PASS (the volume
     row names the level; muted = a NOTE).
5. **Rollback:** the item-3 rollback above (remove the two drop-ins, the monitor back to `--output alsa`). The
   stored mixer, the helper and the keys can stay: Speaker off on a jack is right either way.
