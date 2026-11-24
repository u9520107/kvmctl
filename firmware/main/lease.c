#include "lease.h"

#include <stddef.h>
#include <string.h>

static bool valid_identity(const uint8_t *identity)
{
    return identity != NULL;
}

static bool same_identity(const uint8_t *left, const uint8_t *right)
{
    return valid_identity(left) && valid_identity(right) &&
           memcmp(left, right, LEASE_ID_SIZE) == 0;
}

static bool same_request(const uint8_t *left, const uint8_t *right)
{
    return left != NULL && right != NULL &&
           memcmp(left, right, LEASE_REQUEST_ID_SIZE) == 0;
}

static void bump_epoch(lease_state_t *lease)
{
    ++lease->epoch;
    if (lease->epoch == 0) lease->epoch = 1;
}

static void clear_held(lease_state_t *lease)
{
    lease->held_keys = false;
    lease->held_buttons = false;
}

static void clear_pending(lease_state_t *lease)
{
    lease->pending = false;
    memset(lease->pending_requester, 0, sizeof(lease->pending_requester));
    memset(lease->pending_request_id, 0, sizeof(lease->pending_request_id));
    lease->pending_since_ms = 0;
}

static void invalidate_owner(lease_state_t *lease)
{
    if (!lease->active) return;
    lease->active = false;
    memset(lease->owner, 0, sizeof(lease->owner));
    lease->last_heartbeat_ms = 0;
    clear_held(lease);
    bump_epoch(lease);
}

static void grant_owner(lease_state_t *lease, const uint8_t *owner,
                        uint64_t now_ms)
{
    memcpy(lease->owner, owner, LEASE_ID_SIZE);
    lease->active = true;
    lease->last_heartbeat_ms = now_ms;
    clear_held(lease);
    bump_epoch(lease);
}

static lease_result_t grant_pending(lease_state_t *lease, uint64_t now_ms)
{
    uint8_t requester[LEASE_ID_SIZE];

    memcpy(requester, lease->pending_requester, sizeof(requester));
    clear_pending(lease);
    grant_owner(lease, requester, now_ms);
    return LEASE_TAKEOVER_GRANTED;
}

static uint64_t elapsed(uint64_t now_ms, uint64_t then_ms)
{
    return now_ms >= then_ms ? now_ms - then_ms : 0;
}

void lease_init(lease_state_t *lease)
{
    if (lease == NULL) return;
    memset(lease, 0, sizeof(*lease));
}

lease_result_t lease_request(lease_state_t *lease, const uint8_t *requester,
                             const uint8_t *request_id, uint64_t now_ms)
{
    if (lease == NULL || !valid_identity(requester)) return LEASE_REJECTED;
    if (!lease->active) {
        clear_pending(lease);
        grant_owner(lease, requester, now_ms);
        return LEASE_GRANTED;
    }
    if (same_identity(lease->owner, requester) || lease->pending) return LEASE_BUSY;
    if (request_id == NULL) return LEASE_REJECTED;

    memcpy(lease->pending_requester, requester, LEASE_ID_SIZE);
    memcpy(lease->pending_request_id, request_id, LEASE_REQUEST_ID_SIZE);
    lease->pending_since_ms = now_ms;
    lease->pending = true;
    return LEASE_TAKEOVER_PENDING;
}

lease_result_t lease_takeover_response(lease_state_t *lease, const uint8_t *owner,
                                       uint32_t epoch, const uint8_t *request_id,
                                       bool accepted)
{
    if (lease == NULL || !lease->active || !lease->pending ||
        lease->epoch != epoch ||
        !same_identity(lease->owner, owner) ||
        !same_request(lease->pending_request_id, request_id)) {
        return LEASE_REJECTED;
    }
    if (accepted) return LEASE_TAKEOVER_ACCEPTED;
    clear_pending(lease);
    return LEASE_REJECTED;
}

lease_result_t lease_heartbeat(lease_state_t *lease, const uint8_t *owner,
                               uint32_t epoch, uint64_t now_ms)
{
    if (lease == NULL || !lease->active || lease->epoch != epoch ||
        !same_identity(lease->owner, owner) || now_ms < lease->last_heartbeat_ms) {
        return LEASE_REJECTED;
    }
    if (elapsed(now_ms, lease->last_heartbeat_ms) >= LEASE_EXPIRY_MS) {
        (void)lease_tick(lease, now_ms);
        return LEASE_EXPIRED;
    }
    lease->last_heartbeat_ms = now_ms;
    return LEASE_HEARTBEAT_ACCEPTED;
}

lease_result_t lease_release(lease_state_t *lease, const uint8_t *owner,
                             uint32_t epoch, uint64_t now_ms)
{
    (void)now_ms;
    if (lease == NULL || !lease->active || lease->epoch != epoch ||
        !same_identity(lease->owner, owner)) return LEASE_REJECTED;
    invalidate_owner(lease);
    return LEASE_RELEASED;
}

lease_result_t lease_revoke(lease_state_t *lease, const uint8_t *identity,
                            uint64_t now_ms)
{
    (void)now_ms;
    if (lease == NULL || !valid_identity(identity)) return LEASE_REJECTED;
    if (lease->active && same_identity(lease->owner, identity)) {
        invalidate_owner(lease);
        return LEASE_REVOKED;
    }
    if (lease->pending && same_identity(lease->pending_requester, identity)) {
        clear_pending(lease);
        return LEASE_REVOKED;
    }
    return LEASE_NOOP;
}

lease_result_t lease_restart(lease_state_t *lease)
{
    if (lease == NULL) return LEASE_REJECTED;
    if (lease->active) invalidate_owner(lease);
    clear_pending(lease);
    clear_held(lease);
    bump_epoch(lease);
    return LEASE_RESTARTED;
}

lease_result_t lease_tick(lease_state_t *lease, uint64_t now_ms)
{
    if (lease == NULL || !lease->active) return LEASE_NOOP;
    if (elapsed(now_ms, lease->last_heartbeat_ms) >= LEASE_EXPIRY_MS) {
        invalidate_owner(lease);
        return lease->pending ? LEASE_TAKEOVER_GRANTED : LEASE_EXPIRED;
    }
    if (lease->pending && elapsed(now_ms, lease->pending_since_ms) >=
                              LEASE_TAKEOVER_WINDOW_MS) {
        invalidate_owner(lease);
        return LEASE_TAKEOVER_GRANTED;
    }
    return LEASE_NOOP;
}

bool lease_pending_matches(const lease_state_t *lease, const uint8_t *requester)
{
    return lease != NULL && lease->pending &&
           same_identity(lease->pending_requester, requester);
}

void lease_clear_pending(lease_state_t *lease)
{
    if (lease != NULL) clear_pending(lease);
}

lease_result_t lease_grant_pending(lease_state_t *lease, uint64_t now_ms)
{
    if (lease == NULL || lease->active || !lease->pending) return LEASE_NOOP;
    return grant_pending(lease, now_ms);
}

bool lease_can_emit(const lease_state_t *lease, const uint8_t *owner,
                    uint32_t epoch)
{
    return lease != NULL && lease->active && lease->epoch == epoch &&
           same_identity(lease->owner, owner);
}

bool lease_set_held(lease_state_t *lease, const uint8_t *owner, uint32_t epoch,
                    bool keys, bool buttons)
{
    if (!lease_can_emit(lease, owner, epoch)) return false;
    lease->held_keys = keys;
    lease->held_buttons = buttons;
    return true;
}

bool lease_held_empty(const lease_state_t *lease)
{
    return lease != NULL && !lease->held_keys && !lease->held_buttons;
}

static bool check_owner(const lease_state_t *lease, const uint8_t *owner,
                        uint32_t epoch)
{
    return lease_can_emit(lease, owner, epoch) && !lease_held_empty(lease);
}

bool lease_check_vectors(void)
{
    static const uint8_t owner_a[LEASE_ID_SIZE] = {1};
    static const uint8_t owner_b[LEASE_ID_SIZE] = {2};
    static const uint8_t owner_c[LEASE_ID_SIZE] = {3};
    static const uint8_t request_b[LEASE_REQUEST_ID_SIZE] = {0xb};
    static const uint8_t request_c[LEASE_REQUEST_ID_SIZE] = {0xc};
    lease_state_t lease;
    uint32_t epoch;

    lease_init(&lease);
    if (lease_request(&lease, owner_a, NULL, 0) != LEASE_GRANTED ||
        lease.epoch != 1 || !lease_can_emit(&lease, owner_a, lease.epoch)) return false;
    epoch = lease.epoch;
    if (!lease_set_held(&lease, owner_a, epoch, true, true) ||
        lease_heartbeat(&lease, owner_a, epoch, 1000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_request(&lease, owner_b, request_b, 1000) != LEASE_TAKEOVER_PENDING ||
        lease_request(&lease, owner_c, request_c, 1000) != LEASE_BUSY ||
        lease_takeover_response(&lease, owner_a, epoch, request_b, false) != LEASE_REJECTED ||
        lease.pending || !check_owner(&lease, owner_a, epoch)) return false;

    if (lease_request(&lease, owner_b, request_b, 1000) != LEASE_TAKEOVER_PENDING ||
        lease_takeover_response(&lease, owner_a, epoch + 1, request_b, true) != LEASE_REJECTED ||
        !lease.pending ||
        lease_takeover_response(&lease, owner_a, epoch, request_b, true) != LEASE_TAKEOVER_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 2000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 4000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 6000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 8000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 10000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 12000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 14000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 16000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 18000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 20000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 22000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 24000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 26000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 28000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_heartbeat(&lease, owner_a, epoch, 30000) != LEASE_HEARTBEAT_ACCEPTED ||
        lease_tick(&lease, 30999) != LEASE_NOOP ||
         lease_tick(&lease, 31000) != LEASE_TAKEOVER_GRANTED ||
         lease_grant_pending(&lease, 31000) != LEASE_TAKEOVER_GRANTED ||
        lease.active == false || !same_identity(lease.owner, owner_b) ||
        lease.epoch != 3 || !lease_held_empty(&lease) ||
        lease_can_emit(&lease, owner_a, epoch)) return false;

    epoch = lease.epoch;
    if (lease_request(&lease, owner_a, request_c, 32000) != LEASE_TAKEOVER_PENDING ||
         lease_release(&lease, owner_b, epoch, 32000) != LEASE_RELEASED ||
         lease_grant_pending(&lease, 32000) != LEASE_TAKEOVER_GRANTED ||
        !same_identity(lease.owner, owner_a) || lease.epoch != 5 ||
        !lease_held_empty(&lease)) return false;

    epoch = lease.epoch;
    if (!lease_set_held(&lease, owner_a, epoch, true, true) ||
        lease_request(&lease, owner_b, request_b, 33000) != LEASE_TAKEOVER_PENDING ||
         lease_tick(&lease, 36000) != LEASE_TAKEOVER_GRANTED ||
         lease_grant_pending(&lease, 36000) != LEASE_TAKEOVER_GRANTED ||
        !same_identity(lease.owner, owner_b) || lease.epoch != 7 ||
        !lease_held_empty(&lease)) return false;

    if (!lease_set_held(&lease, owner_b, lease.epoch, true, true) ||
        lease_revoke(&lease, owner_b, 37000) != LEASE_REVOKED || lease.active ||
        !lease_held_empty(&lease) || lease_revoke(&lease, owner_c, 37000) != LEASE_NOOP ||
        lease_request(&lease, owner_c, NULL, 38000) != LEASE_GRANTED ||
        !lease_set_held(&lease, owner_c, lease.epoch, true, true) ||
        lease_restart(&lease) != LEASE_RESTARTED || lease.active ||
        !lease_held_empty(&lease) || lease.epoch != 11) return false;

    lease_init(&lease);
    if (lease_request(&lease, owner_a, NULL, 0) != LEASE_GRANTED ||
        lease_takeover_response(&lease, owner_a, lease.epoch, request_b, true) != LEASE_REJECTED ||
        lease_request(&lease, owner_b, request_b, 1) != LEASE_TAKEOVER_PENDING ||
        lease_release(&lease, owner_a, lease.epoch, 2) != LEASE_RELEASED ||
        lease_grant_pending(&lease, 2) != LEASE_TAKEOVER_GRANTED) return false;
    lease_init(&lease);
    if (lease_request(&lease, owner_a, NULL, 0) != LEASE_GRANTED ||
        lease_request(&lease, owner_b, request_b, 1) != LEASE_TAKEOVER_PENDING ||
        lease_tick(&lease, LEASE_EXPIRY_MS + 1) != LEASE_TAKEOVER_GRANTED ||
        lease_grant_pending(&lease, LEASE_EXPIRY_MS + 1) != LEASE_TAKEOVER_GRANTED) return false;
    return true;
}
