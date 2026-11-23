#include "signaling.h"

#include "client_auth.h"
#include "device_identity.h"
#include "esp_heap_caps.h"
#include "esp_https_server.h"
#include "esp_log.h"
#include "esp_random.h"
#include "esp_timer.h"
#include "mdns.h"
#include "mbedtls/pk.h"
#include "mbedtls/platform_util.h"
#include "mbedtls/ssl.h"
#include "mbedtls/ssl_ciphersuites.h"
#include "mbedtls/x509_crt.h"
#include "psa/crypto.h"
#include "signaling_protocol.h"
#include "transport.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include <stdatomic.h>
#include <stdbool.h>
#include <string.h>
#include "lwip/sockets.h"

#define MAX_TLS_SOCKETS 2
#define MAX_WSS_MESSAGE 16384
#define MAX_SDP_BYTES 8192
#define HANDSHAKE_BURST 4
#define HANDSHAKE_REFILL_US 10000000LL
#define AUTH_HANDSHAKE_TIMEOUT_US 15000000LL

typedef enum {
    SESSION_WAIT_HELLO,
    SESSION_WAIT_CLIENT_PROOF,
    SESSION_WAIT_OFFER,
    SESSION_WAIT_TRANSCRIPT,
    SESSION_READY,
    SESSION_FAILED,
    SESSION_WAIT_UPGRADE,
} session_phase_t;

typedef struct {
    int fd;
    uint64_t started_us;
    uint64_t phase_deadline_us;
    uint64_t message_deadline_us;
    uint64_t receive_sequence;
    uint64_t transmit_sequence;
    uint8_t phase;
    bool transport_bound;
    bool close_requested;
    bool close_queued;
    uint32_t transport_generation;
    size_t partial_len;
    uint8_t fragment_type;
    uint8_t *message;
    uint8_t *answer;
    size_t answer_len;
    signaling_challenge_t challenge;
    signaling_transcript_t transcript;
} signaling_session_t;

typedef struct {
    const uint8_t *data;
    size_t len;
    size_t pos;
} cbor_reader_t;

typedef struct {
    uint8_t *data;
    size_t capacity;
    size_t pos;
} cbor_writer_t;

static const char *TAG = "kvmctl_signal";
static httpd_handle_t s_server;
static atomic_uint s_tls_sockets;
static int s_admitted_fds[MAX_TLS_SOCKETS] = {-1, -1};
static uint64_t s_handshake_refill_us;
static uint8_t s_handshake_tokens;
static mbedtls_x509_crt s_tls_certificate;
static mbedtls_pk_context s_tls_private_key;
static bool s_tls_material_ready;
static portMUX_TYPE s_admission_mux = portMUX_INITIALIZER_UNLOCKED;
static SemaphoreHandle_t s_sessions_lock;
static atomic_bool s_expiry_work_pending;
static signaling_session_t *s_sessions[MAX_TLS_SOCKETS];
static uint8_t s_seen_session_ids[8][16];
static uint8_t s_seen_session_count;
static uint8_t s_seen_session_next;
static bool s_mdns_started;

static bool set_session_phase(signaling_session_t *s, session_phase_t phase, uint64_t timeout_us)
{
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    bool open = !s->close_requested;
    if (open) {
        s->phase = phase;
        if (timeout_us) s->phase_deadline_us = esp_timer_get_time() + timeout_us;
    }
    xSemaphoreGive(s_sessions_lock);
    return open;
}

static bool session_deadline_expired(const signaling_session_t *s, uint64_t now)
{
    return (s->phase != SESSION_READY && now >= s->phase_deadline_us) ||
           (s->message_deadline_us != 0 && now >= s->message_deadline_us);
}

static bool session_can_process(signaling_session_t *s)
{
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    bool open = !s->close_requested && !session_deadline_expired(s, esp_timer_get_time());
    xSemaphoreGive(s_sessions_lock);
    return open;
}

/* Caller holds s_sessions_lock until shutdown completes, preventing fd reuse. */
static void request_session_close(signaling_session_t *s)
{
    if (s->fd >= 0 && !s->close_requested) {
        s->close_requested = true;
        shutdown(s->fd, SHUT_RDWR);
    }
}

/* Runs on the HTTP server task, so a queued close cannot target a reused fd. */
static void close_expired_sessions(void *arg)
{
    (void)arg;
    for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) {
        xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
        signaling_session_t *s = s_sessions[i];
        int fd = s && s->close_requested && !s->close_queued ? s->fd : -1;
        if (fd >= 0) s->close_queued = true;
        xSemaphoreGive(s_sessions_lock);
        if (fd >= 0 && httpd_sess_trigger_close(s_server, fd) != ESP_OK) {
            xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
            for (size_t j = 0; j < MAX_TLS_SOCKETS; ++j) {
                if (s_sessions[j] && s_sessions[j]->fd == fd) s_sessions[j]->close_queued = false;
            }
            xSemaphoreGive(s_sessions_lock);
        }
    }
    atomic_store(&s_expiry_work_pending, false);
}

static void deadline_task(void *arg)
{
    (void)arg;
    for (;;) {
        bool need_close = false;
        uint64_t now = esp_timer_get_time();
        xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
        for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) {
            signaling_session_t *s = s_sessions[i];
            if (!s || s->fd < 0) continue;
            if (!s->close_requested && (session_deadline_expired(s, now) ||
                (s->transport_bound && !transport_client_session_active(
                    s->challenge.client_fingerprint, s->transport_generation)))) {
                /* Interrupt a slow frame read; HTTPD still owns close/free.
                   close_socket uses this mutex before releasing the fd. */
                request_session_close(s);
            }
            need_close |= s->close_requested && !s->close_queued;
        }
        xSemaphoreGive(s_sessions_lock);
        bool pending = false;
        if (need_close && atomic_compare_exchange_strong(&s_expiry_work_pending, &pending, true) &&
            httpd_queue_work(s_server, close_expired_sessions, NULL) != ESP_OK) {
            atomic_store(&s_expiry_work_pending, false);
        }
        vTaskDelay(pdMS_TO_TICKS(250));
    }
}

static bool consume_handshake_token(void)
{
    int64_t now = esp_timer_get_time();
    bool admitted = false;
    portENTER_CRITICAL(&s_admission_mux);
    if (now > s_handshake_refill_us) {
        uint64_t elapsed = (uint64_t)(now - s_handshake_refill_us);
        uint64_t add = elapsed / HANDSHAKE_REFILL_US;
        if (add) {
            uint16_t tokens = s_handshake_tokens + (uint16_t)add;
            s_handshake_tokens = tokens > HANDSHAKE_BURST ? HANDSHAKE_BURST : (uint8_t)tokens;
            s_handshake_refill_us += add * HANDSHAKE_REFILL_US;
        }
    }
    if (s_handshake_tokens != 0) {
        --s_handshake_tokens;
        admitted = true;
    }
    portEXIT_CRITICAL(&s_admission_mux);
    return admitted;
}

static int limit_tls_handshake(mbedtls_ssl_context *ssl)
{
    if (!consume_handshake_token()) {
        transport_note_tls_rate_limited();
        ESP_LOGW(TAG, "TLS handshake rate limited");
        return MBEDTLS_ERR_SSL_HANDSHAKE_FAILURE;
    }
    int result = mbedtls_ssl_set_hs_own_cert(ssl, &s_tls_certificate, &s_tls_private_key);
    if (result != 0) ESP_LOGE(TAG, "TLS certificate selection failed: -0x%04X", -result);
    else ESP_LOGI(TAG, "TLS certificate selection succeeded");
    return result;
}

static esp_err_t prepare_tls_material(const uint8_t *certificate, size_t certificate_len,
                                     uint32_t private_key_id)
{
    if (s_tls_material_ready) return ESP_OK;
    mbedtls_x509_crt_init(&s_tls_certificate);
    mbedtls_pk_init(&s_tls_private_key);
    int result = mbedtls_x509_crt_parse(&s_tls_certificate, certificate, certificate_len);
    if (result == 0) result = mbedtls_pk_wrap_psa(&s_tls_private_key, (psa_key_id_t)private_key_id);
    if (result != 0) {
        ESP_LOGE(TAG, "TLS certificate selection material failed: -0x%04X", -result);
        mbedtls_pk_free(&s_tls_private_key);
        mbedtls_x509_crt_free(&s_tls_certificate);
        return ESP_FAIL;
    }
    if (!mbedtls_pk_can_do_psa(&s_tls_private_key,
                               MBEDTLS_PK_ALG_ECDSA(PSA_ALG_SHA_256),
                               PSA_KEY_USAGE_SIGN_HASH)) {
        ESP_LOGE(TAG, "TLS key is incompatible with the configured ECDSA signature mode");
        mbedtls_pk_free(&s_tls_private_key);
        mbedtls_x509_crt_free(&s_tls_certificate);
        return ESP_FAIL;
    }
    s_tls_material_ready = true;
    return ESP_OK;
}

static bool read_head(cbor_reader_t *r, uint8_t expected_major, uint64_t *value)
{
    if (!r || r->pos >= r->len || !value) return false;
    uint8_t first = r->data[r->pos++];
    if ((first >> 5) != expected_major) return false;
    uint8_t ai = first & 31;
    if (ai <= 23) { *value = ai; return true; }
    size_t bytes = ai == 24 ? 1 : ai == 25 ? 2 : ai == 26 ? 4 : ai == 27 ? 8 : 0;
    if (!bytes || bytes > r->len - r->pos) return false;
    uint64_t n = 0;
    for (size_t i = 0; i < bytes; ++i) n = (n << 8) | r->data[r->pos++];
    if ((bytes == 1 && n < 24) || (bytes == 2 && n <= 0xff) ||
        (bytes == 4 && n <= 0xffff) || (bytes == 8 && n <= 0xffffffff)) return false;
    *value = n;
    return true;
}

static bool read_uint(cbor_reader_t *r, uint64_t expected)
{
    uint64_t value;
    return read_head(r, 0, &value) && value == expected;
}

static bool read_item(cbor_reader_t *r, uint8_t major, const uint8_t **value, size_t *len)
{
    uint64_t n;
    if (!read_head(r, major, &n) || n > r->len - r->pos) return false;
    *value = r->data + r->pos;
    *len = (size_t)n;
    r->pos += *len;
    return true;
}

static bool skip_item(cbor_reader_t *r, unsigned depth)
{
    if (depth > 4 || r->pos >= r->len) return false;
    uint8_t major = r->data[r->pos] >> 5;
    uint64_t count;
    if (major == 0) return read_head(r, 0, &count);
    if (major == 2 || major == 3) {
        const uint8_t *p; size_t n;
        return read_item(r, major, &p, &n);
    }
    if (major == 4 || major == 5) {
        if (!read_head(r, major, &count) || count > 32) return false;
        uint64_t previous = 0;
        for (uint64_t i = 0; i < count * (major == 5 ? 2 : 1); ++i) {
            size_t start = r->pos;
            if (!skip_item(r, depth + 1)) return false;
            if (major == 5 && !(i & 1)) {
                cbor_reader_t key = {.data = r->data + start, .len = r->pos - start};
                uint64_t current;
                if (!read_head(&key, 0, &current) || key.pos != key.len || (i && current <= previous)) return false;
                previous = current;
            }
        }
        return true;
    }
    return false;
}

static bool parse_envelope(const uint8_t *data, size_t len, signaling_session_t *session,
                           uint64_t *type, uint64_t *sequence, cbor_reader_t *payload)
{
    cbor_reader_t r = {.data = data, .len = len};
    uint64_t count;
    const uint8_t *bytes; size_t bytes_len;
    if (!read_head(&r, 5, &count) || count != 7 ||
        !read_uint(&r, 0) || !read_uint(&r, 1) ||
        !read_uint(&r, 1) || !read_uint(&r, 0) ||
        !read_uint(&r, 2) || !read_head(&r, 0, type) ||
        !read_uint(&r, 3) || !read_item(&r, 2, &bytes, &bytes_len) || bytes_len != 16 ||
        !read_uint(&r, 4) || !read_uint(&r, 0) ||
        !read_uint(&r, 5) || !read_head(&r, 0, sequence) ||
        !read_uint(&r, 6)) return false;
    size_t start = r.pos;
    if (!skip_item(&r, 0) || r.pos != r.len) return false;
    if (session->phase == SESSION_WAIT_HELLO && *type == 0x01 && *sequence == 0) {
        memcpy(session->challenge.session_id, bytes, 16);
        uint8_t any = 0;
        for (size_t i = 0; i < 16; ++i) any |= bytes[i];
        if (any == 0) return false;
    } else if (memcmp(bytes, session->challenge.session_id, 16) != 0) {
        return false;
    }
    *payload = (cbor_reader_t){.data = data + start, .len = r.len - start};
    return true;
}

static bool read_payload_bytes(cbor_reader_t *payload, uint8_t *out, size_t expected)
{
    const uint8_t *bytes; size_t len;
    if (!read_item(payload, 2, &bytes, &len) || len != expected) return false;
    memcpy(out, bytes, len);
    return true;
}

static bool write_head(cbor_writer_t *w, uint8_t major, uint64_t value)
{
    if (value <= 23) {
        if (w->pos >= w->capacity) return false;
        w->data[w->pos++] = (uint8_t)((major << 5) | value);
    } else if (value <= 0xff) {
        if (w->capacity - w->pos < 2) return false;
        w->data[w->pos++] = (uint8_t)((major << 5) | 24);
        w->data[w->pos++] = (uint8_t)value;
    } else if (value <= 0xffff) {
        if (w->capacity - w->pos < 3) return false;
        w->data[w->pos++] = (uint8_t)((major << 5) | 25);
        w->data[w->pos++] = (uint8_t)(value >> 8);
        w->data[w->pos++] = (uint8_t)value;
    } else if (value <= 0xffffffff) {
        if (w->capacity - w->pos < 5) return false;
        w->data[w->pos++] = (uint8_t)((major << 5) | 26);
        for (int shift = 24; shift >= 0; shift -= 8) w->data[w->pos++] = (uint8_t)(value >> shift);
    } else {
        if (w->capacity - w->pos < 9) return false;
        w->data[w->pos++] = (uint8_t)((major << 5) | 27);
        for (int shift = 56; shift >= 0; shift -= 8) w->data[w->pos++] = (uint8_t)(value >> shift);
    }
    return true;
}

static bool write_uint(cbor_writer_t *w, uint64_t value) { return write_head(w, 0, value); }

static bool write_bytes(cbor_writer_t *w, const uint8_t *data, size_t len)
{
    if (!data || !write_head(w, 2, len) || len > w->capacity - w->pos) return false;
    memcpy(w->data + w->pos, data, len);
    w->pos += len;
    return true;
}

static bool write_text(cbor_writer_t *w, const char *text)
{
    size_t len = strlen(text);
    if (!write_head(w, 3, len) || len > w->capacity - w->pos) return false;
    memcpy(w->data + w->pos, text, len);
    w->pos += len;
    return true;
}

static bool begin_envelope(cbor_writer_t *w, signaling_session_t *s, uint64_t type,
                           uint64_t payload_items)
{
    return write_head(w, 5, 7) && write_uint(w, 0) && write_uint(w, 1) &&
           write_uint(w, 1) && write_uint(w, 0) && write_uint(w, 2) && write_uint(w, type) &&
           write_uint(w, 3) && write_bytes(w, s->challenge.session_id, 16) &&
           write_uint(w, 4) && write_uint(w, 0) && write_uint(w, 5) && write_uint(w, s->transmit_sequence++) &&
           write_uint(w, 6) && write_head(w, 5, payload_items);
}

static esp_err_t send_frame(httpd_req_t *req, signaling_session_t *s, uint64_t type,
                            uint64_t payload_items, const uint8_t *fields, size_t fields_len)
{
    cbor_writer_t w = {.data = s->message, .capacity = MAX_WSS_MESSAGE};
    if (!begin_envelope(&w, s, type, payload_items) || fields_len > w.capacity - w.pos) return ESP_ERR_INVALID_SIZE;
    if (fields_len) memcpy(w.data + w.pos, fields, fields_len);
    w.pos += fields_len;
    httpd_ws_frame_t frame = {.type = HTTPD_WS_TYPE_BINARY, .payload = w.data, .len = w.pos};
    return httpd_ws_send_frame(req, &frame);
}

static esp_err_t send_error(httpd_req_t *req, signaling_session_t *s, const char *code)
{
    uint8_t payload[64];
    cbor_writer_t w = {.data = payload, .capacity = sizeof(payload)};
    if (!write_uint(&w, 0) || !write_text(&w, code)) return ESP_ERR_INVALID_SIZE;
    return send_frame(req, s, 0x0a, 1, payload, w.pos);
}

static esp_err_t send_challenge(httpd_req_t *req, signaling_session_t *s)
{
    uint8_t spki[DEVICE_IDENTITY_SPKI_MAX], signature[72], payload[384];
    size_t spki_len = 0, signature_len = 0;
    esp_err_t err = device_identity_status(s->challenge.endpoint_fingerprint, s->challenge.tls_spki_hash,
                                            spki, &spki_len);
    if (err == ESP_OK) err = psa_generate_random(s->challenge.device_nonce, 32) == PSA_SUCCESS ? ESP_OK : ESP_FAIL;
    if (err == ESP_OK) err = signaling_sign_challenge(&s->challenge, signature, sizeof(signature), &signature_len);
    if (err != ESP_OK) goto done;
    cbor_writer_t w = {.data = payload, .capacity = sizeof(payload)};
    bool ok = write_uint(&w, 0) && write_bytes(&w, s->challenge.endpoint_fingerprint, 32) &&
              write_uint(&w, 1) && write_bytes(&w, s->challenge.device_nonce, 32) && write_uint(&w, 2) &&
              write_head(&w, 4, 2) && write_text(&w, "control") && write_text(&w, "motion") &&
              write_uint(&w, 3) && write_bytes(&w, s->challenge.tls_spki_hash, 32) &&
              write_uint(&w, 4) && write_bytes(&w, signature, signature_len) &&
              write_uint(&w, 5) && write_bytes(&w, spki, spki_len);
    err = ok ? send_frame(req, s, 0x02, 6, payload, w.pos) : ESP_ERR_INVALID_SIZE;
done:
    mbedtls_platform_zeroize(spki, sizeof(spki));
    mbedtls_platform_zeroize(signature, sizeof(signature));
    mbedtls_platform_zeroize(payload, sizeof(payload));
    return err;
}

static bool session_id_seen_or_add(const uint8_t id[16])
{
    bool seen = false;
    portENTER_CRITICAL(&s_admission_mux);
    for (size_t i = 0; i < s_seen_session_count; ++i) {
        if (memcmp(s_seen_session_ids[i], id, 16) == 0) { seen = true; break; }
    }
    if (!seen) {
        memcpy(s_seen_session_ids[s_seen_session_next], id, 16);
        if (s_seen_session_count < 8) ++s_seen_session_count;
        s_seen_session_next = (uint8_t)((s_seen_session_next + 1) % 8);
    }
    portEXIT_CRITICAL(&s_admission_mux);
    return seen;
}

static bool parse_fingerprint(const uint8_t *sdp, size_t len, uint8_t out[32])
{
    static const char prefix[] = "a=fingerprint:sha-256 ";
    size_t prefix_len = sizeof(prefix) - 1;
    size_t matches = 0;
    for (size_t i = 0; i + prefix_len < len; ++i) {
        if (i && sdp[i - 1] != '\n') continue;
        if (memcmp(sdp + i, prefix, prefix_len) != 0) continue;
        size_t start = i + prefix_len;
        if (start + 95 > len) return false;
        for (size_t j = 0; j < 32; ++j) {
            int a = sdp[start + j * 3], b = sdp[start + j * 3 + 1];
            a = a >= '0' && a <= '9' ? a - '0' : (a >= 'a' && a <= 'f' ? a - 'a' + 10 : (a >= 'A' && a <= 'F' ? a - 'A' + 10 : -1));
            b = b >= '0' && b <= '9' ? b - '0' : (b >= 'a' && b <= 'f' ? b - 'a' + 10 : (b >= 'A' && b <= 'F' ? b - 'A' + 10 : -1));
            if (a < 0 || b < 0 || (j < 31 && sdp[start + j * 3 + 2] != ':')) return false;
            out[j] = (uint8_t)((a << 4) | b);
        }
        size_t end = start + 95;
        if (end < len && sdp[end] != '\r' && sdp[end] != '\n') return false;
        ++matches;
    }
    return matches == 1;
}

static bool parse_hello(cbor_reader_t *payload, signaling_session_t *s)
{
    uint64_t count;
    if (!read_head(payload, 5, &count) || count != 2 || !read_uint(payload, 0) ||
        !read_payload_bytes(payload, s->challenge.client_fingerprint, 32) ||
        !read_uint(payload, 1) || !read_payload_bytes(payload, s->challenge.client_nonce, 32)) return false;
    return payload->pos == payload->len;
}

static bool parse_signature(cbor_reader_t *payload, uint8_t signature[72], size_t *signature_len)
{
    uint64_t count;
    const uint8_t *data; size_t len;
    if (!read_head(payload, 5, &count) || count != 1 || !read_uint(payload, 0) ||
        !read_item(payload, 2, &data, &len) || len < 8 || len > 72 || payload->pos != payload->len) return false;
    memcpy(signature, data, len);
    *signature_len = len;
    return true;
}

static esp_err_t process_message(httpd_req_t *req, signaling_session_t *s, size_t len)
{
    uint64_t type, sequence;
    cbor_reader_t payload;
    if (!session_can_process(s) ||
        !parse_envelope(s->message, len, s, &type, &sequence, &payload) || sequence != s->receive_sequence++) {
        send_error(req, s, "replay");
        return ESP_FAIL;
    }
    if (s->phase == SESSION_WAIT_HELLO && type == 0x01) {
        xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
        bool hello_valid = parse_hello(&payload, s);
        xSemaphoreGive(s_sessions_lock);
        if (!hello_valid || session_id_seen_or_add(s->challenge.session_id)) {
            send_error(req, s, "replay"); return ESP_FAIL;
        }
        if (!client_auth_is_enrolled(s->challenge.client_fingerprint)) {
            send_error(req, s, "unauthorized"); return ESP_FAIL;
        }
        if (!set_session_phase(s, SESSION_WAIT_CLIENT_PROOF, AUTH_HANDSHAKE_TIMEOUT_US)) return ESP_FAIL;
        esp_err_t err = send_challenge(req, s);
        if (err != ESP_OK) return err;
        return ESP_OK;
    }
    if (s->phase == SESSION_WAIT_CLIENT_PROOF && type == 0x03) {
        uint8_t signature[72]; size_t signature_len = 0;
        if (!parse_signature(&payload, signature, &signature_len) ||
            signaling_verify_client_challenge(&s->challenge, signature, signature_len) != ESP_OK) {
            mbedtls_platform_zeroize(signature, sizeof(signature));
            send_error(req, s, "unauthorized"); return ESP_FAIL;
        }
        mbedtls_platform_zeroize(signature, sizeof(signature));
        return set_session_phase(s, SESSION_WAIT_OFFER, 0) ? ESP_OK : ESP_FAIL;
    }
    if (s->phase == SESSION_WAIT_OFFER && type == 0x04) {
        uint64_t count; const uint8_t *offer; size_t offer_len;
        if (!read_head(&payload, 5, &count) || count != 1 || !read_uint(&payload, 0) ||
            !read_item(&payload, 3, &offer, &offer_len) || payload.pos != payload.len ||
            offer_len < 5 || offer_len > MAX_SDP_BYTES || memcmp(offer, "v=0\r\n", 5) != 0 ||
            !parse_fingerprint(offer, offer_len, s->transcript.client_dtls_fingerprint)) {
            send_error(req, s, "invalid_request"); return ESP_FAIL;
        }
        size_t hash_len = 0;
        if (psa_hash_compute(PSA_ALG_SHA_256, offer, offer_len, s->transcript.offer_hash,
                             sizeof(s->transcript.offer_hash), &hash_len) != PSA_SUCCESS ||
            hash_len != sizeof(s->transcript.offer_hash)) return ESP_FAIL;
        esp_err_t err = transport_offer_for_client(s->challenge.client_fingerprint,
                                                    s->challenge.session_id, offer, offer_len,
                                                    s->answer, MAX_SDP_BYTES, &s->answer_len,
                                                    &s->transport_generation);
        if (err == ESP_ERR_INVALID_STATE) { send_error(req, s, "busy"); return ESP_FAIL; }
        if (err == ESP_OK) {
            xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
            s->transport_bound = true;
            xSemaphoreGive(s_sessions_lock);
        }
        if (err != ESP_OK || !parse_fingerprint(s->answer, s->answer_len,
                                                s->transcript.endpoint_dtls_fingerprint)) {
            send_error(req, s, "identity_failed"); return ESP_FAIL;
        }
        if (psa_hash_compute(PSA_ALG_SHA_256, s->answer, s->answer_len, s->transcript.answer_hash,
                             sizeof(s->transcript.answer_hash), &hash_len) != PSA_SUCCESS ||
            hash_len != sizeof(s->transcript.answer_hash)) return ESP_FAIL;
        s->transcript.challenge = s->challenge;
        cbor_writer_t w = {.data = s->message, .capacity = MAX_WSS_MESSAGE};
        if (!begin_envelope(&w, s, 0x05, 1) || !write_uint(&w, 0) ||
            !write_head(&w, 3, s->answer_len) || s->answer_len > w.capacity - w.pos) return ESP_ERR_INVALID_SIZE;
        memcpy(w.data + w.pos, s->answer, s->answer_len);
        w.pos += s->answer_len;
        httpd_ws_frame_t frame = {.type = HTTPD_WS_TYPE_BINARY, .payload = w.data, .len = w.pos};
        err = httpd_ws_send_frame(req, &frame);
        if (err != ESP_OK) return err;
        uint8_t signature[72]; size_t signature_len = 0;
        err = signaling_sign_transcript(&s->transcript, signature, sizeof(signature), &signature_len);
        if (err != ESP_OK) { mbedtls_platform_zeroize(signature, sizeof(signature)); return err; }
        cbor_writer_t proof = {.data = s->message, .capacity = MAX_WSS_MESSAGE};
        bool ok = begin_envelope(&proof, s, 0x08, 1) && write_uint(&proof, 0) &&
                  write_bytes(&proof, signature, signature_len);
        mbedtls_platform_zeroize(signature, sizeof(signature));
        if (!ok) return ESP_ERR_INVALID_SIZE;
        frame = (httpd_ws_frame_t){.type = HTTPD_WS_TYPE_BINARY, .payload = proof.data, .len = proof.pos};
        err = httpd_ws_send_frame(req, &frame);
        if (err != ESP_OK) return err;
        return set_session_phase(s, SESSION_WAIT_TRANSCRIPT, AUTH_HANDSHAKE_TIMEOUT_US) ? ESP_OK : ESP_FAIL;
    }
    if (s->phase == SESSION_WAIT_TRANSCRIPT && type == 0x08) {
        uint8_t signature[72]; size_t signature_len = 0;
        if (!parse_signature(&payload, signature, &signature_len) ||
            signaling_verify_client_transcript(&s->transcript, signature, signature_len) != ESP_OK) {
            mbedtls_platform_zeroize(signature, sizeof(signature));
            send_error(req, s, "unauthorized"); return ESP_FAIL;
        }
        mbedtls_platform_zeroize(signature, sizeof(signature));
        if (!s->transport_bound ||
            transport_authorize_client(s->challenge.client_fingerprint, s->transport_generation) != ESP_OK) {
            send_error(req, s, "identity_failed"); return ESP_FAIL;
        }
        esp_err_t err = send_frame(req, s, 0x09, 0, NULL, 0);
        if (err == ESP_OK && !set_session_phase(s, SESSION_READY, 0)) return ESP_FAIL;
        return err;
    }
    if (s->phase == SESSION_READY) return ESP_OK;
    send_error(req, s, "invalid_request");
    return ESP_FAIL;
}

static void free_session(void *ctx)
{
    signaling_session_t *s = ctx;
    if (!s) return;
    uint8_t client_fingerprint[32];
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    bool transport_bound = s->transport_bound;
    uint32_t transport_generation = s->transport_generation;
    memcpy(client_fingerprint, s->challenge.client_fingerprint, sizeof(client_fingerprint));
    for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) if (s_sessions[i] == s) s_sessions[i] = NULL;
    xSemaphoreGive(s_sessions_lock);
    if (transport_bound) {
        transport_close_for_client(client_fingerprint, transport_generation);
    }
    mbedtls_platform_zeroize(client_fingerprint, sizeof(client_fingerprint));
    if (s->message) {
        mbedtls_platform_zeroize(s->message, MAX_WSS_MESSAGE);
        heap_caps_free(s->message);
    }
    if (s->answer) {
        mbedtls_platform_zeroize(s->answer, MAX_SDP_BYTES);
        heap_caps_free(s->answer);
    }
    mbedtls_platform_zeroize(s, sizeof(*s));
    heap_caps_free(s);
}

static esp_err_t ws_pre_handshake(httpd_req_t *req)
{
    size_t extension_len = httpd_req_get_hdr_value_len(req, "Sec-WebSocket-Extensions");
    size_t protocol_len = httpd_req_get_hdr_value_len(req, "Sec-WebSocket-Protocol");
    char protocol[24] = {0};
    if (extension_len != 0 || protocol_len != 9 ||
        httpd_req_get_hdr_value_str(req, "Sec-WebSocket-Protocol", protocol, sizeof(protocol)) != ESP_OK ||
        strcmp(protocol, "kvmctl.v1") != 0) {
        httpd_resp_set_status(req, "400 Bad Request");
        httpd_resp_send(req, NULL, 0);
        return ESP_FAIL;
    }
    return ESP_OK;
}

#if CONFIG_HTTPD_WS_POST_HANDSHAKE_CB_SUPPORT
static esp_err_t ws_post_handshake(httpd_req_t *req)
{
    int fd = httpd_req_to_sockfd(req);
    signaling_session_t *s = httpd_sess_get_ctx(req->handle, fd);
    if (!s || s->phase != SESSION_WAIT_UPGRADE || !session_can_process(s)) {
        return ESP_ERR_INVALID_STATE;
    }
    s->message = heap_caps_malloc(MAX_WSS_MESSAGE, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    s->answer = heap_caps_malloc(MAX_SDP_BYTES, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!s->message || !s->answer) return ESP_ERR_NO_MEM;
    return set_session_phase(s, SESSION_WAIT_HELLO, 5000000LL) ? ESP_OK : ESP_FAIL;
}
#endif

static esp_err_t ws_handler(httpd_req_t *req)
{
    int fd = httpd_req_to_sockfd(req);
    signaling_session_t *s = httpd_sess_get_ctx(req->handle, fd);
    if (!s || !s->message || !s->answer || !session_can_process(s)) return ESP_ERR_INVALID_STATE;
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    if (!s->message_deadline_us) s->message_deadline_us = esp_timer_get_time() + 5000000LL;
    bool closing = s->close_requested;
    xSemaphoreGive(s_sessions_lock);
    if (closing) return ESP_FAIL;
    httpd_ws_frame_t frame = {0};
    esp_err_t err = httpd_ws_recv_frame(req, &frame, 0);
    if (err != ESP_OK) return err;
    if (frame.type == HTTPD_WS_TYPE_PING || frame.type == HTTPD_WS_TYPE_PONG ||
        frame.type == HTTPD_WS_TYPE_CLOSE) {
        uint8_t control_payload[125];
        if (!frame.final || frame.len > sizeof(control_payload)) return ESP_FAIL;
        frame.payload = control_payload;
        if (frame.len && httpd_ws_recv_frame(req, &frame, frame.len) != ESP_OK) return ESP_FAIL;
        bool close = frame.type == HTTPD_WS_TYPE_CLOSE;
        if (frame.type == HTTPD_WS_TYPE_PING) frame.type = HTTPD_WS_TYPE_PONG;
        else if (!close) goto control_done;
        err = httpd_ws_send_frame(req, &frame);
control_done:
        xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
        if (!s->partial_len) s->message_deadline_us = 0;
        xSemaphoreGive(s_sessions_lock);
        return close ? ESP_FAIL : err;
    }
    if (frame.len == 0) return ESP_FAIL;
    if (frame.len > MAX_WSS_MESSAGE - s->partial_len) {
        send_error(req, s, "resource_exhausted");
        return ESP_FAIL;
    }
    if ((s->partial_len == 0 && frame.type != HTTPD_WS_TYPE_BINARY) ||
        (s->partial_len != 0 && frame.type != HTTPD_WS_TYPE_CONTINUE)) return ESP_FAIL;
    if (s->partial_len == 0) s->fragment_type = frame.type;
    frame.payload = s->message + s->partial_len;
    err = httpd_ws_recv_frame(req, &frame, frame.len);
    if (err != ESP_OK) return err;
    s->partial_len += frame.len;
    if (!frame.final) return ESP_OK;
    size_t message_len = s->partial_len;
    s->partial_len = 0;
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    s->message_deadline_us = 0;
    xSemaphoreGive(s_sessions_lock);
    return process_message(req, s, message_len);
}

static esp_err_t admit_socket(httpd_handle_t handle, int fd)
{
    /* This callback runs after TLS succeeds, so prior handshake diagnostics are stale. */
    transport_clear_tls_diagnostic();
    signaling_session_t *s = heap_caps_calloc(1, sizeof(*s), MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!s) { shutdown(fd, SHUT_RDWR); return ESP_ERR_NO_MEM; }
    bool admitted = false;
    portENTER_CRITICAL(&s_admission_mux);
    if (atomic_load(&s_tls_sockets) < MAX_TLS_SOCKETS) {
        for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) {
            if (s_admitted_fds[i] == -1) {
                s_admitted_fds[i] = fd;
                atomic_fetch_add(&s_tls_sockets, 1);
                admitted = true;
                break;
            }
        }
    }
    portEXIT_CRITICAL(&s_admission_mux);
    if (!admitted) {
        ESP_LOGW(TAG, "TLS admission busy or rate limited");
        heap_caps_free(s);
        shutdown(fd, SHUT_RDWR);
        return ESP_FAIL;
    }
    s->fd = fd;
    s->started_us = esp_timer_get_time();
    s->phase_deadline_us = s->started_us + 5000000LL;
    s->phase = SESSION_WAIT_UPGRADE;
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    size_t slot = MAX_TLS_SOCKETS;
    for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) if (!s_sessions[i]) { slot = i; break; }
    if (slot < MAX_TLS_SOCKETS) s_sessions[slot] = s;
    xSemaphoreGive(s_sessions_lock);
    if (slot == MAX_TLS_SOCKETS) {
        free_session(s);
        shutdown(fd, SHUT_RDWR);
        return ESP_ERR_NO_MEM;
    }
    httpd_sess_set_ctx(handle, fd, s, free_session);
    return ESP_OK;
}

static void close_socket(httpd_handle_t handle, int fd)
{
    (void)handle;
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) {
        if (s_sessions[i] && s_sessions[i]->fd == fd) s_sessions[i]->fd = -1;
    }
    xSemaphoreGive(s_sessions_lock);
    portENTER_CRITICAL(&s_admission_mux);
    for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) {
        if (s_admitted_fds[i] == fd) {
            s_admitted_fds[i] = -1;
            atomic_fetch_sub(&s_tls_sockets, 1);
            break;
        }
    }
    portEXIT_CRITICAL(&s_admission_mux);
    close(fd);
}

static const httpd_uri_t s_ws_uri = {
    .uri = "/v1/ws",
    .method = HTTP_GET,
    .handler = ws_handler,
    .user_ctx = NULL,
    .is_websocket = true,
    /* Receive control frames through the same deadline as binary fragments. */
    .handle_ws_control_frames = true,
    .supported_subprotocol = "kvmctl.v1",
#if CONFIG_HTTPD_WS_PRE_HANDSHAKE_CB_SUPPORT
    .ws_pre_handshake_cb = ws_pre_handshake,
#endif
#if CONFIG_HTTPD_WS_POST_HANDSHAKE_CB_SUPPORT
    /* IDF 6.1 skips the URI handler for the upgrade request itself. */
    .ws_post_handshake_cb = ws_post_handshake,
#endif
};

esp_err_t signaling_init(void)
{
#if !CONFIG_ESP_HTTPS_SERVER_ENABLE || !CONFIG_ESP_HTTPS_SERVER_CERT_SELECT_HOOK || \
    !CONFIG_HTTPD_WS_SUPPORT || !CONFIG_HTTPD_WS_PRE_HANDSHAKE_CB_SUPPORT || \
    !CONFIG_HTTPD_WS_POST_HANDSHAKE_CB_SUPPORT || !CONFIG_HTTPD_WS_STRICTER_RFC6455
    return ESP_ERR_NOT_SUPPORTED;
#else
    if (s_server) return ESP_OK;
    if (!s_sessions_lock) s_sessions_lock = xSemaphoreCreateMutex();
    if (!s_sessions_lock) return ESP_ERR_NO_MEM;
    const uint8_t *certificate = NULL;
    size_t certificate_len = 0;
    uint32_t private_key_id = 0;
    char hostname[DEVICE_IDENTITY_TLS_NAME_MAX];
    esp_err_t err = device_identity_tls_material(&certificate, &certificate_len,
                                                  &private_key_id, hostname);
    if (err != ESP_OK) return err;
    err = prepare_tls_material(certificate, certificate_len, private_key_id);
    if (err != ESP_OK) return err;
    static const int tls_ciphersuites[] = {MBEDTLS_TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256, 0};
    portENTER_CRITICAL(&s_admission_mux);
    s_handshake_tokens = HANDSHAKE_BURST;
    s_handshake_refill_us = esp_timer_get_time();
    portEXIT_CRITICAL(&s_admission_mux);
    httpd_ssl_config_t config = HTTPD_SSL_CONFIG_DEFAULT();
    config.servercert = certificate;
    config.servercert_len = certificate_len;
    config.server_key = NULL;
    config.cert_select_cb = limit_tls_handshake;
    config.tls_handshake_timeout_ms = 5000;
    config.tls_version = ESP_TLS_VER_TLS_1_2;
    config.ciphersuites_list = tls_ciphersuites;
    config.port_secure = 443;
    config.session_tickets = false;
    config.httpd.max_open_sockets = MAX_TLS_SOCKETS;
    config.httpd.backlog_conn = MAX_TLS_SOCKETS;
    config.httpd.max_req_hdr_len = 1024;
    config.httpd.max_uri_len = 64;
    config.httpd.max_uri_handlers = 1;
    config.httpd.recv_wait_timeout = 2;
    config.httpd.send_wait_timeout = 2;
    config.httpd.lru_purge_enable = false;
    config.httpd.open_fn = admit_socket;
    config.httpd.close_fn = close_socket;
    err = httpd_ssl_start(&s_server, &config);
    if (err != ESP_OK) return err;
    err = httpd_register_uri_handler(s_server, &s_ws_uri);
    if (err != ESP_OK) {
        httpd_ssl_stop(s_server);
        s_server = NULL;
        return err;
    }
    uint8_t endpoint_fingerprint[32], tls_spki_hash[32], identity_spki[DEVICE_IDENTITY_SPKI_MAX];
    size_t identity_spki_len = 0;
    char fingerprint_hex[65];
    err = device_identity_status(endpoint_fingerprint, tls_spki_hash, identity_spki, &identity_spki_len);
    if (err == ESP_OK) {
        static const char hex[] = "0123456789abcdef";
        for (size_t i = 0; i < sizeof(endpoint_fingerprint); ++i) {
            fingerprint_hex[i * 2] = hex[endpoint_fingerprint[i] >> 4];
            fingerprint_hex[i * 2 + 1] = hex[endpoint_fingerprint[i] & 15];
        }
        fingerprint_hex[64] = 0;
        char service_fingerprint[65];
        char mdns_hostname[DEVICE_IDENTITY_TLS_NAME_MAX];
        size_t mdns_hostname_len = strlen(hostname);
        static const char local_suffix[] = ".local";
        if (mdns_hostname_len <= sizeof(local_suffix) - 1 ||
            strcmp(hostname + mdns_hostname_len - (sizeof(local_suffix) - 1), local_suffix) != 0) {
            err = ESP_ERR_INVALID_ARG;
        } else {
            mdns_hostname_len -= sizeof(local_suffix) - 1;
            memcpy(mdns_hostname, hostname, mdns_hostname_len);
            mdns_hostname[mdns_hostname_len] = 0;
        }
        memcpy(service_fingerprint, fingerprint_hex, sizeof(service_fingerprint));
        mdns_txt_item_t txt[] = {
            {.key = "fp", .value = service_fingerprint},
            {.key = "path", .value = "/v1/ws"},
            {.key = "v", .value = "1"},
        };
        if (err == ESP_OK) err = mdns_init();
        if (err == ESP_OK) {
            s_mdns_started = true;
            err = mdns_hostname_set(mdns_hostname);
        }
        if (err == ESP_OK) err = mdns_service_add("KVMCTL", "_kvmctl", "_tcp", 443, txt,
                                                  sizeof(txt) / sizeof(txt[0]));
        mbedtls_platform_zeroize(service_fingerprint, sizeof(service_fingerprint));
        mbedtls_platform_zeroize(mdns_hostname, sizeof(mdns_hostname));
        mbedtls_platform_zeroize(fingerprint_hex, sizeof(fingerprint_hex));
    }
    mbedtls_platform_zeroize(endpoint_fingerprint, sizeof(endpoint_fingerprint));
    mbedtls_platform_zeroize(tls_spki_hash, sizeof(tls_spki_hash));
    mbedtls_platform_zeroize(identity_spki, sizeof(identity_spki));
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "DNS-SD advertisement unavailable: %s", esp_err_to_name(err));
        httpd_ssl_stop(s_server);
        s_server = NULL;
        if (s_mdns_started) { mdns_free(); s_mdns_started = false; }
        return err;
    }
    if (xTaskCreate(deadline_task, "signal_deadline", 3072, NULL, 4, NULL) != pdPASS) {
        httpd_ssl_stop(s_server);
        s_server = NULL;
        if (s_mdns_started) { mdns_free(); s_mdns_started = false; }
        return ESP_ERR_NO_MEM;
    }
    ESP_LOGI(TAG, "Pinned WSS signaling on %s:443/v1/ws with _kvmctl._tcp discovery; sockets=%d burst=%d refill_ms=%lld",
             hostname, MAX_TLS_SOCKETS, HANDSHAKE_BURST, HANDSHAKE_REFILL_US / 1000);
    return ESP_OK;
#endif
}

void signaling_invalidate_client(const uint8_t fingerprint[32])
{
    if (!fingerprint || !s_server) return;
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) {
        signaling_session_t *s = s_sessions[i];
        if (s &&
            memcmp(s->challenge.client_fingerprint, fingerprint, 32) == 0) {
            request_session_close(s);
        }
    }
    xSemaphoreGive(s_sessions_lock);
}

void signaling_invalidate_all_clients(void)
{
    if (!s_server) return;
    xSemaphoreTake(s_sessions_lock, portMAX_DELAY);
    for (size_t i = 0; i < MAX_TLS_SOCKETS; ++i) {
        signaling_session_t *s = s_sessions[i];
        if (s) request_session_close(s);
    }
    xSemaphoreGive(s_sessions_lock);
}
