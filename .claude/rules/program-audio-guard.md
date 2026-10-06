---
paths:
  - "scripts/program_audio.py"
  - "scripts/program_audio_ndi.py"
  - "scripts/program_audio_sampler.py"
  - "scripts/program_audio_guard.py"
  - "scripts/rig_serve_files.py"
  - "scripts/rig-marker-mirror.sh"
  - "systemd/program-audio-sampler.*"
  - "systemd/rig-marker-mirror.*"
  - "tests/python/test_program_audio_1404.py"
  - "tests/python/test_program_audio_guard_1404.py"
  - "tests/python/test_rig_marker_mirror_1404.py"
---

# The stream program-audio guard + the cam2 marker mirror (issue 1404)

Two dev1 endpoints next to the rig lease on :8890. They are served by `scripts/rig-lease-server.py`
from its SERVE dir (`/var/tmp/rig-lease-serve`, `$RIG_LEASE_SERVE_DIR`, `scripts/rig_serve_files.py`).
The serve dir is never the lease dir, whose existence means `held=true`.

| Route | Writer | Contract |
|---|---|---|
| `/rig-qpsk-markers.csv` | `scripts/rig-marker-mirror.sh`, `--user` timer every 10 s | cam2's `/run/rig-qpsk-markers.csv`, byte-identical; `text/csv`; `X-Mirror-Age-S`; 404 absent |
| `/program-audio.json` | `scripts/program_audio_sampler.py`, `--user` service | `{schema, ts_utc, age_s, verdict, rms_dbfs, outside_band_pct, window_s, source[, reason]}`; `age_s` recomputed by the server at every request; 404 absent; an unreadable file is served as UNKNOWN |

The consumer CLI is `scripts/program_audio_guard.py`, used by both YouTube gates (camera-box and
restreamer issue 357):
- exit 0: MEASUREMENT or SILENT, and fresh;
- exit 1: FOREIGN, even when stale;
- exit 2: UNKNOWN, stale, unreachable or unreadable (fail closed).

It prints one line: `program-audio verdict=<V> rms=<x> outside_band=<y>% age=<s>[ reason=…]`.

## Why spectral, not a level bar

The owner rule (issue 1404 comment 6016489928): nothing copyrighted on YouTube. A level bar cannot
tell the loud healthy QPSK marker from music. The marker (carrier 442 Hz) and its room sit in
200–800 Hz, so the verdict is the share of energy OUTSIDE that band:
- the FFT is per channel and the channel POWERS are summed. Never a mono downmix: the marker is on
  L and R ~10 ms apart, and their sum comb-filters it;
- the level gates SILENT.

## Calibration (6.10.2026) — the constants in `scripts/program_audio.py`, pinned by the tests

| Audio | windows | outside_band_pct | rms dBFS | verdict |
|---|---|---|---|---|
| Session recordings rec2/rec3a/rec3b/session (48 k stereo) | 1851 | 9.1 … 25.3 (p99 23.7) | −37.0 … −34.9 | MEASUREMENT |
| LIVE stream program via NDI (`STREAM-SNV (stream)`) | 15 | 12.4 … 22.1 | −35.9 … −35.5 | MEASUREMENT |
| LIVE SongPlayer program via NDI (`RESOLUME-SNV (SP-program)`, music) | 8 | 87.6 … 94.0 | −15.4 … −14.6 | FOREIGN |
| Generated white / pink / speech-shaped noise | — | 97.6 / 81.2 / 42.2 | (−20) | FOREIGN |

`FOREIGN_OUTSIDE_BAND_PCT = 30` (4.7 points over the measurement maximum) and
`SILENT_RMS_DBFS = −60` (23 dB under the quietest measurement window).

Known limit: foreign content mixed well BELOW the measurement level is missed. Pink noise at −6 dB
under the measurement reads 26.2 %; at −3 dB it reads 32.3 % and is caught. Music at program level is
~20 dB OVER the measurement and reads ~90 %.

Re-calibrate only from real program audio. Use `analyse()` over 2 s windows of a stream recording
or of a live NDI receive, and never tune the threshold to pass a single run.

## Traps

- **The NDI receiver is ctypes over `/usr/lib/ndi/libndi.so.6`.** The in-repo `src/ndi.rs`
  receiver is video-only (a NULL audio frame). Struct layouts come from the vendored SDK headers,
  pinned by `test_ndi_struct_layout_matches_the_sdk_headers`.
- **The receiver is created BY NAME** (the SDK runs the finder and reconnects by itself), with
  `NDIlib_recv_bandwidth_audio_only`.
- **mDNS only.** Never add an NDI extra-IP list on dev1: an extra-IP finder opens a TCP discovery
  connection into each listed sender (`.claude/rules/ndi-discovery.md`, issue 1389).
- **Away at an event, the sampler finds nothing.** The rig sits behind tailscale, so the verdict is
  UNKNOWN and no audio is pulled over the mobile link.
- **The sampler writes UNKNOWN at every point it is not sampling:** at start, after 5 s without
  audio, when it cannot load libndi, and when it stops. The server's per-request `age_s` covers a
  sampler that died without writing.
- **Noise budget.** The mirror runs ~8600 times a day: its unit sets `LogLevelMax=notice` +
  `SyslogLevel=notice`, and the script prints on success only when the mirror is (re)established.
  The sampler logs verdict changes plus a 10-minute summary.
- **Read-only on the rig.** A live check receives an existing sender. Never play a test sound on
  the rig (only the QPSK marker may sound there): FOREIGN was verified live on the SongPlayer
  program sender, which already carries music.
- **Restart the lease server only while `held=false`** (the runbook in
  `systemd/rig-marker-mirror.README.md`). The existing routes stay byte-identical.
