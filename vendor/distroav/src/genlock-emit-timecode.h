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

/* The frame boundary at or before now on the per-second grid of `framerate` (rounded), 100 ns
 * since the Unix epoch. A framerate <= 0 stamps now. */
int64_t genlock_emit_timecode_100ns(double framerate);
