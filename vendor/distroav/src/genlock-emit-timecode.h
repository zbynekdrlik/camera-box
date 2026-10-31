/* camera-box genlock fork: the ONE genlock emit stamp of every DistroAV sender.
 *
 * Defined in ndi-output.cpp (the NDI program output, the reference the #78 / #1009 gates pin):
 * the OS wall clock (dantesync-disciplined on the broadcast boxes) floored to the 1/fps boundary
 * of its per-second grid, in 100 ns units since the Unix epoch. Issue 1387 slice 3: the JPEG XS
 * program output (jxs-output.cpp) stamps its frames with this same function, so the stream side
 * places them on the grid exactly like `NDI 2ME PGM` -- never a second copy of the stamp.
 */
#pragma once

#include <cstdint>

/* The grid slot of the frame whose render tick is `frame_ts_ns` (libobs' media clock, the
 * `video_data` timestamp) on the per-second grid of `framerate` (rounded), 100 ns since the Unix
 * epoch, never after the floor of now (issue 1367: one frame, one stamp, however late the output
 * callback runs). A frame_ts_ns of 0, ahead of now or over 1 s old stamps the floor of now; a
 * framerate <= 0 stamps now. */
int64_t genlock_emit_timecode_100ns(double framerate, uint64_t frame_ts_ns);
