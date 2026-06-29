---
name: capture
description: V4L2 capture controls (saturation/contrast/hue) — the certified COLOUR vs SHARP sets, device-state persistence, NZXT CAM4 no-controls, and the testable apply path. Load before touching src/capture.rs control logic or diagnosing grayscale/colour-tint cameras.
---

# V4L2 capture controls (src/capture.rs)

## The #1 gotcha — UVC device controls PERSIST on the card across processes

The ShadowCast (CAM1/2/3) cards remember `saturation`/`contrast`/`hue` **on the
device** between camera-box runs. A value set by one process (e.g. a QR-test grab)
stays on the card after that process exits, and the NEXT camera-box start inherits
it. This bricked a live event (#296): a grab set `saturation=0`, production
restarted applying NO controls, and every camera stayed grayscale forever.

**Therefore production must ENFORCE a known control set at EVERY capture open** —
never trust the card's current state. Self-healing, same philosophy as the genlock
lockdown (#150/#257).

## Two certified control sets — do not confuse them

| Set | fn | Values | When |
|---|---|---|---|
| COLOUR (default — production AND grab) | `color_production_controls()` (#296/#338) | saturation=50, contrast=50 (no hue) | ANY no-env open: production OR grab |
| SHARP (on demand only) | `certified_cam1_controls()` (#156) | contrast=75, saturation=0 | ONLY `CAMERA_BOX_CAPTURE_CONTROLS=certified` |

`saturation=50` / `contrast=50` = the ShadowCast factory defaults, normal colour,
proven on the rig (channel_diff ≈ 35). This is the device-default set; both
production and grab now use it.

**#338 — NEVER force hue, and `hue=0` is NOT neutral.** The ShadowCast V4L2 hue is
`min=0 max=100 default=50`, so forcing `hue=0` is a MAX shift = a PINK/magenta tint
on the live camera (the #338 symptom: one cam pink, the NZXT cam clean). The colour
set therefore touches ONLY saturation+contrast and leaves hue alone. Hue is only
ever changed via an explicit `CAMERA_BOX_CAPTURE_CONTROLS=hue=N` operator override.

**#312 — the sharp set is NOT auto-applied to grab.** `saturation=0`/`contrast=75`
was meant to sharpen the filmed QR but HURT the optical decode (run 312005:
ShadowCast w/ sharp set ~50% undecodable; the NZXT card on device defaults read the
SAME monitor CLEAN). Grab now uses the device-default colour set; the sharp set is
opt-in only via `CAMERA_BOX_CAPTURE_CONTROLS=certified`.

Selection is centralised in `select_capture_controls(env_spec, _record_grab)`:
env override → `parse_capture_controls`; else → colour set (record_grab no longer
changes selection — kept only for call-site clarity). `CAMERA_BOX_CAPTURE_CONTROLS`
understands `contrast`/`saturation`/`hue` (and the keyword `certified`); an
empty/whitespace value = explicit "touch nothing".

V4L2 control ids (V4L2_CID_BASE 0x00980900): CONTRAST=+1 (0x0980901),
SATURATION=+2 (0x0980902), HUE=+3 (0x0980903).

## NZXT CAM4 (Signal HD60) exposes NO v4l2 picture controls

CAM4's grab card has none of saturation/contrast/hue. The apply path MUST stay
graceful: a rejected control logs a warning and PROCEEDS — capture never aborts.
This is structural, not conventional: `apply_controls_with()` returns a
`ControlReport` tally (applied/adjusted/failed) and has NO error return; the
`VideoCapture::apply_controls` wrapper is `-> ()`. So enforcing the colour set on a
control-less card is a 2× warn + continue (saturation+contrast — #338 dropped hue
from the set), never fatal.

CAM4's "grayscale" was NOT a code bug (#299 — REFRAMED). The ticket-validator
proved camera-box selects the correct `/dev/video0`, negotiates YUYV, and converts
YUYV→UYVY chroma-preserving; all three boxes measured *identically* grayscale, so
the mono image was SOURCE CONTENT (grayscale console screens), not a colorspace /
2-video-node defect. No colorspace fix was needed; #299 instead shipped an
always-on colour-capture metric (below) so the recurring "is colour actually
captured?" question is answered automatically, not by eye.

## Colour-capture metric — the #299 always-on grayscale watchdog

`mean_chroma(frame, width, height, stride) -> (f32, f32)` in `src/capture.rs`
returns `(mean |U-128|, mean |V-128|)` over the captured YUYV422 frame. Neutral
grey is U=V=128 → both ≈ 0; a coloured source pushes them up. `is_color_frame(u,v)`
classifies via `CHROMA_COLOR_THRESHOLD = 2.0` LSB (named const). `main.rs` samples
once per `CHROMA_SAMPLE_FRAMES = 60` captured frames (~1 Hz @ 60 fps) and logs on
the existing 5 s streaming report:

```
capture chroma: u_dev=3.1 v_dev=4.7 -> colour
capture chroma: u_dev=0.2 v_dev=0.1 -> grayscale (source likely monochrome)
```

To check colour on a live box: `journalctl -u camera-box | grep 'capture chroma'`.
A steady `-> grayscale` while a coloured source is on-camera is the regression.

**Gotcha — sampling MUST honor `stride`.** The V4L2 mmap buffer length is
`stride * height`, NOT `width * 2 * height`; a row-padded device (`stride >
width*2`) would otherwise sample padding bytes as bogus chroma. `mean_chroma`
iterates row-by-row at `row_start = y * stride`, same as `yuyv_to_gray8`. Any
future per-pixel frame analysis in this crate must take and honor `stride` too.
Cost is bounded by `CHROMA_SAMPLE_STRIDE = 64` macropixels/row (~16 k samples at
1080p) so it never perturbs the realtime grab.

## Unit-testing the apply path without /dev/video

The `ControlIo` trait abstracts the v4l2 hardware boundary (`set_ctrl`/`get_ctrl`,
implemented for v4l `Device`). `apply_controls_with::<IO: ControlIo>` runs the real
warn-and-continue policy against any device, so tests inject a `FakeDevice`
(supporting an arbitrary control subset — incl. the empty NZXT case) and assert the
tally. This is the allowed external-hardware mock, not internal-logic mocking. Note
the trait method names are `set_ctrl`/`get_ctrl` (NOT `set_control`/`control`) to
avoid colliding with — and recursing into — the v4l inherent methods.

## Controls apply AFTER set_format/set_params

In `open_with_controls`, `apply_controls` runs after `Capture::set_format` and
`set_params`, just before streaming. Many UVC cards (ShadowCast included) RESET
picture controls to factory defaults on `VIDIOC_S_FMT`/`S_PARM`, so applying
earlier lets the format-set clobber them (#156). Keep the apply where it is.
