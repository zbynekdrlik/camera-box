//! #660 — the byte range to BLANK on a fbdev device so a caller handing scanout
//! control back to another display client (e.g. `probe::kms::KmsPresenter`'s
//! teardown) leaves a KNOWN black frame rather than whatever content the
//! device's memory last held.
//!
//! ## Why this exists
//!
//! The KMS page-flip painter (`probe::kms::KmsPresenter`) drives the HDMI CRTC
//! through its OWN pair of DRM dumb buffers — it NEVER reads or writes
//! `/dev/fb0` itself. When it tears down (releases DRM master), the kernel's
//! generic fbdev-emulation client regains the CRTC and scans out `/dev/fb0`'s
//! memory again — whatever that memory happens to hold. Nothing else clears
//! it between KMS painter runs, so it can carry an ARBITRARILY OLD frame:
//! camera-box's own `--display` module (`display.rs`) writes directly into
//! this same device and does not clear it on exit. (The fbdev-fallback
//! presenter `probe::fb::VsyncFb` used to be in this list too, but since #1186
//! it blanks the device in its OWN `Drop` — so a `VsyncFb` run no longer leaves
//! stale content behind; `--display` is now the remaining un-clearing writer.)
//!
//! Confirmed live (#660, 2026-07-10): two independent `recording-e2e.sh` runs
//! each showed a VALID, CRC-passing dual-QR decode frozen for the last
//! 15-30 frames before `StopRecord` — carrying a run_id from 13-24 MINUTES
//! before that run started (not this run's own last frame). Both cam1's
//! digital burn AND imag's render burn advanced perfectly through the same
//! span, proving the NDI delivery chain was healthy throughout — the stale
//! content was on cam2's PHYSICAL MONITOR, i.e. in `/dev/fb0`'s own memory,
//! revealed the instant `KmsPresenter` released DRM master during its
//! self-exit teardown (which happens partway through `RECORD_PAD`, before
//! `StopRecord`). recording-verdict then mis-reads the frozen tail as an
//! `imag` optical COPY/FREEZE fault.
//!
//! The fix (`probe::fb::blank_fbdev`, called from `KmsPresenter`'s `Drop`
//! before releasing DRM master): write an all-zero (black) frame into the
//! SAME currently-visible region of `/dev/fb0`, so whatever the fbdev-emulation
//! client reveals next is a deterministic black screen — never a stale
//! decodable QR from an unrelated earlier invocation.

/// The byte range within a fbdev device that is CURRENTLY visible (scanned
/// out), given its `FBIOGET_VSCREENINFO`/`FBIOGET_FSCREENINFO` geometry:
/// `yoffset * line_length` .. `+ yres * line_length`. Blanking exactly this
/// range (not the whole virtual buffer) is correct for BOTH a single-buffer fb
/// (`yoffset == 0`, one page == the whole buffer — cam2's real hardware, #68)
/// and, defensively, a double-buffered one (blanks whichever page the fbdev
/// client is currently panned to).
///
/// Returns `(start_byte, len_bytes)`. Uses `u64` + `saturating_mul` throughout
/// so a corrupted/degenerate ioctl read can never panic the teardown path this
/// protects (a `u32 * u32` in native width WOULD overflow-panic in a debug
/// build; the widening cast avoids that even though the realistic byte counts
/// here never approach `u64::MAX`).
pub fn visible_page_range(yoffset: u32, yres: u32, line_length: u32) -> (u64, u64) {
    let line_length = line_length as u64;
    let start = (yoffset as u64).saturating_mul(line_length);
    let len = (yres as u64).saturating_mul(line_length);
    (start, len)
}

#[cfg(test)]
mod tests {
    use super::visible_page_range;

    /// cam2's real hardware (#68): single-buffer, 1920x1080 XRGB8888,
    /// yoffset always 0 — the whole 1080-row buffer is "the visible page".
    #[test]
    fn single_buffer_yoffset_zero_covers_whole_buffer() {
        let line_length = 1920u32 * 4; // 7680
        let (start, len) = visible_page_range(0, 1080, line_length);
        assert_eq!(start, 0);
        assert_eq!(len, 1080u64 * 7680);
    }

    /// A double-buffered fb panned to page 1 (yoffset == yres) blanks the
    /// SECOND page, not the first — defensive correctness even though cam2's
    /// deployed fb is single-buffer (#68).
    #[test]
    fn double_buffer_panned_to_second_page_offsets_start() {
        let yres = 1080u32;
        let line_length = 1920u32 * 4;
        let (start, len) = visible_page_range(yres, yres, line_length);
        assert_eq!(start, (yres as u64) * (line_length as u64));
        assert_eq!(len, (yres as u64) * (line_length as u64));
    }

    /// Degenerate zero geometry (an ioctl that returned zeros) must yield an
    /// EMPTY range, never a bogus nonzero write.
    #[test]
    fn zero_geometry_yields_empty_range() {
        assert_eq!(visible_page_range(0, 0, 0), (0, 0));
        assert_eq!(visible_page_range(0, 1080, 0), (0, 0));
    }

    /// A `u32 * u32` product comfortably fits `u64` (max ~1.8e19 < u64::MAX),
    /// but MUST go through the `u64` cast first — a native `u32` multiply of
    /// these values would overflow-panic in a debug build. Guards that the
    /// widening happens before the multiply, not after.
    #[test]
    fn huge_geometry_widens_before_multiplying() {
        let (start, len) = visible_page_range(u32::MAX, u32::MAX, u32::MAX);
        let expect = (u32::MAX as u64) * (u32::MAX as u64);
        assert_eq!(start, expect);
        assert_eq!(len, expect);
    }
}
