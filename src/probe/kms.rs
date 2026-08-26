//! DRM/KMS page-flip presenter — tear-free, 1:1, vblank-locked QR painter (#79).
//!
//! ## Why this exists
//!
//! cam2's HDMI output is an i915 `drm` device whose fbdev emulation
//! (`/dev/fb0`, name `i915drmfb`) exposes a SINGLE page (`yres_virtual == yres
//! == 1080`), so `FBIOPUT_VSCREENINFO` cannot allocate a second page and fbdev
//! page-flip double buffering is impossible (#68). [`crate::probe::fb::VsyncFb`]
//! works around that with a vsync-GATED single-buffer direct write, which is
//! tear-free *most* of the time but still slips ~2.2% of captured frames into a
//! torn QR (the measured emission-artifact blind spot), and it free-runs the
//! painter at a sub-capture wall-clock rate (5× oversample by design).
//!
//! This presenter goes one layer below fbdev and drives the HDMI CRTC directly
//! through DRM/KMS:
//!
//! 1. Open `/dev/dri/card1` (i915 atomic-capable), take DRM master (which
//!    requires the fbdev console — fbcon — to be detached so it does not own the
//!    CRTC; see [`fbcon`]).
//! 2. Allocate TWO dumb buffer objects (BOs), each with its own framebuffer
//!    handle — a true double buffer the fbdev path could not get.
//! 3. Set the initial CRTC scanout to BO 0.
//! 4. For each painted QR: render into the *back* BO, `drmModePageFlip` it with
//!    `DRM_MODE_PAGE_FLIP_EVENT`, then **block on the flip-complete (vblank)
//!    event** before returning. The scanout switches at the vertical blank, so
//!    the capture never samples a half-written buffer (tear-free), and the
//!    block-on-flip paces the painter to exactly one new id per HDMI vblank —
//!    60 fps, 1:1, phase-locked to the ShadowCast capture (both exactly 60 Hz).
//!
//! The result: the torn-QR blind spot AND the 5× oversample both collapse —
//! every painted frame is a unique, fully-formed, captured-once frame.
//!
//! ## Fallback
//!
//! Taking DRM master detaches the live HDMI console and is not always possible
//! (no `/dev/dri/card*`, master held by Xorg/Wayland, permissions). The
//! [`crate::probe::painter`] selects the presenter at runtime via
//! [`crate::probe::presenter::open_presenter`]; if KMS cannot be brought up it
//! falls back to [`VsyncFb`](crate::probe::fb::VsyncFb) so the harness still
//! runs everywhere it did before.
//!
//! ## Testability
//!
//! The DRM glue (open/master/flip) can only run on the live rig, so it is thin
//! and excluded from coverage like the rest of the hardware layer. The
//! *decisions* it makes — which mode to drive, which BO is the back buffer, how
//! to copy a BGRA frame into a (possibly padded) dumb-buffer pitch — are pure
//! functions unit-tested in CI below.

use crate::probe::presenter::Presenter;
use anyhow::{anyhow, bail, Context, Result};

// ---------------------------------------------------------------------------
// Pure decision logic (unit-tested in CI — no hardware)
// ---------------------------------------------------------------------------

/// A display mode candidate, reduced to the fields the presenter cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeCandidate {
    pub width: u32,
    pub height: u32,
    /// Vertical refresh in milli-Hz (60_000 == 60.000 Hz). DRM reports refresh
    /// as an integer Hz via `vrefresh`; we keep milli-Hz so a 59.94 vs 60.00
    /// preference is expressible and the tear-free 60.000 Hz lock is exact.
    pub refresh_mhz: u32,
    /// True if the connector reports this as its preferred mode.
    pub preferred: bool,
}

/// Choose the mode to drive on a connected HDMI output.
///
/// The decisive hardware fact (#79 recon): the cam2 HDMI CRTC and the
/// ShadowCast 2 capture both run exactly 60.000 Hz, so a 60.000 Hz mode is what
/// makes the 1:1 phase lock physically realizable. BUT the painter renders a
/// FIXED `target_w`×`target_h` QR canvas (1920×1080) and the ShadowCast captures
/// 1080p60 — so the scanned-out mode MUST match that canvas resolution, or the
/// painter's 1080p frame does not fit the BO and the run fails (live cam2 bug:
/// the connector lists `1920x1080@60` AND several `4096x2160@60`, and picking the
/// largest 60 Hz mode drove a 4K scanout the 1080p painter could not fill).
///
/// Selection order:
///
/// 1. The mode that BOTH matches the painter canvas (`target_w`×`target_h`) AND
///    runs at exactly `target_refresh_mhz` (60_000) — the only mode that is both
///    fillable by the painter and 1:1 phase-lockable with the capture. Connector
///    `preferred` flag is the tiebreak among equal candidates.
/// 2. Any other mode at exactly the canvas resolution (so the painter's frame
///    still fits even if it can't run at 60 Hz — a non-locked but working run).
/// 3. Any mode at exactly `target_refresh_mhz`, largest resolution (legacy
///    behaviour — only reached when NO mode matches the canvas; the painter would
///    then need a matching canvas, but a 60 Hz mode is at least phase-lockable).
/// 4. The connector's preferred mode, if any.
/// 5. Otherwise the largest-resolution / highest-refresh mode available.
///
/// Returns `None` only when the candidate list is empty (no usable mode).
pub fn pick_mode(
    modes: &[ModeCandidate],
    target_w: u32,
    target_h: u32,
    target_refresh_mhz: u32,
) -> Option<ModeCandidate> {
    if modes.is_empty() {
        return None;
    }
    let matches_canvas = |m: &ModeCandidate| m.width == target_w && m.height == target_h;

    // 1) canvas-matching AND exact 60 Hz — fillable by the painter AND lockable.
    let canvas_and_locked = modes
        .iter()
        .filter(|m| matches_canvas(m) && m.refresh_mhz == target_refresh_mhz)
        .max_by_key(|m| m.preferred);
    if let Some(m) = canvas_and_locked {
        return Some(*m);
    }
    // 2) canvas-matching at any refresh — the painter's frame still fits.
    let canvas_any_refresh = modes
        .iter()
        .filter(|m| matches_canvas(m))
        .max_by_key(|m| (m.refresh_mhz, m.preferred));
    if let Some(m) = canvas_any_refresh {
        return Some(*m);
    }
    // 3) exact target refresh, largest resolution — no canvas match exists.
    let exact = modes
        .iter()
        .filter(|m| m.refresh_mhz == target_refresh_mhz)
        .max_by_key(|m| (m.width as u64 * m.height as u64, m.preferred));
    if let Some(m) = exact {
        return Some(*m);
    }
    // 4) the connector's preferred mode, if any.
    if let Some(m) = modes.iter().find(|m| m.preferred) {
        return Some(*m);
    }
    // 5) largest area, then highest refresh.
    modes
        .iter()
        .max_by_key(|m| (m.width as u64 * m.height as u64, m.refresh_mhz))
        .copied()
}

/// Whether `mode` runs at exactly the target refresh, i.e. the tear-free 1:1
/// phase lock with the capture is realizable. The painter logs a warning when
/// false so a non-60 Hz fallback run is not silently read as a 1:1 result.
pub fn is_phase_lockable(mode: &ModeCandidate, target_refresh_mhz: u32) -> bool {
    mode.refresh_mhz == target_refresh_mhz
}

/// Compute a mode's vertical refresh in **milli-Hz** from its raw timing, the
/// way the kernel does — `clock_khz * 1e6 / (htotal * vtotal)`, doubled for
/// interlace and halved for double-scan.
///
/// `ModeCandidate::refresh_mhz` is documented as milli-Hz precisely so the
/// 59.94-vs-60.00 distinction is expressible and the exact-60.000 Hz phase lock
/// is real. The `drm` crate's `Mode::vrefresh()` returns ROUNDED INTEGER Hz, so
/// a 59.94 Hz mode reports `60` and would be falsely treated as the exact
/// 60.000 Hz lock — defeating [`is_phase_lockable`]. Deriving from the timing
/// gives true milli-Hz so a 59.94 mode reads as 59_940, not 60_000.
///
/// Returns `0` if the timing is degenerate (`htotal == 0` or `vtotal == 0`),
/// which the caller treats as "no usable refresh" (never the 60.000 target).
pub fn refresh_mhz_from_timing(
    clock_khz: u32,
    htotal: u32,
    vtotal: u32,
    interlace: bool,
    dblscan: bool,
) -> u32 {
    let denom = (htotal as u64).saturating_mul(vtotal as u64);
    if denom == 0 {
        return 0;
    }
    // clock is in kHz; * 1e6 (kHz->Hz is *1000, Hz->mHz is *1000) / lines.
    let mut mhz = (clock_khz as u64).saturating_mul(1_000_000) / denom;
    if interlace {
        mhz = mhz.saturating_mul(2); // two fields per frame -> double the field rate
    }
    if dblscan {
        mhz /= 2; // each line scanned twice -> half the frame rate
    }
    mhz.min(u32::MAX as u64) as u32
}

/// The next back-buffer index for a double-buffered flip, given the index that
/// is currently scanned out (the front buffer). With exactly two BOs this is
/// just the other one; kept as a named pure function so the toggle is tested and
/// cannot silently regress to "always render into the buffer being scanned out"
/// (which would tear — the exact bug DRM double buffering removes).
pub fn next_bo_index(front: usize) -> usize {
    front ^ 1
}

/// Row-copy plan for writing a tightly-packed BGRA source frame into a dumb
/// buffer whose scanline `pitch` may be padded wider than `width*4`.
///
/// Returns `Ok(())` semantics encoded as an enum so the (pure) decision is
/// testable without a real mapped buffer:
/// - [`CopyPlan::Contiguous`] when `pitch == width*4` — one `copy_from_slice`.
/// - [`CopyPlan::PerRow`] when the pitch is padded — copy `width*4` bytes per
///   row at `row * pitch` offsets.
///
/// Errors if the source length is not exactly `width*height*4` (a caller bug)
/// or if `pitch < width*4` (the buffer is too narrow to hold a row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyPlan {
    Contiguous,
    PerRow,
}

pub fn copy_plan(src_len: usize, width: u32, height: u32, pitch: u32) -> Result<CopyPlan> {
    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| anyhow!("width*4 overflow"))?;
    let expect = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(|| anyhow!("frame size overflow"))?;
    if src_len != expect {
        bail!(
            "source frame is {} bytes, expected {} ({}x{} BGRA)",
            src_len,
            expect,
            width,
            height
        );
    }
    if (pitch as usize) < row_bytes {
        bail!(
            "dumb-buffer pitch {} is narrower than a {}-byte BGRA row",
            pitch,
            row_bytes
        );
    }
    if pitch as usize == row_bytes {
        Ok(CopyPlan::Contiguous)
    } else {
        Ok(CopyPlan::PerRow)
    }
}

/// Copy a tightly-packed BGRA `src` into a (possibly padded-pitch) destination
/// `dst` for a `width`×`height` frame, per [`copy_plan`]. Shared by the live
/// presenter and the tests so the stride math has one verified implementation.
pub fn copy_bgra_into(
    dst: &mut [u8],
    src: &[u8],
    width: u32,
    height: u32,
    pitch: u32,
) -> Result<()> {
    let plan = copy_plan(src.len(), width, height, pitch)?;
    let row_bytes = width as usize * 4;
    let pitch = pitch as usize;
    if (height as usize).saturating_mul(pitch) > dst.len() {
        bail!(
            "destination buffer {} bytes too small for {} rows of pitch {}",
            dst.len(),
            height,
            pitch
        );
    }
    match plan {
        CopyPlan::Contiguous => {
            dst[..src.len()].copy_from_slice(src);
        }
        CopyPlan::PerRow => {
            for y in 0..height as usize {
                let s = y * row_bytes;
                let d = y * pitch;
                dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Live DRM/KMS presenter (hardware glue — excluded from coverage)
// ---------------------------------------------------------------------------

/// Target refresh for the tear-free 1:1 lock: exactly 60.000 Hz (#79 recon —
/// HDMI CRTC and ShadowCast capture are both 60.000 Hz).
pub const TARGET_REFRESH_MHZ: u32 = 60_000;

#[cfg(target_os = "linux")]
mod hw {
    use super::*;
    use drm::buffer::{Buffer, DrmFourcc};
    use drm::control::{
        connector, crtc, dumbbuffer::DumbBuffer, framebuffer, Device as ControlDevice, Event, Mode,
        PageFlipFlags,
    };
    use drm::Device as DrmDevice;
    use std::fs::{File, OpenOptions};
    use std::os::unix::io::{AsFd, BorrowedFd};
    use std::time::{Duration, Instant};

    /// A DRM device node wrapping an owned `File` — the `AsFd` glue the `drm`
    /// crate's `Device` / `control::Device` traits build on.
    struct Card(File);

    impl AsFd for Card {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.0.as_fd()
        }
    }
    impl DrmDevice for Card {}
    impl ControlDevice for Card {}

    /// One double-buffer slot: a dumb BO plus its framebuffer handle.
    struct Slot {
        db: DumbBuffer,
        fb: framebuffer::Handle,
    }

    /// Live DRM/KMS page-flip presenter (see module docs).
    pub struct KmsPresenter {
        card: Card,
        crtc: crtc::Handle,
        connector: connector::Handle,
        mode: Mode,
        width: u32,
        height: u32,
        slots: [Slot; 2],
        /// Index of the BO currently scanned out (front buffer).
        front: usize,
        /// Whether the chosen mode runs at exactly the target refresh, so the
        /// 1:1 phase lock with the capture holds.
        phase_locked: bool,
        /// We detached fbcon to take master; rebind it on drop so the device's
        /// console comes back.
        detached_fbcon: bool,
        /// We acquired the DRM master lock; release it on drop.
        held_master: bool,
        /// #660: the fbdev device (e.g. `/dev/fb0`) to BLANK on `Drop`, before
        /// releasing DRM master — see `crate::fb_blank` for why. This presenter
        /// never reads/writes it during normal operation (it drives the CRTC
        /// through its own separate dumb buffers); it is stashed purely for the
        /// teardown blank.
        fb_device: String,
    }

    impl KmsPresenter {
        /// Open `device` (e.g. `/dev/dri/card1`), take DRM master (detaching
        /// fbcon if needed), pick the HDMI mode that matches the painter's
        /// `canvas_w`×`canvas_h` (the fixed QR canvas, e.g. 1920×1080 — driving a
        /// larger mode the painter cannot fill is the live cam2 bug this guards)
        /// AND runs at `mode_refresh_mhz` (default `TARGET_REFRESH_MHZ` = 60_000,
        /// the capture rate; #1179 overrides it, e.g. 100_000 for the
        /// 2560×1080@100 experiment), allocate two dumb BOs, and set the initial
        /// scanout. Returns an error (so the caller can fall back to fbdev) if any
        /// step fails.
        ///
        /// `mode_refresh_mhz` is the mode-SELECTION target only; the tear-free 1:1
        /// phase-lock check ([`is_phase_lockable`]) stays against the fixed 60 fps
        /// capture rate (`TARGET_REFRESH_MHZ`), so a non-60 Hz override run is
        /// honestly reported NOT phase-locked (100 is not a multiple of 60).
        ///
        /// `fb_device` (e.g. `/dev/fb0`) is stashed ONLY for the `Drop` teardown
        /// blank (#660) — this presenter never touches it otherwise.
        pub fn open(
            device: &str,
            fb_device: &str,
            canvas_w: u32,
            canvas_h: u32,
            mode_refresh_mhz: u32,
        ) -> Result<Self> {
            // Detaching fbcon frees the CRTC so DRM master can drive it. Best
            // effort: if there is no fbcon (already detached / headless) this is
            // a no-op. We record whether WE detached it so drop rebinds it.
            let detached_fbcon = super::fbcon::detach().unwrap_or(false);

            // From here on any failure must UNDO the detach (and any master we
            // grab), because `Self` — and thus its `Drop` cleanup — is only
            // constructed on the success path. Without this, a failure after
            // detach (the EXPECTED path in `Auto` mode: master held by a
            // compositor, or no canvas-matching mode → the bail below) would
            // leave the live HDMI console orphaned while the caller "falls back
            // to fbdev". So run the fallible body, and on error rebind fbcon.
            Self::open_inner(
                device,
                fb_device,
                canvas_w,
                canvas_h,
                mode_refresh_mhz,
                detached_fbcon,
            )
            .inspect_err(|_| {
                if detached_fbcon {
                    if let Err(e) = super::fbcon::attach() {
                        tracing::warn!(
                            "KmsPresenter: rebind fbcon after failed open() failed: {:?}",
                            e
                        );
                    }
                }
            })
        }

        /// The fallible body of [`open`](Self::open). Any DRM master it acquires
        /// is released here on the error paths (so a failure after master-grab
        /// does not leak the lock); fbcon rebind on error is handled by the
        /// caller via `detached_fbcon`.
        fn open_inner(
            device: &str,
            fb_device: &str,
            canvas_w: u32,
            canvas_h: u32,
            mode_refresh_mhz: u32,
            detached_fbcon: bool,
        ) -> Result<Self> {
            // O_NONBLOCK so `receive_events()` returns EAGAIN instead of blocking
            // the read forever when no page-flip event is queued — this is what
            // makes `wait_flip_complete`'s 500 ms timeout actually able to fire
            // on a wedged/lost flip (a blocking fd would park the painter thread
            // inside read() and never reach the deadline check).
            use std::os::unix::fs::OpenOptionsExt;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(device)
                .with_context(|| format!("open DRM device {}", device))?;
            let card = Card(file);

            // Become DRM master so set_crtc / page_flip are permitted.
            card.acquire_master_lock()
                .context("acquire DRM master (is another compositor holding the CRTC?)")?;
            // Master is held now; every error path below must release it (the
            // `Card` File close alone does NOT drop master cleanly on all
            // kernels). `with_master` runs the rest and releases on Err.
            Self::with_master(
                card,
                fb_device,
                canvas_w,
                canvas_h,
                mode_refresh_mhz,
                detached_fbcon,
            )
        }

        /// Runs the post-master setup; releases the DRM master lock if any step
        /// errors (so a partial open does not leave the lock held).
        fn with_master(
            card: Card,
            fb_device: &str,
            canvas_w: u32,
            canvas_h: u32,
            mode_refresh_mhz: u32,
            detached_fbcon: bool,
        ) -> Result<Self> {
            match Self::build(
                card,
                fb_device,
                canvas_w,
                canvas_h,
                mode_refresh_mhz,
                detached_fbcon,
            ) {
                Ok(this) => Ok(this),
                Err((card, e)) => {
                    if let Err(re) = card.release_master_lock() {
                        tracing::warn!(
                            "KmsPresenter: release DRM master after failed open() failed: {:?}",
                            re
                        );
                    }
                    Err(e)
                }
            }
        }

        /// The CRTC/mode/buffer setup. On error returns the `Card` back so the
        /// caller can release master (we can't run `?` and still hand the card
        /// back, so map each step's error to `(card, err)`).
        fn build(
            card: Card,
            fb_device: &str,
            canvas_w: u32,
            canvas_h: u32,
            mode_refresh_mhz: u32,
            detached_fbcon: bool,
        ) -> std::result::Result<Self, (Card, anyhow::Error)> {
            macro_rules! tryc {
                ($e:expr) => {
                    match $e {
                        Ok(v) => v,
                        Err(e) => return Err((card, anyhow::Error::from(e))),
                    }
                };
            }
            macro_rules! bailc {
                ($($arg:tt)*) => {
                    return Err((card, anyhow!($($arg)*)))
                };
            }
            let res = tryc!(card.resource_handles().context("read DRM resource handles"));

            // Find a connected connector (prefer HDMI) and its modes.
            let (connector, modes) = tryc!(Self::find_connected(&card, &res));
            let candidates: Vec<ModeCandidate> = modes.iter().map(Self::mode_candidate).collect();
            // #1179: `mode_refresh_mhz` is the mode-SELECTION target (60_000 by default = today's
            // behaviour; overridable to e.g. 100_000). `is_phase_lockable` below stays on the
            // CONSTANT `TARGET_REFRESH_MHZ` (the 60 fps capture rate) — the two are distinct, so a
            // 100 Hz override selects the 100 Hz mode yet is honestly reported NOT 1:1 phase-locked.
            let chosen = match pick_mode(&candidates, canvas_w, canvas_h, mode_refresh_mhz) {
                Some(c) => c,
                None => bailc!("connector has no usable mode"),
            };
            // The painter renders a fixed canvas_w×canvas_h frame; if no mode at
            // that exact resolution exists, the scanned-out buffer would be a
            // different size than every painted frame and present() would reject
            // each one. Fail to open (so the caller falls back to fbdev) rather
            // than light up a mode the painter can never fill.
            if chosen.width != canvas_w || chosen.height != canvas_h {
                bailc!(
                    "no DRM mode matches the painter canvas {}x{} (best available {}x{}@{} mHz) — \
                     the painter cannot fill a different scanout size",
                    canvas_w,
                    canvas_h,
                    chosen.width,
                    chosen.height,
                    chosen.refresh_mhz
                );
            }
            let phase_locked = is_phase_lockable(&chosen, TARGET_REFRESH_MHZ);
            // Map the chosen candidate back to the real Mode (match on the same
            // derived candidate, so the milli-Hz refresh comparison is exact).
            let mode = match modes.iter().find(|m| Self::mode_candidate(m) == chosen) {
                Some(m) => *m,
                None => bailc!("chosen mode vanished from connector list"),
            };

            let (width, height) = (chosen.width, chosen.height);

            // Pick a CRTC that can drive this connector (via its encoder).
            let crtc = tryc!(Self::pick_crtc(&card, &res, connector));

            // Two dumb BOs + framebuffers — the real double buffer. If a later
            // step fails, destroy the slots already created so they don't leak
            // their ~8 MB BO + FB (Slot has no Drop, and on the error path the
            // returned `Self` — whose Drop would free them — is never built).
            let slot0 = tryc!(Self::make_slot(&card, width, height));
            let slot1 = match Self::make_slot(&card, width, height) {
                Ok(s) => s,
                Err(e) => {
                    Self::destroy_slot(&card, &slot0);
                    return Err((card, e));
                }
            };

            // Initial scanout: BO 0.
            if let Err(e) = card
                .set_crtc(crtc, Some(slot0.fb), (0, 0), &[connector], Some(mode))
                .context("set initial CRTC scanout")
            {
                Self::destroy_slot(&card, &slot0);
                Self::destroy_slot(&card, &slot1);
                return Err((card, e));
            }

            if !phase_locked {
                tracing::warn!(
                    "KmsPresenter: chosen mode {}x{}@{} mHz is NOT exactly {} mHz — \
                     tear-free 1:1 phase lock with the 60 Hz capture is NOT guaranteed",
                    width,
                    height,
                    chosen.refresh_mhz,
                    TARGET_REFRESH_MHZ
                );
            } else {
                tracing::info!(
                    "KmsPresenter: {}x{}@60.000Hz, double-buffered DRM page-flip, \
                     vblank-locked 1:1 (fbcon_detached={})",
                    width,
                    height,
                    detached_fbcon
                );
            }

            Ok(Self {
                card,
                crtc,
                connector,
                mode,
                width,
                height,
                slots: [slot0, slot1],
                front: 0,
                phase_locked,
                detached_fbcon,
                held_master: true,
                fb_device: fb_device.to_string(),
            })
        }

        /// Reduce a real DRM `Mode` to a [`ModeCandidate`], deriving the exact
        /// milli-Hz refresh from the timing (not the rounded integer `vrefresh`)
        /// so the 60.000 Hz phase lock is real — see [`refresh_mhz_from_timing`].
        fn mode_candidate(m: &Mode) -> ModeCandidate {
            let flags = m.flags();
            ModeCandidate {
                width: m.size().0 as u32,
                height: m.size().1 as u32,
                refresh_mhz: refresh_mhz_from_timing(
                    m.clock(),
                    m.hsync().2 as u32,
                    m.vsync().2 as u32,
                    flags.contains(drm::control::ModeFlags::INTERLACE),
                    flags.contains(drm::control::ModeFlags::DBLSCAN),
                ),
                preferred: m
                    .mode_type()
                    .contains(drm::control::ModeTypeFlags::PREFERRED),
            }
        }

        fn find_connected(
            card: &Card,
            res: &drm::control::ResourceHandles,
        ) -> Result<(connector::Handle, Vec<Mode>)> {
            // Prefer a connected HDMI connector; otherwise any connected one.
            let mut fallback: Option<(connector::Handle, Vec<Mode>)> = None;
            for &conn in res.connectors() {
                let info = match card.get_connector(conn, true) {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                if info.state() != connector::State::Connected {
                    continue;
                }
                let modes = info.modes().to_vec();
                if modes.is_empty() {
                    continue;
                }
                let is_hdmi = matches!(
                    info.interface(),
                    connector::Interface::HDMIA | connector::Interface::HDMIB
                );
                if is_hdmi {
                    return Ok((conn, modes));
                }
                fallback.get_or_insert((conn, modes));
            }
            fallback.ok_or_else(|| anyhow!("no connected DRM connector with modes"))
        }

        fn pick_crtc(
            card: &Card,
            res: &drm::control::ResourceHandles,
            connector: connector::Handle,
        ) -> Result<crtc::Handle> {
            let info = card.get_connector(connector, false)?;
            // Try the connector's current encoder first.
            if let Some(enc) = info.current_encoder() {
                if let Ok(enc_info) = card.get_encoder(enc) {
                    if let Some(c) = enc_info.crtc() {
                        return Ok(c);
                    }
                }
            }
            // Otherwise the first CRTC any encoder this connector lists permits.
            for &enc in info.encoders() {
                let enc_info = match card.get_encoder(enc) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if let Some(&c) = res.filter_crtcs(enc_info.possible_crtcs()).first() {
                    return Ok(c);
                }
            }
            // Last resort: the first CRTC.
            res.crtcs()
                .first()
                .copied()
                .ok_or_else(|| anyhow!("device has no CRTC"))
        }

        fn make_slot(card: &Card, width: u32, height: u32) -> Result<Slot> {
            let mut db = card
                .create_dumb_buffer((width, height), DrmFourcc::Xrgb8888, 32)
                .context("create dumb buffer")?;
            // From here, any failure must destroy `db` or it leaks (it is only
            // wrapped in a Slot — whose teardown frees it — on success).
            let fb = match card.add_framebuffer(&db, 24, 32) {
                Ok(fb) => fb,
                Err(e) => {
                    let _ = card.destroy_dumb_buffer(db);
                    return Err(anyhow::Error::from(e).context("add framebuffer for dumb buffer"));
                }
            };
            // Clear to white so the first frame before paint is not garbage.
            if let Err(e) = (|| -> Result<()> {
                let mut map = card
                    .map_dumb_buffer(&mut db)
                    .context("map dumb buffer for clear")?;
                for b in map.as_mut() {
                    *b = 0xFF;
                }
                Ok(())
            })() {
                let _ = card.destroy_framebuffer(fb);
                let _ = card.destroy_dumb_buffer(db);
                return Err(e);
            }
            Ok(Slot { db, fb })
        }

        /// Destroy a slot's framebuffer AND its backing dumb buffer (the GEM BO).
        /// Used by both the `build` error paths and `Drop` so the two-object
        /// teardown lives in one place (`DumbBuffer` is `Copy`, no `Drop`).
        fn destroy_slot(card: &Card, slot: &Slot) {
            let _ = card.destroy_framebuffer(slot.fb);
            let _ = card.destroy_dumb_buffer(slot.db);
        }

        /// Block until the next page-flip completes (vblank), with a timeout so a
        /// stuck flip surfaces as an error rather than hanging the run forever.
        ///
        /// The fd is O_NONBLOCK (see `open_inner`), so `receive_events()` returns
        /// EAGAIN/`WouldBlock` when nothing is queued — we treat that as "no
        /// event yet, sleep and re-check the deadline", which is what lets the
        /// timeout actually fire on a wedged flip (a blocking read would never
        /// return control to the deadline check).
        fn wait_flip_complete(&self) -> Result<()> {
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for DRM page-flip completion event");
                }
                match self.card.receive_events() {
                    Ok(events) => {
                        for ev in events {
                            if let Event::PageFlip(_) = ev {
                                return Ok(());
                            }
                        }
                    }
                    // No event ready yet on the non-blocking fd — fall through to
                    // the sleep and re-check the deadline next iteration.
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(anyhow::Error::from(e).context("receive DRM events")),
                }
                // No event yet; brief sleep so we don't spin the CPU.
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }

    impl Presenter for KmsPresenter {
        fn dimensions(&self) -> (u32, u32) {
            (self.width, self.height)
        }

        /// KMS paces the painter via the blocking vblank flip — one new id per
        /// HDMI vblank (60 fps, 1:1). The painter must NOT also sleep.
        fn paces_on_present(&self) -> bool {
            true
        }

        /// The tear-free 1:1 lock holds only when the driven mode is exactly the
        /// 60.000 Hz target (see [`is_phase_lockable`]); a non-60 Hz fallback run
        /// reports `false` so it is not read as a 1:1 result.
        fn phase_locked(&self) -> bool {
            self.phase_locked
        }

        fn present(&mut self, bgra: &[u8]) -> Result<()> {
            let back = next_bo_index(self.front);
            let (w, h) = (self.width, self.height);
            let pitch = self.slots[back].db.pitch();
            {
                let mut map = self
                    .card
                    .map_dumb_buffer(&mut self.slots[back].db)
                    .context("map back dumb buffer")?;
                copy_bgra_into(map.as_mut(), bgra, w, h, pitch)?;
            }
            // Flip the back BO in at the next vblank, requesting the completion
            // event, then block on it so the painter advances exactly one id per
            // vblank (tear-free + 1:1).
            self.card
                .page_flip(self.crtc, self.slots[back].fb, PageFlipFlags::EVENT, None)
                .context("drmModePageFlip")?;
            self.wait_flip_complete()?;
            self.front = back;
            Ok(())
        }
    }

    impl Drop for KmsPresenter {
        fn drop(&mut self) {
            // #660: this presenter never touches `fb_device` itself (it drives the CRTC through
            // its own separate dumb buffers) — but the moment we release DRM master below, the
            // kernel's fbdev-emulation client regains the CRTC and scans out THAT device's memory
            // again, whatever it happens to hold (an old `VsyncFb` fallback write, or camera-box's
            // own `--display` module's last frame — nothing else clears it between painter runs).
            // Blank it FIRST, before touching master/fbcon at all, so what gets revealed is always
            // a deterministic black frame, never a stale decodable QR from an unrelated earlier
            // invocation (confirmed live: a run_id 13-24 minutes old, frozen for the last 15-30
            // recorded frames, mis-read by recording-verdict as an imag optical COPY/FREEZE fault).
            // Best-effort — log-and-continue, never abort teardown over it.
            if let Err(e) = crate::probe::fb::blank_fbdev(&self.fb_device) {
                tracing::warn!(
                    "KmsPresenter: blank {} before teardown failed: {:?}",
                    self.fb_device,
                    e
                );
            }
            // Restore the console: drop master and rebind fbcon so cam2's normal
            // HDMI display returns after the run. Best-effort — log failures.
            let _ = (self.crtc, self.connector, self.mode); // keep fields used
                                                            // Tear down BOTH objects each slot allocated: the framebuffer
                                                            // (metadata) AND the backing dumb buffer (the GEM BO). `drm` 0.15's
                                                            // `DumbBuffer` has no `Drop`, so omitting `destroy_dumb_buffer`
                                                            // leaks the ~8 MB (1080p XRGB) kernel allocation on every teardown.
            for slot in &self.slots {
                Self::destroy_slot(&self.card, slot);
            }
            if self.held_master {
                if let Err(e) = self.card.release_master_lock() {
                    tracing::warn!("KmsPresenter: release DRM master failed: {:?}", e);
                }
            }
            if self.detached_fbcon {
                if let Err(e) = super::fbcon::attach() {
                    tracing::warn!("KmsPresenter: rebind fbcon failed: {:?}", e);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use hw::KmsPresenter;

/// fbcon (the framebuffer console) bind/unbind helpers.
///
/// To take DRM master and drive the CRTC, fbcon must not own it. On the cam2
/// rig fbcon is bound (`/sys/class/vtconsole/vtcon1/name` == "frame buffer
/// device"). Unbinding it (`echo 0 > .../bind`) hands the CRTC to DRM master;
/// re-binding (`echo 1`) restores the text console after the run.
pub mod fbcon {
    use anyhow::{Context, Result};
    use std::fs;
    use std::path::PathBuf;

    /// Find the vtconsole whose `name` is the framebuffer device (fbcon).
    /// Returns the sysfs `bind` path, or `None` if no fbcon is present.
    pub fn fbcon_bind_path() -> Option<PathBuf> {
        let base = "/sys/class/vtconsole";
        let entries = fs::read_dir(base).ok()?;
        for entry in entries.flatten() {
            let dir = entry.path();
            let name = fs::read_to_string(dir.join("name")).unwrap_or_default();
            if name.contains("frame buffer device") {
                return Some(dir.join("bind"));
            }
        }
        None
    }

    /// Read whether the fbcon is currently bound (`bind` file contains "1").
    fn is_bound(bind_path: &std::path::Path) -> bool {
        fs::read_to_string(bind_path)
            .map(|s| s.trim() == "1")
            .unwrap_or(false)
    }

    /// Detach fbcon so DRM master can drive the CRTC. Returns `Ok(true)` if WE
    /// unbound it (so the caller knows to rebind on teardown), `Ok(false)` if
    /// there was nothing bound to detach.
    pub fn detach() -> Result<bool> {
        let Some(bind) = fbcon_bind_path() else {
            return Ok(false);
        };
        if !is_bound(&bind) {
            return Ok(false);
        }
        fs::write(&bind, "0").with_context(|| format!("unbind fbcon ({})", bind.display()))?;
        tracing::info!("KmsPresenter: detached fbcon ({})", bind.display());
        Ok(true)
    }

    /// Re-bind fbcon, restoring the text console after a run.
    pub fn attach() -> Result<()> {
        if let Some(bind) = fbcon_bind_path() {
            fs::write(&bind, "1").with_context(|| format!("rebind fbcon ({})", bind.display()))?;
            tracing::info!("KmsPresenter: rebound fbcon ({})", bind.display());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(width: u32, height: u32, refresh_mhz: u32, preferred: bool) -> ModeCandidate {
        ModeCandidate {
            width,
            height,
            refresh_mhz,
            preferred,
        }
    }

    // The painter's fixed QR canvas (and the ShadowCast capture) is 1080p.
    const CW: u32 = 1920;
    const CH: u32 = 1080;

    #[test]
    fn pick_mode_none_when_empty() {
        assert_eq!(pick_mode(&[], CW, CH, TARGET_REFRESH_MHZ), None);
    }

    #[test]
    fn pick_mode_prefers_exact_60hz_over_preferred_flag() {
        // A 75 Hz preferred mode AND a 60.000 Hz mode at the canvas resolution —
        // the 60 Hz one wins because the 1:1 phase lock requires it, even though
        // it is not flagged preferred.
        let modes = [m(1920, 1080, 75_000, true), m(1920, 1080, 60_000, false)];
        let got = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!(got.refresh_mhz, 60_000);
        assert!(is_phase_lockable(&got, TARGET_REFRESH_MHZ));
    }

    #[test]
    fn pick_mode_selects_2560x1080_at_100hz_for_the_1179_override() {
        // #1179: the LG 34U511A-B lists 2560x1080 at 60/75/100 (+ a 1920x1080@60). With the
        // override CANVAS 2560x1080 and the override SELECTION target 100_000 mHz, pick_mode's
        // step 1 (canvas-match AND exact refresh) must land on 2560x1080@100 — and that mode is
        // NOT 1:1 phase-lockable against the 60 fps capture (100 is not a multiple of 60), so a run
        // on it must be reported honestly as non-locked.
        const CW2: u32 = 2560;
        const CH2: u32 = 1080;
        let modes = [
            m(1920, 1080, 60_000, true),
            m(2560, 1080, 60_000, false),
            m(2560, 1080, 75_000, false),
            m(2560, 1080, 100_000, false),
        ];
        let got = pick_mode(&modes, CW2, CH2, 100_000).unwrap();
        assert_eq!(
            (got.width, got.height, got.refresh_mhz),
            (2560, 1080, 100_000),
            "override must select the canvas-matching 100 Hz mode"
        );
        assert!(
            !is_phase_lockable(&got, TARGET_REFRESH_MHZ),
            "100 Hz is not a multiple of the 60 fps capture — must NOT report 1:1 phase lock"
        );

        // Same panel, DEFAULT selection target (60_000) at the 2560 canvas → the 60 Hz mode, so
        // the selection target is genuinely what drives the choice, not the canvas alone.
        let got60 = pick_mode(&modes, CW2, CH2, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!(
            (got60.width, got60.height, got60.refresh_mhz),
            (2560, 1080, 60_000)
        );
        assert!(is_phase_lockable(&got60, TARGET_REFRESH_MHZ));

        // And the byte-identical default: 1920 canvas at 60_000 still picks 1920x1080@60 even with
        // the 2560 modes present — no --display-mode ⇒ exactly today's choice.
        let got_def = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!(
            (got_def.width, got_def.height, got_def.refresh_mhz),
            (1920, 1080, 60_000)
        );
    }

    #[test]
    fn pick_mode_picks_canvas_match_not_largest_at_exact_60hz() {
        // The LIVE cam2 bug: HDMI-A-2 lists a 1920x1080@60 mode AND several
        // 4096x2160@60 modes. The painter renders a fixed 1920x1080 canvas and
        // the ShadowCast captures 1080p60, so the canvas-matching 1080p mode MUST
        // win — NOT the largest 60 Hz mode (4K), which the painter cannot fill.
        let modes = [
            m(1920, 1080, 60_000, false),
            m(4096, 2160, 60_000, true),
            m(4096, 2160, 60_000, false),
        ];
        let got = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!(
            (got.width, got.height, got.refresh_mhz),
            (1920, 1080, 60_000),
            "must pick the canvas-matching 1080p60 mode, not the larger 4K mode"
        );
        assert!(is_phase_lockable(&got, TARGET_REFRESH_MHZ));
    }

    #[test]
    fn pick_mode_canvas_match_beats_larger_even_when_4k_is_preferred() {
        // Even if the 4K mode is the connector's preferred mode, the painter's
        // 1080p canvas must still win — preferred does not override fillability.
        let modes = [m(4096, 2160, 60_000, true), m(1920, 1080, 60_000, false)];
        let got = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!((got.width, got.height), (1920, 1080));
    }

    #[test]
    fn pick_mode_canvas_match_at_non_60hz_when_no_60hz_canvas_mode() {
        // No 1080p60 mode, but a 1080p59.94 mode exists — take the canvas-matching
        // one (the painter's frame fits) over a 60 Hz 4K mode it cannot fill. Not
        // phase-lockable, but a working (non-1:1) run beats a guaranteed failure.
        let modes = [m(4096, 2160, 60_000, true), m(1920, 1080, 59_940, false)];
        let got = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!(
            (got.width, got.height, got.refresh_mhz),
            (1920, 1080, 59_940)
        );
        assert!(!is_phase_lockable(&got, TARGET_REFRESH_MHZ));
    }

    #[test]
    fn pick_mode_largest_60hz_when_no_canvas_match() {
        // No mode at the canvas resolution at all — fall back to the largest
        // exact-60Hz mode (legacy behaviour; the open() guard then bails so the
        // caller falls back to fbdev, but pick_mode itself still returns a 60 Hz
        // candidate).
        let modes = [
            m(1280, 720, 60_000, true),
            m(3840, 2160, 60_000, false),
            m(1024, 768, 60_000, false),
        ];
        let got = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!((got.width, got.height), (3840, 2160));
    }

    #[test]
    fn pick_mode_falls_back_to_preferred_when_no_canvas_no_60hz() {
        // No canvas-matching mode and no 60.000 Hz mode — fall back to the
        // connector's preferred mode so the panel still lights up.
        let modes = [m(3840, 2160, 30_000, false), m(2560, 1440, 59_940, true)];
        let got = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!(
            (got.width, got.height, got.refresh_mhz),
            (2560, 1440, 59_940)
        );
        assert!(!is_phase_lockable(&got, TARGET_REFRESH_MHZ));
    }

    #[test]
    fn pick_mode_falls_back_to_largest_when_no_target_no_preferred() {
        let modes = [m(1280, 720, 50_000, false), m(2560, 1440, 50_000, false)];
        let got = pick_mode(&modes, CW, CH, TARGET_REFRESH_MHZ).unwrap();
        assert_eq!((got.width, got.height), (2560, 1440));
    }

    #[test]
    fn refresh_mhz_from_timing_distinguishes_5994_from_6000() {
        // CTA-861 1080p60.000: pixel clock 148.5 MHz, 2200x1125 total.
        // 148_500_000 / (2200*1125) = 60.000 Hz exactly -> 60_000 mHz.
        assert_eq!(
            refresh_mhz_from_timing(148_500, 2200, 1125, false, false),
            60_000
        );
        // CTA-861 1080p59.94: pixel clock 148.352 MHz, same totals.
        // 148_352_000 / 2_475_000 = 59.940... -> 59_940 mHz (NOT 60_000).
        let r = refresh_mhz_from_timing(148_352, 2200, 1125, false, false);
        assert!(
            (59_930..=59_950).contains(&r),
            "59.94 must read ~59_940 mHz, not 60_000 — got {r}"
        );
        assert_ne!(
            r, TARGET_REFRESH_MHZ,
            "59.94 must NOT match the 60.000 lock"
        );
    }

    #[test]
    fn refresh_mhz_from_timing_interlace_doubles_dblscan_halves() {
        // Same base timing; interlace -> field rate doubled, dblscan -> halved.
        let base = refresh_mhz_from_timing(74_250, 2200, 562, false, false);
        let inter = refresh_mhz_from_timing(74_250, 2200, 562, true, false);
        let dbl = refresh_mhz_from_timing(74_250, 2200, 562, false, true);
        assert_eq!(inter, base * 2);
        assert_eq!(dbl, base / 2);
    }

    #[test]
    fn refresh_mhz_from_timing_zero_total_is_zero() {
        assert_eq!(refresh_mhz_from_timing(148_500, 0, 1125, false, false), 0);
        assert_eq!(refresh_mhz_from_timing(148_500, 2200, 0, false, false), 0);
    }

    #[test]
    fn next_bo_index_toggles() {
        assert_eq!(next_bo_index(0), 1);
        assert_eq!(next_bo_index(1), 0);
        // Toggling twice returns to start — the double-buffer cycle.
        assert_eq!(next_bo_index(next_bo_index(0)), 0);
    }

    #[test]
    fn copy_plan_contiguous_when_pitch_tight() {
        // 4x2 BGRA, tight pitch 16 == 4*4.
        let plan = copy_plan(4 * 2 * 4, 4, 2, 16).unwrap();
        assert_eq!(plan, CopyPlan::Contiguous);
    }

    #[test]
    fn copy_plan_per_row_when_pitch_padded() {
        // 4x2 BGRA, padded pitch 32 > 16.
        let plan = copy_plan(4 * 2 * 4, 4, 2, 32).unwrap();
        assert_eq!(plan, CopyPlan::PerRow);
    }

    #[test]
    fn copy_plan_rejects_wrong_src_len() {
        let err = copy_plan(10, 4, 2, 16).unwrap_err();
        assert!(err.to_string().contains("expected"), "{err}");
    }

    #[test]
    fn copy_plan_rejects_too_narrow_pitch() {
        // pitch 12 < width*4 == 16.
        let err = copy_plan(4 * 2 * 4, 4, 2, 12).unwrap_err();
        assert!(err.to_string().contains("narrower"), "{err}");
    }

    #[test]
    fn copy_bgra_into_contiguous_roundtrip() {
        let w = 3u32;
        let h = 2u32;
        let pitch = w * 4; // tight
        let src: Vec<u8> = (0..(w * h * 4) as u8).collect();
        let mut dst = vec![0u8; (h * pitch) as usize];
        copy_bgra_into(&mut dst, &src, w, h, pitch).unwrap();
        assert_eq!(dst, src);
    }

    #[test]
    fn copy_bgra_into_padded_pitch_places_rows_correctly() {
        let w = 2u32;
        let h = 2u32;
        let row = (w * 4) as usize; // 8
        let pitch = 12u32; // 4 bytes of pad per row
                           // distinct bytes per row so misplacement is detectable
        let src: Vec<u8> = vec![1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2];
        let mut dst = vec![0u8; (h * pitch) as usize];
        copy_bgra_into(&mut dst, &src, w, h, pitch).unwrap();
        // Row 0 at offset 0, row 1 at offset 12; pad bytes (8..12, 20..24) stay 0.
        assert_eq!(&dst[0..row], &src[0..row]);
        assert_eq!(&dst[8..12], &[0, 0, 0, 0]); // row-0 pad untouched
        assert_eq!(&dst[12..12 + row], &src[row..row * 2]);
        assert_eq!(&dst[20..24], &[0, 0, 0, 0]); // row-1 pad untouched
    }

    #[test]
    fn copy_bgra_into_rejects_undersized_dst() {
        let w = 4u32;
        let h = 4u32;
        let pitch = w * 4;
        let src = vec![0u8; (w * h * 4) as usize];
        let mut dst = vec![0u8; 8]; // way too small
        let err = copy_bgra_into(&mut dst, &src, w, h, pitch).unwrap_err();
        assert!(err.to_string().contains("too small"), "{err}");
    }
}
