// TapFlow engine (Rust, engine/src/lib.rs). Every call blocks; call from a background queue.
// A non-NULL return is an error message; free it (and any *out string) with tf_free.
#ifndef TAPFLOW_ENGINE_H
#define TAPFLOW_ENGINE_H

#include <stdint.h>

typedef void (*tf_log_callback)(const char *line);

void tf_set_log(tf_log_callback callback);
void tf_free(char *ptr);

char *tf_connect(const char *pairing_path, const char *host, uint16_t rp_port);
void tf_disconnect(void);
char *tf_services(char **out);
char *tf_device_status(char **out);
char *tf_mount_ddi(const char *image, const char *manifest, const char *trust_cache);

char *tf_hid_begin(void);
char *tf_hid_tap(uint16_t x, uint16_t y);
char *tf_hid_drag(uint16_t x1, uint16_t y1, uint16_t x2, uint16_t y2, uint32_t steps, uint64_t delay_ms);
char *tf_hid_end(void);

char *tf_find_runners(char **out);
char *tf_wda_start(const char *bundle_id, char **out);
void tf_wda_stop(void);

#endif
