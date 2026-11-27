---
paths:
  - "src/cambox_video_stats.rs"
  - "src/cambox_video_http.rs"
  - "tests/cambox_video_stats_1540.rs"
  - "tests/harness_cambox_video_wiring_1540.rs"
---

# The cambox video counters endpoint `:8896/video.json` (issue 1540 slice 2)

The cambox side of the dcontroller video-monitoring contract (schema 1; the draft field list is
issue 1540 comment 7034656554, the slice-2 decisions comment 7126682640). The OBS-box side is
`scripts/bundle_state_video.py` (`:8899/video-connections.json`).

- **Two crate-root modules, main.rs only calls them.** `cambox_video_stats` = the atomics
  (`CamboxVideoCounters`), the folds, the pure schema-1 `render_json`, `rfc3339_utc`, `run_id`,
  `poll_due`, `NodeMeta::from_env`; `cambox_video_http` = a std `TcpListener` thread. No HTTP crate,
  no tokio in the server: one read-only GET every 5-10 s.
- **Off the realtime path.** The capture thread folds each finished 5-s report window once
  (`note_capture_window`, inside the `if elapsed.as_secs() >= 5 {` block, BEFORE `frame_count` /
  `emit_count` reset) and polls the JPEG XS window at most once a second
  (`poll_due` + `note_jxs_poll(h.take_window())`). Never add a per-frame call.
- **`JxsHandoff::take_window()` is consumed here and nowhere else on a cambox.** It hands each
  vstack window out ONCE (by sequence); a second caller would steal windows from these totals.
  vstack still logs its own `jxs send:` line. A skipped sequence counts in `windows_unread`.
- **The device counters are stored, the window counters summed.** `capture_dropped` /
  `capture_corrupted` are cumulative in vstack already; `capture_lost` adds
  `max(0, sent - captured)` per window (the leg-health term).
- **One deadline per connection** (`CONNECTION_DEADLINE`, 2 s) over request + answer + drain: the
  server takes one connection at a time, so a per-read timeout let a byte-trickling client hold it
  for hours. The test `a_silent_or_trickling_client_is_cut_off...` proves it; keep it.
- **Known limits:** burn (probe/E2E) mode moves the JPEG XS link to the burn thread, so its record
  stays `connected: false`; the jxs record is listed only when the sender started; the bind is
  0.0.0.0 without auth (rig LAN only, read-only).
- `CAMERA_BOX_VIDEO_STATS_PORT` (default 8896, `0` = off). A bind failure is one WARN.
- **Local `cargo mutants` on these files is impractical** (each mutant rebuilds every binary:
  ~5 min per mutant here); rely on the CI diff gate and prove a test bites with a hand mutant.

## Router entry (moved from CLAUDE.md, issue 1552)

- Cambox video counters for dcontroller (`GET :8896/video.json`, issue 1540: atomics folded once per 5-s window, the 1-s JPEG XS `take_window` poll, one deadline per connection) → `.claude/rules/cambox-video-stats.md` (auto-loads on its `paths:`)
