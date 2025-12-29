//! Framebuffer display for HDMI output
//!
//! Simple framebuffer-based display that writes directly to /dev/fb0.
//! Used for displaying NDI streams on the local HDMI output.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
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

        // Scale if needed
        let final_data = if width != self.width || height != self.height {
            self.scale_nearest(&bgra_data, width, height, self.width, self.height)
        } else {
            bgra_data
        };

        // Write to framebuffer
        self.file.seek(SeekFrom::Start(0))?;

        // Write line by line to handle line_length padding
        let src_stride = self.width as usize * 4;
        for y in 0..self.height as usize {
            let src_offset = y * src_stride;
            let src_end = src_offset + src_stride;
            if src_end <= final_data.len() {
                self.file.write_all(&final_data[src_offset..src_end])?;
                // Pad to line_length if needed
                let padding = self.line_length as usize - src_stride;
                if padding > 0 {
                    self.file.write_all(&vec![0u8; padding])?;
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
        for chunk in rgba.chunks_exact(4) {
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
