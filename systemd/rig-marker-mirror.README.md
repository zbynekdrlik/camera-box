# rig-marker-mirror — install note (issue 1404)

Mirrors cam2's QPSK marker log (`/run/rig-qpsk-markers.csv`, written by the cam2 painter) into the
rig-lease server's serve dir, so `http://dev1:8890/rig-qpsk-markers.csv` serves it to restreamer's
YouTube A/V gate (restreamer issue 357). Restreamer has no fleet ssh; the fleet credentials stay
with camera-box.

- `scripts/rig-marker-mirror.sh` resolves cam2 (`camera_resolve`) and the fleet credential (the
  `deploy-fleet.sh` default and the `SSH_PASS` override), then runs `scripts/rig_marker_mirror.py`.
- `rig_marker_mirror.py` holds ONE ssh connection streaming
  `tail -c +1 -F --pid=$PPID /run/rig-qpsk-markers.csv`. One cam2 login writes 11 lines into
  cam2's persistent stick journal, so it never logs in every pass.
  - The remote announces the file size first; nothing is written until that many bytes have
    arrived (the replay), so a consumer never gets a prefix.
  - Complete rows go into `$XDG_RUNTIME_DIR/rig-lease-serve/rig-qpsk-markers.csv` by temp + rename,
    at most every 10 s, and only when there are new rows.
  - A dropped connection is retried with a 10 → 300 s backoff, and the previous file is kept.
  - While cam2's ping RTT is over 20 ms (the rig at a venue, behind tailscale over metered mobile
    data) it does not connect at all: no marker log is pulled over that link.
- `rig-marker-mirror.service`: `--user`, long-running, `Restart=always`, shipped DISABLED. There is
  no timer.
- The lease server serves it as `text/csv` with `X-Mirror-Age-S` = seconds since new rows last
  arrived. A stopped painter, an EVENT-mode purge or a dead connection all read as a growing age.
  A consumer judges freshness from that header.

## Supervisor install + live check (dev1)

The lease server must run the new code first: it gained the two issue-1404 routes, and the
running unit serves the old code until it is restarted. `/rig-lease.json`, `/healthz` and the 404
are byte-identical (golden-pinned by `tests/python/test_rig_serve_routes_1404.py`). Restart only
while no E2E holds the lease, so restreamer's pre-StartStream read never meets a closed port.

```bash
# 0. the integrated dev checkout is what the units run (%h/devel/camera-box)
curl -s http://127.0.0.1:8890/rig-lease.json        # held must be false before the restart

# 1. reload the lease server on the new code (its default serve dir: $XDG_RUNTIME_DIR/rig-lease-serve)
systemctl --user restart rig-lease-server.service
curl -s http://127.0.0.1:8890/healthz; echo        # -> ok
curl -s -o /dev/null -w '%{http_code}\n' http://dev1:8890/rig-qpsk-markers.csv   # -> 404 (no mirror yet)

# 2. a 20 s foreground run, then the service
~/devel/camera-box/scripts/rig-marker-mirror.sh --max-runtime 20
curl -sI http://dev1:8890/rig-qpsk-markers.csv     # 200, text/csv, X-Mirror-Age-S small
cp ~/devel/camera-box/systemd/rig-marker-mirror.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now rig-marker-mirror.service

# 3. verify
sleep 25; curl -sI http://dev1:8890/rig-qpsk-markers.csv | grep -i -E 'x-mirror-age|content-length'   # age <= ~11
journalctl --user -u rig-marker-mirror -n 20       # "following ...", no ERROR lines
```

A rotated fleet password goes into `~/.config/camera-box/rig-marker-mirror.env` (`SSH_PASS=…`,
mode 0600). Never `~/.config/environment.d/`: that is the user manager's global environment, so
every `--user` unit would get the password.
