# Per-source genlock preload as a runtime video-delay control

**Goal:** Let the operator add **video delay** at runtime, per NDI source, via an OBS GUI slider, to compensate for late audio (stream.lan audio is ~1s behind video due to processing). Show the resulting delay in **ms**. Safe to change live (no crash), keeps the delayed frames inside the genlock-disciplined path.

**Approach (chosen):** extend the existing genlock FIFO preload from a global, env-set-at-launch constant into a **per-source, runtime-settable** value with an OBS source-property slider. Each preload frame = one frame of delay in the genlock-disciplined async FIFO, so the delayed output stays genlocked. (Rejected: a separate OBS render-delay filter — its ring buffer is not genlock-tick-driven, so it would add un-genlocked delay.)

## Background (current state)

- `vendor/obs-studio/libobs/obs-source.c`:
  - `genlock_preload_frames()` (~L4138) reads `OBS_GENLOCK_PRELOAD_FRAMES` **once at startup** into a `static int` cache → **global, not per-source, not runtime-changeable**.
  - `genlock_should_consume(depth, preload)` (~L4154) = `depth > preload`; called per-frame in `ready_async_frame()` (~L4197).
  - `GENLOCK_PRELOAD_MAX = 28`, hard cap `MAX_ASYNC_FRAMES = 30` (steady-state queue parks at `preload+1`, must stay `< 30`).
  - Per-source genlock audit counters live in `obs_source_t`; the `genlock-fifo audit` log (~L4162) prints `preload=` every ~5s.
  - The async FIFO (`async_frames`) is a **DARRAY (dynamic, growable), protected by `async_mutex`** — no fixed ring, so growing it is safe.
- `vendor/distroav/src/ndi-source.cpp`: `PROP_GENLOCK_FIFO` bool property (~L330) is the GUI pattern; applied in `ndi_source_update()` via `obs_source_set_genlock_fifo()`. API in `obs.h` (`obs_source_set/get_genlock_fifo`), impl in `obs-source.c`.
- fps via `obs_get_video_info()` → `fps_num`/`fps_den`.

## Design

### 1. Per-source preload field (libobs)
- Add `uint32_t genlock_preload;` to `obs_source_t` (`obs-internal.h`), initialized from the global `OBS_GENLOCK_PRELOAD_FRAMES` env default at source create (back-compat).
- Add API: `obs_source_set_genlock_preload(obs_source_t*, uint32_t)` + `obs_source_get_genlock_preload(const obs_source_t*)` (`obs.h` + `obs-source.c`), clamped to `[0, GENLOCK_PRELOAD_MAX]`.
- `ready_async_frame()` reads `source->genlock_preload` (per-source) **under `async_mutex`** instead of the global static. Set/get also take/respect the lock (per the #93 UAF lesson — no unlocked mutation of a field the A/V thread reads).
- Raise `GENLOCK_PRELOAD_MAX` to **128**.

### 2. Per-source FIFO cap (memory-safe)
- The async drop-cap stays `MAX_ASYNC_FRAMES = 30` for **non-genlock** sources (no memory impact on regular sources).
- For a **genlock_fifo source**, the effective drop-cap = `genlock_preload + RESERVE` (e.g. `+4`), so only a source the operator deliberately delays holds a large buffer. Cap the absolute max at `GENLOCK_PRELOAD_MAX + RESERVE` (= 132).

### 3. GUI (NDI source property)
- In `ndi-source.cpp`, add `obs_properties_add_int_slider(props, PROP_GENLOCK_PRELOAD, "Genlock preload (video delay)", 0, 128, 1)`, shown/active alongside `PROP_GENLOCK_FIFO`.
- A read-only info text property below it shows the live conversion: `≈ <ms> ms (@ <fps> fps)`, recomputed by a `modified_callback` on the slider using `obs_get_video_info()` (`ms = frames * 1000 * fps_den / fps_num`).
- Apply in `ndi_source_update()`: `obs_source_set_genlock_preload(source, obs_data_get_int(settings, PROP_GENLOCK_PRELOAD))`. Persists in the scene (survives restart). Default slider value = the env/global default (1).

### 4. Audit + observability
- Add the ms equivalent to the `genlock-fifo audit` log line (`preload=N (=M ms @ Ffps)`), computed from `obs_get_video_info()`.

### 5. Runtime safety
- FIFO is dynamic + `async_mutex`-protected → **no crash/UAF** on live change. Increasing preload → brief one-time hold while the buffer fills to the new depth (the delay being added); decreasing → brief fast-forward as excess frames drain. These transients are inherent to a delay buffer and acceptable (operator sets ~1s once); "safe" = no crash/corruption, not glitch-free mid-change.

## Testing
- **Unit (Rust + C-testable logic):** preload→ms conversion (frames×1000×den/num); per-source set/get + clamp to [0,128]; `genlock_should_consume` at high preload; the per-source drop-cap (`preload+RESERVE` for genlock, 30 for non-genlock).
- **Source/CI guard:** a test asserting the slider property + the per-source preload API are present (mirror `tests/genlock_preload.rs` / the vendored-source guard convention).
- **Live verification (stream.lan):** set ~1s preload on the strih-PGM ingest on stream OBS → confirm the program video delays ~1s (matches the late audio), the property shows the ms, the audit logs the ms, and OBS does NOT crash. Restore after.

## Out of scope
- Auto-measuring the audio delay (operator sets it manually).
- A dedicated dock or web control (per-source slider chosen; a dock can be a later follow-up).
- 60fps end-to-end (#11, excluded) — the slider is frame-based so it works at whatever the output fps is.

## Deploy
- Vendored OBS change → rebuild via `.github/workflows/windows-genlock.yml`, redeploy to stream (+ strih if wanted), drift-guard. Same flow as #93.
