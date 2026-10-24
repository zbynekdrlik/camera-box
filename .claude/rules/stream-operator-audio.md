---
paths:
  - "scripts/stream_operator_audio.py"
  - "scripts/lib/stream-operator-audio.sh"
  - "scripts/strih_stream_monitor.py"
  - "systemd/strih-stream-monitor.service"
  - "tests/python/test_stream_operator_audio_1465.py"
  - "tests/python/test_stream_operator_monitor_seed_1465.py"
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

## What is allowed (everything else is denied)

`scripts/stream_operator_audio.py` declares it, `scripts/lib/stream-operator-audio.sh` installs and grades it.

- **The device** = the stream monitor's own rule (`strih_stream_monitor.resolve_headphone`, imported, never
  copied): the ONE USB audio interface's playback PCM (stream-snv: the Scarlett Solo 4th Gen, card `Gen`),
  else the jack (stream-pp: `ALC257 Analog` on `Generic_1`).
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
- **The speaker path on a jack** stays the monitor's own act: `mixer_unity_cmds` (Master / Headphone / PCM
  0 dB, Speaker mute) before every VLC start -- with pro-audio PipeWire never rewrites the mixer, so that is
  where the unity writes are "still right". A USB box keeps its own mixer (unchanged).
- **Plugging a headphone box in or out needs setup step 12 again** (the drop-in names ONE device; the
  player alone would switch at its next VLC start, the locked PipeWire does not).
- **The sink volume** is 100 % and unmuted, read back by verify; nothing in the box changes it (no restore, no
  mixer UI). The per-app level is VLC's slider and the YouTube player's.

## setup-strih step 12 / verify-strih item 9b (both at 1000 lines)

- `obs_box_step_applies 12-strih-audio` (strih role only) branches step 12: the stream role runs
  `stream_operator_audio_install` (both drop-ins through `obs_box_write_if_changed`, then removes the five
  strih-role leftovers `STREAM_OPERATOR_AUDIO_LEFTOVERS`); a box where no headphone device resolves fails
  step 12 with nothing written or removed. Setup NEVER restarts the operator's audio (the runbook below).
- **A monitor drop-in with `--pcm-match`** (the 23.10.2026 stream-snv hand drop-in `10-scarlett.conf`) is
  refused by the install and FAILs the files row: the player and the locked PipeWire must pick the SAME card
  (review round 1). Remove it before setup.
- Item 9b on the stream role: `stream_operator_audio_grade_report` = 9 rows: `(operator-audio-files)` (both
  files as rendered, no leftover), the module's six read-back rows from `pw-dump` + `pw-metadata -n settings`
  / `-n sm-settings` run in the operator session (`-sink` exactly one Audio/Sink = the operator sink on the
  headphone device; `-cards` no other ALSA device, no bluez; `-capture` no Audio/Source; `-volume` 1.0
  unmuted; `-clock`; `-restore`), `(operator-audio-mixer)` (the jack: Speaker off, Master / Headphone at 0 dB
  unmuted, read with `amixer -c <id> sget`, a control the card lacks named; a USB box: a NOTE), and
  `(operator-audio-obs)` = `strih_scenes.py --pulse-capture-inputs`
  (none; a NOTE while OBS does not answer). An unreadable session FAILs every read-back row.
- The strih role keeps item 9b's MiniFuse/quantum grade unchanged.

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
   - `wpctl get-volume @DEFAULT_AUDIO_SINK@` = `Volume: 1.00`;
   - `cat /proc/asound/card<N>/pcm0p/sub0/status` reads `closed` until something plays, then names the `pipewire` owner;
   - `journalctl --user -u strih-stream-monitor -n 20`: `watching ... pulse output to the sink 'stream-operator-headphones' ...`;
   - play a YouTube video in Chrome and start a stream: both in the headphones, nothing on the notebook
     speakers, VLC's slider and the YouTube volume move only their own app;
   - restart OBS once at a convenient moment (or wait for the next launch) so the seed removes `Zvuk z mixu`
     on stream-snv (its seed line names it);
   - `sudo ./scripts/verify-strih.sh --box <box>`: item 9b's nine `(operator-audio-*)` rows PASS (the mixer
     row is a NOTE on the Scarlett).
6. **Rollback** (the owner's "vyskusat"): `rm` the two `60-stream-operator-*.conf`, `systemctl --user restart
   pipewire pipewire-pulse wireplumber`; for the monitor a drop-in `[Service]` + `ExecStart=` + the unit's
   ExecStart without `--output pulse`, `systemctl --user daemon-reload`, restart the monitor. The strih-role
   leftovers are not put back (the stream role never needed them).
