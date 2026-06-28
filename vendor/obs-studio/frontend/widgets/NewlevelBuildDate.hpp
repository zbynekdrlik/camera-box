/* genlock (#152 / #313): the compiler build-date parse, extracted from
 * OBSBasic::UpdateTitleBar()'s NewlevelBuildDate() helper into a pure, OBS/Qt-free
 * function so its behaviour is unit-testable OFF-RIG. The OBS frontend only compiles on
 * the 150-min `windows-genlock.yml` build, so a parse bug here (e.g. #313, where a short
 * `__DATE__` threw `std::_Xran` deep in OBSBasic construction and ABORTED OBS at startup)
 * cannot be caught by the normal PR CI — but this header compiles standalone, so a tiny
 * C++ harness (tests/obs_titlebar_newlevel_parse.rs) can exercise it directly.
 *
 * `newlevel_iso_date()` reformats a compiler `__DATE__` string ("Mmm DD YYYY", the day
 * space-padded when < 10) to ISO "YYYY-MM-DD". A title-bar helper must NEVER be able to
 * crash OBS, so this function must never throw on a malformed/short input.
 *
 * Pure: depends only on <string> + <sstream> — the same dependency NewlevelBuildDate()
 * already had inline, so no new transitive footprint on the Windows frontend build. */
#pragma once

#include <sstream>
#include <string>

static inline std::string newlevel_iso_date(const std::string &compileDate)
{
	/* #313: never index/`substr` out of range — a short/empty/malformed date threw
	 * std::out_of_range (MSVC std::_Xran "invalid string position") out of
	 * UpdateTitleBar() during OBSBasic construction and ABORTED OBS at startup. A
	 * well-formed `__DATE__` is exactly the 11-char "Mmm DD YYYY"; anything shorter
	 * cannot be parsed, so return a safe fallback BEFORE any d[..] / d.substr(7, 4). */
	if (compileDate.size() < 11)
		return "unknown";

	static const std::string months = "JanFebMarAprMayJunJulAugSepOctNovDec";
	const std::string &d = compileDate;
	const std::string::size_type mpos = months.find(d.substr(0, 3));
	const int month = mpos == std::string::npos ? 0 : int(mpos / 3) + 1;
	const char dayTens = d[4]; /* ' ' when the day is a single digit */
	const char dayOnes = d[5];
	std::ostringstream iso;
	iso << d.substr(7, 4) << "-" << (month < 10 ? "0" : "") << month << "-"
	    << (dayTens == ' ' ? '0' : dayTens) << dayOnes;
	return iso.str();
}
