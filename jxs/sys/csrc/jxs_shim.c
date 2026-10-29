/* Issue 1387 -- see jxs_shim.h. */
#include "jxs_shim.h"

#include <SvtJpegxsDec.h>
#include <SvtJpegxsEnc.h>
#include <stdlib.h>
#include <string.h>

struct cbjxs_enc {
    svt_jpeg_xs_encoder_api_t api;
};

struct cbjxs_dec {
    svt_jpeg_xs_decoder_api_t api;
};

/* The library logs to stderr by default; camera-box logs through tracing and owns the journal. */
static void cbjxs_quiet_log(void* context, SvtLogLevel level, const char* tag, const char* fmt, va_list args) {
    (void)context;
    (void)level;
    (void)tag;
    (void)fmt;
    (void)args;
}

static void cbjxs_install_quiet_log(void) {
    /* A no-op after the first call (documented in SvtJpegxs.h), so calling it per open is safe. */
    svt_jpeg_xs_set_log_callback(cbjxs_quiet_log, NULL);
}

uint32_t cbjxs_format_yuv422(void) { return (uint32_t)COLOUR_FORMAT_PLANAR_YUV422; }

int32_t cbjxs_enc_open(const cbjxs_enc_settings* s, cbjxs_enc** out, uint32_t* bytes_per_frame) {
    if (!s || !out || !bytes_per_frame) return (int32_t)SvtJxsErrorBadParameter;
    *out = NULL;
    cbjxs_install_quiet_log();
    cbjxs_enc* e = (cbjxs_enc*)calloc(1, sizeof(*e));
    if (!e) return (int32_t)SvtJxsErrorInsufficientResources;
    SvtJxsErrorType_t err =
        svt_jpeg_xs_encoder_load_default_parameters(SVT_JPEGXS_API_VER_MAJOR, SVT_JPEGXS_API_VER_MINOR, &e->api);
    if (err != SvtJxsErrorNone) {
        free(e);
        return (int32_t)err;
    }
    e->api.source_width = s->width;
    e->api.source_height = s->height;
    e->api.input_bit_depth = 8;
    e->api.colour_format = COLOUR_FORMAT_PLANAR_YUV422;
    e->api.bpp_numerator = s->bpp_numerator;
    e->api.bpp_denominator = s->bpp_denominator;
    e->api.ndecomp_v = s->ndecomp_v;
    e->api.threads_num = s->threads_num;
    e->api.slice_height = s->slice_height;
    e->api.cpu_profile = s->cpu_profile;
    e->api.slice_packetization_mode = s->slice_packets;
    e->api.verbose = VERBOSE_NONE;

    svt_jpeg_xs_image_config_t cfg;
    memset(&cfg, 0, sizeof(cfg));
    uint32_t bpf = 0;
    err = svt_jpeg_xs_encoder_get_image_config(SVT_JPEGXS_API_VER_MAJOR, SVT_JPEGXS_API_VER_MINOR, &e->api, &cfg,
                                               &bpf);
    if (err != SvtJxsErrorNone) {
        free(e);
        return (int32_t)err;
    }
    err = svt_jpeg_xs_encoder_init(SVT_JPEGXS_API_VER_MAJOR, SVT_JPEGXS_API_VER_MINOR, &e->api);
    if (err != SvtJxsErrorNone) {
        free(e);
        return (int32_t)err;
    }
    *bytes_per_frame = bpf;
    *out = e;
    return (int32_t)SvtJxsErrorNone;
}

int32_t cbjxs_enc_send(cbjxs_enc* e, const uint8_t* y, const uint8_t* u, const uint8_t* v, uint32_t stride_y,
                       uint32_t stride_c, uint8_t* bitstream, uint32_t bitstream_size) {
    if (!e || !y || !u || !v || !bitstream) return (int32_t)SvtJxsErrorBadParameter;
    svt_jpeg_xs_frame_t in;
    memset(&in, 0, sizeof(in));
    uint32_t h = e->api.source_height;
    in.image.data_yuv[0] = (void*)y;
    in.image.data_yuv[1] = (void*)u;
    in.image.data_yuv[2] = (void*)v;
    in.image.stride[0] = stride_y;
    in.image.stride[1] = stride_c;
    in.image.stride[2] = stride_c;
    in.image.alloc_size[0] = stride_y * h;
    in.image.alloc_size[1] = stride_c * h;
    in.image.alloc_size[2] = stride_c * h;
    in.bitstream.buffer = bitstream;
    in.bitstream.allocation_size = bitstream_size;
    return (int32_t)svt_jpeg_xs_encoder_send_picture(&e->api, &in, 1);
}

int32_t cbjxs_enc_get_packet(cbjxs_enc* e, const uint8_t** data, uint32_t* len, uint8_t* last) {
    if (!e || !data || !len || !last) return (int32_t)SvtJxsErrorBadParameter;
    svt_jpeg_xs_frame_t o;
    memset(&o, 0, sizeof(o));
    SvtJxsErrorType_t err = svt_jpeg_xs_encoder_get_packet(&e->api, &o, 1);
    *data = o.bitstream.buffer;
    *len = o.bitstream.used_size;
    *last = o.bitstream.last_packet_in_frame;
    return (int32_t)err;
}

void cbjxs_enc_close(cbjxs_enc* e) {
    if (!e) return;
    svt_jpeg_xs_encoder_close(&e->api);
    free(e);
}

int32_t cbjxs_dec_open(const uint8_t* codestream, size_t len, cbjxs_dec** out, uint32_t* width,
                       uint32_t* height, uint32_t* format, uint32_t* bit_depth) {
    if (!codestream || !out || !width || !height || !format || !bit_depth) return (int32_t)SvtJxsErrorBadParameter;
    *out = NULL;
    cbjxs_install_quiet_log();
    cbjxs_dec* d = (cbjxs_dec*)calloc(1, sizeof(*d));
    if (!d) return (int32_t)SvtJxsErrorInsufficientResources;
    d->api.packetization_mode = 1;
    d->api.verbose = VERBOSE_NONE;
    d->api.use_cpu_flags = CPU_FLAGS_ALL;
    d->api.proxy_mode = proxy_mode_full;
    svt_jpeg_xs_image_config_t cfg;
    memset(&cfg, 0, sizeof(cfg));
    SvtJxsErrorType_t err =
        svt_jpeg_xs_decoder_init(SVT_JPEGXS_API_VER_MAJOR, SVT_JPEGXS_API_VER_MINOR, &d->api, codestream, len, &cfg);
    if (err != SvtJxsErrorNone) {
        free(d);
        return (int32_t)err;
    }
    *width = cfg.width;
    *height = cfg.height;
    *format = (uint32_t)cfg.format;
    *bit_depth = cfg.bit_depth;
    *out = d;
    return (int32_t)SvtJxsErrorNone;
}

int32_t cbjxs_dec_send_packet(cbjxs_dec* d, const uint8_t* packet, uint32_t len, uint8_t* y, uint8_t* u, uint8_t* v,
                              uint32_t stride_y, uint32_t stride_c, uint32_t size_y, uint32_t size_c) {
    if (!d || !packet || !y || !u || !v) return (int32_t)SvtJxsErrorBadParameter;
    svt_jpeg_xs_frame_t in;
    memset(&in, 0, sizeof(in));
    in.image.data_yuv[0] = y;
    in.image.data_yuv[1] = u;
    in.image.data_yuv[2] = v;
    in.image.stride[0] = stride_y;
    in.image.stride[1] = stride_c;
    in.image.stride[2] = stride_c;
    in.image.alloc_size[0] = size_y;
    in.image.alloc_size[1] = size_c;
    in.image.alloc_size[2] = size_c;
    in.bitstream.buffer = (uint8_t*)packet;
    in.bitstream.used_size = len;
    in.bitstream.allocation_size = len;
    uint32_t used = 0;
    SvtJxsErrorType_t err = svt_jpeg_xs_decoder_send_packet(&d->api, &in, &used);
    /* "Too short" is the packet-mode answer for a frame that still needs its next packet. */
    if (err == SvtJxsErrorDecoderBitstreamTooShort) return (int32_t)SvtJxsErrorNone;
    return (int32_t)err;
}

int32_t cbjxs_dec_get_frame(cbjxs_dec* d) {
    if (!d) return (int32_t)SvtJxsErrorBadParameter;
    svt_jpeg_xs_frame_t o;
    memset(&o, 0, sizeof(o));
    return (int32_t)svt_jpeg_xs_decoder_get_frame(&d->api, &o, 1);
}

void cbjxs_dec_close(cbjxs_dec* d) {
    if (!d) return;
    svt_jpeg_xs_decoder_close(&d->api);
    free(d);
}
