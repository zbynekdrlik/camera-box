/*
 * camera-box issue 1439 -- the lift-and-run driver of the SHIPPED vendor/obs-vban/src/vban-source.c,
 * used by tests/vban_source_loss_1439.rs (which replaces @SOURCE@ with the source's absolute path and
 * compiles it under -Wall -Wextra -Werror). It stubs every libobs / UDP call the source makes and
 * replays the packets and setting changes named on its command line. Moved out of the Rust test in
 * review round 4 (the test file was near its line budget).
 */
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
#include <pthread.h>
#include <time.h>
#include <errno.h>
#include <stdbool.h>
/* round 5 (ROZHODNUTE 6347872706): the source's playout worker. By default the driver runs its step on
 * simulated time (deterministic); `thread` runs the real thread on the real clock (the lifecycle). */
static bool driver_spawn_worker(void *s);
static void driver_join_worker(void *s);
#define VBAN_SRC_SPAWN_WORKER(s) driver_spawn_worker(s)
#define VBAN_SRC_JOIN_WORKER(s) driver_join_worker(s)
#include "@SOURCE@"
#undef blog

static uint64_t g_now;
/* simulated time (review round 5): when the worker wakes next. It wakes ONLY at a time it scheduled
 * itself (its step's next deadline or look, capped like the shipped thread's sleep) or, idle, when the
 * receive thread signals it -- never at a packet's arrival otherwise, exactly like the shipped thread. */
static bool g_idle = true;
static uint64_t g_wake;
/* `stall` (review round 5): the worker takes no look before the next packet's arrival (a scheduling
 * stall), so every slot it owes by then goes out at that arrival */
static int g_stall = 0;
/* `thread`: the real worker thread on the real clock; packet times are offsets from the start */
static int g_real_thread = 0;
static uint64_t g_real_start;

static uint64_t real_ns(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}
/* the source's settings, changed by `port=` / `name=` / `host=` tokens (each one runs update()) */
static long long g_port = 6980;
static char g_name[32] = "stream";
static char g_host[64] = "";
static long long g_buffer = 40;
/* added to every 24-bit sample after the 16-bit value is scaled up (`low24=N`): the decoder must keep
 * the low byte */
static int g_low24 = 0;
/* the plugin's log lines so far (each `out` line carries the count: the tests order a log line against an
 * output, review of ROZHODNUTE 6352253147); both the receive side and the worker log, so it is atomic */
static unsigned long g_blogs;

uint64_t os_gettime_ns(void)
{
	return g_real_thread ? real_ns() : g_now;
}

bool os_sleepto_ns(uint64_t time_target)
{
	const uint64_t now = os_gettime_ns();
	if (time_target <= now)
		return false;
	const uint64_t d = time_target - now;
	struct timespec req = {(time_t)(d / 1000000000ULL), (long)(d % 1000000000ULL)};
	while (nanosleep(&req, &req) != 0 && errno == EINTR) {
	}
	return true;
}

void os_set_thread_name(const char *name)
{
	(void)name;
}

/* os_event on a mutex + condition variable (auto-reset) */
struct os_event_data {
	pthread_mutex_t m;
	pthread_cond_t c;
	bool signalled;
};

int os_event_init(os_event_t **event, enum os_event_type type)
{
	(void)type;
	struct os_event_data *e = calloc(1, sizeof(*e));
	if (!e)
		abort();
	pthread_mutex_init(&e->m, NULL);
	pthread_cond_init(&e->c, NULL);
	*event = e;
	return 0;
}

void os_event_destroy(os_event_t *event)
{
	pthread_cond_destroy(&event->c);
	pthread_mutex_destroy(&event->m);
	free(event);
}

int os_event_signal(os_event_t *event)
{
	pthread_mutex_lock(&event->m);
	event->signalled = true;
	pthread_cond_signal(&event->c);
	pthread_mutex_unlock(&event->m);
	return 0;
}

int os_event_timedwait(os_event_t *event, unsigned long milliseconds)
{
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	ts.tv_sec += (time_t)(milliseconds / 1000);
	ts.tv_nsec += (long)(milliseconds % 1000) * 1000000L;
	if (ts.tv_nsec >= 1000000000L) {
		ts.tv_sec++;
		ts.tv_nsec -= 1000000000L;
	}
	int r = 0;
	pthread_mutex_lock(&event->m);
	while (!event->signalled && r == 0)
		r = pthread_cond_timedwait(&event->c, &event->m, &ts);
	const bool got = event->signalled;
	event->signalled = false;
	pthread_mutex_unlock(&event->m);
	return got ? 0 : ETIMEDOUT;
}

static bool driver_spawn_worker(void *s)
{
	struct vban_src_s *src = s;
	if (!g_real_thread) {
		/* the step runs on simulated time below; no thread to join. A started worker steps at once,
		 * and a stop's signal is not a packet's */
		src->worker_wake->signalled = false;
		g_idle = false;
		g_wake = g_now;
		return true;
	}
	printf("worker spawn\n");
	return pthread_create(&src->worker, NULL, vban_src_worker_main, src) == 0;
}

static void driver_join_worker(void *s)
{
	struct vban_src_s *src = s;
	if (!g_real_thread)
		return;
	pthread_join(src->worker, NULL);
	printf("worker joined\n");
}

void blog(int level, const char *format, ...)
{
	va_list args;
	va_start(args, format);
	__atomic_fetch_add(&g_blogs, 1UL, __ATOMIC_SEQ_CST);
	fprintf(stderr, "blog %d: ", level);
	vfprintf(stderr, format, args);
	fputc('\n', stderr);
	va_end(args);
}

void *bmalloc(size_t size)
{
	void *p = malloc(size ? size : 1);
	if (!p)
		abort();
	return p;
}

void *brealloc(void *ptr, size_t size)
{
	void *p = realloc(ptr, size ? size : 1);
	if (!p)
		abort();
	return p;
}

void bfree(void *ptr)
{
	free(ptr);
}

void *bmemdup(const void *ptr, size_t size)
{
	void *p = bmalloc(size);
	if (size)
		memcpy(p, ptr, size);
	return p;
}

const char *obs_module_text(const char *lookup_string)
{
	return lookup_string;
}

long long obs_data_get_int(obs_data_t *data, const char *name)
{
	(void)data;
	if (strcmp(name, "port") == 0)
		return g_port;
	if (strcmp(name, "receive_buffer_ms") == 0)
		return g_buffer;
	return 0;
}

const char *obs_data_get_string(obs_data_t *data, const char *name)
{
	(void)data;
	if (strcmp(name, "stream_name") == 0)
		return g_name;
	if (strcmp(name, "ip_from") == 0)
		return g_host;
	return "";
}

void obs_data_set_default_int(obs_data_t *data, const char *name, long long val)
{
	(void)data;
	(void)name;
	(void)val;
}

obs_properties_t *obs_properties_create(void)
{
	return NULL;
}

obs_property_t *obs_properties_add_int(obs_properties_t *props, const char *name, const char *description, int min,
				       int max, int step)
{
	(void)props;
	(void)name;
	(void)description;
	(void)min;
	(void)max;
	(void)step;
	return NULL;
}

obs_property_t *obs_properties_add_text(obs_properties_t *props, const char *name, const char *description,
					enum obs_text_type type)
{
	(void)props;
	(void)name;
	(void)description;
	(void)type;
	return NULL;
}

const char *obs_source_get_name(const obs_source_t *source)
{
	(void)source;
	return "Zvuk z mixu";
}

/* Channel 0's sample at FRAME on the 16-bit scale (ROZHODNUTE 6352253147 b: the tests read a concealed slot's
 * middle and last frame), truncated like `first`. */
static long sample16(const struct obs_source_audio *audio, uint32_t frame)
{
	const size_t i = (size_t)frame * (size_t)audio->speakers;
	switch (audio->format) {
	case AUDIO_FORMAT_16BIT:
		return ((const int16_t *)audio->data[0])[i];
	case AUDIO_FORMAT_U8BIT:
		return (long)((const uint8_t *)audio->data[0])[i] - 128;
	case AUDIO_FORMAT_32BIT:
		return (long)(((const int32_t *)audio->data[0])[i] / 65536);
	case AUDIO_FORMAT_FLOAT_PLANAR:
		return (long)(((const float *)audio->data[0])[frame] * 32768.0f);
	case AUDIO_FORMAT_FLOAT:
		return (long)(((const float *)audio->data[0])[i] * 32768.0f);
	default:
		return 0;
	}
}

void obs_source_output_audio(obs_source_t *source, const struct obs_source_audio *audio)
{
	(void)source;
	int zero = 1;
	long first = 0;
	size_t n = (size_t)audio->frames * (size_t)audio->speakers;
	if (audio->format == AUDIO_FORMAT_16BIT) {
		const int16_t *d = (const int16_t *)audio->data[0];
		first = d[0];
		for (size_t i = 0; i < n; i++)
			if (d[i] != 0)
				zero = 0;
	} else if (audio->format == AUDIO_FORMAT_U8BIT) {
		const uint8_t *d = (const uint8_t *)audio->data[0];
		first = (long)d[0] - 128;
		for (size_t i = 0; i < n; i++)
			if (d[i] != 128)
				zero = 0;
	} else if (audio->format == AUDIO_FORMAT_32BIT) {
		const int32_t *d = (const int32_t *)audio->data[0];
		first = (long)(d[0] / 65536);
		for (size_t i = 0; i < n; i++)
			if (d[i] != 0)
				zero = 0;
	} else if (audio->format == AUDIO_FORMAT_FLOAT_PLANAR) {
		const float *d0 = (const float *)audio->data[0];
		first = (long)(d0[0] * 32768.0f);
		for (int ch = 0; ch < (int)audio->speakers; ch++) {
			const float *d = (const float *)audio->data[ch];
			for (uint32_t i = 0; i < audio->frames; i++)
				if (d[i] != 0.0f)
					zero = 0;
		}
	} else if (audio->format == AUDIO_FORMAT_FLOAT) {
		const float *d = (const float *)audio->data[0];
		first = (long)(d[0] * 32768.0f);
		for (size_t i = 0; i < n; i++)
			if (d[i] != 0.0f)
				zero = 0;
	} else {
		zero = -1;
	}
	/* the first sample on the 24-bit scale (exact in a float up to 2^24), float planar only */
	long first24 = audio->format == AUDIO_FORMAT_FLOAT_PLANAR
			       ? (long)(((const float *)audio->data[0])[0] * 8388608.0f)
			       : 0;
	printf("out ts=%" PRIu64 " frames=%" PRIu32 " ch=%d rate=%" PRIu32 " zero=%d first=%ld fmt=%d first24=%ld"
	       " at=%" PRIu64 " mid=%ld last=%ld logs=%lu\n",
	       audio->timestamp, audio->frames, (int)audio->speakers, audio->samples_per_sec, zero, first,
	       (int)audio->format, first24, os_gettime_ns(), sample16(audio, audio->frames / 2),
	       sample16(audio, audio->frames - 1), __atomic_load_n(&g_blogs, __ATOMIC_SEQ_CST));
}

static int g_udp_dummy;

vban_udp_t *vban_udp_find_or_create(int port)
{
	(void)port;
	return (vban_udp_t *)&g_udp_dummy;
}

void vban_udp_release(vban_udp_t *dev)
{
	(void)dev;
}

/* Whether the source had already asked for a new timeline, and which Receive Buffer it holds, when it
 * let a new port / stream / host in: from that call on, the receive thread may deliver packets. */
static void udp_report(const char *op, void *data)
{
	const struct vban_src_s *s = data;
	printf("udp %s reset=%d r=%ld\n", op, os_atomic_load_bool(&s->timeline_reset) ? 1 : 0,
	       os_atomic_load_long(&s->receive_buffer_ms));
}

void vban_udp_add_callback(vban_udp_t *dev, vban_udp_cb_t cb, void *data)
{
	(void)dev;
	(void)cb;
	udp_report("add", data);
}

void vban_udp_remove_callback(vban_udp_t *dev, vban_udp_cb_t cb, void *data)
{
	(void)dev;
	(void)cb;
	(void)data;
}

void vban_udp_set_name(vban_udp_t *dev, vban_udp_cb_t cb, void *data, const char *name)
{
	(void)dev;
	(void)cb;
	(void)name;
	udp_report("name", data);
}

void vban_udp_set_host(vban_udp_t *dev, vban_udp_cb_t cb, void *data, const char *host)
{
	(void)dev;
	(void)cb;
	(void)host;
	udp_report("host", data);
}

/* Simulated time: one wake of the worker at AT -- its step, then its next wake (the shipped thread's
 * sleep: the step's next time, at most VBAN_SRC_WORKER_SLEEP_MAX_NS ahead; 0 = idle). */
static void sim_wake(struct vban_src_s *s, uint64_t at)
{
	if (at > g_now)
		g_now = at;
	const uint64_t next = vban_src_worker_step(s, g_now);
	if (next == 0) {
		g_idle = true;
		return;
	}
	const uint64_t cap = g_now + VBAN_SRC_WORKER_SLEEP_MAX_NS;
	g_wake = next < cap ? next : cap;
}

/* Simulated time: the worker's wakes before UNTIL (and at UNTIL when INCLUSIVE). */
static void run_worker(void *src, uint64_t until, int inclusive)
{
	struct vban_src_s *s = src;
	while (!g_idle && (g_wake < until || (inclusive && g_wake == until)))
		sim_wake(s, g_wake);
	if (until > g_now)
		g_now = until;
}

/* Simulated time, at the end: the worker's wakes until every stored packet went out (nothing past the
 * last). */
static void flush_worker(void *src)
{
	struct vban_src_s *s = src;
	while (s->store_count > 0 && !g_idle)
		sim_wake(s, g_wake);
}

/* One stereo 16-bit 48 kHz packet of FRAMES frames, every sample = VALUE. */
/* One stereo 48 kHz packet of FRAMES frames in BITFMT (a VBAN_BITFMT_*), every sample = VALUE on the
 * 16-bit scale. */
static void send_packet(void *src, uint32_t nu_frame, uint32_t frames, int bitfmt, int16_t value, uint64_t now)
{
	/* room for 256 stereo 32-bit frames: the test keeps one packet size for every format (a real
	 * 32-bit sender sends smaller packets, VBAN_PROTOCOL_MAX_SIZE; the source never checks that) */
	char buf[VBAN_HEADER_SIZE + 256 * 2 * 4];
	memset(buf, 0, sizeof(buf));
	struct VBanHeader *h = (struct VBanHeader *)buf;
	memcpy(&h->vban, "VBAN", 4);
	h->format_SR = 3;
	h->format_nbs = (uint8_t)(frames - 1);
	h->format_nbc = 1;
	h->format_bit = (uint8_t)bitfmt;
	strncpy(h->streamname, "stream", VBAN_STREAM_NAME_SIZE);
	h->nuFrame = nu_frame;
	uint8_t *pcm = (uint8_t *)(buf + VBAN_HEADER_SIZE);
	size_t bytes = 0;
	for (uint32_t i = 0; i < frames * 2; i++) {
		if (bitfmt == VBAN_BITFMT_8_INT) {
			pcm[bytes++] = (uint8_t)(value + 128);
		} else if (bitfmt == VBAN_BITFMT_16_INT) {
			int16_t v = value;
			memcpy(pcm + bytes, &v, 2);
			bytes += 2;
		} else if (bitfmt == VBAN_BITFMT_24_INT) {
			int32_t v = (int32_t)value * 256 + g_low24;
			pcm[bytes++] = (uint8_t)(v & 0xff);
			pcm[bytes++] = (uint8_t)((v >> 8) & 0xff);
			pcm[bytes++] = (uint8_t)((v >> 16) & 0xff);
		} else if (bitfmt == VBAN_BITFMT_32_INT) {
			int32_t v = (int32_t)value * 65536;
			memcpy(pcm + bytes, &v, 4);
			bytes += 4;
		} else {
			float v = (float)value / 32768.0f;
			memcpy(pcm + bytes, &v, 4);
			bytes += 4;
		}
	}
	if (g_real_thread) {
		/* the real clock: send at the start + NOW */
		const uint64_t at = g_real_start + now;
		const uint64_t cur = real_ns();
		if (at > cur) {
			const uint64_t d = at - cur;
			struct timespec req = {(time_t)(d / 1000000000ULL), (long)(d % 1000000000ULL)};
			while (nanosleep(&req, &req) != 0 && errno == EINTR) {
			}
		}
		printf("in nu=%" PRIu32 " now=%" PRIu64 " value=%d\n", nu_frame, now, (int)value);
		vban_src_callback(buf, VBAN_HEADER_SIZE + bytes, src);
		return;
	}
	/* simulated time: the worker's wakes before the packet, the packet, then a wake at it -- the idle
	 * worker the receive thread signalled, or one it had scheduled for exactly then -- as the shipped
	 * worker thread would */
	if (g_stall) {
		g_stall = 0;
		if (!g_idle && g_wake < now)
			g_wake = now;
	}
	run_worker(src, now, 0);
	g_now = now;
	printf("in nu=%" PRIu32 " now=%" PRIu64 " value=%d\n", nu_frame, now, (int)value);
	vban_src_callback(buf, VBAN_HEADER_SIZE + bytes, src);
	struct vban_src_s *s = src;
	if (s->worker_wake->signalled) {
		s->worker_wake->signalled = false;
		if (g_idle) {
			g_idle = false;
			g_wake = now;
		}
	}
	run_worker(src, now, 1);
}

int main(int argc, char **argv)
{
	if (argc < 2)
		return 2;
	char *rest = NULL;
	uint32_t frames = (uint32_t)strtoul(argv[1], &rest, 10);
	int bitfmt = (rest && *rest == ',') ? atoi(rest + 1) : VBAN_BITFMT_16_INT;
	int first = 2;
	if (argc > 2 && strcmp(argv[2], "thread") == 0) {
		g_real_thread = 1;
		g_real_start = real_ns();
		first = 3;
	}
	void *src = vban_source_info.create(NULL, (obs_source_t *)&g_udp_dummy);
	for (int i = first; i < argc; i++) {
		if (strncmp(argv[i], "low24=", 6) == 0) {
			g_low24 = atoi(argv[i] + 6);
			continue;
		}
		if (strncmp(argv[i], "port=", 5) == 0 || strncmp(argv[i], "name=", 5) == 0 ||
		    strncmp(argv[i], "host=", 5) == 0 || strncmp(argv[i], "buffer=", 7) == 0) {
			if (argv[i][0] == 'b')
				g_buffer = atoll(argv[i] + 7);
			else if (argv[i][0] == 'p')
				g_port = atoll(argv[i] + 5);
			else if (argv[i][0] == 'n')
				snprintf(g_name, sizeof(g_name), "%s", argv[i] + 5);
			else
				snprintf(g_host, sizeof(g_host), "%s", argv[i] + 5);
			printf("update %s\n", argv[i]);
			vban_source_info.update(src, NULL);
			continue;
		}
		if (strcmp(argv[i], "stall") == 0) {
			g_stall = 1;
			continue;
		}
		/* NU:NOW:VALUE, or NU:NOW:VALUE:FRAMES for a packet of another size */
		unsigned long nu = 0, value = 0, size = 0;
		unsigned long long now = 0;
		const int got = sscanf(argv[i], "%lu:%llu:%lu:%lu", &nu, &now, &value, &size);
		if (got != 3 && got != 4)
			return 3;
		send_packet(src, (uint32_t)nu, got == 4 ? (uint32_t)size : frames, bitfmt, (int16_t)value,
			    (uint64_t)now);
	}
	if (g_real_thread) {
		/* the worker sends the last stored slots by their deadlines (R ahead) */
		const struct timespec settle = {0, 100000000L};
		nanosleep(&settle, NULL);
	} else {
		flush_worker(src);
	}
	vban_source_info.destroy(src);
	return 0;
}
