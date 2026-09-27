/* camera-box issue 1357: the CPU pin of the genlock render tick (Linux only).
 *
 * Included ONCE, by obs-video.c, inside its `#if defined(__linux__) && !defined(_WIN32)` block. It keeps
 * static state for the ONE libobs graphics thread, so no other translation unit may include it. The
 * includer must define _GNU_SOURCE before its first libc header (cpu_set_t / CPU_* / the pthread
 * affinity calls are GNU extensions); obs-video.c does.
 *
 * tests/genlock_render_tick_pin_1357.rs lifts the block between the BEGIN and END markers below and
 * compiles it three ways (C-vs-Rust parity against src/genlock_render_tick_pin.rs, a stubbed syscall
 * trace, and real threads). Keep everything between the markers self-contained: it may use only libc,
 * blog() and os_gettime_ns(). */
#pragma once

#if defined(__linux__) && !defined(_WIN32)

#ifndef _GNU_SOURCE
#error "obs-genlock-render-tick-pin.h needs _GNU_SOURCE defined before the first libc header"
#endif

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "util/base.h"
#include "util/platform.h"

/* camera-box issue 1357 render-tick pin BEGIN
 *
 * The genlock render tick (the libobs graphics thread, which drives video_sleep ->
 * genlock_next_deadline) may run on cores the kernel keeps free for it, under a LOW SCHED_FIFO
 * priority, so its wake-ups are not jittered by kernel housekeeping (#484, the analogue of
 * camera-box's src/affinity.rs #289 capture-thread pin).
 *
 * Issue 1357 reworked it after the pin harmed strih-lx twice (23.9 and 27.9.2026):
 * - The pin cores are the kernel's ISOLATED cores that are ALSO nohz_full. Either list empty means
 *   NO pin and one "not pinned: no isolated cores" line. The old hardcoded {10,11} fallback pinned a
 *   box with no isolated core onto two ordinary cores.
 * - The pin is held only while the tick SLEEPS: video_sleep narrows the mask (and raises FIFO) right
 *   before os_sleepto_ns and drops FIFO + restores the saved startup mask right after it. The
 *   graphics thread creates threads while it works (NDI receivers, driver threads) for its whole
 *   life, and a new thread inherits its creator's affinity + policy. Pinned for its whole life, it
 *   leaked cores 10-11 to 40 OBS threads (and, with an rtprio grant, SCHED_FIFO to 28 of them). A
 *   sleeping thread creates nothing, so nothing inherits the pin now. FIFO also carries
 *   SCHED_RESET_ON_FORK, so the kernel resets it for any thread created under it. The flag stays on
 *   the thread once FIFO worked (an unprivileged thread may not clear it), so a child it creates
 *   also starts at nice 0 -- harmless, the tick itself runs at nice 0.
 * - What the pin buys is honest and small: the WAKE-UP happens on a quiet tickless core under FIFO.
 *   The render work after it runs SCHED_OTHER on the shared process mask, like every other OBS
 *   thread. A tick that is already late skips the pin (no sleep, nothing to protect).
 * - It costs four syscalls and a migration per tick, and only on a box that really has isolated
 *   nohz_full cores. The shared OBS-box baseline grader FAILs kernel isolation and no box grants
 *   rtprio, so no current box pins at all: this code only arms on a box built for it on purpose.
 *
 * SAFETY -- the priority is LOW and every failure is WARN-and-CONTINUE. A HIGH-priority runaway FIFO
 * thread in this ~106-thread OBS process can lock out kernel housekeeping and HANG a headless box,
 * so we use a low priority and, on ANY syscall failure, log LOUD and keep running SCHED_OTHER on the
 * process mask. Never abort, never retry-loop, never hang. */
#define GENLOCK_RT_PRIORITY 10 /* LOW FIFO prio: on-time wakeups without starving the kernel */
#ifndef GENLOCK_SYSFS_ISOLATED
#define GENLOCK_SYSFS_ISOLATED "/sys/devices/system/cpu/isolated"
#endif
#ifndef GENLOCK_SYSFS_NOHZ_FULL
#define GENLOCK_SYSFS_NOHZ_FULL "/sys/devices/system/cpu/nohz_full"
#endif

/* Parse a Linux cpulist ("10-11" / "10,11" / "10", trailing newline tolerated) into `set`. */
static inline void genlock_parse_cpulist_into_set(const char *s, cpu_set_t *set)
{
	const char *p = s;
	while (*p) {
		while (*p == ' ' || *p == '\t' || *p == '\n' || *p == ',')
			p++;
		if (*p < '0' || *p > '9')
			break;
		/* Cap digit accumulation at CPU_SETSIZE so a pathological/corrupted /sys read (an
		 * implausibly long digit run) cannot integer-overflow `a`/`b` — once the value is
		 * already out of CPU_SET's range, stop accumulating but keep consuming the digits so
		 * parsing of the rest of the list is not thrown off. */
		int a = 0;
		while (*p >= '0' && *p <= '9') {
			if (a < CPU_SETSIZE)
				a = a * 10 + (*p - '0');
			p++;
		}
		int b = a;
		if (*p == '-') {
			p++;
			b = 0;
			while (*p >= '0' && *p <= '9') {
				if (b < CPU_SETSIZE)
					b = b * 10 + (*p - '0');
				p++;
			}
		}
		for (int c = a; c <= b && c >= 0 && c < CPU_SETSIZE; c++)
			CPU_SET(c, set);
	}
}

/* The pin cores: the isolated cores that are also nohz_full. Empty = do not pin. Pure. */
static inline void genlock_render_tick_pin_set(const char *isolated, const char *nohz_full, cpu_set_t *out)
{
	cpu_set_t iso, nohz;
	CPU_ZERO(&iso);
	CPU_ZERO(&nohz);
	genlock_parse_cpulist_into_set(isolated, &iso);
	genlock_parse_cpulist_into_set(nohz_full, &nohz);
	CPU_ZERO(out);
	CPU_AND(out, &iso, &nohz);
}

/* First line of a sysfs cpulist file, newline stripped; "" when unreadable. */
static inline void genlock_read_cpulist_file(const char *path, char *buf, size_t size)
{
	buf[0] = '\0';
	FILE *f = fopen(path, "r");
	if (!f)
		return;
	if (!fgets(buf, (int)size, f))
		buf[0] = '\0';
	fclose(f);
	buf[strcspn(buf, "\n")] = '\0';
}

/* Graphics-thread-only state, (re)initialised by genlock_pin_render_tick_thread. */
struct genlock_tick_pin {
	bool armed;     /* the pin cores are set and the pin worked at startup */
	bool fifo;      /* SCHED_FIFO worked at startup */
	cpu_set_t pin;  /* isolated AND nohz_full */
	cpu_set_t home; /* the thread's mask at startup = the process mask */
};
static struct genlock_tick_pin genlock_tick_pin;

/* Narrow to the pin cores, then raise FIFO. 0 or an errno. */
static inline int genlock_tick_pin_enter(struct genlock_tick_pin *p)
{
	if (!p->armed)
		return 0;
	int err = pthread_setaffinity_np(pthread_self(), sizeof(p->pin), &p->pin);
	if (err != 0)
		return err;
	if (p->fifo) {
		struct sched_param param;
		memset(&param, 0, sizeof(param));
		param.sched_priority = GENLOCK_RT_PRIORITY;
		if (sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param) != 0)
			return errno;
	}
	return 0;
}

/* Drop FIFO first, then widen back to the saved mask: never FIFO on a shared core. Always tries both.
 * The reset flag stays set (an unprivileged thread may not clear it). 0 or the first errno. */
static inline int genlock_tick_pin_leave(struct genlock_tick_pin *p)
{
	if (!p->armed)
		return 0;
	int err = 0;
	if (p->fifo) {
		struct sched_param param;
		memset(&param, 0, sizeof(param));
		if (sched_setscheduler(0, SCHED_OTHER | SCHED_RESET_ON_FORK, &param) != 0)
			err = errno;
	}
	const int aerr = pthread_setaffinity_np(pthread_self(), sizeof(p->home), &p->home);
	return err != 0 ? err : aerr;
}

/* A per-tick failure: restore what can be restored, stop pinning, say so once. */
static inline void genlock_tick_pin_disarm(int err, const char *where)
{
	struct genlock_tick_pin *p = &genlock_tick_pin;
	if (!p->armed)
		return;
	(void)genlock_tick_pin_leave(p);
	p->armed = false;
	blog(LOG_WARNING,
	     "genlock: render-tick pin %s failed (errno %d) -- pin disabled, continuing SCHED_OTHER on the "
	     "process mask (issue 1357)",
	     where, err);
}

/* video_sleep: right before the tick sleeps until `deadline_ns` (the os_gettime_ns timebase). Pins only
 * when armed and the deadline is still ahead; returns whether it did, for genlock_tick_pin_sleep_end. */
static inline bool genlock_tick_pin_sleep_begin(uint64_t deadline_ns)
{
	if (!genlock_tick_pin.armed || deadline_ns <= os_gettime_ns())
		return false;
	const int err = genlock_tick_pin_enter(&genlock_tick_pin);
	if (err != 0) {
		genlock_tick_pin_disarm(err, "enter");
		return false;
	}
	return true;
}

/* video_sleep: right after the tick woke up, before it works (and may create threads). */
static inline void genlock_tick_pin_sleep_end(bool pinned)
{
	if (!pinned)
		return;
	const int err = genlock_tick_pin_leave(&genlock_tick_pin);
	if (err != 0)
		genlock_tick_pin_disarm(err, "leave");
}

/* obs_graphics_thread, once at startup: decide the pin and trial it. */
static inline void genlock_pin_render_tick_thread(void)
{
	struct genlock_tick_pin *p = &genlock_tick_pin;
	char isolated[256];
	char nohz_full[256];
	memset(p, 0, sizeof(*p));
	genlock_read_cpulist_file(GENLOCK_SYSFS_ISOLATED, isolated, sizeof(isolated));
	genlock_read_cpulist_file(GENLOCK_SYSFS_NOHZ_FULL, nohz_full, sizeof(nohz_full));
	genlock_render_tick_pin_set(isolated, nohz_full, &p->pin);
	if (CPU_COUNT(&p->pin) == 0) {
		blog(LOG_INFO,
		     "genlock: render-tick thread not pinned: no isolated cores (isolated=[%s] nohz_full=[%s]) "
		     "-- it runs SCHED_OTHER on the process mask (issue 1357)",
		     isolated, nohz_full);
		return;
	}

	int err = pthread_getaffinity_np(pthread_self(), sizeof(p->home), &p->home);
	if (err != 0) {
		blog(LOG_WARNING,
		     "genlock: could NOT read the render-tick thread's CPU mask (errno %d) -- not pinned, "
		     "continuing SCHED_OTHER (issue 1357)",
		     err);
		return;
	}
	err = pthread_setaffinity_np(pthread_self(), sizeof(p->pin), &p->pin);
	if (err != 0) {
		blog(LOG_WARNING,
		     "genlock: could NOT pin render-tick thread to the isolated cores (isolated=[%s] nohz_full=[%s], "
		     "errno %d) -- continuing SCHED_OTHER on the process mask (issue 1357)",
		     isolated, nohz_full, err);
		return;
	}

	struct sched_param param;
	memset(&param, 0, sizeof(param));
	param.sched_priority = GENLOCK_RT_PRIORITY;
	if (sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param) != 0) {
		blog(LOG_WARNING,
		     "genlock: could NOT set render-tick thread SCHED_FIFO prio %d (errno %d -- "
		     "no rtprio grant, the baseline keeps it off) -- continuing SCHED_OTHER (#484, issue 1357)",
		     GENLOCK_RT_PRIORITY, errno);
		p->fifo = false;
	} else {
		blog(LOG_INFO,
		     "genlock: render-tick thread set SCHED_FIFO prio %d on the isolated core while it sleeps "
		     "(#484, issue 1357)",
		     GENLOCK_RT_PRIORITY);
		p->fifo = true;
	}

	/* The trial worked: arm it and go back to the process mask until the first sleep. */
	p->armed = true;
	err = genlock_tick_pin_leave(p);
	if (err != 0) {
		genlock_tick_pin_disarm(err, "startup leave");
		return;
	}
	blog(LOG_INFO,
	     "genlock: render-tick thread pinned to %d isolated core(s) (isolated=[%s] nohz_full=[%s]) only "
	     "while it sleeps; threads it creates keep the process mask (issue 1357)",
	     CPU_COUNT(&p->pin), isolated, nohz_full);
}
/* camera-box issue 1357 render-tick pin END */

#endif /* __linux__ && !_WIN32 */
