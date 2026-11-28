#pragma once

#include "esp_err.h"
#include "hid_protocol.h"

/* Transport serializes these operations with lease changes and its HID worker. */
esp_err_t hid_bench_init(const uint8_t fingerprint[32]);
bool hid_bench_attached(void);
bool hid_bench_can_arm(void);
const char *hid_bench_enqueue(const kvmctl_hid_event_t *event);
void hid_bench_disarm(void);
bool hid_bench_service(bool *detached);
bool hid_bench_cleanup_complete(void);
bool hid_bench_releasing(void);
uint32_t hid_bench_reports(void);
uint32_t hid_bench_fifo_high_water(void);
uint32_t hid_bench_fifo_overflows(void);
void hid_bench_reset_fifo_metrics(void);
