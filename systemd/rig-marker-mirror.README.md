# rig-marker-mirror — install note (issue 1404)

Mirrors cam2's QPSK marker log (`/run/rig-qpsk-markers.csv`, written by the cam2 painter) into the
rig-lease server's serve dir every 10 s, so `http://dev1:8890/rig-qpsk-markers.csv` serves it to
restreamer's YouTube A/V gate (restreamer issue 357). Restreamer has no fleet ssh; the fleet
credentials stay with camera-box.

- `scripts/rig-marker-mirror.sh` — one pass: `camera_resolve cam2`, scp with the fleet credential
  (the `deploy-fleet.sh` default and `SSH_PASS` override), temp file + atomic rename into
  `$RIG_LEASE_SERVE_DIR` (default `/var/tmp/rig-lease-serve`). Any failure exits non-zero with an
  `ERROR rig-marker-mirror:` line and keeps the previous mirror.
- `rig-marker-mirror.timer` — every 10 s (`AccuracySec=1s`), shipped DISABLED.
- `scripts/rig-lease-server.py` serves the file: `text/csv`, `X-Mirror-Age-S` = seconds since the
  last successful pass, 404 while no pass ever succeeded. A consumer judges freshness from that
  header. EVENT mode purges the file on cam2, so during an event every pass fails and the age grows.

The serve dir is never the lease dir: the lease dir's existence means `held=true`.

## Supervisor install + live check (dev1)

The lease server must run the new code first: it gained the two issue-1404 routes, and the
running unit still serves the old code until it is restarted. `/rig-lease.json` and `/healthz`
are byte-identical (pinned by `tests/python/test_rig_marker_mirror_1404.py`). Restart only while
no E2E holds the lease, so restreamer's pre-StartStream read never meets a closed port.

```bash
# 0. the integrated dev checkout is what the units run (%h/devel/camera-box)
curl -s http://127.0.0.1:8890/rig-lease.json        # held must be false before the restart

# 1. reload the lease server on the new code
systemctl --user restart rig-lease-server.service
curl -s http://127.0.0.1:8890/healthz; echo        # -> ok
curl -s -o /dev/null -w '%{http_code}\n' http://dev1:8890/rig-qpsk-markers.csv   # -> 404 (no mirror yet)

# 2. one manual pass, then the timer
~/devel/camera-box/scripts/rig-marker-mirror.sh     # -> "mirror established cam2 (10.77.9.62) ..."
cp ~/devel/camera-box/systemd/rig-marker-mirror.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now rig-marker-mirror.timer

# 3. verify
curl -sI http://dev1:8890/rig-qpsk-markers.csv     # 200, text/csv, X-Mirror-Age-S <= 10
sleep 25; curl -sI http://dev1:8890/rig-qpsk-markers.csv | grep -i x-mirror-age   # still <= 10
journalctl --user -u rig-marker-mirror.service --since -5min   # no "Failed with result", no status=203
```

The success path is quiet by design (one line when the mirror is (re)established); a failed pass
prints its `ERROR` line every time.
