---
paths:
  - "scripts/vstack-release.pin"
  - "tests/vstack_pin_1487.rs"
  - "tests/support/vstack.rs"
  - "tests/python/vstack_src.py"
  - "Cargo.toml"
  - ".github/workflows/linux-genlock.yml"
---

# camera-box on a pinned vstack tag (issue 1487)

`zbynekdrlik/vstack` owns the JPEG XS link, the rig primitives, the QPSK marker protocol and, since
v0.1.7 (vstack#8), the test-burn geometry (`burn-geom`: burn_regions, burn_quiet_zone, burn_tick_cache,
colour_scale; re-exported at the old paths, the C++ parity tests stay here), and since v0.1.8/v0.1.9
the cambox capture side, since v0.1.10 the NDI side. A `vstack:<path>` in this repo's prose is that path inside the pinned tag.

- **v0.1.8/v0.1.9 moved the capture side** (behaviour unchanged from camera-box dev 4ee558aba):
  - `rig_core::genlock_stamp` (portable now, `floor_boundary_100ns` is `pub` there; `src/ndi.rs`, since
    v0.1.10 `vstack:av/ndi/src/ndi.rs`, imports it, no copy here) and `rig_core::send_stall`;
  - the crate `av-capture` (`vstack:av/capture`): `capture` (Linux only, with the `v4l` crate),
    `capture_phase` (+ `source_phase`), `capture_rate_health`, `capture_stall`, `capture_wedge`. The
    decision modules build for Windows, so the probe build can reach them.
  - Kept here: `capture_rate_selfheal` (the box's own USB reset), `src/capture_phase_bench.rs`,
    `tests/harness_capture_phase_wiring_1367.rs` and `tests/obs_frame_stamp_parity_1367.rs` (on the
    re-exports). The stamp's own cases (the old `tests/obs_frame_stamp_1367.rs`) run in rig-core.
  - A test pinning a constant or a wiring line of `capture.rs` reads it through
    `vstack::read("av/capture/src/capture.rs")` (Rust) / `vstack_src.read(...)` (pytest).

- **v0.1.10 moved the NDI side** (behaviour unchanged from camera-box dev 4ee558aba): the crate
  `av-ndi` (`vstack:av/ndi`): `ndi` (Linux only; takes av-capture's `Frame`/`FrameRate`/`FrameInfo`
  and rig-core's `floor_boundary_100ns`), `send_stagger`, `reannounce`, `preview_source` (portable,
  so the Windows probe build still reaches them). New internal test seams there:
  `usable_ipv4_addrs`, `NdiSender::maybe_reannounce_with`, `NdiReceiver::connect_with_lib`,
  `run_mjpeg_decoder`, `uyvy_stride`; `maybe_reannounce` and `connect_with` keep their signatures.
  - Kept here, on the re-exports: `ndi_display`, `tests/ndi_reannounce.rs`,
    `tests/harness_send_stagger_1242.rs` and the probe binaries.
  - `next_boundary_100ns` is crate-private in av-ndi: a doc comment here names it as plain code
    text, never an intra-doc link.
  - Tests that pin `ndi.rs` / `preview_source.rs` text read `av/ndi/src/...` from the tag.

- **One pin:** `scripts/vstack-release.pin` (tag + commit). The tag is repeated on every vstack git
  dependency in root `Cargo.toml` (`camera-box-jxs`, `rig-core`, `qpsk-marker`, `burn-geom`,
  `av-capture`, `av-ndi`, optional `svt-jpegxs-sys`). `tests/vstack_pin_1487.rs` fails until all of them and the lock agree.
- **To bump:** edit the pin file and every tag, `cargo update -p rig-core`, run the suite. A new
  pin triggers linux-genlock.
- **`src/lib.rs` keeps the old paths** with `pub use`, so `camera_box::genlock_wall_step` and the
  binaries work unchanged. `qpsk_marker` is written `::qpsk_marker::qpsk_marker`: the leading `::`
  names the crate, not the re-export.
- **The benches stay here** on the re-exports (`genlock_n2_grid_bench`, `genlock_fifo_relabel_bench`,
  `genlock_shallow_av_bench`, the ASRC benches). `genlock_audio_pairing_bench.rs` uses `use super::*`,
  so `src/lib.rs` mounts it in a `genlock_audio_pairing` module that glob re-exports rig-core; that
  module carries `#[path = "."]`, because a `../` path needs a `src/genlock_audio_pairing/` directory.
- **A script that runs inside the full-path E2E job cannot read the pinned source.** That job has no
  cargo, so no checkout of the tag. `scripts/genlock_n2_grid.py` reads the vendored C
  `#define GENLOCK_N2_AGE_BASE_NS`, which the parity gate holds equal to the pinned Rust. Only tests
  use the pinned-source helpers.
- **`jxs-recv-ffi` / `jxs-send-ffi` are never root dependencies.** They pull the codec in
  unconditionally, and a default build must need no cmake or nasm.
- **The moved crates' unit tests run in vstack's CI, not here.** Cargo refuses `-p` on a dependency,
  so nextest, clippy, llvm-cov and mutants here cover the root package. Parity tests against the
  vendored C stay here and test the pinned crate. A vendored header change and its Rust twin are a
  vstack tag plus a pin bump here, in one change.
- **linux-genlock builds `libjxs_recv_ffi.a` in a clone of the tag** and refuses a commit that differs
  from the pin. vstack's root carries the release profile (LTO, one codegen unit, strip) since v0.1.4.
- **Reading pinned source from a test:** `tests/support/vstack.rs` (Rust), `tests/python/vstack_src.py`
  (pytest). Both find cargo's git checkout by the pin commit. The python-tests job compiles nothing, so
  it runs `cargo fetch` first; a missing checkout fails, it never skips.
- **Vendored comments keep old paths** on purpose (`vendor/obs-studio`, `vendor/distroav`): editing them
  starts an OBS rebuild. They go with the next real vendor change.
