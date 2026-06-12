# Building the vendored AV stack

Proven build (dev1, Ubuntu 24.04, 2026-06-12): vendored OBS 32.1.2 frontend binary +
DistroAV 6.2.1 `distroav.so` against it, exit 0.

## Linux prototype build (dev1)

```bash
# one-time deps beyond the usual OBS set (Ubuntu 24.04):
sudo apt-get install -y libwebsocketpp-dev libasio-dev nlohmann-json3-dev

# 1. OBS (genlock prototype config — hardware encoders + browser OFF, websocket ON)
cmake -S vendor/obs-studio -B /tmp/obs-vendor-build -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DENABLE_AJA=OFF -DENABLE_BROWSER=OFF -DENABLE_VLC=OFF -DENABLE_VST=OFF \
  -DENABLE_DECKLINK=OFF -DENABLE_TEST_INPUT=OFF -DENABLE_WEBSOCKET=ON \
  -DENABLE_NVENC=OFF -DENABLE_QSV11=OFF -DENABLE_WEBRTC=OFF
cmake --build /tmp/obs-vendor-build -j$(($(nproc)-2))          # → frontend/obs
cmake --install /tmp/obs-vendor-build --prefix /tmp/obs-vendor-install

# 2. DistroAV against the vendored OBS
cmake -S vendor/distroav -B /tmp/distroav-build -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DCMAKE_PREFIX_PATH=/tmp/obs-vendor-install
cmake --build /tmp/distroav-build -j$(($(nproc)-2))            # → distroav.so
```

Build dirs stay in /tmp — never inside the repo. NDI runtime (`libndi.so.6`, **≥ 6.3.0**
for this DistroAV) must be present at runtime (`/usr/lib/ndi`); headers come from
`vendor/distroav/lib/ndi/` at compile time, the runtime is licensed per-machine.

## Windows production build (strih/stream) — required before any deploy (#42 B-phase)

Targets the boxes' real usage, so unlike the prototype: `ENABLE_BROWSER=ON` (CEF via
obs-deps), hardware encoders ON (NVENC), and the **update dialog disabled** (#43).
Uses upstream's obs-deps prebuilt dependency bundle + Visual Studio toolchain — build
on a Windows machine or CI runner, NOT cross-compiled. Exact recipe lands with #43
(first Windows build of our patched tree); deploy only per `deploy-from-clean-tree`
with the user, off-air.

## Notes

- The 12 disabled-feature flags are the PROTOTYPE set. Disabling them does not change
  libobs core (where the genlock work lives — `ready_async_frame`/render tick), so
  prototype findings transfer to the production build.
- rnnoise: cmake warns, then uses OBS's internal copy — fine.
- Plugin ABI: DistroAV built against 32.1.2 libobs must ship together with that exact
  OBS — never mix with a stock OBS install (#45 drift guard enforces this).
