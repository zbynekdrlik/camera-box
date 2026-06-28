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
| SHARP (grab/QR decode) | `certified_cam1_controls()` (#156) | contrast=75, saturation=0 | `--record-grab` or `CAMERA_BOX_CAPTURE_CONTROLS` |
| COLOUR (production) | `color_production_controls()` (#296) | saturation=50, contrast=50, hue=0 | production (no env, no grab) |

`saturation=0` = the sharp grayscale grab (kills chroma for QR edges).
`saturation=50` = the ShadowCast factory colour level, proven on the rig
(channel_diff ≈ 35). `hue=0` = neutral.

Selection is centralised in `select_capture_controls(env_spec, record_grab)`:
env override → `parse_capture_controls`; else grab → sharp set; else → colour set.
`CAMERA_BOX_CAPTURE_CONTROLS` understands `contrast`/`saturation`/`hue` (and the
keyword `certified`); an empty/whitespace value = explicit "touch nothing".

V4L2 control ids (V4L2_CID_BASE 0x00980900): CONTRAST=+1 (0x0980901),
SATURATION=+2 (0x0980902), HUE=+3 (0x0980903).

## NZXT CAM4 (Signal HD60) exposes NO v4l2 picture controls

CAM4's grab card has none of saturation/contrast/hue. The apply path MUST stay
graceful: a rejected control logs a warning and PROCEEDS — capture never aborts.
This is structural, not conventional: `apply_controls_with()` returns a
`ControlReport` tally (applied/adjusted/failed) and has NO error return; the
`VideoCapture::apply_controls` wrapper is `-> ()`. So enforcing the colour set on a
control-less card is a 3× warn + continue, never fatal.

CAM4 was *also* mono during the incident but from a DIFFERENT root (colorspace /
2-video-node / YUV range) — tracked separately in **#299**, NOT fixed by #296.

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
