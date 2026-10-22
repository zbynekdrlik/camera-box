/*
 * camera-box issue 1439 -- one sample of a VBAN packet in the obs-vban receive source, read and written in
 * the packet's own libobs sample format (ROZHODNUTE 6352253147 item (b)): a concealed slot repeats the
 * previous packet in ITS format (libobs resets the source's resampler, and with it the genlock ASRC, at every
 * format change), so the concealment math (vban-conceal.h, on doubles) reads and writes through these.
 *
 * The formats are the ones the receive callback hands over: 8-bit unsigned, 16-bit and 32-bit signed and
 * 32-bit float interleaved, and float planar (the 24-bit packets, converted). The scale is [-1, 1): an integer
 * sample is divided by its full scale; a written one is rounded half away from zero (vban_recv_round, no
 * libm) and clamped to the format's range. Included by vban-source.c after <obs-module.h> and
 * vban-receive-timeline.h.
 */

#pragma once

#include <stdint.h>
#include <string.h>

/* camera-box issue 1439 (ROZHODNUTE 6352253147 b): sample I of channel CH of AUDIO on the [-1, 1) scale, in
 * the packet's own format (interleaved in data[0], planar in data[CH]). */
static inline double vban_src_sample_get(const struct obs_source_audio *audio, size_t ch, size_t i)
{
	const size_t channels = (size_t)audio->speakers;
	switch (audio->format) {
	case AUDIO_FORMAT_U8BIT:
		return ((double)audio->data[0][i * channels + ch] - 128.0) / 128.0;
	case AUDIO_FORMAT_16BIT: {
		int16_t v;
		memcpy(&v, audio->data[0] + (i * channels + ch) * sizeof(v), sizeof(v));
		return (double)v / 32768.0;
	}
	case AUDIO_FORMAT_32BIT: {
		int32_t v;
		memcpy(&v, audio->data[0] + (i * channels + ch) * sizeof(v), sizeof(v));
		return (double)v / 2147483648.0;
	}
	case AUDIO_FORMAT_FLOAT: {
		float v;
		memcpy(&v, audio->data[0] + (i * channels + ch) * sizeof(v), sizeof(v));
		return (double)v;
	}
	case AUDIO_FORMAT_FLOAT_PLANAR: {
		float v;
		memcpy(&v, audio->data[ch] + i * sizeof(v), sizeof(v));
		return (double)v;
	}
	default:
		return 0.0;
	}
}

/* camera-box issue 1439 (ROZHODNUTE 6352253147 b): V (the [-1, 1) scale) as sample I of channel CH of the
 * planes at BUF (PLANE_BYTES each) in FORMAT, rounded and clamped for the integer formats. */
static inline void vban_src_sample_put(enum audio_format format, size_t channels, uint8_t *buf,
				       size_t plane_bytes, size_t ch, size_t i, double v)
{
	switch (format) {
	case AUDIO_FORMAT_U8BIT: {
		const int64_t q = vban_recv_round(v * 128.0 + 128.0);
		buf[i * channels + ch] = (uint8_t)(q < 0 ? 0 : q > 255 ? 255 : q);
		break;
	}
	case AUDIO_FORMAT_16BIT: {
		const int64_t q = vban_recv_round(v * 32768.0);
		const int16_t x = (int16_t)(q < INT16_MIN ? INT16_MIN : q > INT16_MAX ? INT16_MAX : q);
		memcpy(buf + (i * channels + ch) * sizeof(x), &x, sizeof(x));
		break;
	}
	case AUDIO_FORMAT_32BIT: {
		const int64_t q = vban_recv_round(v * 2147483648.0);
		const int32_t x = (int32_t)(q < INT32_MIN ? INT32_MIN : q > INT32_MAX ? INT32_MAX : q);
		memcpy(buf + (i * channels + ch) * sizeof(x), &x, sizeof(x));
		break;
	}
	case AUDIO_FORMAT_FLOAT: {
		const float x = (float)v;
		memcpy(buf + (i * channels + ch) * sizeof(x), &x, sizeof(x));
		break;
	}
	case AUDIO_FORMAT_FLOAT_PLANAR: {
		const float x = (float)v;
		memcpy(buf + ch * plane_bytes + i * sizeof(x), &x, sizeof(x));
		break;
	}
	default:
		break;
	}
}
