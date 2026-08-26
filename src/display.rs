//! Framebuffer display for HDMI output
//!
//! Simple framebuffer-based display that writes directly to /dev/fb0.
//! Used for displaying NDI streams on the local HDMI output.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;

// Framebuffer ioctl constants
const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FbVarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    // Timing
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FbFixScreenInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    fb_type: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

/// Framebuffer display wrapper
pub struct FramebufferDisplay {
    file: File,
    width: u32,
    height: u32,
    #[allow(dead_code)]
    bits_per_pixel: u32,
    line_length: u32,
    /// #244: reusable zero buffer for the per-row right-margin padding in the
    /// letterboxed/padded-stride branch of `display_frame`, hoisted out of the hot path so
    /// it allocates once (resized in place) instead of `vec![0u8; right_pad]` every frame.
    pad_buf: Vec<u8>,
    /// #244: reusable zero buffer for one full blank fb row (the bottom-letterbox fill),
    /// same per-frame-alloc hoist as `pad_buf`.
    blank_row: Vec<u8>,
}

impl FramebufferDisplay {
    /// Open the framebuffer device
    pub fn open(device: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .with_context(|| format!("Failed to open framebuffer: {}", device))?;

        let fd = file.as_raw_fd();

        // Get variable screen info
        let mut vinfo = FbVarScreenInfo::default();
        let ret = unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO, &mut vinfo) };
        if ret < 0 {
            anyhow::bail!("Failed to get framebuffer variable info");
        }

        // Get fixed screen info
        let mut finfo = FbFixScreenInfo::default();
        let ret = unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO, &mut finfo) };
        if ret < 0 {
            anyhow::bail!("Failed to get framebuffer fixed info");
        }

        tracing::info!(
            "Framebuffer: {}x{} {}bpp (line_length: {})",
            vinfo.xres,
            vinfo.yres,
            vinfo.bits_per_pixel,
            finfo.line_length
        );

        Ok(Self {
            file,
            width: vinfo.xres,
            height: vinfo.yres,
            bits_per_pixel: vinfo.bits_per_pixel,
            line_length: finfo.line_length,
            // #244: start empty; grown in place on first use of the padded/letterbox branch.
            pad_buf: Vec::new(),
            blank_row: Vec::new(),
        })
    }

    /// Get display dimensions
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Display a frame (handles format conversion and scaling)
    pub fn display_frame(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        fourcc: u32,
    ) -> Result<()> {
        // Convert to BGRA for framebuffer
        let bgra_data = self.convert_to_bgra(data, width, height, fourcc)?;

        // #135: NEVER software-upscale beyond the source. Render at min(fb, source)
        // per axis — a 1080p source on a (possibly latched 4K phantom) fb renders 1:1
        // (the killer 1080→4K nested-loop upscale is never taken); a larger source is
        // downscaled to fit. The rendered frame is top-left aligned into the fb and the
        // remaining fb area is left as letterbox (untouched), so a bigger fb costs only
        // a 1:1 copy, not a per-fb-pixel upscale.
        let (render_w, render_h) = clamp_render_dims(width, height, self.width, self.height);
        let render_data = if render_w != width || render_h != height {
            self.scale_nearest(&bgra_data, width, height, render_w, render_h)
        } else {
            bgra_data
        };

        // Write the rendered frame into the fb. When the render exactly fills the fb
        // width and the fb has no row padding, write it in one shot; otherwise write
        // row by row (left-aligned), padding each fb row out to its full line_length
        // and blacking out the rows BELOW the render (vertical letterbox).
        let render_stride = render_w as usize * 4;
        let fb_stride = self.line_length as usize;
        if render_w == self.width && fb_stride == render_stride && render_h == self.height {
            // Exact fit, no padding - write entire frame at once.
            self.file.write_all_at(&render_data, 0)?;
        } else {
            // Row-by-row: copy each rendered row left-aligned, pad the rest of the fb
            // row with zeros (right margin). Then black out the rows below the render
            // (bottom margin) so a smaller-than-fb render never leaves stale framebuffer
            // content (boot console / a previous taller frame) visible in the letterbox.
            self.file.seek(SeekFrom::Start(0))?;
            let right_pad = fb_stride.saturating_sub(render_stride);
            // #244: reuse the hoisted zero buffer for the per-row right margin instead of a
            // fresh `vec![0u8; right_pad]` every frame. After the first padded frame at a
            // given size this allocates nothing — only the render+write work remains in this
            // CPU-sensitive display path.
            ensure_zero_scratch(&mut self.pad_buf, right_pad);
            for y in 0..render_h as usize {
                let src_offset = y * render_stride;
                let src_end = src_offset + render_stride;
                if src_end <= render_data.len() {
                    self.file.write_all(&render_data[src_offset..src_end])?;
                    if right_pad > 0 {
                        self.file.write_all(&self.pad_buf)?;
                    }
                }
            }
            // Black out the bottom letterbox (rows render_h..fb_height) with the hoisted,
            // reused full-stride zero buffer (#244: no per-frame alloc).
            if render_h < self.height {
                ensure_zero_scratch(&mut self.blank_row, fb_stride);
                for _ in render_h..self.height {
                    self.file.write_all(&self.blank_row)?;
                }
            }
        }

        Ok(())
    }

    /// Convert various formats to BGRA
    fn convert_to_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        fourcc: u32,
    ) -> Result<Vec<u8>> {
        let fourcc_bytes = fourcc.to_le_bytes();
        let fourcc_str = std::str::from_utf8(&fourcc_bytes).unwrap_or("????");

        match fourcc_str {
            "UYVY" => Ok(self.uyvy_to_bgra(data, width, height)),
            "BGRA" | "BGRX" => Ok(data.to_vec()),
            "RGBA" => Ok(self.rgba_to_bgra(data)),
            _ => {
                tracing::warn!(
                    "Unknown fourcc: {} (0x{:08x}), treating as UYVY",
                    fourcc_str,
                    fourcc
                );
                Ok(self.uyvy_to_bgra(data, width, height))
            }
        }
    }

    /// Convert UYVY to BGRA
    fn uyvy_to_bgra(&self, uyvy: &[u8], width: u32, height: u32) -> Vec<u8> {
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);

        for y in 0..height as usize {
            for x in (0..width as usize).step_by(2) {
                let idx = (y * width as usize + x) * 2;
                if idx + 3 >= uyvy.len() {
                    break;
                }

                let u = uyvy[idx] as i32 - 128;
                let y0 = uyvy[idx + 1] as i32;
                let v = uyvy[idx + 2] as i32 - 128;
                let y1 = uyvy[idx + 3] as i32;

                // YUV to RGB (BT.601)
                let r0 = (y0 + (359 * v) / 256).clamp(0, 255) as u8;
                let g0 = (y0 - (88 * u) / 256 - (183 * v) / 256).clamp(0, 255) as u8;
                let b0 = (y0 + (454 * u) / 256).clamp(0, 255) as u8;

                let r1 = (y1 + (359 * v) / 256).clamp(0, 255) as u8;
                let g1 = (y1 - (88 * u) / 256 - (183 * v) / 256).clamp(0, 255) as u8;
                let b1 = (y1 + (454 * u) / 256).clamp(0, 255) as u8;

                // BGRA format
                bgra.push(b0);
                bgra.push(g0);
                bgra.push(r0);
                bgra.push(255);

                bgra.push(b1);
                bgra.push(g1);
                bgra.push(r1);
                bgra.push(255);
            }
        }

        bgra
    }

    /// Convert RGBA to BGRA (swap R and B)
    fn rgba_to_bgra(&self, rgba: &[u8]) -> Vec<u8> {
        let mut bgra = Vec::with_capacity(rgba.len());
        for chunk in rgba.as_chunks::<4>().0 {
            bgra.push(chunk[2]); // B
            bgra.push(chunk[1]); // G
            bgra.push(chunk[0]); // R
            bgra.push(chunk[3]); // A
        }
        bgra
    }

    /// Simple nearest-neighbor scaling
    fn scale_nearest(&self, src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
        let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];

        for dst_y in 0..dst_h {
            let src_y = (dst_y * src_h / dst_h).min(src_h - 1);
            for dst_x in 0..dst_w {
                let src_x = (dst_x * src_w / dst_w).min(src_w - 1);

                let src_idx = ((src_y * src_w + src_x) * 4) as usize;
                let dst_idx = ((dst_y * dst_w + dst_x) * 4) as usize;

                if src_idx + 3 < src.len() && dst_idx + 3 < dst.len() {
                    dst[dst_idx] = src[src_idx];
                    dst[dst_idx + 1] = src[src_idx + 1];
                    dst[dst_idx + 2] = src[src_idx + 2];
                    dst[dst_idx + 3] = src[src_idx + 3];
                }
            }
        }

        dst
    }

    /// Clear the display to black
    #[allow(dead_code)]
    pub fn clear(&mut self) -> Result<()> {
        let black = vec![0u8; (self.line_length * self.height) as usize];
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&black)?;
        Ok(())
    }
}

// Standalone conversion functions for testing and potential reuse
// These mirror the FramebufferDisplay methods but don't require a framebuffer

/// Convert UYVY to BGRA (standalone version for testing)
pub fn convert_uyvy_to_bgra(uyvy: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut bgra = Vec::with_capacity((width * height * 4) as usize);

    for y in 0..height as usize {
        for x in (0..width as usize).step_by(2) {
            let idx = (y * width as usize + x) * 2;
            if idx + 3 >= uyvy.len() {
                break;
            }

            let u = uyvy[idx] as i32 - 128;
            let y0 = uyvy[idx + 1] as i32;
            let v = uyvy[idx + 2] as i32 - 128;
            let y1 = uyvy[idx + 3] as i32;

            // YUV to RGB (BT.601)
            let r0 = (y0 + (359 * v) / 256).clamp(0, 255) as u8;
            let g0 = (y0 - (88 * u) / 256 - (183 * v) / 256).clamp(0, 255) as u8;
            let b0 = (y0 + (454 * u) / 256).clamp(0, 255) as u8;

            let r1 = (y1 + (359 * v) / 256).clamp(0, 255) as u8;
            let g1 = (y1 - (88 * u) / 256 - (183 * v) / 256).clamp(0, 255) as u8;
            let b1 = (y1 + (454 * u) / 256).clamp(0, 255) as u8;

            // BGRA format
            bgra.push(b0);
            bgra.push(g0);
            bgra.push(r0);
            bgra.push(255);

            bgra.push(b1);
            bgra.push(g1);
            bgra.push(r1);
            bgra.push(255);
        }
    }

    bgra
}

/// Convert RGBA to BGRA (standalone version for testing)
pub fn convert_rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = Vec::with_capacity(rgba.len());
    for chunk in rgba.as_chunks::<4>().0 {
        bgra.push(chunk[2]); // B
        bgra.push(chunk[1]); // G
        bgra.push(chunk[0]); // R
        bgra.push(chunk[3]); // A
    }
    bgra
}

/// Simple nearest-neighbor scaling (standalone version for testing)
pub fn scale_nearest_neighbor(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];

    for dst_y in 0..dst_h {
        let src_y = (dst_y * src_h / dst_h).min(src_h - 1);
        for dst_x in 0..dst_w {
            let src_x = (dst_x * src_w / dst_w).min(src_w - 1);

            let src_idx = ((src_y * src_w + src_x) * 4) as usize;
            let dst_idx = ((dst_y * dst_w + dst_x) * 4) as usize;

            if src_idx + 3 < src.len() && dst_idx + 3 < dst.len() {
                dst[dst_idx] = src[src_idx];
                dst[dst_idx + 1] = src[src_idx + 1];
                dst[dst_idx + 2] = src[src_idx + 2];
                dst[dst_idx + 3] = src[src_idx + 3];
            }
        }
    }

    dst
}

/// Resize a reusable zero-fill scratch buffer to exactly `len` bytes, reusing its existing
/// allocation (#244: hoisted out of the per-frame `display_frame` path so the
/// padded/letterbox branch no longer does a `vec![0u8; n]` allocation every frame).
///
/// These scratch buffers are only ever WRITE SOURCES of zeros (their bytes are never
/// mutated after a resize), so resizing only when the length changes keeps every byte 0:
/// growing fills the new bytes with 0, shrinking drops the (already-zero) tail, and an
/// unchanged length is a no-op. After the first call at a given size, subsequent frames at
/// the same size reuse the buffer's heap allocation — zero per-frame allocation.
pub fn ensure_zero_scratch(buf: &mut Vec<u8>, len: usize) {
    if buf.len() != len {
        buf.resize(len, 0);
    }
}

/// Cap the render resolution so the display never SOFTWARE-UPSCALES beyond the
/// source (#135). Render at `min(fb, source)` per axis:
/// - source ≤ fb (e.g. 1080p source on a latched 4K phantom fb) → render 1:1 at the
///   source size (let the fb show it letterboxed); the killer `scale_nearest` 1080→4K
///   upscale (a per-pixel nested loop → 99.9% CPU) is NEVER taken.
/// - source > fb → downscale to the fb (cheap and necessary so it fits).
///
/// Upscaling a 1080p frame to 4K is pointless (no new detail) and heavy — the exact
/// CPU sink behind the pre-event cam1 incident (load 4.5, ~400ms emit latency).
pub fn clamp_render_dims(src_w: u32, src_h: u32, fb_w: u32, fb_h: u32) -> (u32, u32) {
    (src_w.min(fb_w), src_h.min(fb_h))
}

/// Decide whether a DRM connector is ACTIVELY DRIVING A REAL MONITOR — i.e. it is
/// the connector scanning out to the framebuffer AND a monitor is attached — from its
/// sysfs `status` / `enabled` / `modes` files (#135).
///
/// The framebuffer is scanned out by the connector whose `enabled == "enabled"`.
/// A real monitor on that connector reports `status=connected enabled=enabled
/// modes=[...]`. After hot-unplug, i915 leaves fb0 LATCHED at the old mode but the
/// connector flips to `status=disconnected enabled=disabled modes=[]` — a "phantom"
/// fb that opens and writes fine, so only this sysfs signal catches it.
///
/// We require BOTH `enabled=enabled` AND `status=connected`: a connector that is not
/// `enabled` is not driving the fb (so its presence is irrelevant to whether the fb is
/// real), and a `disconnected` connector has no monitor. This is the correct
/// granularity on a MULTI-connector box — a second connector with its own monitor does
/// NOT make the fb-driving connector's phantom state "connected".
pub fn connector_is_connected(status: &str, enabled: &str, _modes: &str) -> bool {
    status.trim() == "connected" && enabled.trim() == "enabled"
}

/// Whether the framebuffer is driven by a connector with a REAL monitor — i.e. at
/// least one DRM connector under `drm_class_dir` (normally `/sys/class/drm`) is both
/// `enabled` and `connected` (#135). Reads each `card*-<connector>/status` (+`enabled`)
/// sysfs file (a plain file read — NO `drm` crate, which is probe-feature-only; the
/// `--display` runtime mode is default-feature).
///
/// Returns `Some(true)` when an enabled+connected connector exists (render),
/// `Some(false)` when connectors are readable but NONE is enabled+connected (the
/// fb-driving connector is a phantom — skip render), or `None` when no connector status
/// could be read at all (sysfs absent / unexpected layout) — the caller falls back to
/// rendering (never silently go dark on an unknown layout). Requiring `enabled`
/// scopes the check to the connector actually scanning out to the fb, so a second
/// connector's monitor on a multi-connector box cannot defeat the phantom-fb gate.
pub fn any_connector_connected(drm_class_dir: &str) -> Option<bool> {
    let entries = std::fs::read_dir(drm_class_dir).ok()?;
    let mut saw_any_status = false;
    let mut connected = false;
    for entry in entries.flatten() {
        let path = entry.path();
        // Connector dirs are like `cardN-HDMI-A-1`; they contain a `status` file.
        let status_path = path.join("status");
        if let Ok(status) = std::fs::read_to_string(&status_path) {
            saw_any_status = true;
            let enabled = std::fs::read_to_string(path.join("enabled")).unwrap_or_default();
            let modes = std::fs::read_to_string(path.join("modes")).unwrap_or_default();
            if connector_is_connected(&status, &enabled, &modes) {
                connected = true;
            }
        }
    }
    if saw_any_status {
        Some(connected)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- #244: reusable per-frame zero scratch buffer (no per-frame alloc) -----

    #[test]
    fn ensure_zero_scratch_grows_with_zeros() {
        // From empty (the open() initial state) → grown to the needed length, all zero.
        let mut buf = Vec::new();
        ensure_zero_scratch(&mut buf, 5);
        assert_eq!(
            buf,
            vec![0u8; 5],
            "grow from empty must be all-zero of the new len"
        );
    }

    #[test]
    fn ensure_zero_scratch_shrinks_keeping_zeros() {
        // A bigger buffer reused for a smaller pad shrinks; the retained prefix is zero.
        let mut buf = vec![0u8; 8];
        ensure_zero_scratch(&mut buf, 3);
        assert_eq!(
            buf,
            vec![0u8; 3],
            "shrink must keep all-zero of the new len"
        );
    }

    #[test]
    fn ensure_zero_scratch_same_len_is_noop_and_reuses_alloc() {
        // The steady-state hot path: same size every frame → no realloc, capacity preserved
        // (no per-frame allocation — the #244 fix), contents stay all-zero.
        let mut buf = vec![0u8; 4];
        let cap_before = buf.capacity();
        let ptr_before = buf.as_ptr();
        ensure_zero_scratch(&mut buf, 4);
        assert_eq!(buf, vec![0u8; 4]);
        assert_eq!(buf.capacity(), cap_before, "same-len must not reallocate");
        assert_eq!(
            buf.as_ptr(),
            ptr_before,
            "same-len must reuse the existing allocation"
        );
    }

    #[test]
    fn ensure_zero_scratch_zero_len() {
        // The no-padding case (right_pad == 0) yields an empty buffer, never a panic.
        let mut buf = vec![0u8; 6];
        ensure_zero_scratch(&mut buf, 0);
        assert!(buf.is_empty(), "len 0 must yield an empty buffer");
    }

    // ----- #135: upscale cap (never software-upscale beyond source) -----

    #[test]
    fn clamp_render_dims_never_upscales_beyond_source() {
        // The incident: a 1080p source upscaled 1080->4K to a latched phantom fb ->
        // 99.9% CPU. Render dims must be capped to the source so an oversized fb never
        // triggers a heavy software upscale; render 1:1 and let the fb show it letterboxed.
        assert_eq!(
            clamp_render_dims(1920, 1080, 3840, 2160),
            (1920, 1080),
            "1080p source on a 4K fb => render 1:1 (1920x1080), NOT upscaled to 4K"
        );
    }

    #[test]
    fn clamp_render_dims_downscales_to_fb_when_source_larger() {
        // A source LARGER than the fb is still capped to the fb (downscale is cheap and
        // necessary so the frame fits) — only UPSCALING beyond source is forbidden.
        assert_eq!(
            clamp_render_dims(3840, 2160, 1920, 1080),
            (1920, 1080),
            "4K source on a 1080p fb => downscale to fb (1920x1080)"
        );
    }

    #[test]
    fn clamp_render_dims_matches_when_equal() {
        // The light, correct case (cam4: 1080p source, 1080p monitor) renders 1:1.
        assert_eq!(clamp_render_dims(1920, 1080, 1920, 1080), (1920, 1080));
    }

    #[test]
    fn clamp_render_dims_caps_each_axis_independently() {
        // src 1920x1080, fb 1280x2160: width capped to 1280 (fb<src), height capped to
        // 1080 (src<fb) — never exceed source on any axis.
        assert_eq!(clamp_render_dims(1920, 1080, 1280, 2160), (1280, 1080));
    }

    // ----- #135: connector-presence detection (skip render to a phantom fb) -----

    #[test]
    fn connector_connected_when_status_connected() {
        // cam2/cam4 real monitor: status=connected enabled=enabled modes=[1920x1080,...]
        assert!(connector_is_connected(
            "connected",
            "enabled",
            "1920x1080\n1280x720\n"
        ));
    }

    #[test]
    fn connector_disconnected_phantom_fb() {
        // The phantom/latched fb after hot-unplug: status=disconnected enabled=disabled
        // modes=[]. fb0 stays latched but NO monitor is present -> must NOT render.
        assert!(!connector_is_connected("disconnected", "disabled", ""));
    }

    #[test]
    fn connector_status_trims_whitespace() {
        // sysfs files carry a trailing newline.
        assert!(connector_is_connected(
            "connected\n",
            "enabled\n",
            "1920x1080\n"
        ));
        assert!(!connector_is_connected(
            "disconnected\n",
            "disabled\n",
            "\n"
        ));
    }

    #[test]
    fn connector_disconnected_even_if_enabled_lingers() {
        // status is the authoritative monitor-presence signal; a disconnected connector
        // is "no monitor" regardless of a lingering enabled flag.
        assert!(!connector_is_connected(
            "disconnected",
            "enabled",
            "1920x1080\n"
        ));
    }

    #[test]
    fn connector_connected_but_not_enabled_is_not_driving_fb() {
        // A connector with a monitor but NOT enabled is not scanning out to the fb — so
        // it must NOT count as "the fb is real". This is the multi-connector key case:
        // a second monitor on an unused connector must not mask the fb-driving
        // connector's phantom state.
        assert!(!connector_is_connected(
            "connected",
            "disabled",
            "1920x1080\n"
        ));
    }

    fn write_connector(dir: &std::path::Path, name: &str, status: &str, enabled: &str) {
        let c = dir.join(name);
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("status"), status).unwrap();
        std::fs::write(c.join("enabled"), enabled).unwrap();
        std::fs::write(c.join("modes"), "1920x1080\n").unwrap();
    }

    #[test]
    fn any_connector_none_when_no_status_files() {
        // Unknown / non-DRM layout (no connector has a status file) => None, so the
        // caller renders rather than silently going dark.
        let tmp = std::env::temp_dir().join(format!("cb-drm-none-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("renderD128")).unwrap();
        std::fs::create_dir_all(tmp.join("version")).unwrap();
        assert_eq!(any_connector_connected(tmp.to_str().unwrap()), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn any_connector_none_when_dir_missing() {
        assert_eq!(any_connector_connected("/no/such/drm/dir/xyz"), None);
    }

    #[test]
    fn any_connector_true_when_an_enabled_connected_connector_exists() {
        // Live cam2 layout: HDMI-A-1 disconnected, A-2 connected+enabled, A-3
        // disconnected. Non-connector dirs (no status) are skipped. => Some(true).
        let tmp = std::env::temp_dir().join(format!("cb-drm-true-{}", std::process::id()));
        write_connector(&tmp, "card1-HDMI-A-1", "disconnected\n", "disabled\n");
        write_connector(&tmp, "card1-HDMI-A-2", "connected\n", "enabled\n");
        write_connector(&tmp, "card1-HDMI-A-3", "disconnected\n", "disabled\n");
        std::fs::create_dir_all(tmp.join("renderD128")).unwrap(); // no status file
        assert_eq!(any_connector_connected(tmp.to_str().unwrap()), Some(true));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn any_connector_false_when_fb_connector_phantom_despite_other_monitor() {
        // The multi-connector phantom case: the fb-driving connector (A-1) hot-unplugged
        // (disconnected+disabled = phantom latched fb), while a SECOND monitor sits on a
        // non-enabled connector (A-2 connected but NOT enabled — not scanning out to the
        // fb). Requiring enabled+connected => Some(false) => skip render (the gate is NOT
        // defeated by the second monitor).
        let tmp = std::env::temp_dir().join(format!("cb-drm-phantom-{}", std::process::id()));
        write_connector(&tmp, "card1-HDMI-A-1", "disconnected\n", "disabled\n");
        write_connector(&tmp, "card1-HDMI-A-2", "connected\n", "disabled\n");
        assert_eq!(any_connector_connected(tmp.to_str().unwrap()), Some(false));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn incident_scenario_no_heavy_upscale_to_phantom_4k_fb() {
        // The pre-event cam1 incident (#135, #105 TDD bar): 1080p source, fb latched at
        // 4K, monitor hot-unplugged so the connector is disconnected. The display path
        // MUST do NEITHER a heavy 1080->4K software upscale NOR a write to the phantom
        // fb. Both guards independently neutralize the incident:
        //   1. connector disconnected => render skipped entirely (no fb write at all)
        let phantom_disconnected = connector_is_connected("disconnected", "disabled", "");
        assert!(!phantom_disconnected, "phantom fb => render is skipped");
        //   2. even if the gate were bypassed, the render is capped to source (1:1),
        //      so the 99.9%-CPU 1080->4K nested-loop upscale is never executed.
        assert_eq!(
            clamp_render_dims(1920, 1080, 3840, 2160),
            (1920, 1080),
            "no 1080->4K upscale even if connector check is bypassed"
        );
        // No-regression on the healthy case (cam4 / live cam2: connected 1080p monitor,
        // 1080p fb): render proceeds 1:1 (no upscale, no skip).
        assert!(connector_is_connected(
            "connected",
            "enabled",
            "1920x1080\n"
        ));
        assert_eq!(clamp_render_dims(1920, 1080, 1920, 1080), (1920, 1080));
    }

    #[test]
    fn test_uyvy_to_bgra_black() {
        // Black in UYVY: Y=16 (video black), U=128, V=128
        // UYVY format: U Y0 V Y1
        let uyvy = vec![128, 16, 128, 16]; // 2 black pixels
        let bgra = convert_uyvy_to_bgra(&uyvy, 2, 1);

        // Should produce near-black pixels
        assert_eq!(bgra.len(), 8); // 2 pixels * 4 bytes
                                   // First pixel BGRA
        assert!(bgra[0] < 30, "Blue should be dark: {}", bgra[0]);
        assert!(bgra[1] < 30, "Green should be dark: {}", bgra[1]);
        assert!(bgra[2] < 30, "Red should be dark: {}", bgra[2]);
        assert_eq!(bgra[3], 255, "Alpha should be 255");
    }

    #[test]
    fn test_uyvy_to_bgra_white() {
        // White in UYVY: Y=235 (video white), U=128, V=128
        let uyvy = vec![128, 235, 128, 235]; // 2 white pixels
        let bgra = convert_uyvy_to_bgra(&uyvy, 2, 1);

        assert_eq!(bgra.len(), 8);
        // First pixel should be near-white
        assert!(bgra[0] > 220, "Blue should be bright: {}", bgra[0]);
        assert!(bgra[1] > 220, "Green should be bright: {}", bgra[1]);
        assert!(bgra[2] > 220, "Red should be bright: {}", bgra[2]);
        assert_eq!(bgra[3], 255);
    }

    #[test]
    fn test_uyvy_to_bgra_red() {
        // Red in UYVY: Y=81, U=90, V=240 (approximate)
        let uyvy = vec![90, 81, 240, 81];
        let bgra = convert_uyvy_to_bgra(&uyvy, 2, 1);

        assert_eq!(bgra.len(), 8);
        // Red channel should be high, blue/green low
        assert!(bgra[2] > bgra[0], "Red > Blue for red pixel");
        assert!(bgra[2] > bgra[1], "Red > Green for red pixel");
    }

    #[test]
    fn test_uyvy_to_bgra_green() {
        // Green in UYVY: Y=145, U=54, V=34 (approximate)
        let uyvy = vec![54, 145, 34, 145];
        let bgra = convert_uyvy_to_bgra(&uyvy, 2, 1);

        assert_eq!(bgra.len(), 8);
        // Green channel should be highest
        assert!(bgra[1] > bgra[0], "Green > Blue for green pixel");
        assert!(bgra[1] > bgra[2], "Green > Red for green pixel");
    }

    #[test]
    fn test_uyvy_to_bgra_blue() {
        // Blue in UYVY: Y=41, U=240, V=110 (approximate)
        let uyvy = vec![240, 41, 110, 41];
        let bgra = convert_uyvy_to_bgra(&uyvy, 2, 1);

        assert_eq!(bgra.len(), 8);
        // Blue channel should be highest
        assert!(bgra[0] > bgra[1], "Blue > Green for blue pixel");
        assert!(bgra[0] > bgra[2], "Blue > Red for blue pixel");
    }

    #[test]
    fn test_uyvy_to_bgra_output_size() {
        // 4x2 image in UYVY = 4*2*2 = 16 bytes
        let uyvy = vec![128u8; 16];
        let bgra = convert_uyvy_to_bgra(&uyvy, 4, 2);

        // 4x2 in BGRA = 4*2*4 = 32 bytes
        assert_eq!(bgra.len(), 32);
    }

    #[test]
    fn test_rgba_to_bgra_swap() {
        // RGBA: R=255, G=128, B=64, A=200
        let rgba = vec![255, 128, 64, 200];
        let bgra = convert_rgba_to_bgra(&rgba);

        assert_eq!(bgra.len(), 4);
        assert_eq!(bgra[0], 64, "B should be from R position");
        assert_eq!(bgra[1], 128, "G stays in place");
        assert_eq!(bgra[2], 255, "R should be from B position");
        assert_eq!(bgra[3], 200, "A stays in place");
    }

    #[test]
    fn test_rgba_to_bgra_multiple_pixels() {
        // 2 pixels
        let rgba = vec![
            255, 0, 0, 255, // Red pixel
            0, 255, 0, 255, // Green pixel
        ];
        let bgra = convert_rgba_to_bgra(&rgba);

        assert_eq!(bgra.len(), 8);
        // First pixel (was RGBA red, now BGRA)
        assert_eq!(bgra[0], 0); // B
        assert_eq!(bgra[1], 0); // G
        assert_eq!(bgra[2], 255); // R
        assert_eq!(bgra[3], 255); // A

        // Second pixel (was RGBA green, now BGRA)
        assert_eq!(bgra[4], 0); // B
        assert_eq!(bgra[5], 255); // G
        assert_eq!(bgra[6], 0); // R
        assert_eq!(bgra[7], 255); // A
    }

    #[test]
    fn test_scale_nearest_passthrough() {
        // Same size should be identity (but creates new buffer)
        let src = vec![1, 2, 3, 4, 5, 6, 7, 8]; // 2x1 image
        let dst = scale_nearest_neighbor(&src, 2, 1, 2, 1);

        assert_eq!(dst.len(), 8);
        assert_eq!(dst, src);
    }

    #[test]
    fn test_scale_nearest_downscale_2x() {
        // 4x2 → 2x1 (4x downscale)
        // Source: 4 pixels wide, 2 tall
        let mut src = vec![0u8; 4 * 2 * 4];
        // Set pixel (0,0) to red
        src[0] = 0;
        src[1] = 0;
        src[2] = 255;
        src[3] = 255;
        // Set pixel (2,0) to green
        src[8] = 0;
        src[9] = 255;
        src[10] = 0;
        src[11] = 255;

        let dst = scale_nearest_neighbor(&src, 4, 2, 2, 1);

        assert_eq!(dst.len(), 8); // 2 pixels * 4 bytes
                                  // First output pixel should sample from (0,0) area - red
        assert_eq!(dst[2], 255, "First pixel should be red");
        // Second output pixel should sample from (2,0) area - green
        assert_eq!(dst[5], 255, "Second pixel should be green");
    }

    #[test]
    fn test_scale_nearest_upscale_2x() {
        // 2x1 → 4x2 (4x upscale)
        let src = vec![
            255, 0, 0, 255, // Blue pixel
            0, 255, 0, 255, // Green pixel
        ];

        let dst = scale_nearest_neighbor(&src, 2, 1, 4, 2);

        assert_eq!(dst.len(), 4 * 2 * 4);
        // All pixels in left half should be blue
        assert_eq!(dst[0], 255, "Pixel (0,0) should be blue");
        assert_eq!(dst[4], 255, "Pixel (1,0) should be blue");
        // All pixels in right half should be green
        assert_eq!(dst[9], 255, "Pixel (2,0) should be green");
        assert_eq!(dst[13], 255, "Pixel (3,0) should be green");
    }

    #[test]
    fn test_scale_nearest_odd_dimensions() {
        // 3x3 → 5x5 (non-integer scale factor)
        let src = vec![128u8; 3 * 3 * 4];
        let dst = scale_nearest_neighbor(&src, 3, 3, 5, 5);

        assert_eq!(dst.len(), 5 * 5 * 4);
        // All pixels should have value 128
        for chunk in dst.chunks(4) {
            assert_eq!(chunk[0], 128);
        }
    }

    #[test]
    fn test_uyvy_to_bgra_empty_input() {
        let uyvy: Vec<u8> = vec![];
        let bgra = convert_uyvy_to_bgra(&uyvy, 0, 0);
        assert!(bgra.is_empty());
    }

    #[test]
    fn test_rgba_to_bgra_empty_input() {
        let rgba: Vec<u8> = vec![];
        let bgra = convert_rgba_to_bgra(&rgba);
        assert!(bgra.is_empty());
    }

    #[test]
    fn test_uyvy_to_bgra_large_frame() {
        // Full HD frame
        let width = 1920u32;
        let height = 1080u32;
        let uyvy = vec![128u8; (width * height * 2) as usize];
        let bgra = convert_uyvy_to_bgra(&uyvy, width, height);

        assert_eq!(bgra.len(), (width * height * 4) as usize);
    }

    #[test]
    fn test_yuv_clamping() {
        // Test that extreme YUV values clamp properly and don't overflow
        // Max Y, extreme U/V that would cause overflow without clamping
        let uyvy = vec![255, 255, 255, 255];
        let bgra = convert_uyvy_to_bgra(&uyvy, 2, 1);

        // Should produce 2 pixels (8 bytes) without panicking
        assert_eq!(bgra.len(), 8);
        // Values should be valid u8 (this mainly tests no panic occurred)
        assert!(!bgra.is_empty());
    }
}
