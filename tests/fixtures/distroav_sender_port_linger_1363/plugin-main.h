// issue 1363 gate stub: only what ndi-sender-port.cpp uses from the real plugin-main.h.
#pragma once
#include <cstdarg>
#include <cstdio>
#define LOG_ERROR 100
#define LOG_WARNING 200
#define LOG_INFO 300
#define LOG_DEBUG 400
typedef struct NDIlib_send_instance_type *NDIlib_send_instance_t;
typedef struct {
	const char *p_ndi_name;
	const char *p_groups;
	bool clock_video;
	bool clock_audio;
} NDIlib_send_create_t;
typedef struct {
	NDIlib_send_instance_t (*send_create)(const NDIlib_send_create_t *p_create_settings);
	void (*send_destroy)(NDIlib_send_instance_t p_instance);
} NDIlib_v6;
extern const NDIlib_v6 *ndiLib;
static inline void obs_log(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static inline void obs_log(int level, const char *fmt, ...)
{
	va_list ap;
	va_start(ap, fmt);
	printf("LOG%d: ", level);
	vprintf(fmt, ap);
	printf("\n");
	va_end(ap);
}
