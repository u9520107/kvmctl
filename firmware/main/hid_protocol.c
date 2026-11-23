#include "hid_protocol.h"

#include "protocol_vectors.h"
#include <string.h>

typedef struct { const uint8_t *data; size_t len; size_t pos; } reader_t;

static bool head(reader_t *r, uint8_t major, uint64_t *value)
{
    if (!r || !value || r->pos >= r->len) return false;
    uint8_t first = r->data[r->pos++];
    if ((first >> 5) != major) return false;
    uint8_t ai = first & 31;
    size_t width = ai <= 23 ? 0 : ai == 24 ? 1 : ai == 25 ? 2 : ai == 26 ? 4 : ai == 27 ? 8 : 99;
    if (width == 99 || width > r->len - r->pos) return false;
    if (width == 0) { *value = ai; return true; }
    uint64_t n = 0;
    for (size_t i = 0; i < width; ++i) n = (n << 8) | r->data[r->pos++];
    if ((width == 1 && n < 24) || (width == 2 && n <= 0xff) ||
        (width == 4 && n <= 0xffff) || (width == 8 && n <= 0xffffffff)) return false;
    *value = n;
    return true;
}

static bool uint_value(reader_t *r, uint64_t expected)
{
    uint64_t value = 0;
    return head(r, 0, &value) && value == expected;
}

static bool bytes(reader_t *r, uint8_t *out, size_t expected)
{
    uint64_t length = 0;
    if (!head(r, 2, &length) || length != expected || length > r->len - r->pos) return false;
    memcpy(out, r->data + r->pos, expected);
    r->pos += expected;
    return true;
}

static bool map(reader_t *r, uint64_t expected)
{
    uint64_t count = 0;
    return head(r, 5, &count) && count == expected;
}

static bool array(reader_t *r, uint64_t expected)
{
    uint64_t count = 0;
    return head(r, 4, &count) && count == expected;
}

static bool signed16(reader_t *r, int16_t *out)
{
    if (!r || !out || r->pos >= r->len) return false;
    uint8_t major = r->data[r->pos] >> 5;
    uint64_t value = 0;
    if (major == 0) {
        if (!head(r, 0, &value) || value > 32767) return false;
        *out = (int16_t)value;
        return true;
    }
    if (major != 1 || !head(r, 1, &value) || value > 32767) return false;
    *out = (int16_t)(-1 - (int32_t)value);
    return true;
}

static bool payload(reader_t *r, uint64_t type, kvmctl_hid_event_t *event)
{
    uint64_t value = 0;
    switch (type) {
    case KVMCTL_HID_LEASE_REQUEST:
    case KVMCTL_HID_HEARTBEAT:
    case KVMCTL_HID_LEASE_RELEASE:
        if (!map(r, 0)) return false;
        return true;
    case KVMCTL_HID_TAKEOVER_REQUEST:
        return map(r, 1) && uint_value(r, 0) && bytes(r, event->request_id, 16);
    case KVMCTL_HID_TAKEOVER_RESPONSE:
        if (!map(r, 2) || !uint_value(r, 0) || !bytes(r, event->request_id, 16) ||
            !uint_value(r, 1) || r->pos >= r->len) return false;
        value = r->data[r->pos++];
        if (value != 0xf4 && value != 0xf5) return false;
        event->accepted = value == 0xf5;
        return true;
    case KVMCTL_HID_LEASE_STATE:
        if (!map(r, 2) || !uint_value(r, 0) || r->pos >= r->len) return false;
        if (r->data[r->pos] == 0xf6) {
            ++r->pos;
            event->owner_present = false;
        } else {
            event->owner_present = true;
            if (!bytes(r, event->owner, 32)) return false;
        }
        if (!uint_value(r, 1) || !head(r, 0, &value) || value > 2) return false;
        event->state = (uint8_t)value;
        return true;
    case KVMCTL_HID_TAKEOVER_NOTICE:
        if (!map(r, 3) || !uint_value(r, 0) || !bytes(r, event->request_id, 16) ||
            !uint_value(r, 1) || !bytes(r, event->requester, 32) ||
            !uint_value(r, 2) || !head(r, 0, &event->response_window_ms) ||
            event->response_window_ms > 10000) return false;
        return true;
    case KVMCTL_HID_ERROR: {
        uint64_t length = 0;
        if (!map(r, 1) || !uint_value(r, 0) || !head(r, 3, &length) || length > 32 ||
            length > r->len - r->pos) return false;
        memcpy(event->error_code, r->data + r->pos, (size_t)length);
        event->error_code[length] = '\0';
        r->pos += (size_t)length;
        return strcmp(event->error_code, "unsupported_version") == 0 ||
               strcmp(event->error_code, "unauthorized") == 0 ||
               strcmp(event->error_code, "pin_mismatch") == 0 ||
               strcmp(event->error_code, "replay") == 0 ||
               strcmp(event->error_code, "busy") == 0 ||
               strcmp(event->error_code, "lease_expired") == 0 ||
               strcmp(event->error_code, "stale_epoch") == 0 ||
               strcmp(event->error_code, "malformed_event") == 0 ||
               strcmp(event->error_code, "invalid_capability") == 0 ||
               strcmp(event->error_code, "rate_limited") == 0 ||
               strcmp(event->error_code, "resource_exhausted") == 0;
    }
    case KVMCTL_HID_KEYBOARD_STATE: {
        if (!map(r, 2) || !uint_value(r, 0) || !head(r, 0, &value) || value > 255 ||
            !uint_value(r, 1)) return false;
        event->modifier_mask = (uint8_t)value;
        if (!head(r, 4, &value) || value > KVMCTL_HID_MAX_KEYS) return false;
        event->key_count = (uint8_t)value;
        uint16_t previous_page = 0, previous_usage = 0;
        for (size_t i = 0; i < event->key_count; ++i) {
            uint64_t page = 0, usage = 0;
            if (!array(r, 2) || !head(r, 0, &page) || !head(r, 0, &usage) || page > 0xffff || usage > 0xffff) return false;
            if (i != 0 && (page < previous_page || (page == previous_page && usage <= previous_usage))) return false;
            previous_page = (uint16_t)page; previous_usage = (uint16_t)usage;
            event->keys[i][0] = previous_page; event->keys[i][1] = previous_usage;
        }
        return true;
    }
    case KVMCTL_HID_MOUSE_BUTTONS:
        if (!map(r, 1) || !uint_value(r, 0) || !head(r, 0, &value) || value > 0x1f) return false;
        event->button_mask = (uint8_t)value;
        return true;
    case KVMCTL_HID_WHEEL:
        return map(r, 2) && uint_value(r, 0) && signed16(r, &event->first_delta) &&
               uint_value(r, 1) && signed16(r, &event->second_delta);
    case KVMCTL_HID_MOTION:
        return map(r, 2) && uint_value(r, 0) && signed16(r, &event->first_delta) &&
               uint_value(r, 1) && signed16(r, &event->second_delta);
    default:
        return false;
    }
}

bool kvmctl_hid_decode(const uint8_t *data, size_t length, bool motion_channel,
                       kvmctl_hid_event_t *event)
{
    if (!data || !event || length == 0 || length > (motion_channel ? KVMCTL_HID_MAX_MOTION : KVMCTL_HID_MAX_CONTROL)) return false;
    reader_t r = {.data = data, .len = length, .pos = 0};
    uint64_t type = 0, epoch = 0;
    memset(event, 0, sizeof(*event));
    if (!map(&r, 7) || !uint_value(&r, 0) || !uint_value(&r, 1) ||
        !uint_value(&r, 1) || !uint_value(&r, 0) || !uint_value(&r, 2) ||
        !head(&r, 0, &type) || !uint_value(&r, 3) || !bytes(&r, event->session_id, 16) ||
        !uint_value(&r, 4) || !head(&r, 0, &epoch) || epoch > 0xffffffff ||
        !uint_value(&r, 5) || !head(&r, 0, &event->sequence) || !uint_value(&r, 6) ||
        !payload(&r, type, event) || r.pos != r.len) return false;
    uint8_t any_session_byte = 0;
    for (size_t i = 0; i < sizeof(event->session_id); ++i) any_session_byte |= event->session_id[i];
    if (any_session_byte == 0) return false;
    event->type = (kvmctl_hid_type_t)type;
    event->lease_epoch = (uint32_t)epoch;
    if (motion_channel != (type == KVMCTL_HID_MOTION)) return false;
    return true;
}

typedef struct { uint8_t *data; size_t len; size_t pos; bool ok; } writer_t;

static void write_head(writer_t *w, uint8_t major, uint64_t value)
{
    size_t width = value < 24 ? 0 : value <= 0xff ? 1 : value <= 0xffff ? 2 :
                    value <= 0xffffffff ? 4 : 8;
    if (!w || !w->ok || width + 1 > w->len - w->pos) { if (w) w->ok = false; return; }
    w->data[w->pos++] = (uint8_t)((major << 5) | (width == 0 ? value : 24 + (width == 1 ? 0 : width == 2 ? 1 : width == 4 ? 2 : 3)));
    for (size_t i = width; i != 0; --i) w->data[w->pos++] = (uint8_t)(value >> ((i - 1) * 8));
}

static void write_uint(writer_t *w, uint64_t value) { write_head(w, 0, value); }

static void write_bytes(writer_t *w, const uint8_t *data, size_t length)
{
    write_head(w, 2, length);
    if (!w || !w->ok || length > w->len - w->pos) { if (w) w->ok = false; return; }
    memcpy(w->data + w->pos, data, length);
    w->pos += length;
}

static void write_text(writer_t *w, const char *text)
{
    size_t length = text ? strlen(text) : 0;
    write_head(w, 3, length);
    if (!w || !w->ok || length > w->len - w->pos) { if (w) w->ok = false; return; }
    memcpy(w->data + w->pos, text, length);
    w->pos += length;
}

size_t kvmctl_hid_encode(const kvmctl_hid_event_t *event, uint8_t *data,
                         size_t capacity)
{
    if (!event || !data || capacity == 0 ||
        (event->type != KVMCTL_HID_TAKEOVER_RESPONSE &&
         event->type != KVMCTL_HID_LEASE_STATE &&
         event->type != KVMCTL_HID_TAKEOVER_NOTICE &&
         event->type != KVMCTL_HID_ERROR)) return 0;
    uint8_t any_session_byte = 0;
    for (size_t i = 0; i < sizeof(event->session_id); ++i) any_session_byte |= event->session_id[i];
    if (any_session_byte == 0) return 0;
    if (event->type == KVMCTL_HID_LEASE_STATE && (!event->owner_present && event->state > 2)) return 0;
    if (event->type == KVMCTL_HID_TAKEOVER_NOTICE && event->response_window_ms > 10000) return 0;
    if (event->type == KVMCTL_HID_ERROR &&
        (strlen(event->error_code) > 32 ||
         (strcmp(event->error_code, "busy") != 0 && strcmp(event->error_code, "stale_epoch") != 0 &&
          strcmp(event->error_code, "lease_expired") != 0 && strcmp(event->error_code, "unauthorized") != 0 &&
         strcmp(event->error_code, "malformed_event") != 0 &&
         strcmp(event->error_code, "unsupported_version") != 0 &&
         strcmp(event->error_code, "pin_mismatch") != 0 &&
         strcmp(event->error_code, "replay") != 0 &&
         strcmp(event->error_code, "invalid_capability") != 0 &&
         strcmp(event->error_code, "rate_limited") != 0 &&
         strcmp(event->error_code, "resource_exhausted") != 0))) return 0;

    writer_t w = {.data = data, .len = capacity, .ok = true};
    write_head(&w, 5, 7);
    write_uint(&w, 0); write_uint(&w, 1);
    write_uint(&w, 1); write_uint(&w, 0);
    write_uint(&w, 2); write_uint(&w, event->type);
    write_uint(&w, 3); write_bytes(&w, event->session_id, sizeof(event->session_id));
    write_uint(&w, 4); write_uint(&w, event->lease_epoch);
    write_uint(&w, 5); write_uint(&w, event->sequence);
    write_uint(&w, 6);
    if (event->type == KVMCTL_HID_TAKEOVER_RESPONSE) {
        write_head(&w, 5, 2); write_uint(&w, 0);
        write_bytes(&w, event->request_id, sizeof(event->request_id));
        write_uint(&w, 1); if (w.ok && w.pos < w.len) w.data[w.pos++] = event->accepted ? 0xf5 : 0xf4;
        else w.ok = false;
    } else if (event->type == KVMCTL_HID_LEASE_STATE) {
        write_head(&w, 5, 2); write_uint(&w, 0);
        if (event->owner_present) write_bytes(&w, event->owner, sizeof(event->owner));
        else if (w.ok && w.pos < w.len) w.data[w.pos++] = 0xf6;
        else w.ok = false;
        write_uint(&w, 1); write_uint(&w, event->state);
    } else if (event->type == KVMCTL_HID_TAKEOVER_NOTICE) {
        write_head(&w, 5, 3); write_uint(&w, 0); write_bytes(&w, event->request_id, sizeof(event->request_id));
        write_uint(&w, 1); write_bytes(&w, event->requester, sizeof(event->requester));
        write_uint(&w, 2); write_uint(&w, event->response_window_ms);
    } else {
        write_head(&w, 5, 1); write_uint(&w, 0); write_text(&w, event->error_code);
    }
    return w.ok ? w.pos : 0;
}

static int hex_value(char c)
{
    return c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : -1;
}

bool kvmctl_hid_check_vectors(void)
{
    static const char hex[] = KVMCTL_M14_HEARTBEAT_HEX;
    uint8_t bytes_value[64]; size_t length = 0;
    for (size_t i = 0; hex[i] != '\0'; i += 2) {
        int high = hex_value(hex[i]), low = hex_value(hex[i + 1]);
        if (high < 0 || low < 0 || length == sizeof(bytes_value)) return false;
        bytes_value[length++] = (uint8_t)((high << 4) | low);
    }
    kvmctl_hid_event_t event;
    if (!kvmctl_hid_decode(bytes_value, length, false, &event) ||
        event.type != KVMCTL_HID_HEARTBEAT || event.lease_epoch != 7 || event.sequence != 0) return false;

    /* Keep the firmware-side rejection cases beside the shared vector. */
    uint8_t rejected[64];
    memcpy(rejected, bytes_value, length);
    rejected[2] = 0x18; /* non-shortest encoding for version 1 */
    if (kvmctl_hid_decode(rejected, length, false, &event)) return false;
    memcpy(rejected, bytes_value, length);
    rejected[0] = 0xa8; /* unknown extra envelope field */
    if (kvmctl_hid_decode(rejected, length, false, &event)) return false;
    memcpy(rejected, bytes_value, length);
    rejected[length - 1] = 0xa1; /* heartbeat payload must be empty */
    if (kvmctl_hid_decode(rejected, length, false, &event)) return false;

    kvmctl_hid_event_t response = {0};
    response.type = KVMCTL_HID_LEASE_STATE;
    memcpy(response.session_id, bytes_value + 10, sizeof(response.session_id));
    response.lease_epoch = 7;
    response.owner_present = false;
    response.state = 0;
    uint8_t encoded[KVMCTL_HID_MAX_CONTROL];
    size_t encoded_length = kvmctl_hid_encode(&response, encoded, sizeof(encoded));
    if (encoded_length == 0 || !kvmctl_hid_decode(encoded, encoded_length, false, &event) ||
        event.type != KVMCTL_HID_LEASE_STATE || event.owner_present || event.state != 0) return false;
    response.type = KVMCTL_HID_TAKEOVER_RESPONSE;
    response.request_id[0] = 1;
    response.accepted = true;
    encoded_length = kvmctl_hid_encode(&response, encoded, sizeof(encoded));
    return encoded_length != 0 && kvmctl_hid_decode(encoded, encoded_length, false, &event) &&
           event.type == KVMCTL_HID_TAKEOVER_RESPONSE && event.request_id[0] == 1 && event.accepted;
}
