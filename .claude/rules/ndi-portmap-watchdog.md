---
paths:
  - "scripts/lib/ndi-portmap-health.sh"
  - "scripts/ndi-portmap-audit.sh"
  - "scripts/ndi-portmap-alert-watchdog.sh"
  - "scripts/ndi-portmap-baseline.json"
  - "tests/python/test_ndi_portmap_watchdog_1181.py"
  - "systemd/ndi-portmap-alert-watchdog.*"
---

# NDI sender port-map stability watchdog (#1181)

The dev1-side watchdog that alerts when a STRIH-SNV OBS NDI **sender** changes port (which silently
hands stock NDI Studio Monitor / building TVs the WRONG sender under a cached port). The operator
doctrine + the reshuffle root cause live in `.claude/rules/distroav-receiver-lifecycle.md`'s #1181
section; THIS file is the implementation gotchas for the three scripts.

## The three-file model (netcfg-parallel)

Same shape as `scripts/netcfg-audit.sh` + `scripts/netcfg-drift-alert-watchdog.sh` +
`scripts/lib/netcfg-audit.sh`:
- `scripts/lib/ndi-portmap-health.sh` — PURE map-diff (no I/O), source-only, Tier-0-testable.
- `scripts/ndi-portmap-audit.sh` — the avahi read + OBS-instance isolation + baseline JSON;
  `--capture`/`--check`/`--json`; exit **0=STABLE / 3=CHANGED (a moved port) / 2=gather error**.
- `scripts/ndi-portmap-alert-watchdog.sh` — dev1 timer, reuses `scripts/lib/obs-watchdog-decision.sh`
  confirm/throttle, ONE Slovak Discord alert, ships DISABLED.

## Gotchas that cost time to re-derive

- **avahi `-p` escapes are DECIMAL `\DDD`, NOT octal.** `\032`=space, `\040`=`(`, `\041`=`)`,
  `\092`=`\`. `ndi_avahi_unescape` decodes `d+0` (awk decimal). A resolved line (`=;…`) is
  semicolon-delimited: field **4**=escaped name, **7**=mDNS hostname, **8**=ip, **9**=port. The
  escaped name never contains a raw `;` (avahi escapes it `\059`), so splitting on `;` is safe.
- **The strih box advertises TWO NDI machine instances at the SAME IP (10.77.9.202) with the SAME
  `STRIH-SNV ` name prefix.** The OBS instance (mDNS host `…0000211c-c948`: 2ME PGM/PVW/Grading/
  MULTIVIEW/interkom) and a SEPARATE Arena/CG-bridge Spout (`…00001550-3342`: `Arena - bible`,
  :5961). Prefix+IP alone does NOT separate them. `ndi_portmap_select` isolates the OBS instance by
  the mDNS-hostname GROUP containing the anchor (`STRIH-SNV (2ME PGM)`, `NDI_PORTMAP_ANCHOR`), and
  BAILS (fail-safe empty = gather error, never a page) if the anchor is seen under >1 hostname.
- **The baseline stores name→port ONLY, never the mDNS-hostname suffix** — that hex is an
  NDI-instance implementation detail that can change on reinstall. `--check` RE-derives the OBS
  instance group each pass via the anchor, so hostname-suffix instability across restarts never
  matters; the reshuffle (same process = same hostname group, only ports move) is still caught.
- **An empty / anchor-absent live map is a GATHER ERROR (exit 2), NEVER a page.** OBS down / avahi
  unreachable / anchor renamed is box-reachability (#1001's lane), not a port change. Only a MOVED
  port on a still-present name pages (`ndi_portmap_verdict` → CHANGED iff any MOVED).
- **Root cause of the high PGM/PVW ports (for the #1185 pin follow-up):** DistroAV
  `vendor/distroav/src/plugin-main.cpp:478-491` defers `main_output_init()`/`preview_output_init()`
  to `OBS_FRONTEND_EVENT_FINISHED_LOADING` via `Qt::QueuedConnection` — AFTER the scene-collection
  `ndi_filter` republishes — so filters win the low ports (libndi assigns 5961+ in creation order).

## Tier-0 verify (#557 — no local cargo)

`bash -n` + `shellcheck -S warning` all three scripts; the map-diff + audit + watchdog are proven by
`tests/python/test_ndi_portmap_watchdog_1181.py`, which SHELLS OUT to bash (no cargo, no rig, no
avahi). Offline: set `NDI_PORTMAP_AVAHI_FIXTURE=<file>` on the audit to feed captured `avahi-browse
-rtp _ndi._tcp` output instead of running avahi. Re-capture the checked-in baseline from a LIVE read
only (`scripts/ndi-portmap-audit.sh --capture`), never hand-type it.
