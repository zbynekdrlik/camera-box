# program-audio-sampler — install note (issue 1404)

The YouTube channel guard. A dev1 `--user` service receives the stream program's audio from stream
OBS's NDI program output (`STREAM-SNV (stream)`). The receiver is audio-only and read-only, like
any NDI monitor. Every 2 s the service classifies the audio and rewrites `program-audio.json` in
the rig-lease server's serve dir. MEASUREMENT needs the cam2 QPSK marker itself (a timecode chain of
4 markers over the trailing 4 s), decoded by the dock's own decoder through a small library built on
dev1 with g++ (`scripts/build-qpsk-guard-shim.sh`). Nothing reads MEASUREMENT in the first 4 s after a
start or a span restart: a window whose spectrum alone says FOREIGN reads FOREIGN, the rest UNKNOWN.
The span follows the SENDER's NDI audio timeline (the frames' SDK timestamps), never dev1's arrival
time: a late delivery while dev1 is busy keeps it, a hole up to 250 ms AHEAD of the timeline is
bridged with zeros and keeps it, and a frame behind the timeline, a longer hole or a larger date
step restarts it (one warm-up); only a frame without a timestamp falls back to a 1 s arrival gap.
Gates call
`scripts/program_audio_guard.py --url http://dev1:8890/program-audio.json --max-age 10` and stop
the broadcast on any exit but 0 (1 FOREIGN, also a FOREIGN window within `--latch-s` 30 s; 2 UNKNOWN /
stale / unreachable). The served file lives in `$XDG_RUNTIME_DIR/rig-lease-serve` (tmpfs).

Verdicts, thresholds, calibration and limits: `.claude/rules/program-audio-guard.md`.

## Supervisor install + live check (dev1)

Prerequisite: the lease server runs the issue-1404 code (step 1 of `rig-marker-mirror.README.md`).

**The sampler unit is already installed and enabled on dev1** (issue-1404 Task 2), and it runs from
the `~/devel/camera-box` checkout. So the steps below are due the moment that checkout moves to the
marker-requirement code, not at some later install: until then the old process keeps writing
MEASUREMENT on the spectral share alone. The lease server serves such a payload as UNKNOWN once IT
runs the new code (step 1), and the camera-box guard refuses it either way; both fail closed.

```bash
# 1. reload the lease server on the new code, ONLY while the lease is free: it serves a MEASUREMENT
#    without a marker chain as UNKNOWN to every reader (restreamer too). The restart runs only when
#    the lease reads held=false; otherwise re-run this step later (every step order fails closed).
if curl -sf http://127.0.0.1:8890/rig-lease.json \
     | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("held") is False else 1)'; then
  systemctl --user restart rig-lease-server.service
else
  echo "rig lease held or unreadable -- lease server NOT restarted, retry later"
fi

# 2. build the QPSK marker decoder library (g++, a few seconds) -- the sampler refuses to run
#    without it (UNKNOWN + exit 1), and a sampler process started before this change keeps writing
#    MEASUREMENT without a marker chain, which the new guard reads as UNKNOWN until it restarts.
#    Rebuild after every pull that touches scripts/qpsk_guard_shim.cpp or
#    vendor/av-sync-dock/src/camera-box-{audio,marker-scan}.hpp (the sampler logs a WARNING when the
#    library was built from other sources). The build renames the new library over the old one, so
#    a running sampler keeps its own copy until it restarts.
bash ~/devel/camera-box/scripts/build-qpsk-guard-shim.sh   # -> ~/.local/lib/camera-box/libqpsk-guard-shim.so

# 3. a 30 s foreground run: the log must show "marker decoder ... params={... 'carrier_hz': 442 ...}"
#    and "UNKNOWN -> MEASUREMENT" within ~5 s (the first 4 s are the marker warm-up)
timeout 30 python3 ~/devel/camera-box/scripts/program_audio_sampler.py
python3 ~/devel/camera-box/scripts/program_audio_guard.py   # verdict=UNKNOWN reason=sampler stopped, exit 2

# 4. install + (re)start (a running sampler from before this change MUST be restarted)
cp ~/devel/camera-box/systemd/program-audio-sampler.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable program-audio-sampler.service
systemctl --user restart program-audio-sampler.service

# 5. verify
sleep 8
python3 ~/devel/camera-box/scripts/program_audio_guard.py   # verdict=MEASUREMENT ... markers=8 chain=7 (chain >= 6), exit 0
curl -s http://dev1:8890/program-audio.json; echo            # carries "markers_decoded" + "marker_chain"
journalctl --user -u program-audio-sampler -n 20             # no WARNING about the decoder sources
```

## Update: span continuity on the sender timeline (issue 1404, design 6030385284)

Only the sampler's Python changed (`program_audio.py`, `program_audio_sampler.py`,
`program_audio_ndi.py`); the shim, the unit and the lease server did not. Once the `~/devel/camera-box`
checkout carries the change, restart the sampler. The restart writes UNKNOWN for ~4 s (start +
warm-up), and restreamer stops a running YouTube session on 2 consecutive UNKNOWN polls or 3 within
60 s, so restart only while the rig lease is free:

```bash
if curl -sf http://127.0.0.1:8890/rig-lease.json \
     | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("held") is False else 1)'; then
  systemctl --user restart program-audio-sampler.service
else
  echo "rig lease held or unreadable -- sampler NOT restarted, retry later"
fi
sleep 8
python3 ~/devel/camera-box/scripts/program_audio_guard.py      # verdict=MEASUREMENT ... chain >= 6, exit 0
journalctl --user -u program-audio-sampler -n 5 --no-pager      # start line: continuity=sender timeline (frame+20ms)
# after 10 min: the summary line reads receive_gaps=0 and few timeline_breaks (a real hole in the
# sender audio, the nightly date step; 1 in the 25-min STEP-0 run), and max_offset_ms (the sender's
# largest jitter on frames that continued) well under the 41.3 ms tolerance (STEP 0: 24.5 and 29.5);
# a busy dev1 shows up as late_bursts=N with "late burst after 1.x s without audio: the sender
# timeline continues" lines, and no MEASUREMENT -> UNKNOWN transition for them
journalctl --user -u program-audio-sampler --since -15min --no-pager | grep -E 'summary|late burst|discontinuity'
```

## Update: a short hole is bridged, not a restart (issue 1404, design 6036098516)

Only `program_audio.py` and `program_audio_sampler.py` changed; the shim, the unit and the lease
server did not. On 7.10.2026, under dev1 load, the receiver dropped two NDI frames at a time: 55
`audio timeline discontinuity` lines in an hour, each a 4 s UNKNOWN warm-up (one summary:
`timeline_breaks=33 UNKNOWN=22`), which stopped restreamer's YouTube gate. A frame up to 250 ms ahead
of the timeline is now bridged: the missing samples go into the window as zeros, the marker span is
kept, and the chain is decoded over the real samples only. Restart the sampler the same way as
above, only while the rig lease is free:

```bash
if curl -sf http://127.0.0.1:8890/rig-lease.json \
     | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("held") is False else 1)'; then
  systemctl --user restart program-audio-sampler.service
else
  echo "rig lease held or unreadable -- sampler NOT restarted, retry later"
fi
sleep 8
python3 ~/devel/camera-box/scripts/program_audio_guard.py      # verdict=MEASUREMENT ... chain >= 6, exit 0
journalctl --user -u program-audio-sampler -n 5 --no-pager      # start line: ... holes up to 250ms bridged
curl -s http://dev1:8890/program-audio.json; echo               # carries "holes_bridged" + "bridged_ms"
# after 10 min: the summary line ends "holes_bridged=N bridged_ms=X"; a +44.7 ms hole reads
# "audio timeline hole: the frame sits +44.7 ms ahead ... bridged with 2146 zero samples (44.7 ms),
# the marker span is kept" (round(offset * 48 kHz): 1987...2525 for the live +41.4...+52.6 ms) with no
# MEASUREMENT -> UNKNOWN for it; timeline_breaks counts only the holes over 250 ms, frames behind
# the timeline, a format change at a hole and the larger date steps
journalctl --user -u program-audio-sampler --since -15min --no-pager | grep -E 'summary|bridged|discontinuity'
```

To re-check the marker bars on new real audio (for example after a decoder change), run
`python3 ~/devel/camera-box/scripts/program_audio_marker_calibrate.py --real <recordings…>
--synthetic-trials 50`; it exits 1 when a bar fails.

The sampler runs its receiver with a private, empty `NDI_CONFIG_DIR`, so an NDI extra-IP list on
dev1 can never reach it. With mDNS only it opens no TCP discovery connection into any sender
(`.claude/rules/ndi-discovery.md`). While the rig is away at an event, behind tailscale, the
receiver finds nothing. It then reads UNKNOWN and pulls nothing over the mobile link.

Overrides (`PROGRAM_AUDIO_SOURCE`, `RIG_LEASE_SERVE_DIR`, `NDI_LIB_PATH`, `QPSK_GUARD_SHIM`) go into
`~/.config/camera-box/program-audio-sampler.env`, never `~/.config/environment.d/` (the user
manager's global environment).
