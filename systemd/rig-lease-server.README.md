# rig-lease-server — install note (#1277)

Read-only HTTP exposure of the #830 cross-repo rig lease (`/var/tmp/rig-lease/`) on dev1, port
**8890**. It exists because `scripts/lib/rig-lease.sh`'s lockdir contract assumes both lease
participants run ON dev1's local filesystem — true for camera-box's own `full-path-e2e.yml`
runner, **false** for restreamer's OBS-driving E2E jobs, which run on the Windows **stream box**
(10.77.9.204) as a SYSTEM-level self-hosted runner: a different host that can never see dev1's
local lockdir at all. This server is the read-only window restreamer's pre-`StartStream` check
needs (restreamer#349) — no new SSH credential, no new lease participant, GET-only.

Full consumer contract for restreamer's own runner: `.claude/rules/rig-lease-http.md`.

## Unlike the alert-watchdog units in this directory, this one ships intended to run ENABLED

Every other `*-alert-watchdog.service`/`.timer` pair in this directory ships **disabled by
default** — an alert's absence merely delays a Discord notification. This is different: it is a
**coordination endpoint**, not an alarm. Without it, restreamer's pre-`StartStream` check has no
way to see whether camera-box currently holds the rig lease and must fail OPEN (proceed
regardless) — the exact race #830/#1277 exist to close. The supervisor still performs the actual
`enable --now` at integration (this ticket's own worker never installs/enables/starts anything);
this note documents the INTENDED end state, not something already done.

## Why GET-only, no auth

The payload is `held` (bool) + `holder` (repo/run_id/job/timestamps) + a TTL number — nothing
secret (the issue's own explicit call). The server never accepts a write; restreamer's own
"streaming in progress" state is already ITS lease signal toward camera-box (`rig-busy-gate.sh`'s
existing OBS-state check), so this endpoint only ever needs to be **read**, never written.

## Supervisor install + live-verify procedure

```bash
# 1. Manual smoke test — confirm the server serves BOTH shapes correctly before installing the unit:
python3 scripts/rig-lease-server.py --bind 127.0.0.1 --port 18890 &
curl -sS http://127.0.0.1:18890/rig-lease.json   # genuinely free lockdir -> {"held": false, ...}
curl -sS http://127.0.0.1:18890/healthz          # -> ok
kill %1

# 2. Install the --user unit (dev1) — REQUIRES linger enabled, or the unit stops the moment the
#    installing user's last login session ends (a --user manager without linger is torn down on
#    logout). Confirm/enable it once:
loginctl show-user "$USER" -p Linger   # must read "Linger=yes"; if not:
sudo loginctl enable-linger "$USER"
mkdir -p ~/.config/systemd/user
cp systemd/rig-lease-server.service ~/.config/systemd/user/
systemctl --user daemon-reload

# 3. Live-verify BEFORE (or right after) enabling:
systemctl --user start rig-lease-server.service
curl -sS http://10.77.9.103:8890/rig-lease.json   # LAN — the path restreamer's stream-box runner uses
curl -sS http://100.104.8.125:8890/rig-lease.json # tailscale (if reachable from wherever you check)
journalctl --user -u rig-lease-server -n 30

# 4. Enable it to survive reboots:
systemctl --user enable rig-lease-server.service
systemctl --user is-enabled rig-lease-server.service   # -> enabled

# 5. Confirm the LIVE lease transition is visible (proves "computed fresh per request", never a
#    stale cached snapshot) — during an actual E2E run holding the lease:
curl -sS http://10.77.9.103:8890/rig-lease.json   # before: held=false
#   ... start a full-path-e2e.yml run (or scripts/rig-busy-gate.sh acquire manually) ...
curl -sS http://10.77.9.103:8890/rig-lease.json   # during: held=true, holder={...}, stale=false
```

## What this does NOT do

- It does **not** write anything — no lease acquire/release/reclaim. `scripts/lib/rig-lease.sh`
  (via `scripts/rig-busy-gate.sh`) stays the SOLE writer of `/var/tmp/rig-lease/`; this server is a
  pure read window onto that same lockdir.
- It does **not** make restreamer a lease *participant* — restreamer never acquires or holds this
  lease. Its own "currently streaming" state is what protects the REVERSE direction
  (camera-box → restreamer), unchanged by this ticket.
- It does **not** run `RIG_LEASE_RUN_STATUS_CMD` (the bash lease's optional external
  "is the holder's run still alive" checker, unset in every real deployment) — the HTTP mirror's
  staleness verdict rests on the heartbeat age alone. See `scripts/rig_lease_state.py`'s own module
  doc for the full mirror contract.
- It does **not** provide real mount-namespace isolation on dev1 today — see the unit file's own
  `VERIFIED-INERT ON DEV1 TODAY` comment above `NoNewPrivileges=`/`PrivateTmp=`/`ProtectHome=`:
  under this box's current `--user`-unit + kernel-policy combination, systemd silently skips
  namespacing rather than failing the unit, so those directives are currently declared intent, not
  an active guarantee (re-verify against the live kernel policy before relying on them).

## Known cross-unit gotcha — `~/.config/environment.d/*.conf` is GLOBAL, not per-unit

Any file under `~/.config/environment.d/` (including this unit's own `rig-lease-server.conf`, if
you ever create one) is ingested by the systemd **user manager itself** at login and applied to
**every** `--user` unit on this box, not just this one — the SAME mechanism the sibling
`bundle-state-alert-watchdog.README.md` documents. If you add a `RIG_LEASE_DIR` override there to
point this server at a non-default lockdir (e.g. for a local test), it will ALSO leak into every
other `--user` unit dev1 runs, including any that happens to read the same-named variable for an
unrelated purpose. Prefer the unit's own `EnvironmentFile=` (already wired to
`rig-lease-server.conf` specifically) or the CLI flags for anything that must stay scoped to this
one service.

## Tunables (env, override via `~/.config/environment.d/rig-lease-server.conf` or the unit's own
`EnvironmentFile=`)

| Var | Default | Meaning |
|---|---|---|
| `RIG_LEASE_DIR` | `/var/tmp/rig-lease` | the SAME lockdir `scripts/lib/rig-lease.sh` reads/writes — must never diverge |
| `RIG_LEASE_STALE_SECS` | `5400` | heartbeat-staleness threshold — must match `scripts/rig-busy-gate.sh`'s own default (locked by a pytest lock-step test) |

CLI flags (`--bind`/`--port`/`--lease-dir`/`--stale-secs`) always override the env; see
`scripts/rig-lease-server.py --help`.
