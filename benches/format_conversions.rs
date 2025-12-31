//! Benchmarks for format conversion functions
//!
//! Run with: cargo bench

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

// Import the standalone conversion functions from the library
use camera_box::display::{convert_rgba_to_bgra, convert_uyvy_to_bgra, scale_nearest_neighbor};
use camera_box::ndi::{convert_bgra_to_uyvy, convert_nv12_to_uyvy, convert_yuyv_to_uyvy_scalar};

#[cfg(target_arch = "x86_64")]
use camera_box::ndi::{convert_yuyv_to_uyvy_avx2, has_avx2};

fn bench_yuyv_to_uyvy(c: &mut Criterion) {
    let frame_1080p = vec![128u8; 1920 * 1080 * 2]; // YUYV is 2 bytes/pixel

    let mut group = c.benchmark_group("yuyv_to_uyvy");
    group.throughput(Throughput::Bytes(frame_1080p.len() as u64));

    group.bench_function("scalar_1080p", |b| {
        b.iter(|| convert_yuyv_to_uyvy_scalar(black_box(&frame_1080p)))
    });

    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        group.bench_function("avx2_1080p", |b| {
            b.iter(|| unsafe { convert_yuyv_to_uyvy_avx2(black_box(&frame_1080p)) })
        });
    }

    group.finish();
}

fn bench_uyvy_to_bgra(c: &mut Criterion) {
    let frame_1080p = vec![128u8; 1920 * 1080 * 2]; // UYVY is 2 bytes/pixel

    let mut group = c.benchmark_group("uyvy_to_bgra");
    group.throughput(Throughput::Bytes(frame_1080p.len() as u64));

    group.bench_function("1080p", |b| {
        b.iter(|| convert_uyvy_to_bgra(black_box(&frame_1080p), 1920, 1080))
    });

    group.finish();
}

fn bench_bgra_to_uyvy(c: &mut Criterion) {
    let frame_1080p = vec![128u8; 1920 * 1080 * 4]; // BGRA is 4 bytes/pixel

    let mut group = c.benchmark_group("bgra_to_uyvy");
    group.throughput(Throughput::Bytes(frame_1080p.len() as u64));

    group.bench_function("1080p", |b| {
        b.iter(|| convert_bgra_to_uyvy(black_box(&frame_1080p), 1920, 1080))
    });

    group.finish();
}

fn bench_nv12_to_uyvy(c: &mut Criterion) {
    // NV12 is 1.5 bytes per pixel (Y plane + UV plane at half resolution)
    let y_size = 1920 * 1080;
    let uv_size = 1920 * 1080 / 2;
    let frame_1080p = vec![128u8; y_size + uv_size];

    let mut group = c.benchmark_group("nv12_to_uyvy");
    group.throughput(Throughput::Bytes(frame_1080p.len() as u64));

    group.bench_function("1080p", |b| {
        b.iter(|| convert_nv12_to_uyvy(black_box(&frame_1080p), 1920, 1080))
    });

    group.finish();
}

fn bench_rgba_to_bgra(c: &mut Criterion) {
    let frame_1080p = vec![128u8; 1920 * 1080 * 4];

    let mut group = c.benchmark_group("rgba_to_bgra");
    group.throughput(Throughput::Bytes(frame_1080p.len() as u64));

    group.bench_function("1080p", |b| {
        b.iter(|| convert_rgba_to_bgra(black_box(&frame_1080p)))
    });

    group.finish();
}

fn bench_scale_nearest(c: &mut Criterion) {
    // Source 720p, scale to 1080p
    let frame_720p = vec![128u8; 1280 * 720 * 4];

    let mut group = c.benchmark_group("scale_nearest");
    group.throughput(Throughput::Bytes((1920 * 1080 * 4) as u64)); // Output size

    group.bench_function("720p_to_1080p", |b| {
        b.iter(|| scale_nearest_neighbor(black_box(&frame_720p), 1280, 720, 1920, 1080))
    });

    // Downscale 4K to 1080p
    let frame_4k = vec![128u8; 3840 * 2160 * 4];
    group.bench_function("4k_to_1080p", |b| {
        b.iter(|| scale_nearest_neighbor(black_box(&frame_4k), 3840, 2160, 1920, 1080))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_yuyv_to_uyvy,
    bench_uyvy_to_bgra,
    bench_bgra_to_uyvy,
    bench_nv12_to_uyvy,
    bench_rgba_to_bgra,
    bench_scale_nearest,
);
criterion_main!(benches);
