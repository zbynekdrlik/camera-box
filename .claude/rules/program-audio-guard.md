---
paths:
  - "scripts/program_audio.py"
  - "scripts/program_audio_ndi.py"
  - "scripts/program_audio_sampler.py"
  - "scripts/program_audio_guard.py"
  - "scripts/rig_serve_files.py"
  - "scripts/rig-marker-mirror.sh"
  - "scripts/rig_marker_mirror.py"
  - "systemd/program-audio-sampler.*"
  - "systemd/rig-marker-mirror.*"
  - "tests/python/test_program_audio_1404.py"
  - "tests/python/test_program_audio_guard_1404.py"
  - "tests/python/test_rig_marker_mirror_1404.py"
  - "tests/python/test_rig_serve_routes_1404.py"
---

# The stream program-audio guard + the cam2 marker mirror (issue 1404)

Two dev1 endpoints next to the rig lease on :8890. `scripts/rig-lease-server.py` serves them from
its SERVE dir (`scripts/rig_serve_files.py`): `$XDG_RUNTIME_DIR/rig-lease-serve`, overridable with
`$RIG_LEASE_SERVE_DIR`.
- **tmpfs:** the mirror rewrites a multi-MB file every 10 s, which must not wear the SSD.
- **0700:** no other dev1 account can plant a file.
- **Never the lease dir or inside it:** the lease dir's existence means `held=true`.
- **Only files this user owns are served.**

| Route | Writer | Contract |
|---|---|---|
| `/rig-qpsk-markers.csv` | `rig-marker-mirror` `--user` service (`scripts/rig-marker-mirror.sh` → `rig_marker_mirror.py`) | cam2's `/run/rig-qpsk-markers.csv`, complete rows; `text/csv`; `X-Mirror-Age-S` = seconds since new rows last arrived; 404 absent |
| `/program-audio.json` | `program-audio-sampler` `--user` service | `{schema, ts_utc, age_s, verdict, rms_dbfs, outside_band_pct, window_s, source, last_foreign_ts_utc, last_foreign_age_s[, reason]}`; both ages recomputed by the server per request; 404 absent; unreadable or foreign-owned = UNKNOWN |

The consumer CLI is `scripts/program_audio_guard.py`, used by both YouTube gates (camera-box and
restreamer issue 357):
- exit 0: MEASUREMENT or SILENT, fresh, and no FOREIGN window within `--max-age`;
- exit 1: FOREIGN. That includes a stale FOREIGN, and a clean current window when a FOREIGN window
  ended within `--latch-s` (default 30 s, its own hold, longer than `--max-age`): a gate that polls
  at least every ~25 s (a 10 s poll plus the guard's runtime has margin) never misses one;
- exit 2: UNKNOWN, stale (more than 1 s in the future counts as stale), unreachable, or a broken
  HTTP response (fail closed).

It prints one line: `program-audio verdict=<V> rms=<x> outside_band=<y>% age=<s>[ reason=…]`.

## Why spectral, not a level bar

The owner rule (issue 1404 comment 6016489928): nothing copyrighted on YouTube. A level bar cannot
tell the loud healthy QPSK marker from music. The marker (carrier 442 Hz) and its room sit in
200–800 Hz, so the verdict is the share of energy OUTSIDE that band:
- the FFT is per channel and the channel POWERS are summed. Never a mono downmix: the marker is on
  L and R ~10 ms apart, and an anti-phase pair would cancel to zero;
- the level gates SILENT;
- a NaN/Inf sample is UNKNOWN.

**The declared measurement tone lines** (`MEASUREMENT_TONE_LINES_HZ = (1000.0,)`, ±3 Hz) are
removed before measuring, from the level and from both sides of the share.
- The plan's Task 5 CG clip plays the QPSK marker over a −30 dBFS 1 kHz bed. Marker + bed read
  84 % outside the band, which would make every CG session stop itself.
- Why not count the bed as measurement: the louder bed would then dilute foreign content under it.
- A bed-only program reads SILENT.
- The Task 5 generator must import this constant.

## Calibration (6.10.2026) — the constants in `scripts/program_audio.py`, pinned by the tests

| Audio | windows | outside_band_pct | rms dBFS | verdict |
|---|---|---|---|---|
| Session recordings rec2/rec3a/rec3b/session (48 k stereo) | 1851 | 9.1 … 25.3 (p99 23.7) | −37.0 … −34.9 | MEASUREMENT |
| LIVE stream program via NDI (`STREAM-SNV (stream)`) | 15 + 11 | 12.4 … 22.1 | −35.9 … −35.5 | MEASUREMENT |
| LIVE SongPlayer program via NDI (`RESOLUME-SNV (SP-program)`, music) | 8 | 87.6 … 94.0 | −15.4 … −14.6 | FOREIGN |
| Generated white / pink / speech-shaped noise | — | 97.6 / 81.2 / 42.2 | (−20) | FOREIGN |

`FOREIGN_OUTSIDE_BAND_PCT = 30` (4.7 points over the measurement maximum) and
`SILENT_RMS_DBFS = −60` (23 dB under the quietest measurement window).

Known limits:
- **Quiet foreign content is missed.** Foreign content mixed well BELOW the measurement level is
  not caught: pink noise at −6 dB under it reads 26.2 % (at −3 dB, 32.3 %, caught). Music at
  program level is ~20 dB over the measurement and reads ~90 %.
- **Tonal content inside 200–800 Hz reads MEASUREMENT** (a soft C-E-G chord reads 0.7 %).
  Requiring the QPSK marker itself is the discriminator for that, a follow-up candidate.

Re-calibrate only from real program audio. Use `analyse()` over 2 s windows of a stream recording
or of a live NDI receive, and never tune the threshold to pass a single run.

## The marker mirror: ONE ssh connection, never a login per pass

One cam2 login writes **11 lines** into cam2's PERSISTENT journal on its USB stick
(`Storage=persistent`, `SystemMaxUse=200M`, measured 6.10.2026). A 10 s scp timer would have
meant ~95 000 lines a day, and a full multi-MB copy every 10 s, over the metered link when the rig
is at a venue. So the mirror is a long-running service holding one
`ssh … exec tail -c +1 -F --pid=$PPID /run/rig-qpsk-markers.csv`:
- **The replay is counted, never guessed.** The remote prints the file size (`stat`, 0 when
  absent) before `tail -c +1`.
  - The replay is complete once that many bytes arrived, or once a second session header arrived
    (the painter restarted mid-replay).
  - Nothing is written before that: not after a stall, not when the connection dies or the service
    stops mid-replay. An idle-gap rule failed review: a 50 ms marker cadence leaves no gap at all.
  - A replay not complete after 120 s (the file replaced between `stat` and the tail's open) ends
    the connection and reconnects for a fresh size.
  - A copy equal to, or a prefix of, the served file is not written, so a reconnect with no new
    rows keeps the served file and its age.
  - A new header that follows a half row on the same line (a truncation mid-row) starts the new
    session there.
- **Name following.** `-F` follows the name through a painter restart (`File::create` = truncate +
  a new `# qpsk-params` header, which restarts the copy) and through an EVENT purge + re-creation.
- **No leftover tail.** `--pid=$PPID` (the sshd session) ends the remote tail when the connection
  drops. Checked live: no leftover tail on cam2.
- **Writes.** Only complete rows are kept, written by temp + rename at most every 10 s and only when
  something changed. Live, about 63 bytes/s of new rows.
- **Reconnects.** A dropped connection logs `ERROR` with ssh's stderr and backs off
  10 → 300 s (back to 10 s after a connection that lived 300 s). The previous file is kept. The
  backoff is waited in 0.5 s slices: `time.sleep` resumes after SIGTERM, so one long sleep held a
  `systemctl stop` past its 90 s timeout. A failed write still stops the ssh process group.
- **Memory.** The copy grows with the painter session (~5 MB a day) in RAM and on tmpfs; cam2 holds
  the same file in its own tmpfs, and a painter restart starts it over. A line over 4096 bytes and
  bytes before the first session header are dropped and logged.
- **The password** goes through `sshpass -e` (`$SSHPASS`), never argv. A oneshot timer cannot hold
  a connection: systemd kills a ControlPersist master with the oneshot's cgroup.

## Traps

- **The NDI receiver is ctypes over `/usr/lib/ndi/libndi.so.6`.** The in-repo `src/ndi.rs`
  receiver is video-only (a NULL audio frame). Struct layouts come from the vendored SDK headers,
  pinned by a test.
- **The receiver is created BY NAME, audio-only.** It runs with a private, empty
  `NDI_CONFIG_DIR` (mDNS only, enforced in code; an extra-IP finder opens TCP discovery connections
  into senders, `.claude/rules/ndi-discovery.md`). Checked live: it still finds the sender.
- **Away at an event, the sampler finds nothing.** The rig sits behind tailscale, so the verdict is
  UNKNOWN and no audio crosses the mobile link.
- **The sampler writes UNKNOWN at every point it is not sampling:** at start, after 5 s without
  audio, when it cannot load libndi, and when it stops. A sample rate ≤ 0 is dropped (it once
  looped forever), and an NDI error frame sleeps instead of spinning.
- **Logging.** Both services log state changes, never per window or row. Private env files
  `~/.config/camera-box/*.env`, never the global `environment.d`.
- **Read-only on the rig.** Never play a test sound there (only the QPSK marker may sound):
  FOREIGN was verified live on the SongPlayer program sender, which already carries music.
- **Restart the lease server only while `held=false`.** `/rig-lease.json`, `/healthz` and the 404
  are pinned to golden bytes captured from the pre-change server.
