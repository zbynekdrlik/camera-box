---
name: capture
description: V4L2 capture controls (saturation/contrast/hue) — the certified COLOUR vs SHARP sets, MODEL-GATED zero-touch-by-default policy (#729), device-state persistence, runtime grabber-model detection, NZXT CAM4 no-controls, and the testable apply path. Load before touching src/capture.rs control logic or diagnosing grayscale/colour-tint cameras.
---

# V4L2 capture controls (src/capture.rs)

## #738 (2026-07-13, supersedes #729's Elgato-corrective-by-default) — the tint correction moved OBS-side

**`GrabberModel::Elgato4kS` is ZERO-TOUCH by default again** — the V4L2 saturation-only corrective
set (#729 follow-up) is superseded as the DEFAULT the SAME day it shipped: the tint correction now
lives on the RECEIVING OBS boxes (strih's 'NDI cam5'(physical CAM1)/'NDI cam6'(physical CAM6)
inputs + imag-nb's 'NDI CAM1') as a genuine per-CHANNEL `color_filter_v2` `color_multiply` gain —
strictly more powerful than the V4L2 card's saturation/contrast/hue-only controls (no
per-channel gain exists there at all). Live-verified, screenshots + chroma numbers: cast magnitude
collapsed from ~12.6-12.9 to ~1.6-1.7 (matching cam5's own near-neutral reference) — a
demonstrably better result than the V4L2-only compromise, which could only ever cut the SAME
saturation gain from the tint and real colour together. See the dedicated OBS-side section near
the end of this file for the calibration method, the two real gotchas it took to get there, and
the drift-guard facet. `elgato_4k_s_corrective_controls()` stays FULLY in code — reachable via an
explicit `CAMERA_BOX_CAPTURE_CONTROLS` override — as a switchable manual fallback, never dead code.

## #729 (2026-07-12) — zero-touch by default, model-gated

**camera-box does NOT write any V4L2 colour control unless the RUNTIME-DETECTED grabber model
has a specifically documented, proven need.** `select_capture_controls(model, env_spec,
record_grab)` — no env override -> `documented_controls_for_model(model)`: `GrabberModel::ShadowCast2`
gets the certified COLOUR set (the real #296 need below); `GrabberModel::Elgato4kS`,
`NzxtSignalHd60`, and `Unknown` all stay zero-touch, plug-and-play, factory defaults, no ceremony
(#738 moved the Elgato correction OBS-side — see the section above). An explicit
`CAMERA_BOX_CAPTURE_CONTROLS` override still always wins, for any model.

**`model` comes from `capture_rate_health::resolve_grabber_model(hostname, detected_card)`** —
runtime V4L2 `card`-string detection (`capture::query_card_name`, a best-effort non-exclusive
`VIDIOC_QUERYCAP` read) WINS over the static hostname convention whenever available, so a
physical card swap (#728) can never again silently apply a stale colour policy (or a stale
self-heal tolerance — same shared detection feeds both). `main.rs` logs a WARN on any detected
mismatch. See `.claude/skills/ops`'s `#663`/`#685` section for the self-heal-envelope half of the
same shared detection.

**Before assuming "the code" explains a box's current control state, check for a per-host
`camera-box.service.d/*.conf` systemd env override** (`systemctl show camera-box -p Environment`
on the live box) — an explicit override is invisible to `git grep` (nothing in this repo writes
one) and always wins over whatever the code's own policy would otherwise pick. See the dedicated
GOTCHA further down this file (cam6, 2026-07-12) for a live example of a stale one.

## The #1 gotcha — UVC device controls PERSIST on the card across processes

The ShadowCast (CAM1/2/3) cards remember `saturation`/`contrast`/`hue` **on the
device** between camera-box runs. A value set by one process (e.g. a QR-test grab)
stays on the card after that process exits, and the NEXT camera-box start inherits
it. This bricked a live event (#296): a grab set `saturation=0`, production
restarted applying NO controls, and every camera stayed grayscale forever.

**Therefore ShadowCast 2 production must ENFORCE a known control set at EVERY capture open** —
never trust the card's current state. Self-healing, same philosophy as the genlock
lockdown (#150/#257). **This need is SPECIFIC to ShadowCast 2 (#729) — every other model is
zero-touch by default, see the section above.**

## Three certified control sets — do not confuse them

| Set | fn | Values (reference %, #456) | When |
|---|---|---|---|
| COLOUR (ShadowCast 2's documented need, #729) | `color_production_controls()` (#296/#338) | saturation=50%, contrast=50% (no hue) | `GrabberModel::ShadowCast2`, no env override |
| ELGATO CORRECTIVE (#729 follow-up; superseded as default by #738's OBS-side fix — see above) | `elgato_4k_s_corrective_controls()` | saturation=12% only (contrast/brightness/hue untouched) | ONLY `CAMERA_BOX_CAPTURE_CONTROLS` override now — a switchable manual fallback, not the `Elgato4kS` default |
| SHARP (on demand only) | `certified_cam1_controls()` (#156) | contrast=75%, saturation=0% | ONLY `CAMERA_BOX_CAPTURE_CONTROLS=certified` |

`saturation=50%` / `contrast=50%` = the ShadowCast factory defaults, normal
colour, proven on the rig (channel_diff ≈ 35). This is the device-default set;
both production and grab use it, but ONLY on ShadowCast 2 (#729) — Elgato 4K S and
NZXT Signal HD60 are zero-touch. **These are PERCENTAGES, not literal V4L2
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

## GOTCHA — `v4l2-ctl --stream-count=N --stream-to=file` can silently fill the 100MB `/tmp`
## tmpfs mid-grab (#728, 2026-07-12)

A raw dump (not the tiny CSV the content-hash discriminator above writes) is `width*height*2`
bytes PER FRAME (4,147,200 for 1920x1080 YUYV) — `--stream-count=300` on a 100MB tmpfs silently
TRUNCATES at ~25 frames the moment the tmpfs fills (v4l2-ctl keeps printing progress but the file
just stops growing), and once the tmpfs is 100% full, EVERY subsequent write on that box fails
(`No space left on device`) — including an unrelated `journalctl | grep > /tmp/foo.txt` pipe run
minutes later for a totally different check. Confirmed live: a `--stream-count=300` grab on cam5
silently landed at 104,857,600 bytes (~25 frames) and then broke a `journalctl` analysis pipeline
with no obvious link between "I asked for 300 frames" and "this unrelated command now fails".

**Rule: always pass an EXPLICIT, SMALL `--stream-count`** — `frame_bytes * count` must leave
comfortable headroom under 100MB (15 frames of 1080p YUYV = 62MB, safe; NEVER request "however
many frames I probably want" and let the tmpfs decide). After ANY raw grab, `df -h /tmp` before
doing anything else on that box, and `rm` the raw file (or the whole grab) the MOMENT you're done
analyzing it — don't leave it for "cleanup at the end of the session". If you hit `No space left
on device` on ANY command, `df -h /tmp` immediately and clear stale grabs before concluding
anything else is broken.

## Diagnosing a hardware/ISP colour defect vs a software control bug (#729, 2026-07-12)

When a camera/grabber shows wrong colour and the obvious V4L2 controls (saturation/contrast/hue)
are already at their card's own factory default, don't assume "must be a control/software bug" —
test whether the defect survives OUTSIDE camera-box's own code path entirely:

1. **Grab via the card's OWN onboard hardware encoder, not just raw YUYV.** Most UVC capture
   dongles support `MJPG` too (`v4l2-ctl --list-formats`); JPEG encoding happens in the chip's own
   ISP, completely bypassing any YUYV byte-order/format assumption in your own code. If the SAME
   visual defect appears in the onboard-MJPG grab, the defect is upstream of anything your own
   code touches — it's in the card's own ISP/AWB, not a camera-box bug, no matter how the raw YUYV
   path is parsed.
2. **Try a different resolution.** A bandwidth-driven negotiation/fallback defect (common on
   marginal USB links) usually clears at a lower resolution; a defect that reproduces IDENTICALLY
   at 1080p and 720p rules out a bandwidth/negotiation cause.
3. **Check whether chroma tracks luma, not just its magnitude.** camera-box's own `mean_chroma`
   metric reports MEAN ABSOLUTE deviation (`|U-128|`, `|V-128|`) — enough to say "there's
   colour when there shouldn't be" but not WHY. Pull a raw frame, compute per-pixel `Y` (mean of
   the two luma bytes per macropixel) and the raw `U`/`V` byte, and check the Pearson correlation
   between `Y` and `U`/`V` across a few thousand sampled pixels. A near-linear correlation
   (`|corr| > 0.9`) on content that SHOULD be near-neutral gray (e.g. a black/white QR test
   pattern) is the signature of "chroma is actually a scaled/offset copy of luma" — a real ISP/AWB
   defect, not a hue-rotation-fixable colour cast (a hue rotation preserves an already-nonzero
   chroma vector's magnitude; it can't null a luma-driven error that should be zero).
4. **`saturation=0` fully suppressing the defect does NOT mean saturation is the bug** — it just
   proves the saturation control reaches the real pipeline (post-ISP linear gain). If desaturating
   ALSO kills genuine colour content (a colour-bar test pattern, real footage) at the same rate,
   there is no control value that removes only the false colour — confirms an ISP-level defect,
   not a control mis-set.

If all of the above hold, the defect is a genuine hardware/firmware characteristic of that grabber
MODEL (confirmed on cam1+cam6, both Elgato 4K S, identical `u_dev≈35 v_dev≈21` and identical
purple/lime look in both raw YUYV and onboard MJPG, at both 1080p and 720p, with every control at
factory default) — no camera-box code change can NEUTRALIZE it while preserving full colour
fidelity; a full, lossless fix is not possible with the 4 controls this card exposes.

**#729 follow-up (2026-07-13) — the partial-saturation compromise WAS implemented as the
documented-proven-need exception, once the achievable tradeoff was measured.** See
`elgato_4k_s_corrective_controls()` in `src/capture.rs` and the dedicated section below for the
empirical tuning method + the certified value. The magnitude CAN be reduced to a healthy-band
chroma reading; genuine colour fidelity is reduced by the same proportion (it's not free) — this
was accepted and shipped as the new Elgato 4K S default, not left to "the church's call", because
the visual improvement on the (most visually jarring) low-light/dark-scene case was dramatic and
the tradeoff was judged worth it. If real, brightly-lit colourful content is later found to look
unacceptably washed out at this setting, RETUNE `elgato_4k_s_corrective_controls()`'s
`reference_pct` (below) — it's a one-line, one-function change, same as any other documented
per-model policy entry.

## Elgato 4K S corrective-saturation tuning method (#729 follow-up, 2026-07-13)

**How the certified value was found — reuse this method if retuning, or if a THIRD grabber model
ever needs a similar corrective set.**

1. **Sweep ONE control at a time on the LIVE box**, reading the `capture chroma:` journal line
   after each change (`journalctl -u camera-box -n 5 | grep 'capture chroma' | tail -1`; wait
   ~6-8s between changes for a fresh sample — `CHROMA_SAMPLE_FRAMES=60` @ 60fps ≈ 1 sample/s, and
   the log line only appears on the 5s streaming report). Changing a V4L2 control via `v4l2-ctl
   -d /dev/videoN --set-ctrl=name=value` takes effect on the ALREADY-RUNNING capture immediately —
   no service restart needed to observe the effect, only to make it PERSIST (below).
2. **Hue sweep FIRST (cheap, would be ideal if it worked):** full 0-255 sweep at default
   saturation/contrast/brightness. Result: hue shifts WHICH of u_dev/v_dev is larger but does NOT
   reduce `sqrt(u_dev²+v_dev²)` (stayed ~45-53 across the whole sweep) — confirms the theoretical
   prediction (a hue rotation preserves the error vector's magnitude, it only moves it between
   channels) and rules hue out as a corrective lever.
3. **Saturation sweep second:** clean, near-perfectly LINEAR relationship between the saturation
   setting and the chroma reading (measured live on cam6, 2026-07-13):
   ```
   saturation  128(100%)  96(75%)  64(50%)  48(37.5%)  32(25%)  24(18.75%)  16   8    0
   u_dev       33.2       25.1     16.4     12.5       8.2      6.3         4.2  2.0  0.0
   v_dev       42.4       31.7     20.8     15.6       10.5     7.8         5.2  2.7  0.0
   ```
   `saturation=32` (25% of the card's own default 128, ≈12.5% of its 0-255 range) landed closest
   to the healthy target (`u_dev≈7, v_dev≈10.7`) and was cross-checked on the SECOND affected unit
   (cam1: u_dev=8.3 v_dev=10.5, near-identical to cam6's 8.2/10.5) before committing to it as the
   shared default for the whole model.
4. **Contrast/brightness sweep last (ruled out as alternatives):** brightness swept the FULL
   0-255 range and barely moved chroma (33.1→31.3); contrast=0 measurably reduced chroma too, but
   ONLY by flattening the entire image toward uniform grey (useless as broadcast video) — neither
   is a viable lever, so the corrective set touches saturation ONLY.
5. **Convert the chosen literal to `reference_pct`** for `ControlTarget::RangeScaled` (#456): on
   this card's queried `[0,255]` range, `reference_pct=12` resolves to `round(0.12×255)=31` (1 LSB
   off the literal `32` tested live — negligible, confirmed by the post-deploy reading below).
6. **Verify PERSISTENCE across a genuine service restart**, not just the live v4l2-ctl tweak:
   deploy the binary, `systemctl stop/start camera-box` (or a fresh reboot), then read
   `v4l2-ctl -d /dev/video1 --list-ctrls` — the corrective set is enforced at EVERY capture open
   (same self-healing philosophy as the ShadowCast 2 COLOUR set), so it must show the corrective
   value with ZERO manual intervention. Confirmed live on both cam1 and cam6 post-deploy: fresh
   `systemctl` restart → `saturation=31` automatically, chroma settled to `u_dev≈8.0-8.3
   v_dev≈10.2-10.6` on both boxes independently.

**Honest limitation of this tuning session:** validation was done against whatever REAL content
happened to be live at diagnosis time (a dark, low-colour room — cam2's test-pattern painter that
would normally provide a bright colour reference was unreachable, see the cam2 disk GOTCHA in the
project CLAUDE.md). The linear saturation/chroma relationship is clean enough to generalize with
confidence (real colour content will read at ~25% of its normal saturation on these 2 units), but
a fresh visual check against genuinely bright/colourful content is worth doing opportunistically
next time either camera points at one (e.g. during a real service) — if the muted colour turns out
to look worse than expected in practice, retune `reference_pct` per point 5 above.

## #738 (2026-07-13) — the tint correction moved OBS-side: `scripts/obs_colour_correction_calibrate.py`

**Why:** the V4L2 saturation cut above shares ONE gain between the tint and real colour (proven —
there's no partial value that removes proportionally more of the defect than of real colour). OBS's
`color_filter_v2` filter's `color_multiply` setting is a genuine independent PER-CHANNEL gain
(`vendor/obs-studio/plugins/obs-filters/color-correction-filter.c`: `filter->color_matrix.{x.x,
y.y,z.z} = color_multiply_v4.{x,y,z}` set separately) — closer to a true white-balance fix, and able
to neutralize a directional R/B-vs-G cast without crushing overall chroma the way a blanket
saturation scale must.

**Method — "grey-world" white balance, NOT a literal cam5-frame match.** The issue asked to
calibrate "against cam5's rendition of the same splitter content" — that needs `rig-mode.sh test`
to paint a shared reference pattern through the HDMI splitter, which touches cam2 (OFF-LIMITS,
#737's dying disk). Live sampling confirmed cam5 and cam1/cam6 are pointed at genuinely DIFFERENT
real scenes right now anyway (cam5 near-black; cam1/cam6 a dim room) — a literal frame match would
be comparing apples to oranges. Instead: the standard grey-world assumption (a large mixed scene
averages near-neutral) computes a per-channel gain that brings the input's mean R/G/B toward
neutral — cam5's OWN near-neutral cast is used only as a sanity reference for "what does an
undamaged camera's cast look like", never as the literal target frame.

**Two real gotchas found only by testing live (read `scripts/obs_colour_correction_calibrate.py`'s
own module doc for the full derivation) — do NOT re-derive from scratch:**

1. **`color_multiply` is sRGB-gamma-decoded before use as a linear multiplier**
   (`vec4_from_rgba_srgb`) while a `GetSourceScreenshot` PNG is gamma-ENCODED — so a single
   grey-world pass computed directly in screenshot-space under-corrects (a `damping=1.0` "full"
   correction only reduced the measured cast by ~27%, not to ~0). Fix:
   `calibrate_source_iterative` — measure the CURRENT rendered result, compute a correction
   relative to it, COMPOSE (never replace) onto the cumulative gain, repeat (2-3 rounds converges
   from ~12.7 mag down to ~1.6-1.7, matching cam5's own reference order of magnitude).
2. **`color_multiply`'s byte range (0..255) can only represent a gain in [0.0, 1.0] — it can DIM a
   channel, never BOOST one above its input level.** `grey_world_gains` anchors the target on the
   DARKEST channel (never the mean) — anchoring on the mean silently needs gain>1.0 on
   below-average channels, which `pack_color_multiply` clamps to a no-op (byte 255); confirmed
   live, a "boosted" channel's actual rendered value never moved across 4 rounds despite its
   nominal gain climbing to ~1.9. The trade-off: the corrected image gets somewhat DARKER (all
   channels dimmed to match the darkest one), never brighter — inherent to a multiply-only (no
   `color_add` boost) instrument.

**Live-verified result (strih, 2026-07-13):** cam1 (`'NDI cam5'` input) and cam6 (`'NDI cam6'`
input) both went from a visibly purple/violet cast (screenshot evidence) to a near-neutral dark
tone matching cam5's own reference, cast magnitude ~12.6→~1.6 (a ~87% reduction). imag-nb's own
`'NDI CAM1'` (clean 1:1 mapping, unlike strih's inverted labels — see the genlock skill) confirmed
the SAME correction independently (`(0.557, 0.942, 0.612)` there vs strih's `(0.572, 0.942,
0.621)`) — consistent, corroborating evidence this is a real hardware characteristic, not scene
noise. imag's `'NDI CAM6'` was NOT on program at sampling time (all-zero screenshot, no signal) —
the filter is present with identity (no-op) settings, ready whenever it IS shown.

**strih NDI-input-label GOTCHA bit this investigation too — always read `ndi_source_name` live,
never trust the input's OBS LABEL** (the genlock skill's own inversion table is stale/incomplete
for cam4/cam5/cam6): a first exploratory pass applied a filter to the literally-named `'NDI cam1'`
input, which actually carries physical **CAM3** (an unrelated, unrelated-content camera reading
near-black) — not one of the tinted Elgato units at all. `GetInputSettings` on the input and
reading its `ndi_source_name` field is the only reliable way to know which physical camera is
really behind an OBS input name; confirmed live mapping (2026-07-13): `NDI cam1`→CAM3(usb),
`NDI cam2`→CAM2(usb), `NDI cam3`→CAM4(usb), `NDI cam4`→CAM5(usb, the ShadowCast reference!),
`NDI cam5`→CAM1(usb, tinted), `NDI cam6`→CAM6(usb, tinted, matches its own label for once).

**Persistence / drift-guard facet:** `classify_persisted_correction` / `check_correction_persisted`
(same module) report `missing`/`disabled`/`identity`/`applied` for a source's filter — mirrors
`obs_burn_filter.py`'s presence+enabled check shape, plus a THIRD check this filter specifically
needs (has it drifted back to the neutral identity value?). `--check` CLI mode is read-only, exits
non-zero unless every given source reports `applied`. Not yet wired into the full `drift-guard.sh`
manifest/pin system (that 1886-line file's own gather+check+README-pin machinery is a materially
bigger, separate change) — this standalone check is the facet that exists today; wiring it into the
periodic drift-guard run is a natural, bounded follow-up if the user wants it on that cadence.

**Acceptance note:** per the #738 decision, the OBS-side correction was shipped based on live
screenshot + chroma-number evidence against whatever REAL (currently dim/dark) content was
available — the SAME honest limitation the #729 V4L2 tuning session itself carried (no genuinely
bright/colourful content was available to validate against either time). The user's own eyeball
against real broadcast content remains the final acceptance; if it ever looks wrong in practice,
retune `grey_world_gains`'s `damping` (currently 0.6-0.8 in practice) or revert to the
`elgato_4k_s_corrective_controls()` V4L2 fallback (still fully in code, one env-var away).

## GOTCHA — a per-host `camera-box.service.d/*.conf` systemd env override can silently smear a
## LITERAL control value outside any code path OR provisioning script (#729, cam6, 2026-07-12)

`CAMERA_BOX_CAPTURE_CONTROLS` set via `Environment=` in a systemd drop-in ALWAYS wins over
whatever the code's own model-gated policy (`capture::select_capture_controls`) would otherwise
select — this is correct, intentional behavior (an operator override should win), but it also
means a drop-in written by hand YEARS before a later code fix (here: pre-#456's range-aware
resolution) can sit forever, invisible in `git grep`, silently defeating a later redesign (#729's
zero-touch policy) that assumes "no override = the code decides". Found live: cam6 had
`/etc/systemd/system/camera-box.service.d/capture-controls.conf` setting a literal
`contrast=128,saturation=128` — its own comment said "Proper fix = range-aware colour in
camera-box", i.e. it was ALREADY known to be a stopgap the day it was written, and nobody ever
came back to remove it once #456 shipped the real fix.

**When auditing/fixing ANY per-box V4L2/capture-control behavior, check
`/etc/systemd/system/camera-box.service.d/*.conf` on the LIVE box for a leftover
`CAMERA_BOX_CAPTURE_CONTROLS` override BEFORE trusting the code's own logic to explain what's
actually happening** — `systemctl show camera-box -p Environment` shows the fully-merged env,
which is the ground truth, not `grep`-ing the repo (this override was never written by any
provisioning script, so `git grep` finds nothing). If a stopgap override predates a later proper
code fix and now just duplicates what the code already does correctly, remove the drop-in +
`daemon-reload` + restart — don't leave dead ceremony sitting on one box that a future person will
trip over.
