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

        // Write to framebuffer using pwrite (atomic position + write)
        let src_stride = self.width as usize * 4;
        if self.line_length as usize == src_stride {
            // No padding needed - write entire frame at once at offset 0
            self.file.write_all_at(&final_data, 0)?;
        } else {
            // Write line by line with padding
            self.file.seek(SeekFrom::Start(0))?;
            for y in 0..self.height as usize {
                let src_offset = y * src_stride;
                let src_end = src_offset + src_stride;
                if src_end <= final_data.len() {
                    self.file.write_all(&final_data[src_offset..src_end])?;
                    let padding = self.line_length as usize - src_stride;
                    if padding > 0 {
                        self.file.write_all(&vec![0u8; padding])?;
                    }
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
    for chunk in rgba.chunks_exact(4) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
