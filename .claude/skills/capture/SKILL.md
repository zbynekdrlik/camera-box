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

| Set | fn | Values (reference %, #456) | When |
|---|---|---|---|
| COLOUR (default — production AND grab) | `color_production_controls()` (#296/#338) | saturation=50%, contrast=50% (no hue) | ANY no-env open: production OR grab |
| SHARP (on demand only) | `certified_cam1_controls()` (#156) | contrast=75%, saturation=0% | ONLY `CAMERA_BOX_CAPTURE_CONTROLS=certified` |

`saturation=50%` / `contrast=50%` = the ShadowCast factory defaults, normal
colour, proven on the rig (channel_diff ≈ 35). This is the device-default set;
both production and grab now use it. **These are PERCENTAGES, not literal V4L2
values** — see the #456 range-aware resolution section below; on the ShadowCast
card 50%/75%/0% happen to equal the literal values 50/75/0.

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

The `ControlIo` trait abstracts the v4l2 hardware boundary (`set_ctrl`/`get_ctrl`/
`query_range`, implemented for v4l `Device`). `apply_controls_with::<IO: ControlIo>`
runs the real warn-and-continue policy against any device, so tests inject a
`FakeDevice` (supporting an arbitrary control subset — incl. the empty NZXT case,
plus `.with_range(id, range)` to model a `VIDIOC_QUERY_EXT_CTRL` response) and
assert the tally. This is the allowed external-hardware mock, not internal-logic
mocking. Note the trait method names are `set_ctrl`/`get_ctrl`/`query_range` (NOT
`set_control`/`control`/`query_controls`) to avoid colliding with — and recursing
into — the v4l inherent methods.

## #456 range-aware control resolution — a literal 50 is NOT the same "neutral" on every card

The certified sets above are calibrated against the ShadowCast card's native
0-100 range. **cam5's grab card has a 0-255 range (default 128)** — applying the
literal `50` verbatim landed at ~20% of ITS range = a dark/washed-out image
(#456). `CaptureControl.target` is now a `ControlTarget` enum, not a bare `i64`:

- `ControlTarget::Literal(v)` — an explicit `CAMERA_BOX_CAPTURE_CONTROLS=name=value`
  operator override. Applied verbatim, NEVER range-scaled (the operator already
  picked the exact device value for THAT card).
- `ControlTarget::RangeScaled { reference_pct }` — what BOTH certified sets use.
  Resolved at apply time (`resolve_control_target` → `scale_to_range`) against the
  device's OWN `VIDIOC_QUERY_EXT_CTRL` range (`ControlIo::query_range`):
  - `reference_pct == 50` (the COLOUR set's neutral) → prefers the device's
    queried `default_value` directly (the true manufacturer neutral — not
    necessarily the numeric midpoint on every card, only on the two known ones).
  - Any other `reference_pct` (the SHARP set's 75/0) → pure proportional scaling
    onto `[minimum,maximum]`.
  - Range query fails (old/quirky driver) → falls back to `reference_pct` applied
    literally, same as the pre-#456 behaviour — never a hard failure.

**Gotcha — this ran in TWO PRs** (#509 the fix, #510 a same-day follow-up): the
dispatched deep-review pass caught that the first cut queried `default_value` but
never USED it (pure proportional midpoint math only), which happens to be correct
for ShadowCast (default 50 == midpoint 50) and cam5 (default 128 == midpoint 128)
but would silently miss the true neutral on a THIRD card whose default isn't its
numeric midpoint. Lesson: when a queried "default" field is added to a struct,
either wire it into the logic or don't carry it — an unused-but-plausible-looking
field is exactly the kind of thing a second review pass catches and a first
self-review misses.

## Controls apply AFTER set_format/set_params

In `open_with_controls`, `apply_controls` runs after `Capture::set_format` and
`set_params`, just before streaming. Many UVC cards (ShadowCast included) RESET
picture controls to factory defaults on `VIDIOC_S_FMT`/`S_PARM`, so applying
earlier lets the format-set clobber them (#156). Keep the apply where it is.

## Diagnostic — raw v4l2 capture bypassing camera-box entirely (#696, no v4l2-ctl/ffmpeg on the boxes)

The cam boxes are locked-down appliances: `/` is mounted `ro`, `/tmp` is a tiny (100MB) `tmpfs`,
and there is NO `v4l2-ctl`/`ffmpeg`/`opencv`/`python3-v4l2` installed — `apt-get install` fails
(`Read-only file system` writing `/var/lib/apt/lists`). To grab raw frames straight off
`/dev/videoN` for a corruption/tearing investigation (bypassing camera-box/NDI/genlock entirely):

```bash
mkdir -p /tmp/pylibs
python3 -m pip install --target=/tmp/pylibs --no-warn-script-location v4l2py   # ~4.4MB, pypi reachable
sudo systemctl stop camera-box                       # frees /dev/video0 for exclusive mmap streaming
PYTHONPATH=/tmp/pylibs python3 - <<'PY'
from v4l2py import Device
dev = Device("/dev/video0"); dev.open()
for frame in dev:
    buff = frame.buff                    # real ctypes v4l2_buffer — .flags, .bytesused, .sequence
    is_err = bool(buff.flags & 0x0040)   # V4L2_BUF_FLAG_ERROR
    ...                                   # inspect bytes(frame); break after N frames
dev.close()
PY
sudo systemctl start camera-box                      # ALWAYS restore before ending the session
rm -rf /tmp/pylibs /tmp/<your-grab-dir>               # tmpfs is tiny — clean up your own files
```

**Do NOT hand-roll the V4L2 `ioctl` struct layout in raw `struct.pack`** — `struct v4l2_buffer`'s
real x86_64 layout (`v4l2_timecode` is 16 bytes, not the 44 a naive read of the struct might
suggest; there's non-obvious padding before `timestamp`/the `m` union) is easy to get wrong, and a
wrong-sized buffer handed to a V4L2 `ioctl` reads/writes past your allocation — `v4l2py`
(`pip install v4l2py`, pulls `linuxpy`) does the marshalling correctly via `ctypes.Structure` and
is safe to install into the tmpfs at basically zero risk to the device. Sanity-check `sizeof(struct
v4l2_buffer)` (88 bytes) by decoding the ioctl request code's embedded size (bits 16-29 of e.g.
`VIDIOC_QUERYBUF=0xC0585609`) if you ever need to verify a size independently.

**A per-frame "roughness" statistic (mean adjacent-byte delta over a handful of sampled rows) is a
cheap way to characterize corruption without decoding every frame to PNG** — real video content
has strongly correlated neighboring bytes (low roughness); a torn/speckled corrupt frame spikes it.
Only save the handful of frames that look anomalous (or a few evenly-spaced samples) to disk — the
100MB tmpfs fills after ~24 raw 1920×1080 YUYV frames (4,147,200 bytes each). To actually LOOK at
a saved `.yuyv` frame, pull it to dev1 (`scp`) and `ffmpeg -f rawvideo -pixel_format yuyv422
-video_size WxH -i frame.yuyv frame.png` (dev1 has ffmpeg/numpy/PIL; the cam boxes don't).

## Content-hash discriminator — is duplication happening AT the raw capture stage? (#674)

**Question this answers:** downstream (imag/strih) optical checks show duplicate-content judder on
a camera whose reported capture RATE is clean (60-64fps, no drops, no stall) — is the duplication
already present in the grabber's own raw V4L2 buffers, or is it introduced somewhere downstream
(NDI transport, receiver-side decode)? This is the natural next question after the "roughness"
corruption check above — same raw-capture setup, different per-frame statistic.

**Method (confirmed live on cam1, #674, 2026-07-12):** with the shared monitor genuinely animating
(painter in TEST mode — `scripts/rig-mode.sh test`, so there IS new content every capture tick;
without this the test is meaningless, everything looks "duplicate"), open the raw device per the
recipe above and for each frame compute TWO cheap, C-speed statistics — no numpy needed, none of
the cam boxes have it:

```python
import hashlib
full_hash = hashlib.blake2b(buf, digest_size=8).digest()     # exact-duplicate test (~1-3ms/4MB frame)
sample = buf[::1024]                                          # ~4050 decimated bytes, mixed Y/U/V
sad = sum(abs(a - b) for a, b in zip(sample, prev_sample))    # cheap "did content change" signal
exact_dup = full_hash == prev_full_hash
```

- `exact_dup` (full-buffer byte-for-byte match vs the previous frame) is the actual duplicate test —
  a UVC grabber re-delivering a stale buffer when its input/output clock phase drifts produces an
  **exact** byte match, not a "close" one; don't bother with a fuzzy/tolerance compare for the
  primary signal.
- `sad` on the decimated sample is a cheap SANITY check, not the detector: confirms (a) the source
  is genuinely animating almost every frame (min SAD across real-content pairs, NOT the min across
  ALL pairs — non-duplicate pairs) should be enormous relative to the near-dup threshold — cam1's
  #674 run measured min 38,384 vs a 2,000 threshold, a 19x margin, zero pairs fell in between), and
  (b) `exact_dup` isn't accidentally missing "same content, different noise" cases — in that same
  run every `exact_dup` also had `sad == 0` and NOTHING scored 0 < sad < threshold, i.e. ShadowCast's
  duplicate frames are perfect byte copies, not merely similar. Log `(idx, seq, t_monotonic, err,
  exact_dup, sad)` per frame to a tiny CSV (bytes, not the raw frames) — the 100MB tmpfs never fills.
- **Exclude the first ~2s as a startup transient before computing a duplication rate** — the #674
  run showed 91% "duplicate" in the first ~45 frames right after `dev.open()` (buffer/queue still
  settling), which is a capture-open artifact, not the phenomenon under test. Steady-state rate =
  duplicates / frames over `t_monotonic > first_t + 2.0`.
- **Cross-check the rate arithmetically**: `(achieved_fps - target_fps) / achieved_fps` should be
  close to the measured duplicate-pair rate if the mechanism is "free-running clock repeats a frame
  whenever input/output phase drifts through a beat" (#685's ShadowCast-2 model characteristic) — a
  match here (cam1, #674: 4.3% arithmetic vs 4.23% measured) is strong independent confirmation you
  found the right thing, not a coincidental artifact of your own detector.
- **`/dev/videoN` numbering can shift** — a UVC device that drops/re-enumerates on USB (visible as
  repeated `Found UVC 1.00 device` lines in `dmesg`) gets a NEW `/dev/videoN` node; check
  `/sys/class/video4linux/videoN/name` + `index` (the capture node is `index 0`, a second node for
  the same card is usually a metadata/still node at `index 1`) rather than assuming `/dev/video0`.
  This project's own `config.toml` uses `device = "auto"`, so camera-box's own restart is unaffected
  by the renumbering — but YOUR raw-capture script must open whichever node is currently the capture
  one, not a hardcoded `/dev/video0`.
