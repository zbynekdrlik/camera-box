//! Issue 1387 slice 3: a raw planar picture handed to the JPEG XS sender by a caller that is not
//! the cambox capture loop (the strih OBS program output, through `jxs/send-ffi`). Pure types,
//! every platform; the hand-off that copies them into the encoder's picture is
//! `JxsHandoff::offer_raw` (feature `jxs`).

/// The memory layout of a raw picture. JPEG XS codes 4:2:0, 4:2:2 and 4:4:4 as they are; NV12 is
/// 4:2:0 with its U and V interleaved in one plane (the OBS canvas default), split on the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawLayout {
    /// Y, U, V planes; U and V `width/2 x height/2`.
    I420,
    /// Y, U, V planes; U and V `width/2 x height`.
    I422,
    /// Y, U, V planes, all `width x height`.
    I444,
    /// Y plane + one `U V U V ...` plane of `width/2` pairs x `height/2` rows.
    Nv12,
}

impl RawLayout {
    /// The `CBJXS_SEND_*` code of `camera_box_jxs_send.h` (`jxs/send-ffi`): 1 I420, 2 I422,
    /// 3 I444, 4 NV12; anything else is `None`.
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::I420),
            2 => Some(Self::I422),
            3 => Some(Self::I444),
            4 => Some(Self::Nv12),
            _ => None,
        }
    }

    /// The chroma sampling the encoder codes it as (`(horizontal, vertical)` subsampling).
    pub fn subsampling(self) -> (u32, u32) {
        match self {
            Self::I420 | Self::Nv12 => (2, 2),
            Self::I422 => (2, 1),
            Self::I444 => (1, 1),
        }
    }

    /// U/V samples per row and chroma rows of a `width x height` picture of this layout.
    pub fn chroma_dims(self, width: u32, height: u32) -> (u32, u32) {
        let (sx, sy) = self.subsampling();
        (width / sx, height / sy)
    }

    /// Whether `width x height` subsamples without a remainder (non-zero, even where halved).
    pub fn fits(self, width: u32, height: u32) -> bool {
        let (sx, sy) = self.subsampling();
        width != 0 && height != 0 && width.is_multiple_of(sx) && height.is_multiple_of(sy)
    }

    /// The source planes this layout uses (NV12: 2).
    pub fn plane_count(self) -> usize {
        match self {
            Self::Nv12 => 2,
            _ => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::I420 => "I420",
            Self::I422 => "I422",
            Self::I444 => "I444",
            Self::Nv12 => "NV12",
        }
    }
}

/// One raw picture: its planes (borrowed for the call only; the hand-off copies them), their row
/// strides in bytes, and the genlock timecode it is stamped with.
#[derive(Debug, Clone, Copy)]
pub struct RawFrame<'a> {
    pub layout: RawLayout,
    pub width: u32,
    pub height: u32,
    /// Y, then U, V (NV12: Y, UV; the third is unused and may be empty).
    pub planes: [&'a [u8]; 3],
    pub strides: [usize; 3],
    /// The genlock wall-clock timecode (100 ns since the Unix epoch), the same stamp an NDI
    /// sender puts on the frame.
    pub timecode_100ns: i64,
    /// The frame rate the caller emits at: selects the frames on the 30 fps link grid
    /// (`jxs_link::on_jxs_grid`; at 30 or below every frame goes).
    pub emit_fps: u32,
}

/// What the hand-off did with an offered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    /// Copied and queued for the encoder.
    Taken,
    /// Not on the 30 fps link grid (the other frame of a 60 fps pair): never sent, never a drop.
    OffGrid,
    /// Every picture was in use: dropped and counted (`dropped_busy`).
    Busy,
    /// The geometry, a stride or a plane did not fit: dropped and counted (`encode_errors`).
    Refused,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_codes_and_chroma_geometry() {
        assert_eq!(RawLayout::from_code(1), Some(RawLayout::I420));
        assert_eq!(RawLayout::from_code(4), Some(RawLayout::Nv12));
        assert_eq!(RawLayout::from_code(0), None);
        assert_eq!(RawLayout::from_code(5), None);
        assert_eq!(RawLayout::Nv12.chroma_dims(1920, 1080), (960, 540));
        assert_eq!(RawLayout::I420.chroma_dims(1920, 1080), (960, 540));
        assert_eq!(RawLayout::I422.chroma_dims(1920, 1080), (960, 1080));
        assert_eq!(RawLayout::I444.chroma_dims(1920, 1080), (1920, 1080));
        assert!(RawLayout::I444.fits(1921, 1081));
        assert!(!RawLayout::Nv12.fits(1920, 1081));
        assert!(!RawLayout::I422.fits(1921, 1080));
        assert!(!RawLayout::I420.fits(0, 2));
        assert_eq!(RawLayout::Nv12.plane_count(), 2);
        assert_eq!(RawLayout::I444.plane_count(), 3);
    }
}
