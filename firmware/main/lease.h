#pragma once

#include <stdbool.h>
#include <stdint.h>

#define LEASE_ID_SIZE 32u
#define LEASE_REQUEST_ID_SIZE 16u
#define LEASE_HEARTBEAT_INTERVAL_MS 1000u
#define LEASE_EXPIRY_MS 3000u
#define LEASE_TAKEOVER_WINDOW_MS 30000u

typedef enum {
    LEASE_NOOP = 0,
    LEASE_GRANTED,
    LEASE_TAKEOVER_PENDING,
    LEASE_TAKEOVER_ACCEPTED,
    LEASE_REJECTED,
    LEASE_BUSY,
    LEASE_HEARTBEAT_ACCEPTED,
    LEASE_RELEASED,
    LEASE_EXPIRED,
    LEASE_REVOKED,
    LEASE_RESTARTED,
    LEASE_TAKEOVER_GRANTED,
} lease_result_t;

typedef struct {
    bool active;
    uint8_t owner[LEASE_ID_SIZE];
    uint32_t epoch;
    uint64_t last_heartbeat_ms;

    bool pending;
    uint8_t pending_requester[LEASE_ID_SIZE];
    uint8_t pending_request_id[LEASE_REQUEST_ID_SIZE];
    uint64_t pending_since_ms;

    bool held_keys;
    bool held_buttons;
} lease_state_t;

void lease_init(lease_state_t *lease);

lease_result_t lease_request(lease_state_t *lease, const uint8_t *requester,
                             const uint8_t *request_id, uint64_t now_ms);
lease_result_t lease_takeover_response(lease_state_t *lease, const uint8_t *owner,
                                       uint32_t epoch, const uint8_t *request_id,
                                       bool accepted);
lease_result_t lease_heartbeat(lease_state_t *lease, const uint8_t *owner,
                               uint32_t epoch, uint64_t now_ms);
lease_result_t lease_release(lease_state_t *lease, const uint8_t *owner,
                             uint32_t epoch, uint64_t now_ms);
lease_result_t lease_revoke(lease_state_t *lease, const uint8_t *identity,
                            uint64_t now_ms);
lease_result_t lease_restart(lease_state_t *lease);
lease_result_t lease_tick(lease_state_t *lease, uint64_t now_ms);
bool lease_pending_matches(const lease_state_t *lease, const uint8_t *requester);
void lease_clear_pending(lease_state_t *lease);
lease_result_t lease_grant_pending(lease_state_t *lease, uint64_t now_ms);

bool lease_can_emit(const lease_state_t *lease, const uint8_t *owner,
                    uint32_t epoch);
bool lease_set_held(lease_state_t *lease, const uint8_t *owner, uint32_t epoch,
                    bool keys, bool buttons);
bool lease_held_empty(const lease_state_t *lease);

bool lease_check_vectors(void);
