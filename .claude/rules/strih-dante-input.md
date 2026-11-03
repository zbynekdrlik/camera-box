---
paths:
  - "scripts/lib/strih-dante-input.sh"
  - "scripts/dante_input_continuity.py"
  - "scripts/lib/program-audio-continuity.sh"
  - "tests/python/test_strih_dante_input_1485.py"
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
  input kind, the `dante-input start: config=` line (`device_rate=96000 halved=true`), any
  `dante-input error:` line, and two `dante-input status:` lines 12 s apart (clock=media, holdover 0, a
  grandmaster, delivered_frames advancing; any of the gate's loss counters GROWING between the two lines
  FAILs, a total since the start that holds still is only named). The `(vban-leg) vban` arrival
  row is a NOTE on a Dante box. PipeWire stays purged (`dante:strih-obs` counts as a direct program).
- **The E2E gate:** `program-audio-continuity.sh` also marks the strih OBS log (through `strih-log-read.sh`)
  when `$STRIH` is a Dante box, and folds `scripts/dante_input_continuity.py`'s window result into the
  `program_audio_continuity` block (`strih_dante`, the worse verdict, `strih Dante:` reasons, so the
  report needs no second key): any rx_loss / rx_holes / underruns (skipped blocks) / rx_gaps / socket_drops /
  rebootstraps / rx_written_late / re_anchors increase (the obs project's meanings, 3.11.2026: audio other
  than FOH sent; rx_late, overruns, late_blocks are timing only and reported; a line before obs v0.3.0 has
  no rx_holes and reads UNKNOWN; in v0.2.0 `underruns` mixed late blocks with skips, so it proves nothing), clock=free or holdover=1, or a `dante-input error:` in the window = FAIL; no
  opening line, a restart, a counter backwards, no delivered frames = UNKNOWN = FAIL. The closing line is
  the second status line after the stop.
- Clocks + latency: `.claude/rules/audio-clock-map.md` (Dante on strih-snv).
