#pragma once

/*
 * Test-only reference emitter of the cam2 QPSK marker (a port of qpsk_marker.rs::marker_signal at
 * c = 1), shared by camera-box-selftest.cpp and the issue-1381 audio self-test and bench. Never
 * compiled into the plugin: the dock never emits, cam2 does.
 */

#include "../src/camera-box-audio.hpp"

#include <cmath>
#include <cstdint>
#include <vector>

namespace cbtest {

inline uint32_t enc_crc4(uint32_t data, uint32_t size)
{
	data <<= 4;
	uint32_t p = 0x13u << (size - 1);
	long s = (long)size;
	while (s > 0) {
		if (data & (0x8u << s))
			data ^= p;
		s -= 1;
		p >>= 1;
	}
	return data;
}

inline uint32_t enc_payload_word(uint8_t index)
{
	uint32_t data16 = 0xF000u | (uint32_t)index;
	return (data16 << 4) | enc_crc4(data16, 16);
}

inline double sym_wave(uint32_t sym, double phase)
{
	switch (sym) {
	case 0:
		return std::sin(phase);
	case 1:
		return std::cos(phase);
	case 2:
		return -std::cos(phase);
	case 3:
		return -std::sin(phase);
	}
	return 0.0;
}

inline std::vector<float> marker_signal_from_word(uint32_t word)
{
	const uint32_t sr = camerabox::CB_AUDIO_SAMPLE_RATE, f = camerabox::CB_AUDIO_CARRIER_HZ,
		       c = camerabox::CB_AUDIO_C;
	uint32_t sym[10];
	for (uint32_t i = 0; i < 10; i++)
		sym[i] = (word >> (20 - 2 - 2 * i)) & 0x3u;
	size_t n = camerabox::cb_signal_len(sr, f, c);
	std::vector<float> out;
	out.reserve(n);
	const double CONT = 0.25;
	for (uint32_t i = 0; i < n; i++) {
		double phase = (double)i * 2.0 * camerabox::CB_PI * (double)f / (double)sr;
		uint32_t k = (i * f) / (sr * c);
		if (k > 9)
			k = 9;
		double v = sym_wave(sym[k], phase);
		double f_sym = (double)((i * f) % (sr * c)) / (double)sr;
		int prev = k > 0 ? (int)sym[k - 1] : -1;
		int next = k + 1 < 10 ? (int)sym[k + 1] : -1;
		if (f_sym < CONT && (int)sym[k] != prev)
			v *= 0.5 - std::cos(f_sym / CONT * camerabox::CB_PI) * 0.5;
		else if (((double)c - f_sym) < CONT && (int)sym[k] != next)
			v *= 0.5 - std::cos(((double)c - f_sym) / CONT * camerabox::CB_PI) * 0.5;
		out.push_back((float)v);
	}
	return out;
}

inline std::vector<float> marker_signal(uint8_t index)
{
	return marker_signal_from_word(enc_payload_word(index));
}

/* Add `index`'s marker, scaled by `gain`, into `buf` at sample `at` (clipped at the end). */
inline void add_marker(std::vector<float> &buf, size_t at, uint8_t index, float gain)
{
	std::vector<float> m = marker_signal(index);
	for (size_t j = 0; j < m.size() && at + j < buf.size(); j++)
		buf[at + j] += gain * m[j];
}

} // namespace cbtest
