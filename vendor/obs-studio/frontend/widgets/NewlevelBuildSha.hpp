/* genlock (#152 / #313 / #1018): the deployed-build short-SHA formatter for the OBS
 * window-title build identity, extracted from OBSBasic::UpdateTitleBar()'s
 * NewlevelBuildSha() glue into a pure, OBS/Qt-free function so its behaviour is
 * unit-testable OFF-RIG (the OBS frontend only compiles on the 150-min
 * `windows-genlock.yml`, so a bug here cannot be caught by the normal PR CI — but this
 * header compiles standalone, so tests/obs_titlebar_newlevel_sha_parse.rs exercises it
 * directly in a tiny C++ harness).
 *
 * Why a SHA read, not the compiler `__DATE__` (#1018): OBS builds the frontend with
 * `/Brepro` (reproducible builds, cmake/windows/compilerconfig.cmake), which blanks
 * `__DATE__` to a short placeholder — so the old `newlevel_iso_date(__DATE__)` returned
 * its #313 "unknown" fallback on EVERY production build. And a compile date is the WRONG
 * signal anyway: a FAST obs.dll hot-swap advances the deployed build but never reswaps
 * obs64.exe. `GENLOCK_BUILD_SHA.txt` (written at the install root by every deploy, full
 * or fast) is the authoritative "what build is running", so the glue reads it and this
 * function formats its contents to the short SHA shown in the title.
 *
 * `newlevel_short_sha()` takes the raw file contents, trims to the first whitespace-
 * delimited token, and — only if that token is a plausible hex SHA (>= 7 hex chars) —
 * returns its lowercased short form (first `kNewlevelShortShaLen` chars, the repo's
 * display convention). ANY malformed/empty/non-hex input returns "unknown". A title-bar
 * helper must NEVER be able to crash OBS (#313: an out-of-range substr aborted OBS at
 * startup), so this function must never throw on any input.
 *
 * Pure: depends only on <string> + <cctype> + <algorithm> + <cstddef> — no OBS, no Qt,
 * no filesystem (the file read lives in the OBSBasic.cpp glue). */
#pragma once

#include <algorithm>
#include <cctype>
#include <cstddef>
#include <string>

/* Short-SHA display length. 9 hex chars matches the repo's convention everywhere
 * (autopilot-log, drift-guard notes, and issue #1018's own `be3dacfff`). */
static const std::size_t kNewlevelShortShaLen = 9;

static inline std::string newlevel_short_sha(const std::string &contents)
{
	/* First whitespace-delimited token. All index math below is bounds-safe:
	 * substr(start, len) with start <= size() never throws. */
	std::string::size_type start = 0;
	/* Skip a leading UTF-8 BOM if a BOM-emitting editor ever wrote the marker — its
	 * bytes are non-hex and would otherwise make the whole read fall back to "unknown". */
	if (contents.size() >= 3 && static_cast<unsigned char>(contents[0]) == 0xEF &&
	    static_cast<unsigned char>(contents[1]) == 0xBB &&
	    static_cast<unsigned char>(contents[2]) == 0xBF)
		start = 3;
	while (start < contents.size() &&
	       std::isspace(static_cast<unsigned char>(contents[start])))
		++start;
	std::string::size_type end = start;
	while (end < contents.size() &&
	       !std::isspace(static_cast<unsigned char>(contents[end])))
		++end;
	const std::string token = contents.substr(start, end - start);

	/* A plausible git SHA is at least 7 hex chars (git's default short length). */
	if (token.size() < 7)
		return "unknown";

	std::string lower;
	lower.reserve(token.size());
	for (char c : token) {
		const char lc = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
		const bool isHex = (lc >= '0' && lc <= '9') || (lc >= 'a' && lc <= 'f');
		if (!isHex)
			return "unknown"; /* not a SHA — never show garbage as a build id */
		lower.push_back(lc);
	}

	return lower.substr(0, std::min(kNewlevelShortShaLen, lower.size()));
}
