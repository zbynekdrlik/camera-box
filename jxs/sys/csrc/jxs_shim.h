/*
 * Issue 1387 -- the thin C shim between camera-box and SVT-JPEG-XS.
 *
 * The SVT API passes large public structs by pointer (svt_jpeg_xs_encoder_api_t is ~200 bytes
 * with trailing padding the library uses for its ABI). Re-declaring them in Rust would make the
 * Rust side depend on their exact layout. This shim includes the REAL headers and exposes only
 * scalars, plain pointers and one small opaque handle per codec, so the Rust FFI never sees an
 * SVT struct.
 *
 * Every function returns the SvtJxsErrorType_t value as int32_t (0 = SvtJxsErrorNone).
 */
#ifndef CAMERA_BOX_JXS_SHIM_H
#define CAMERA_BOX_JXS_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct cbjxs_enc cbjxs_enc;
typedef struct cbjxs_dec cbjxs_dec;

/* Encoder settings the camera-box sender chooses (issue 1387 measurements 6489539657). */
typedef struct cbjxs_enc_settings {
    uint32_t width;
    uint32_t height;
    uint32_t bpp_numerator;   /* bits per pixel = numerator / denominator */
    uint32_t bpp_denominator;
    uint32_t ndecomp_v;       /* vertical decomposition depth (2 = the measured setting) */
    uint32_t threads_num;     /* 0 = the library's minimum */
    uint32_t slice_height;    /* luma lines per slice (16 = the library default, set explicitly) */
    uint8_t cpu_profile;      /* 0 = low latency (the measured setting), 1 = low CPU */
    uint8_t slice_packets;    /* 1 = one packet per slice (+ the picture header first) */
    /* Library tuning knobs (issue 1387 encoder optimisation). CBJXS_DEFAULT keeps the value
     * svt_jpeg_xs_encoder_load_default_parameters() set; anything else is written verbatim. */
    uint32_t rate_control_mode;          /* 0..3, see SvtJpegxsEnc.h */
    uint32_t coding_signs_handling;      /* 0 off, 1 fast, 2 full */
    uint32_t coding_significance;        /* 0 / 1 */
    uint32_t coding_vertical_prediction; /* 0..2 */
    uint32_t quantization;               /* 0 deadzone, 1 uniform */
} cbjxs_enc_settings;

/* "Keep the library's default" for a tuning knob. */
#define CBJXS_DEFAULT 0xFFFFFFFFu

/* Open a planar YUV 4:2:2 8-bit encoder. *bytes_per_frame = the bitstream buffer size the
 * encoder requires per frame. */
int32_t cbjxs_enc_open(const cbjxs_enc_settings* s, cbjxs_enc** out, uint32_t* bytes_per_frame);

/* Hand one picture to the encoder (blocking until queued). The planes and the bitstream buffer
 * must stay valid until the packet with *last == 1 of this picture was returned. */
int32_t cbjxs_enc_send(cbjxs_enc* e, const uint8_t* y, const uint8_t* u, const uint8_t* v, uint32_t stride_y,
                       uint32_t stride_c, uint8_t* bitstream, uint32_t bitstream_size);

/* Block until the next packet of the oldest picture is out. *data points into the bitstream
 * buffer given to cbjxs_enc_send; it stays valid until that picture's last packet. */
int32_t cbjxs_enc_get_packet(cbjxs_enc* e, const uint8_t** data, uint32_t* len, uint8_t* last);

void cbjxs_enc_close(cbjxs_enc* e);

/* Open a packet-mode decoder from one whole frame's codestream (the library's init reads the
 * full frame; the header packet alone is too short). Writes the decoded geometry. */
int32_t cbjxs_dec_open(const uint8_t* codestream, size_t len, cbjxs_dec** out, uint32_t* width,
                       uint32_t* height, uint32_t* format, uint32_t* bit_depth);

/* Feed one packet (header or slice) of the frame whose planes are given. Returns
 * SvtJxsErrorDecoderBitstreamTooShort while the frame still needs packets. */
int32_t cbjxs_dec_send_packet(cbjxs_dec* d, const uint8_t* packet, uint32_t len, uint8_t* y, uint8_t* u, uint8_t* v,
                              uint32_t stride_y, uint32_t stride_c, uint32_t size_y, uint32_t size_c);

/* Take a decoded frame. blocking = 0 returns SvtJxsErrorNoErrorEmptyQueue when none is ready.
 * A frame the library gave up on (a corrupt slice) comes out with an error code. */
int32_t cbjxs_dec_get_frame(cbjxs_dec* d, uint8_t blocking);

void cbjxs_dec_close(cbjxs_dec* d);

/* The colour format value of planar YUV 4:2:2 in this library build (for the Rust check). */
uint32_t cbjxs_format_yuv422(void);

#ifdef __cplusplus
}
#endif

#endif
