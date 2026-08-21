use std::hash::Hasher;

// ── (#889) content-dupe detection ─────────────────────────────────────────────

/// How many rows of a captured YUYV422 frame [`dupe_content_hash`] samples — a FEW rows, not
/// the whole frame (cheap, mirrors [`crate::capture::mean_chroma`]'s row-sampling cost
/// discipline), spread evenly across the frame height. Validated on the rig (#889): a fast
/// grabber's internal buffer repeat reproduces the frame byte-for-byte (sampled rows included);
/// real camera sensor noise + painter motion makes every non-dupe frame's content differ even
/// in a small sampled subset, so byte-exact equality over these rows alone is a reliable
/// "same vs different" test.
const DUPE_HASH_SAMPLE_ROWS: usize = 8;

/// Cheap content fingerprint for grabber-dupe detection. Samples up to
/// [`DUPE_HASH_SAMPLE_ROWS`] rows evenly spaced across the frame height, honoring `stride` (the
/// V4L2 mmap buffer is `stride * height`, NOT `width * 2 * height` — the same gotcha
/// [`crate::capture::mean_chroma`] guards) so a row-padded device never hashes padding bytes.
/// FNV-1a: collision RESISTANCE is not the goal here (only "same vs different" on real
/// hardware, never adversarial safety), just a fast, deterministic, well-distributed fold. A
/// degenerate (zero width/height/stride) frame hashes to 0 — harmless: a zero-size frame never
/// reaches the NDI send path, so two degenerate frames comparing "equal" has no observable
/// effect.
pub fn dupe_content_hash(frame: &[u8], width: usize, height: usize, stride: usize) -> u64 {
    dupe_content_sig(frame, width, height, stride).0
}

/// (#1145 round 3) Pixel stride (in whole PIXELS) between adjacent luma (Y) samples along each of
/// the [`DUPE_HASH_SAMPLE_ROWS`] sampled rows for the noise-tolerant signature lattice. `8` → for a
/// 1920-wide frame, 8 rows × (1920/8) = 1920 lattice points; small enough that a painted QR/burn
/// flip lands decisively on tens of points, sparse enough to stay cheap at 60 fps 1080p (a few
/// thousand byte reads/frame, no allocation beyond the small `Vec`).
pub const DUPE_SIG_PIXEL_STRIDE: usize = 8;

/// (#1145 round 3) The per-point ABSOLUTE luma-diff (after median-offset compensation) at/above
/// which a sampled lattice point counts as CHANGED between two frames. `48` sits deliberately in
/// the wide gap between two physically-separated magnitudes: per-point optical sensor NOISE on the
/// rig path is σ≈2–8 luma (48 is ≥5σ above it, so a genuine noisy re-sample crosses it essentially
/// never), while a painted QR/burn MODULE flip swings ≈100–180 luma even after optical contrast
/// loss (48 ≤ half that swing, so a genuinely-different painted frame crosses it decisively).
/// Calibration value (the ≥5σ / ≤½-swing margins are order-of-magnitude, not tuned); the live E2E
/// re-measure (uniformity ≥0.95 AND clean QR-contiguity) validates it. See [`frames_are_content_dupes`].
pub const NOISY_DUPE_DIFF_THETA: i32 = 48;

/// (#1145 round 3) The maximum number of CHANGED lattice points ([`NOISY_DUPE_DIFF_THETA`]) for two
/// frames to be classified a NOISY content-dupe. `6` (~0.3 % of a ~1920-point lattice) is a small
/// slack for a handful of hot/outlier pixels while staying FAR below the tens of points a real QR
/// flip moves — so the false-POSITIVE direction (calling a genuinely-different painted frame a dupe,
/// which would DROP a real unique) needs a QR flip that somehow touches ≤6 sampled points, which the
/// [`DUPE_SIG_PIXEL_STRIDE`] density makes impossible for the rig's burn geometry. Calibration value;
/// biased hard to false-NEGATIVE (a miss just falls back to today's heuristic — status quo).
pub const NOISY_DUPE_MAX_CHANGED: usize = 6;

/// (#1145 round 3) Single-pass content signature: the exact FNV fingerprint (BYTE-identical to
/// [`dupe_content_hash`]'s historical value, so a buffer-repeat dupe like CAM1's still short-circuits
/// on it) PLUS a luma (Y) lattice for the NOISE-TOLERANT compare a marginal jittery over-rate card
/// (CAM2) needs — its surplus is a noisy optical RE-SAMPLE of the same painted frame, NOT a
/// byte-identical repeat, so exact equality misses it (#1145 root cause). Samples the SAME
/// [`DUPE_HASH_SAMPLE_ROWS`] evenly-spaced rows the hash reads (stride-honoring — never padding
/// bytes), taking the Y byte (even offset in YUYV422) every [`DUPE_SIG_PIXEL_STRIDE`] pixels into
/// the lattice. A degenerate (zero width/height/stride) frame → `(0, empty)`; an empty lattice
/// compares not-dupe in [`frames_are_content_dupes`] (fail-safe).
pub fn dupe_content_sig(
    frame: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> (u64, Vec<u8>) {
    let row_bytes = width * 2; // YUYV422: 2 bytes/pixel
    if height == 0 || row_bytes == 0 || stride == 0 {
        return (0, Vec::new());
    }
    let mut hasher = FnvHasher::new();
    let mut luma: Vec<u8> = Vec::new();
    let step = (height / DUPE_HASH_SAMPLE_ROWS).max(1);
    let mut y = 0usize;
    while y < height {
        let row_start = y * stride;
        let row_end = row_start + row_bytes;
        // Exact fingerprint: hash the real pixel bytes of this row (clamped to the buffer), IDENTICAL
        // to the historical `dupe_content_hash` byte-for-byte (verified by its retained tests).
        if row_end <= frame.len() {
            hasher.write(&frame[row_start..row_end]);
        } else if row_start < frame.len() {
            hasher.write(&frame[row_start..]);
        }
        // Luma lattice: the Y byte (even offset) every DUPE_SIG_PIXEL_STRIDE pixels along this row.
        let mut x = 0usize;
        while x < width {
            let px = row_start + x * 2;
            if px < frame.len() {
                luma.push(frame[px]);
            }
            x += DUPE_SIG_PIXEL_STRIDE;
        }
        y += step;
    }
    (hasher.finish(), luma)
}

/// (#1145 round 3) NOISE-TOLERANT content-dupe test over two luma lattices from [`dupe_content_sig`]
/// (same length). Two captures of the SAME painted frame differ only by per-point sensor NOISE (a
/// handful of points, if any, cross [`NOISY_DUPE_DIFF_THETA`]); two DIFFERENT painted frames differ
/// in the burn/QR region (many points cross it). So: `is_dupe = changed_count ≤ `[`NOISY_DUPE_MAX_CHANGED`].
///
/// The per-point diff is compensated by the MEDIAN of all diffs first — a calibration-free global
/// exposure / display-PWM-backlight-beat offset (a same-frame re-capture can be uniformly a few luma
/// brighter/darker). The median is robust to the QR outliers (they are a minority, so a real flip
/// still stands out AFTER the subtraction — a bidirectional flip keeps the median near 0). Mismatched
/// or empty lattices → NOT a dupe (fail-safe: the caller then keeps today's exact-hash behavior).
///
/// Asymmetric by design: a false-NEGATIVE (missing a noisy dupe) only reverts to the pre-existing
/// heuristic shed (status quo); a false-POSITIVE would DROP a genuine unique (a real gap), so both
/// thresholds are set well inside the physical margin and the caller arms this ONLY under sustained
/// over-rate and never on two consecutive frames.
pub fn frames_are_content_dupes(prev: &[u8], now: &[u8]) -> bool {
    if prev.is_empty() || prev.len() != now.len() {
        return false;
    }
    let mut diffs: Vec<i32> = now
        .iter()
        .zip(prev.iter())
        .map(|(&a, &b)| a as i32 - b as i32)
        .collect();
    diffs.sort_unstable();
    let median = diffs[diffs.len() / 2];
    let changed = diffs
        .iter()
        .filter(|&&d| (d - median).abs() >= NOISY_DUPE_DIFF_THETA)
        .count();
    changed <= NOISY_DUPE_MAX_CHANGED
}

/// Minimal FNV-1a (64-bit) — no extra crate dependency for a "same vs different" fingerprint.
struct FnvHasher(u64);

impl FnvHasher {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}
