//! Vsync, double-buffered framebuffer presenter for the painter.
//!
//! A direct `/dev/fb0` write tears: the capture can sample the framebuffer while
//! a frame is half-written, producing an image where the QR is partially updated
//! and will not decode. That surfaces as occasional single-frame "loss" that is a
//! rig artifact, not a real pipeline drop. This presenter writes each frame to an
//! off-screen page and flips it at vblank, so the capture only ever sees complete
//! frames.
//!
//! #68: on the live cam2 rig the DRM fbdev emulation (`i915drmfb`) exposes
//! `yres_virtual == yres` (1080), so `FBIOPUT_VSCREENINFO` cannot allocate a
//! second page and page-flip double buffering is UNAVAILABLE — every painter
//! write went straight to the scanned-out buffer, tearing ~2.2% of captured QR
//! frames (the measured decode-failure blind spot). The fallback now writes
//! TEAR-FREE without a second page by gating the write on the vertical blank:
//! `FBIO_WAITFORVSYNC` blocks until vblank, then the full ~8 MB frame is written
//! in one shot during/just after the blanking interval, so the QR is settled
//! before the camera's next sample reads it. Combined with the painter holding
//! each id across multiple capture periods (sub-capture-rate paint), the camera
//! never samples a half-written QR. If `FBIO_WAITFORVSYNC` is unsupported the
//! presenter degrades to a plain direct write (the original behaviour).

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOPUT_VSCREENINFO: libc::c_ulong = 0x4601;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;
const FBIOPAN_DISPLAY: libc::c_ulong = 0x4606;
// _IOW('F', 0x20, __u32)
const FBIO_WAITFORVSYNC: libc::c_ulong = 0x4004_4620;
const FB_ACTIVATE_VBL: u32 = 16;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

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

/// Double-buffered, vsync-flipping framebuffer presenter.
pub struct VsyncFb {
    /// #1186: the fbdev device path (e.g. `/dev/fb0`) this presenter writes,
    /// stashed so `Drop` can blank it on teardown — the fbdev-fallback sibling of
    /// `KmsPresenter`'s #660 blank. Unlike KMS (which hands the CRTC back to fbdev
    /// emulation), `VsyncFb` writes this device DIRECTLY, so a dropped painter
    /// otherwise leaves its last frame scanned out on the monitor with nothing to
    /// clear it.
    device: String,
    file: File,
    width: u32,
    height: u32,
    line_length: u32,
    vinfo: FbVarScreenInfo,
    double_buffered: bool,
    back_page: u32,
    /// #68: when page-flip double buffering is unavailable, gate each direct write
    /// on `FBIO_WAITFORVSYNC` so the frame is written during/just after vblank and
    /// is settled before the camera samples it (tear-free single-buffer write).
    /// `false` if the driver does not support the ioctl — then a plain direct write
    /// (the original behaviour) is used.
    vsync_wait: bool,
}

/// Open `device` and read its CURRENT `FBIOGET_VSCREENINFO`/`FBIOGET_FSCREENINFO`
/// geometry, without changing anything. Shared by [`VsyncFb::open`] (which then
/// additionally negotiates double buffering via a `FBIOPUT_VSCREENINFO`) and
/// `blank_fbdev` (#660 — which only ever needs to READ the current geometry,
/// never to change it).
fn open_and_read_screeninfo(device: &str) -> Result<(File, FbVarScreenInfo, FbFixScreenInfo)> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .with_context(|| format!("open framebuffer {device}"))?;
    let fd = file.as_raw_fd();

    let mut vinfo = FbVarScreenInfo::default();
    if unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO, &mut vinfo) } < 0 {
        bail!("FBIOGET_VSCREENINFO failed for {device}");
    }
    let mut finfo = FbFixScreenInfo::default();
    if unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO, &mut finfo) } < 0 {
        bail!("FBIOGET_FSCREENINFO failed for {device}");
    }
    Ok((file, vinfo, finfo))
}

impl VsyncFb {
    /// Open the framebuffer and try to enable double buffering.
    pub fn open(device: &str) -> Result<Self> {
        let (file, vinfo, finfo) = open_and_read_screeninfo(device)?;
        let fd = file.as_raw_fd();
        let width = vinfo.xres;
        let height = vinfo.yres;
        let line_length = finfo.line_length;

        // Request a virtual height of 2*yres so we can pan between two pages.
        let mut want = vinfo;
        want.yres_virtual = height * 2;
        want.xres_virtual = width.max(vinfo.xres_virtual);
        want.xoffset = 0;
        want.yoffset = 0;
        want.activate = 0; // FB_ACTIVATE_NOW
        let put_ok = unsafe { libc::ioctl(fd, FBIOPUT_VSCREENINFO, &want) } >= 0;

        let mut got = FbVarScreenInfo::default();
        unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO, &mut got) };
        let double_buffered = put_ok && got.yres_virtual >= height * 2;

        // #68: if we can't page-flip, probe whether FBIO_WAITFORVSYNC works so the
        // single-buffer fallback can gate writes on vblank (tear-free). One probe
        // call at open: if the driver rejects it, plain direct writes are used.
        let vsync_wait = if double_buffered {
            false // page-flip path already waits for vsync on each flip
        } else {
            let mut crtc: u32 = 0;
            unsafe { libc::ioctl(fd, FBIO_WAITFORVSYNC, &mut crtc) >= 0 }
        };

        if double_buffered {
            tracing::info!(
                "VsyncFb: double-buffered {}x{} (virtual {}x{}), vsync flips enabled",
                width,
                height,
                got.xres_virtual,
                got.yres_virtual
            );
        } else if vsync_wait {
            tracing::info!(
                "VsyncFb: single-buffer (yres_virtual={}), vsync-gated direct writes \
                 (tear-free, #68)",
                got.yres_virtual
            );
        } else {
            tracing::warn!(
                "VsyncFb: single-buffer (yres_virtual={}) AND no FBIO_WAITFORVSYNC; \
                 plain direct write, tearing possible",
                got.yres_virtual
            );
        }

        Ok(Self {
            device: device.to_string(),
            file,
            width,
            height,
            line_length,
            vinfo: got,
            double_buffered,
            back_page: 1,
            vsync_wait,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Present a full BGRA frame (`width*height*4` bytes). Tear-free when
    /// double-buffered (write off-screen page → pan at vblank); on a single-buffer
    /// fb, tear-free via a vsync-gated direct write (#68): wait for vblank, then
    /// write the whole frame so it is settled before the camera's next sample. If
    /// neither is available, a plain direct write (tearing possible).
    pub fn present(&mut self, bgra: &[u8]) -> Result<()> {
        let page = if self.double_buffered {
            self.back_page
        } else {
            0
        };
        let page_off = (page as u64) * (self.height as u64) * (self.line_length as u64);
        let src_stride = (self.width * 4) as usize;

        // #68: single-buffer tear-free write — block until vblank so the write
        // lands during/just after the blanking interval and the full QR is settled
        // before the camera samples the next scanned-out frame. (Double-buffered
        // writes go to the off-screen page, so they don't need this; their vsync
        // wait happens at the pan-flip below.)
        if !self.double_buffered && self.vsync_wait {
            let fd = self.file.as_raw_fd();
            let mut crtc: u32 = 0;
            if unsafe { libc::ioctl(fd, FBIO_WAITFORVSYNC, &mut crtc) } < 0 {
                // The ioctl started working then stopped — degrade to plain writes.
                self.vsync_wait = false;
            }
        }

        if self.line_length as usize == src_stride {
            self.file.write_all_at(bgra, page_off)?;
        } else {
            for y in 0..self.height as usize {
                let s = y * src_stride;
                self.file.write_all_at(
                    &bgra[s..s + src_stride],
                    page_off + (y as u64) * (self.line_length as u64),
                )?;
            }
        }

        if self.double_buffered {
            let fd = self.file.as_raw_fd();
            let mut v = self.vinfo;
            v.xoffset = 0;
            v.yoffset = page * self.height;
            v.activate = FB_ACTIVATE_VBL;
            if unsafe { libc::ioctl(fd, FBIOPAN_DISPLAY, &v) } < 0 {
                // Pan failed at runtime; degrade to direct writes.
                self.double_buffered = false;
            } else {
                let mut crtc: u32 = 0;
                // Best-effort: wait for the flip so we don't reuse the page too soon.
                unsafe { libc::ioctl(fd, FBIO_WAITFORVSYNC, &mut crtc) };
                self.back_page ^= 1;
            }
        }
        Ok(())
    }
}

impl Drop for VsyncFb {
    /// #1186: blank the fbdev device on teardown — the fbdev-fallback sibling of
    /// `KmsPresenter`'s #660 `Drop` blank. `VsyncFb` writes `self.device`
    /// DIRECTLY (it IS the scanned-out fbdev), so a dropped painter — a clean
    /// `--duration-secs` self-exit OR a signal-driven stop that #1186 turned into
    /// a clean loop break + return — otherwise leaves its last painted frame on
    /// cam2's HDMI monitor with nothing to clear it. `blank_fbdev` re-opens the
    /// device (a second handle is fine; `self.file` is still open here, dropped
    /// only after this method returns) and re-reads the LIVE geometry, so it
    /// blanks whichever page is currently visible (single- or double-buffered).
    /// Best-effort — log-and-continue, never panic in `Drop`.
    fn drop(&mut self) {
        if let Err(e) = blank_fbdev(&self.device) {
            tracing::warn!("VsyncFb: blank {} on teardown failed: {:?}", self.device, e);
        }
    }
}

/// #660: write an all-zero (black) frame into `device`'s CURRENTLY visible page,
/// so a caller handing control of the CRTC back to another display client (the
/// kernel's fbdev-emulation helper — see `crate::probe::kms::KmsPresenter`'s
/// `Drop`) reveals a KNOWN black screen, never whatever ARBITRARILY OLD content
/// (camera-box's own `--display` module's last frame, or — pre-#1186 — a prior
/// `VsyncFb` run; since #1186 `VsyncFb` self-blanks in its own `Drop` via this
/// same function) happened to still be sitting in the device's memory.
/// `KmsPresenter` never
/// touches this device itself (it drives the CRTC through its own separate DRM
/// dumb buffers), so nothing else clears it between painter runs — see
/// `crate::fb_blank` for the full incident writeup.
///
/// Best-effort by design: the caller (`Drop`) cannot propagate an error, so this
/// returns `Result` for the caller to log-and-continue on failure rather than
/// abort teardown.
pub fn blank_fbdev(device: &str) -> Result<()> {
    let (file, vinfo, finfo) = open_and_read_screeninfo(device)
        .with_context(|| format!("open/read {device} geometry to blank"))?;

    let (start, len) =
        crate::fb_blank::visible_page_range(vinfo.yoffset, vinfo.yres, finfo.line_length);
    if len == 0 {
        bail!("degenerate fbdev geometry while blanking {device} (yres or line_length is 0)");
    }
    let zeros = vec![0u8; len as usize];
    file.write_all_at(&zeros, start)
        .with_context(|| format!("write black frame to {device}"))?;
    tracing::info!(
        device = device,
        start = start,
        len = len,
        "blanked fbdev visible page before KMS teardown (#660)"
    );
    Ok(())
}
