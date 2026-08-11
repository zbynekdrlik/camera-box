/* camera-box #929 -- ASRC resampling quality A/B measurement harness.
 *
 * Exercises the REAL libswresample engine (system libswresample-dev, the same library family
 * OBS's vendored audio-resampler-ffmpeg.c links) using the EXACT call shape
 * `audio_resampler_create()` uses (vendor/obs-studio/libobs/media-io/audio-resampler-ffmpeg.c
 * ~L124-131: `swr_alloc_set_opts2(..., 0, NULL)`, mono, 48kHz -> 48kHz) and the EXACT formula
 * `audio_resampler_set_compensation_ppm()` uses (~L221-237) to drive `swr_set_compensation()`.
 *
 * The vendored OBS tree (vendor/obs-studio/libobs) has no local compile path on this box
 * (CI-only, see project CLAUDE.md "Local Build Policy") -- this harness lives OUTSIDE the vendor
 * tree and links the
 * distro's own libswresample/libavutil instead, so it is buildable+runnable on dev1 without
 * touching the vendored OBS build. It is NOT part of the Rust `cargo` build (no Cargo integration,
 * no new runtime dep on the shipped binary) -- purely an offline measurement tool, run manually
 * or via run_ab.sh, with its numbers copied into the issue-929 review comment.
 *
 * Two modes:
 *   --mode quality       measure resampled-signal fidelity (THD+N via analyze_thdn.py) + CPU
 *                         cost, for a given engine-options config, at a given compensation ppm.
 *   --mode compensation  measure the ACTUAL achieved ppm (from real input/output sample counts)
 *                         vs the REQUESTED ppm, replicating obs-source.c's per-callback re-issue
 *                         cadence -- proves/disproves whether small requested ppm values actually
 *                         reach the resampler once integer sample-delta rounding is applied.
 *
 * Build: gcc -O2 -o asrc_ab_harness asrc_ab_harness.c $(pkg-config --cflags --libs libswresample libavutil) -lm
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>
#include <libavutil/opt.h>
#include <libavutil/channel_layout.h>
#include <libswresample/swresample.h>

#define SAMPLE_RATE 48000
#define BLOCK_FRAMES 1024 /* AUDIO_OUTPUT_FRAMES, vendor/obs-studio/libobs/media-io/audio-io.h */
#define COMPENSATION_DISTANCE_MS 1000 /* the ONE caller's hardcoded distance_ms, obs-source.c L4194 */

typedef struct {
    double t_ns;
} timer_t_ns;

static double now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts);
    return (double)ts.tv_sec * 1e9 + (double)ts.tv_nsec;
}

/* Mirrors audio_resampler_set_compensation_ppm() EXACTLY (audio-resampler-ffmpeg.c L221-237). */
static void apply_compensation_ppm(struct SwrContext *ctx, double ppm, uint32_t distance_ms, uint32_t output_freq)
{
    const int64_t distance_samples = ((int64_t)output_freq * (int64_t)distance_ms) / 1000;
    if (distance_samples <= 0)
        return;
    const int sample_delta = (int)llround(ppm / 1000000.0 * (double)distance_samples);
    swr_set_compensation(ctx, sample_delta, (int)distance_samples);
}

static struct SwrContext *make_resampler(const char *config)
{
    struct SwrContext *ctx = NULL;
    AVChannelLayout in_ch, out_ch;
    av_channel_layout_default(&in_ch, 1);
    av_channel_layout_default(&out_ch, 1);

    /* Exactly matches audio_resampler_create()'s swr_alloc_set_opts2 call: 0 extra opts. Any
     * "maxq" tuning is then layered on via av_opt_set BEFORE swr_init, which is exactly what a
     * vendor-code change to audio_resampler_create() would do if this measurement warrants it. */
    int rc = swr_alloc_set_opts2(&ctx, &out_ch, AV_SAMPLE_FMT_FLT, SAMPLE_RATE, &in_ch, AV_SAMPLE_FMT_FLT,
                                  SAMPLE_RATE, 0, NULL);
    if (rc < 0 || !ctx) {
        fprintf(stderr, "swr_alloc_set_opts2 failed rc=%d\n", rc);
        exit(1);
    }

    if (strcmp(config, "bypass") == 0) {
        swr_free(&ctx);
        return NULL; /* caller memcpy's instead */
    } else if (strcmp(config, "default") == 0) {
        /* no overrides -- library defaults as OBS ships today */
    } else if (strcmp(config, "maxq_moderate") == 0) {
        av_opt_set_int(ctx, "linear_interp", 1, 0);
        av_opt_set_int(ctx, "exact_rational", 1, 0);
        av_opt_set_int(ctx, "filter_size", 128, 0);
        av_opt_set_int(ctx, "phase_shift", 12, 0); /* 4096 phases */
        av_opt_set_double(ctx, "cutoff", 0.95, 0);
    } else if (strcmp(config, "maxq_extreme") == 0) {
        av_opt_set_int(ctx, "linear_interp", 1, 0);
        av_opt_set_int(ctx, "exact_rational", 1, 0);
        av_opt_set_int(ctx, "filter_size", 512, 0);
        av_opt_set_int(ctx, "phase_shift", 18, 0); /* 262144 phases */
        av_opt_set_double(ctx, "cutoff", 0.99, 0);
    } else {
        fprintf(stderr, "unknown config: %s\n", config);
        exit(1);
    }

    int errcode = swr_init(ctx);
    if (errcode != 0) {
        fprintf(stderr, "swr_init failed errcode=%d\n", errcode);
        exit(1);
    }
    return ctx;
}

static void gen_tone_block(float *buf, int frames, double freq, double amp, double *phase)
{
    const double inc = 2.0 * M_PI * freq / (double)SAMPLE_RATE;
    for (int i = 0; i < frames; i++) {
        buf[i] = (float)(amp * sin(*phase));
        *phase += inc;
        if (*phase > 2.0 * M_PI * 1e6)
            *phase -= 2.0 * M_PI * 1e6; /* keep phase bounded, avoids precision loss on long runs */
    }
}

static int mode_quality(int argc, char **argv)
{
    const char *config = "default";
    double ppm = 0.0;
    double duration_s = 10.0;
    double freq = 997.0; /* AES17 standard THD+N test frequency */
    double amp = 0.891;  /* -1 dBFS */
    const char *out_path = NULL;
    int reissue_every = 1; /* blocks between compensation re-issues; 1 = every callback, matches
                             * obs-source.c's real asrc_process_audio() cadence exactly */

    for (int i = 2; i < argc; i++) {
        if (!strcmp(argv[i], "--config") && i + 1 < argc)
            config = argv[++i];
        else if (!strcmp(argv[i], "--ppm") && i + 1 < argc)
            ppm = atof(argv[++i]);
        else if (!strcmp(argv[i], "--duration") && i + 1 < argc)
            duration_s = atof(argv[++i]);
        else if (!strcmp(argv[i], "--freq") && i + 1 < argc)
            freq = atof(argv[++i]);
        else if (!strcmp(argv[i], "--out") && i + 1 < argc)
            out_path = argv[++i];
        else if (!strcmp(argv[i], "--reissue-every") && i + 1 < argc)
            reissue_every = atoi(argv[++i]);
    }
    if (reissue_every < 1)
        reissue_every = 1;
    if (!out_path) {
        fprintf(stderr, "--out required\n");
        return 1;
    }

    struct SwrContext *ctx = make_resampler(config);
    FILE *fout = fopen(out_path, "wb");
    if (!fout) {
        fprintf(stderr, "cannot open %s\n", out_path);
        return 1;
    }

    long total_in_frames = (long)(duration_s * SAMPLE_RATE);
    long fed = 0;
    double phase = 0.0;
    float inbuf[BLOCK_FRAMES];
    float outbuf[BLOCK_FRAMES * 4]; /* headroom: compensation can grow a block slightly */
    double cpu_ns_total = 0.0;
    long blocks = 0;
    double since_last_comp_ms = 0.0;
    const double block_ms = 1000.0 * BLOCK_FRAMES / SAMPLE_RATE;

    while (fed < total_in_frames) {
        int this_block = (int)((total_in_frames - fed) < BLOCK_FRAMES ? (total_in_frames - fed) : BLOCK_FRAMES);
        gen_tone_block(inbuf, this_block, freq, amp, &phase);

        if (!ctx) {
            /* bypass: no resampler at all */
            fwrite(inbuf, sizeof(float), this_block, fout);
        } else {
            if (ppm != 0.0 && (blocks % reissue_every) == 0) {
                /* obs-source.c calls this on EVERY audio callback (reissue_every=1 is the real
                 * cadence) -- reissue_every>1 is a DIAGNOSTIC to isolate whether distortion comes
                 * from the re-trigger cadence itself vs the resampler engine settings. */
                double t0 = now_ns();
                apply_compensation_ppm(ctx, ppm, COMPENSATION_DISTANCE_MS, SAMPLE_RATE);
                cpu_ns_total += now_ns() - t0;
            }
            const uint8_t *in_planes[1] = {(const uint8_t *)inbuf};
            uint8_t *out_planes[1] = {(uint8_t *)outbuf};
            double t0 = now_ns();
            int got = swr_convert(ctx, out_planes, BLOCK_FRAMES * 4, in_planes, this_block);
            cpu_ns_total += now_ns() - t0;
            if (got < 0) {
                fprintf(stderr, "swr_convert error %d\n", got);
                return 1;
            }
            if (got > 0)
                fwrite(outbuf, sizeof(float), got, fout);
        }
        fed += this_block;
        blocks++;
        since_last_comp_ms += block_ms;
    }

    if (ctx) {
        /* flush any buffered samples */
        uint8_t *out_planes[1] = {(uint8_t *)outbuf};
        int got;
        while ((got = swr_convert(ctx, out_planes, BLOCK_FRAMES * 4, NULL, 0)) > 0)
            fwrite(outbuf, sizeof(float), got, fout);
        swr_free(&ctx);
    }
    fclose(fout);

    fprintf(stdout, "config=%s ppm=%g in_frames=%ld blocks=%ld cpu_ns_total=%.0f cpu_ns_per_block=%.1f\n", config,
            ppm, total_in_frames, blocks, cpu_ns_total, blocks ? cpu_ns_total / blocks : 0.0);
    return 0;
}

static int mode_compensation(int argc, char **argv)
{
    const char *config = "default";
    double ppm = 5.0;
    double duration_s = 20.0;

    for (int i = 2; i < argc; i++) {
        if (!strcmp(argv[i], "--config") && i + 1 < argc)
            config = argv[++i];
        else if (!strcmp(argv[i], "--ppm") && i + 1 < argc)
            ppm = atof(argv[++i]);
        else if (!strcmp(argv[i], "--duration") && i + 1 < argc)
            duration_s = atof(argv[++i]);
    }

    struct SwrContext *ctx = make_resampler(config);
    if (!ctx) {
        fprintf(stderr, "compensation mode needs a real resampler (not bypass)\n");
        return 1;
    }

    long total_in_frames = (long)(duration_s * SAMPLE_RATE);
    long fed = 0, produced = 0;
    double phase = 0.0;
    float inbuf[BLOCK_FRAMES];
    float outbuf[BLOCK_FRAMES * 4];

    while (fed < total_in_frames) {
        int this_block = (int)((total_in_frames - fed) < BLOCK_FRAMES ? (total_in_frames - fed) : BLOCK_FRAMES);
        gen_tone_block(inbuf, this_block, 997.0, 0.5, &phase);

        /* Called every callback, exactly like asrc_process_audio() -> obs-source.c L4194. */
        apply_compensation_ppm(ctx, ppm, COMPENSATION_DISTANCE_MS, SAMPLE_RATE);

        const uint8_t *in_planes[1] = {(const uint8_t *)inbuf};
        uint8_t *out_planes[1] = {(uint8_t *)outbuf};
        int got = swr_convert(ctx, out_planes, BLOCK_FRAMES * 4, in_planes, this_block);
        if (got < 0) {
            fprintf(stderr, "swr_convert error %d\n", got);
            return 1;
        }
        produced += got;
        fed += this_block;
    }
    uint8_t *out_planes[1] = {(uint8_t *)outbuf};
    int got;
    while ((got = swr_convert(ctx, out_planes, BLOCK_FRAMES * 4, NULL, 0)) > 0)
        produced += got;
    swr_free(&ctx);

    double achieved_ppm = ((double)produced - (double)fed) / (double)fed * 1000000.0;
    fprintf(stdout, "config=%s requested_ppm=%g in_frames=%ld out_frames=%ld achieved_ppm=%.4f\n", config, ppm, fed,
            produced, achieved_ppm);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc < 2) {
        fprintf(stderr, "usage: %s --mode quality|compensation [options]\n", argv[0]);
        return 1;
    }
    if (!strcmp(argv[1], "--mode") && argc >= 3) {
        if (!strcmp(argv[2], "quality"))
            return mode_quality(argc, argv);
        if (!strcmp(argv[2], "compensation"))
            return mode_compensation(argc, argv);
    }
    fprintf(stderr, "unknown mode\n");
    return 1;
}
