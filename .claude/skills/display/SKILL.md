---
name: display
description: >
  camera-box `--display` (local HDMI preview) path — connector-presence detection,
  upscale cap, and the capture-dropped counter. Load when touching src/display.rs or
  src/ndi_display.rs, the framebuffer render, or diagnosing display CPU spikes / a
  phantom (latched) framebuffer. Relates #130, #135, #131, #105, #125.
---

# camera-box `--display` path

`--display <NDI source>` renders an NDI return feed to the local HDMI framebuffer
(`/dev/fb0`) on a low-priority decoupled thread (`main.rs` spawns it; `apply_low_priority`
sets nice 19 + core-0 affinity). It is SECONDARY — it must NEVER degrade capture/NDI-emit.

## Phantom / latched framebuffer — the core gotcha (#135)

i915 KMS LATCHES the last monitor's mode on fb0. After a monitor is hot-unplugged, the
connector goes `disconnected` but **fb0 stays latched at the old resolution** — a "phantom"
fb that `open()`s and `write()`s fine. So neither the fb-open retry nor the write-failure
path detects "no monitor". The ONLY reliable presence signal is the DRM connector status in
sysfs:

```
/sys/class/drm/<cardN>-<connector>/status   # connected | disconnected
/sys/class/drm/<cardN>-<connector>/enabled  # enabled   | disabled
/sys/class/drm/<cardN>-<connector>/modes    # list, or [] when none
```

- **Real monitor:**  `status=connected   enabled=enabled  modes=[1920x1080,...]`
- **Phantom fb:**    `status=disconnected enabled=disabled modes=[]`

The connector SCANNING OUT to fb0 is the one with `enabled=enabled`; a real monitor on it
is `status=connected enabled=enabled`. So presence = BOTH `enabled=enabled` AND
`status=connected` (`connector_is_connected()`). Requiring `enabled` is what scopes the
check to the fb-driving connector — on a MULTI-connector box (cam2 has 3 HDMI connectors) a
second monitor on a non-enabled connector must NOT mask the fb connector's phantom state.
`any_connector_connected("/sys/class/drm")` scans all `cardN-*` dirs (non-connector dirs like
`renderD128`/`version`/`card1` have no `status` file → skipped) and returns `Some(true)` if
any enabled+connected, `Some(false)` if connectors readable but none qualifies (phantom),
`None` on unknown layout → render anyway (never silently go dark).

**Read sysfs as a PLAIN FILE — do NOT pull the `drm` crate.** `--display` is a
DEFAULT-FEATURE runtime mode; the `drm` crate is `probe`-feature-only and pulling it into
default features would balloon the shared dev1 `target/` (the #185 disk-fill). A
`std::fs::read_to_string` of the status file is all that's needed.

## Never software-upscale beyond source (#135)

`scale_nearest` is a per-pixel nested loop → a 1080→4K upscale to a phantom 4K fb was the
99.9%-CPU sink behind the pre-event cam1 incident (load 4.5, ~400ms emit latency).
`clamp_render_dims(src, fb) = (min(src_w,fb_w), min(src_h,fb_h))` — render at min(fb,source)
per axis: an oversized fb renders 1:1 (letterboxed, written left-aligned), a larger source
downscales to fit. UPSCALING beyond source is never allowed. cam4 (1080p monitor, 1:1) was
the live proof this path is light (load 0.37).

## capture-dropped counter (#130)

`sequence_gap(prev, cur)` derives capture-card drops from V4L2 `sequence` numbers. A BACKWARD
sequence (`cur < prev`: stream reset / frame reorder / 60→30 decimation re-numbering) must
return 0, NOT `~u32::MAX`. The old `cur.wrapping_sub(prev).saturating_sub(1)` produced the
garbage `k*2^32 + 1` counter live on cam2 (e.g. `34359738369 = 8*2^32 + 1`). Discriminator:
forward wrapping distance `> u32::MAX/2` ⇒ really a small backward step ⇒ 0; a small forward
distance (incl. the legit u32 wrap `MAX→0`) still counts.

The display NDI receiver legitimately polls `Ok(None)` for long stretches on a cam whose
display source isn't feeding it (cam2) — log the no-frame gap at DEBUG until a frame was
actually delivered on the connection; only a real stall (frames flowed, then stopped) is a
WARN (`no_frame_log_level` / `NoFrameLevel`). Otherwise it floods the journal at steady 30fps.

## Verifying on a cam

cam boxes are Linux, ssh allowed: `sshpass -p "$CAM_PW" ssh root@10.77.9.6x` (cam2 = .62).
- Counter: `journalctl -u camera-box -n 400 | grep capture-dropped` — must read 0 / small, never k*2^32+1.
- Spurious WARN: `journalctl -u camera-box | grep "No frames"` — must NOT flood during normal flow.
- Connector: read the sysfs `status`/`enabled`/`modes` for each connector to confirm the skip/render decision.
- cam2 normally has a CONNECTED 1:1 1080p monitor → the connected-render no-regression target.
