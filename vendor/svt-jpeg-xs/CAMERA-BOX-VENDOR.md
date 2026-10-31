# SVT-JPEG-XS in camera-box (issue 1387)

- **Upstream:** https://github.com/OpenVisualCloud/SVT-JPEG-XS
- **Pinned commit:** `01bc7cc7123d2e4a3e330632fabec873f3cee62b` (master, 28.10.2026; library and
  API version 0.10.0). The only release tag, v0.9.0 (20.6.2024), has an older ABI. This commit is
  the tree every issue-1387 measurement was built from (encode cost 6489131141, JPEG XS vs NDI
  6489509200, the N100 slice latency 6489539657).
- **Licence:** BSD-2-Clause-Patent (`LICENSE.md`).
- **Imported:** `CMakeLists.txt`, `LICENSE.md`, `Source/API`, `Source/Lib`, `third_party/cpuinfo`,
  verbatim. Not imported: the apps (`Source/App`), tests, googletest, docs, the ffmpeg/MTL plugins
  and the `Build`/`Bin` dirs. The top `CMakeLists.txt` reaches them only behind `BUILD_APPS`,
  `BUILD_TESTING` and `JPEGXS_PGO`, all forced OFF by `jxs/sys/csrc/CMakeLists.txt`.
- **One local patch** (issue 1387, live 31.10.2026): `Source/Lib/Common/Codec/Threads/SvtThreads.c`
  `svt_jxs_create_thread` no longer calls `pthread_setschedparam(..., SCHED_FIFO, 99)` when the
  process runs as root. camera-box runs as root, so upstream's block put the encoder workers ABOVE
  the capture thread (SCHED_FIFO 90) on the encoder cores; a worker now keeps its creator's
  scheduling (SCHED_OTHER, or the opt-in `CAMERA_BOX_JXS_FIFO_PRIO`). Pinned by
  `tests/jxs_wiring_1387.rs` `the_vendored_codec_never_raises_its_workers_to_a_realtime_policy`.
  Re-apply it after every update (step 1 below copies upstream verbatim).
- **Built by** `jxs/sys/build.rs`: static, Release, no LTO, no `-march=native`, outputs in cargo's
  `OUT_DIR` (never in this tree). Needs cmake >= 3.10, a C compiler and nasm >= 2.14 (`ASM_NASM`
  names one that is not on PATH).

## Updating

1. Copy the same five paths from the new upstream commit over these, verbatim, then re-apply the
   local patch above (the pinned test fails until it is back).
2. Update the commit here and in `vendor/README.md`.
3. Run `cargo test -p svt-jpegxs-sys` and `cargo test --features jxs --test jxs_loopback_1387`.
   An API change shows up in `jxs/sys/csrc/jxs_shim.c`, which includes the real headers; the
   Rust side declares no SVT struct.
4. Re-measure on a cambox (`.claude/rules/jxs-link.md`, live acceptance) before the fleet runs it.
