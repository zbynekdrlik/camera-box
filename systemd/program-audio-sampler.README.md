# program-audio-sampler — install note (issue 1404)

The YouTube channel guard. A dev1 `--user` service receives the stream program's audio from stream
OBS's NDI program output (`STREAM-SNV (stream)`). The receiver is audio-only and read-only, like
any NDI monitor. Every 2 s the service classifies the audio and rewrites `program-audio.json` in
the rig-lease server's serve dir. Gates call
`scripts/program_audio_guard.py --url http://dev1:8890/program-audio.json --max-age 10` and stop
the broadcast on any exit but 0 (1 FOREIGN, 2 UNKNOWN / stale / unreachable).

Verdicts, thresholds, calibration and limits: `.claude/rules/program-audio-guard.md`.

## Supervisor install + live check (dev1)

Prerequisite: the lease server runs the issue-1404 code (step 1 of `rig-marker-mirror.README.md`).

```bash
# 1. a 30 s foreground run first: the log must show "UNKNOWN -> MEASUREMENT" within ~3 s
timeout 30 python3 ~/devel/camera-box/scripts/program_audio_sampler.py
python3 ~/devel/camera-box/scripts/program_audio_guard.py   # verdict=UNKNOWN reason=sampler stopped, exit 2

# 2. install + start
cp ~/devel/camera-box/systemd/program-audio-sampler.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now program-audio-sampler.service

# 3. verify
sleep 6
python3 ~/devel/camera-box/scripts/program_audio_guard.py   # verdict=MEASUREMENT ... age<=3, exit 0
curl -s http://dev1:8890/program-audio.json; echo
journalctl --user -u program-audio-sampler -n 20
```

Never add an NDI extra-IP list (`~/.ndi/ndi-config.v1.json` `networks.ips`) on dev1 for this
receiver. With mDNS only it opens no TCP discovery connection into any sender
(`.claude/rules/ndi-discovery.md`). While the rig is away at an event, behind tailscale, the
receiver finds nothing. It then reads UNKNOWN and pulls nothing over the mobile link.
