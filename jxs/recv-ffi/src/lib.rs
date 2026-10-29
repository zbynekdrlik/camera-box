//! Issue 1387 slice 2: a C ABI over camera-box's JPEG XS receiver (RED stub: the structs only).
#![cfg(target_os = "linux")]

/// `CBJXS_ABI_VERSION` of `include/camera_box_jxs.h`.
pub const CBJXS_ABI_VERSION: u32 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CbjxsFrame {
    pub width: u32,
    pub height: u32,
    pub y: *const u8,
    pub u: *const u8,
    pub v: *const u8,
    pub y_stride: u32,
    pub uv_stride: u32,
    pub timecode_100ns: i64,
    pub first_arrival_100ns: i64,
    pub last_arrival_100ns: i64,
    pub decoded_at_100ns: i64,
    pub lost_slices: u32,
    pub camera_id: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CbjxsStats {
    pub frames: u64,
    pub concealed_frames: u64,
    pub lost_slices: u64,
    pub undecodable_frames: u64,
    pub datagrams: u64,
    pub bytes: u64,
    pub duplicate: u64,
    pub stale: u64,
    pub malformed: u64,
    pub resyncs: u64,
    pub foreign: u64,
    pub reports_sent: u64,
    pub report_errors: u64,
    pub header_changes: u64,
    pub dropped_unread: u64,
    pub camera_id: u8,
    pub reserved: [u8; 7],
}
