/* camera-box issue 1387 -- the process-wide JPEG XS state table (design 6506884476).

	The JPEG XS source (jxs-source.cpp) and the program output (jxs-output.cpp) publish one row
	each: their name, a detail (the port / the target), the time of their last frame and the
	counters of their last 5 s status window. The frontend's JPEG XS indicator reads the table once
	a second through the libobs proc handler `camera_box_jxs_state` (registered in jxs-source.cpp),
	which returns json(now). Nothing else reads or writes it.

	C++17, header-only, std only (no OBS call): tests/genlock_jxs_indicator_1387.rs compiles THIS
	file into a driver and runs it under `cargo test`. Every call takes the one mutex for a few
	field writes; json() builds the whole snapshot under it, so a reader never sees half a row. */
#pragma once

#include <algorithm>
#include <cinttypes>
#include <cstdint>
#include <cstdio>
#include <map>
#include <mutex>
#include <string>
#include <vector>

/* The row kinds (the frontend's GENLOCK_JXS_KIND_INPUT / _OUTPUT). */
#define CB_JXS_STATE_INPUT 0
#define CB_JXS_STATE_OUTPUT 1
/* The JSON's schema version (the frontend reads "v"). */
#define CB_JXS_STATE_VERSION 1

struct cb_jxs_state_row {
	int kind = CB_JXS_STATE_INPUT;
	std::string name;
	std::string detail;
	uint64_t last_frame_ns = 0; /* os_gettime_ns() of the last frame, 0 = none yet */
	bool window = false;        /* a status window has been published */
	uint32_t fps_x100 = 0;
	uint32_t bpp_x100 = 0;
	uint64_t lost = 0;
	uint64_t undecodable = 0;
	uint64_t dropped = 0;
	uint64_t errors = 0;
	int64_t report_age_ms = -1; /* the output's last receiver report, -1 = never */
};

/* The length of the well-formed UTF-8 sequence at s[i] (RFC 3629: no overlong form, no surrogate,
 * nothing above U+10FFFF), 0 = not one. */
static inline size_t cb_jxs_state_utf8_len(const std::string &s, size_t i)
{
	const unsigned char c = (unsigned char)s[i];
	size_t n = 0;
	unsigned char lo = 0x80, hi = 0xBF; /* the bounds of the second byte */
	if (c >= 0xC2 && c <= 0xDF) {
		n = 2;
	} else if (c >= 0xE0 && c <= 0xEF) {
		n = 3;
		lo = c == 0xE0 ? 0xA0 : 0x80;
		hi = c == 0xED ? 0x9F : 0xBF;
	} else if (c >= 0xF0 && c <= 0xF4) {
		n = 4;
		lo = c == 0xF0 ? 0x90 : 0x80;
		hi = c == 0xF4 ? 0x8F : 0xBF;
	} else {
		return 0;
	}
	if (i + n > s.size())
		return 0;
	for (size_t k = 1; k < n; k++) {
		const unsigned char b = (unsigned char)s[i + k];
		if (b < (k == 1 ? lo : 0x80) || b > (k == 1 ? hi : 0xBF))
			return 0;
	}
	return n;
}

/* A JSON string body: quote, backslash and control characters escaped (source names are free
 * text); well-formed UTF-8 passes through, every byte that is not part of one becomes '?', so the
 * snapshot is always JSON the frontend's parser (jansson, strict UTF-8) takes. */
static inline void cb_jxs_state_json_escape(std::string &out, const std::string &s)
{
	for (size_t i = 0; i < s.size();) {
		const unsigned char c = (unsigned char)s[i];
		if (c >= 0x80) {
			const size_t n = cb_jxs_state_utf8_len(s, i);
			if (n == 0) {
				out += '?';
				i++;
			} else {
				out.append(s, i, n);
				i += n;
			}
			continue;
		}
		if (c == '"' || c == '\\') {
			out += '\\';
			out += (char)c;
		} else if (c < 0x20) {
			char buf[8];
			snprintf(buf, sizeof buf, "\\u%04x", (unsigned)c);
			out += buf;
		} else {
			out += (char)c;
		}
		i++;
	}
}

/* A counter as JSON reads it: jansson holds integers as int64, so anything above INT64_MAX (a
 * window delta that wrapped) is clamped, never printed as a number the parser rejects. */
static inline uint64_t cb_jxs_state_json_u64(uint64_t v)
{
	return v > (uint64_t)INT64_MAX ? (uint64_t)INT64_MAX : v;
}

class cb_jxs_state_table {
public:
	/* Create or rename a row (the key is the publishing object's own pointer). */
	void set_entry(const void *key, int kind, const std::string &name, const std::string &detail)
	{
		std::lock_guard<std::mutex> lock(mutex_);
		cb_jxs_state_row &r = rows_[key];
		r.kind = kind;
		r.name = name;
		r.detail = detail;
	}

	/* A frame went through (the source handed it to libobs, the sender took it). */
	void note_frame(const void *key, uint64_t now_ns)
	{
		std::lock_guard<std::mutex> lock(mutex_);
		auto it = rows_.find(key);
		if (it != rows_.end())
			it->second.last_frame_ns = now_ns;
	}

	/* The counters of the status window that just ended. */
	void publish_window(const void *key, uint32_t fps_x100, uint32_t bpp_x100, uint64_t lost, uint64_t undecodable,
			    uint64_t dropped, uint64_t errors, int64_t report_age_ms)
	{
		std::lock_guard<std::mutex> lock(mutex_);
		auto it = rows_.find(key);
		if (it == rows_.end())
			return;
		cb_jxs_state_row &r = it->second;
		r.window = true;
		r.fps_x100 = fps_x100;
		r.bpp_x100 = bpp_x100;
		r.lost = lost;
		r.undecodable = undecodable;
		r.dropped = dropped;
		r.errors = errors;
		r.report_age_ms = report_age_ms;
	}

	/* The publisher is gone (the source destroyed, the output stopped). */
	void remove(const void *key)
	{
		std::lock_guard<std::mutex> lock(mutex_);
		rows_.erase(key);
	}

	/* The snapshot: rows sorted by kind, then name; age_ms -1 for a row with no frame yet. */
	std::string json(uint64_t now_ns) const
	{
		std::vector<cb_jxs_state_row> rows;
		{
			std::lock_guard<std::mutex> lock(mutex_);
			for (const auto &kv : rows_)
				rows.push_back(kv.second);
		}
		std::sort(rows.begin(), rows.end(), [](const cb_jxs_state_row &a, const cb_jxs_state_row &b) {
			return a.kind != b.kind ? a.kind < b.kind : a.name < b.name;
		});
		std::string j = "{\"v\":" + std::to_string(CB_JXS_STATE_VERSION) + ",\"entries\":[";
		bool first = true;
		for (const cb_jxs_state_row &r : rows) {
			const int64_t age_ms =
				r.last_frame_ns == 0 ? -1
						     : (now_ns > r.last_frame_ns ? (int64_t)((now_ns - r.last_frame_ns) / 1000000u)
										 : 0);
			char nums[512];
			snprintf(nums, sizeof nums,
				 ",\"age_ms\":%" PRId64 ",\"window\":%s,\"fps_x100\":%u,\"bpp_x100\":%u,\"lost\":%" PRIu64
				 ",\"undecodable\":%" PRIu64 ",\"dropped\":%" PRIu64 ",\"errors\":%" PRIu64
				 ",\"report_age_ms\":%" PRId64 "}",
				 age_ms, r.window ? "true" : "false", (unsigned)r.fps_x100, (unsigned)r.bpp_x100,
				 cb_jxs_state_json_u64(r.lost), cb_jxs_state_json_u64(r.undecodable),
				 cb_jxs_state_json_u64(r.dropped), cb_jxs_state_json_u64(r.errors), r.report_age_ms);
			if (!first)
				j += ',';
			first = false;
			j += "{\"kind\":";
			j += r.kind == CB_JXS_STATE_OUTPUT ? "\"output\"" : "\"input\"";
			j += ",\"name\":\"";
			cb_jxs_state_json_escape(j, r.name);
			j += "\",\"detail\":\"";
			cb_jxs_state_json_escape(j, r.detail);
			j += '"';
			j += nums;
		}
		j += "]}";
		return j;
	}

	size_t size() const
	{
		std::lock_guard<std::mutex> lock(mutex_);
		return rows_.size();
	}

private:
	mutable std::mutex mutex_;
	std::map<const void *, cb_jxs_state_row> rows_;
};

/* The one table of the process: a function-local static of an `inline` (NOT `static inline`)
 * function, so jxs-source.cpp and jxs-output.cpp share ONE instance (vague linkage, merged in the
 * plugin); `static` would give each translation unit its own table. Created on first use,
 * thread-safe. */
inline cb_jxs_state_table &cb_jxs_state()
{
	static cb_jxs_state_table table;
	return table;
}
