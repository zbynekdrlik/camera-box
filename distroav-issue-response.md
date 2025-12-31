# DistroAV Issue #1386 - Root Cause Analysis and Fix

## Summary

I've identified the root cause of the "Network" sync mode failing when OBS starts before NDI sources. The issue is a **time domain mismatch** between NDI timestamps and OBS's internal frame buffering system.

## Root Cause

### How OBS Async Frame Buffering Works

In `libobs/obs-source.c`, the `ready_async_frame()` function manages frame timing:

```c
frame_offset = frame_time - source->last_frame_ts;
source->last_frame_ts += sys_offset;  // sys_offset = actual wall clock delta
```

And in `get_closest_frame()`:
```c
if (!source->last_frame_ts)
    source->last_frame_ts = frame->timestamp;
```

### The Problem

1. **First frame arrives** → OBS sets `last_frame_ts = ndi_frame->timestamp * 100`
   - This timestamp is from the **sender's clock** (NTP-synchronized in our case)
   - Could be any large value like `1735567200000000000` (NTP nanoseconds)

2. **Second frame arrives** → OBS calls `ready_async_frame()`
   - `sys_offset` = actual wall clock time that passed (e.g., 16.6ms = 16,600,000 ns)
   - OBS does: `last_frame_ts += sys_offset`
   - **This mixes two time domains**: NDI/NTP timestamps + OBS wall clock

3. **Result**: `last_frame_ts` drifts incorrectly, causing:
   - `frame_out_of_bounds()` triggers repeatedly
   - Frames are skipped or displayed at wrong times
   - The "timestamp jumped by X" warnings in logs

### Why Restart Fixes It

When you restart OBS with NDI sources already running:
- The first frame's timestamp and OBS's system time are more closely aligned
- The `sys_offset` additions don't create as large a drift
- But this is just luck - the fundamental bug remains

### Why "Lowest (Unbuffered)" Works

Line 1043 in ndi-source.cpp:
```cpp
const bool is_unbuffered = (s->config.latency == PROP_LATENCY_LOWEST);
obs_source_set_async_unbuffered(obs_source, is_unbuffered);
```

Unbuffered mode **bypasses OBS's timestamp-based buffering entirely**, avoiding the bug.

## The Fix

The DistroAV plugin should **translate NDI timestamps to OBS time domain** before passing them to `obs_source_output_video()`:

```cpp
// On first frame:
base_obs_time = os_gettime_ns();
base_ndi_time = ndi_frame->timestamp;

// For all subsequent frames:
obs_timestamp = base_obs_time + (ndi_frame->timestamp - base_ndi_time) * 100;
```

This:
- Preserves **relative timing** between frames (important for smooth playback)
- Uses timestamps that **align with OBS's expectations**
- Works regardless of OBS/NDI startup order

## Proof of Concept Patch

I've created a patch that implements this fix. The key changes:

1. Add `ndi_timestamp_sync_t` structure to track baseline times
2. Add `translate_ndi_to_obs_time()` function for translation
3. Reset sync state when receiver is reset
4. Update `ndi_source_thread_process_video2()` and `ndi_source_thread_process_audio3()` to use translation

## Testing Notes

Our environment:
- Multiple NDI cameras with NTP-synchronized clocks (via Dante sync)
- OBS frequently started before camera boxes boot
- "Network" sync mode required for multi-camera synchronization

Before fix: Frame drops at startup, requires OBS restart or latency mode toggle
After fix: Clean startup every time, frames displayed by network timestamp

## Alternative OBS-side Fix

The issue could also be fixed in OBS itself by modifying `ready_async_frame()` to handle the first-frame case properly:

```c
if (!source->timestamp_base_set) {
    source->timestamp_base_set = true;
    source->frame_time_base = frame_time;
    source->sys_time_base = sys_time;
}
// Use relative offset instead of raw timestamps
```

But fixing it in DistroAV is more practical and can ship faster.

---

Let me know if you'd like me to submit a PR with the complete implementation.
