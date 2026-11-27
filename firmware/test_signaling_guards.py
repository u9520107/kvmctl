"""Check production transport ownership and signaling deadline guards.

Uses clang/lld or MSVC to build a temporary shared library, never a standalone exe.
Hardware checks still cover socket expiry, actual DTLS, and heap recovery.
"""
import argparse
import ctypes
from pathlib import Path
import subprocess
import sys
import tempfile


def function(source: str, name: str) -> str:
    start = source.rfind("\n", 0, source.index(name + "(")) + 1
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


root = Path(__file__).resolve().parent
transport = (root / "main/transport.c").read_text()
signaling = (root / "main/signaling.c").read_text()
client_auth = (root / "main/client_auth.c").read_text()
guards = "\n".join(function(transport, name) for name in (
    "fail_session", "peer_disconnected", "on_state", "on_channel_close",
    "find_offer_slot", "transport_status",
    "find_slot", "transport_client_session_active", "transport_close_for_client",
    "transport_authorize_client", "transport_invalidate_client", "transport_invalidate_all_clients"
)) + "\n" + function(signaling, "session_deadline_expired")
guards += "\n" + "\n".join(function(client_auth, name) for name in (
    "client_auth_revoke", "client_auth_revoke_all"
))
harness = r"""
#include <stddef.h>
typedef unsigned char uint8_t;
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;
typedef unsigned int EventBits_t;
typedef int esp_err_t;
typedef _Bool bool;
#define true 1
#define false 0
#define ESP_OK 0
#define ESP_ERR_INVALID_ARG 1
#define ESP_ERR_INVALID_STATE 2
#define ESP_ERR_TIMEOUT 3
#define ESP_ERR_NOT_FOUND 4
#define ESP_FAIL 5
#define BIT_CLOSED 1
#define BIT_FAILED 2
#define pdFALSE 0
#define pdTRUE 1
#define pdMS_TO_TICKS(x) (x)
#define atomic_load(p) (*(p))
#define atomic_store(p, v) (*(p) = (v))
#define portENTER_CRITICAL(p) ((void)(p))
#define portEXIT_CRITICAL(p) ((void)(p))
#define SESSION_READY 4
#define CLIENT_AUTH_SPKI_BYTES 91
#define CLIENT_AUTH_MAX_CLIENTS 8
#define MAX_TRANSPORT_PEERS 2
#define portMAX_DELAY 0
#define xSemaphoreTake(lock, ticks) ((void)(lock), (void)(ticks))
#define xSemaphoreGive(lock) ((void)(lock))
typedef struct {
    bool active, failed, identity_rejected, application_authorized, close_requested, usb;
    unsigned int open_channels;
    uint32_t generation;
    uint8_t client_fingerprint[32];
    void *events;
} transport_peer_t;
typedef enum {
    TRANSPORT_STATUS_IDLE, TRANSPORT_STATUS_CONNECTING, TRANSPORT_STATUS_OPEN,
    TRANSPORT_STATUS_IDENTITY_REJECTED, TRANSPORT_STATUS_FAILED,
} transport_status_t;
typedef enum {
    ESP_PEER_STATE_CONNECT_FAILED, ESP_PEER_STATE_DATA_CHANNEL_DISCONNECTED,
} esp_peer_state_t;
typedef struct { int unused; } esp_peer_data_channel_info_t;
static int s_peer_mux;
static transport_peer_t s_peers[MAX_TRANSPORT_PEERS];
static bool enrolled = true;
static unsigned int waits;
static bool s_ready = true;
static bool s_revoke_persistence_pending;
static int s_lock;
static void recovery_storage_write_lock(void) {}
static void recovery_storage_write_unlock(void) {}
static struct { unsigned int count; uint8_t spki[CLIENT_AUTH_MAX_CLIENTS][CLIENT_AUTH_SPKI_BYTES]; } s_store;
static esp_err_t persist_error = ESP_FAIL;
static unsigned int persist_calls, signaling_invalidations;
typedef struct { unsigned int phase; uint64_t phase_deadline_us, message_deadline_us; } signaling_session_t;
int memcmp(const void *a, const void *b, size_t len) {
    const uint8_t *left = a, *right = b;
    for (size_t i = 0; i < len; ++i) if (left[i] != right[i]) return left[i] - right[i];
    return 0;
}
void *memset(void *out, int value, size_t len) {
    uint8_t *bytes = out;
    for (size_t i = 0; i < len; ++i) bytes[i] = (uint8_t)value;
    return out;
}
void *memmove(void *out, const void *in, size_t len) {
    uint8_t *to = out;
    const uint8_t *from = in;
    if (to < from) for (size_t i = 0; i < len; ++i) to[i] = from[i];
    else while (len) { --len; to[len] = from[len]; }
    return out;
}
static EventBits_t xEventGroupWaitBits(void *event, EventBits_t bits, bool clear, bool all, unsigned int ticks) {
    (void)event; (void)clear; (void)all; (void)ticks; ++waits; return bits;
}
static void xEventGroupSetBits(void *event, EventBits_t bits) { (void)event; (void)bits; }
static bool client_auth_is_enrolled(const uint8_t *fingerprint) { (void)fingerprint; return enrolled; }
static void signaling_invalidate_client(const uint8_t *fingerprint) { (void)fingerprint; ++signaling_invalidations; }
static void signaling_invalidate_all_clients(void) { signaling_invalidate_client(NULL); }
static esp_err_t persist(void) { ++persist_calls; return persist_error; }
static esp_err_t fingerprint_spki(const uint8_t *spki, uint8_t fingerprint[32]) {
    memset(fingerprint, 0, 32); fingerprint[0] = spki[0]; return ESP_OK;
}
GUARDS
#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)
int run_checks(void) {
    uint8_t client_a[32] = {1}, client_b[32] = {2};
    transport_peer_t *peer_a = &s_peers[0], *peer_b = &s_peers[1];
    bool any_active;
    CHECK(find_offer_slot(NULL, &any_active) == peer_a && !any_active);
    peer_a->active = peer_a->usb = true;
    CHECK(find_offer_slot(NULL, &any_active) == NULL && any_active);
    CHECK(find_offer_slot(client_b, &any_active) == NULL && any_active);
    peer_a->usb = false;
    peer_a->client_fingerprint[0] = 1;
    CHECK(find_offer_slot(NULL, &any_active) == NULL && any_active);
    CHECK(find_offer_slot(client_a, &any_active) == NULL && any_active);
    CHECK(find_offer_slot(client_b, &any_active) == peer_b && any_active);

    peer_a->open_channels = 3;
    on_state(ESP_PEER_STATE_DATA_CHANNEL_DISCONNECTED, peer_a);
    CHECK(peer_a->close_requested && !peer_a->failed);
    peer_a->close_requested = false;
    on_channel_close(NULL, peer_a);
    CHECK(peer_a->close_requested && !peer_a->failed);
    peer_a->active = false;
    CHECK(transport_status() == TRANSPORT_STATUS_IDLE);
    peer_a->active = true;
    peer_a->open_channels = 1;
    peer_a->close_requested = false;
    on_channel_close(NULL, peer_a);
    CHECK(!peer_a->close_requested && peer_a->failed);
    peer_a->failed = false;
    on_state(ESP_PEER_STATE_CONNECT_FAILED, peer_a);
    peer_a->active = false;
    CHECK(transport_status() == TRANSPORT_STATUS_FAILED);
    peer_a->identity_rejected = true;
    CHECK(transport_status() == TRANSPORT_STATUS_IDENTITY_REJECTED);
    peer_a->failed = peer_a->identity_rejected = false;
    peer_a->active = true; peer_a->generation = 3; peer_a->client_fingerprint[0] = 1;
    peer_b->active = true; peer_b->generation = 2; peer_b->client_fingerprint[0] = 2;
    peer_a->application_authorized = peer_b->application_authorized = true;
    peer_a->open_channels = peer_b->open_channels = 3;
    CHECK(transport_client_session_active(client_a, 3));
    CHECK(transport_client_session_active(client_b, 2));
    CHECK(!transport_client_session_active(client_a, 1));
    CHECK(!transport_client_session_active(client_b, 1));
    peer_b->failed = true;
    CHECK(!transport_client_session_active(client_b, 2));
    peer_b->failed = false; peer_b->active = false;
    CHECK(!transport_client_session_active(client_b, 2));
    peer_b->active = true;
    CHECK(transport_close_for_client(client_a, 1) == ESP_OK);
    CHECK(transport_close_for_client(client_b, 1) == ESP_OK);
    CHECK(transport_close_for_client(client_a, 2) == ESP_OK);
    CHECK(peer_a->application_authorized && !peer_a->close_requested);
    CHECK(peer_b->application_authorized && !peer_b->close_requested && waits == 0);
    peer_b->application_authorized = false;
    CHECK(transport_authorize_client(client_a, 1) == ESP_ERR_INVALID_STATE);
    CHECK(transport_authorize_client(client_b, 1) == ESP_ERR_INVALID_STATE);
    CHECK(transport_authorize_client(client_a, 2) == ESP_ERR_INVALID_STATE);
    CHECK(!peer_b->application_authorized);
    CHECK(transport_authorize_client(client_b, 2) == ESP_OK && peer_b->application_authorized);
    peer_b->application_authorized = false; peer_b->open_channels = 1;
    CHECK(transport_authorize_client(client_b, 2) == ESP_ERR_INVALID_STATE && !peer_b->application_authorized);
    peer_b->open_channels = 3; peer_b->identity_rejected = true;
    CHECK(transport_authorize_client(client_b, 2) == ESP_ERR_INVALID_STATE && !peer_b->application_authorized);
    peer_b->identity_rejected = false; enrolled = false;
    CHECK(transport_authorize_client(client_b, 2) == ESP_ERR_INVALID_STATE && !peer_b->application_authorized);
    enrolled = true; peer_b->application_authorized = true;
    CHECK(transport_close_for_client(client_b, 2) == ESP_OK);
    CHECK(!peer_b->application_authorized && peer_b->close_requested && waits == 1);
    CHECK(transport_authorize_client(client_b, 2) == ESP_ERR_INVALID_STATE);
    CHECK(!transport_client_session_active(client_b, 2));
    CHECK(peer_a->application_authorized && !peer_a->close_requested);
    signaling_session_t s = {0, 5000, 0};
    CHECK(!session_deadline_expired(&s, 4999));
    CHECK(session_deadline_expired(&s, 5000));
    s.message_deadline_us = 3000;
    CHECK(session_deadline_expired(&s, 3000));
    s.phase = SESSION_READY; s.message_deadline_us = 0;
    CHECK(!session_deadline_expired(&s, 9000));
    s.message_deadline_us = 10000;
    CHECK(!session_deadline_expired(&s, 9999));
    CHECK(session_deadline_expired(&s, 10000));
    /* Inject a persistence failure into the production revoke paths. The
       runtime store must keep revoked clients blocked and report failure. */
    s_store.count = 2; s_store.spki[0][0] = 1; s_store.spki[1][0] = 2;
    peer_b->application_authorized = true; peer_b->close_requested = false;
    CHECK(client_auth_revoke(client_a) == ESP_FAIL);
    CHECK(s_store.count == 1 && s_store.spki[0][0] == 2 && s_store.spki[1][0] == 0);
    CHECK(!peer_a->application_authorized && peer_a->close_requested);
    CHECK(peer_b->application_authorized && !peer_b->close_requested);
    CHECK(signaling_invalidations == 1 && persist_calls == 1);
    CHECK(s_revoke_persistence_pending);
    CHECK(client_auth_revoke(client_a) == ESP_FAIL && persist_calls == 2);
    persist_error = ESP_OK;
    CHECK(client_auth_revoke(client_a) == ESP_OK && persist_calls == 3);
    CHECK(!s_revoke_persistence_pending);
    persist_error = ESP_FAIL;
    CHECK(client_auth_revoke_all() == ESP_FAIL && s_store.count == 0 && s_store.spki[0][0] == 0);
    CHECK(!peer_b->application_authorized && peer_b->close_requested);
    CHECK(signaling_invalidations == 2 && persist_calls == 4);
    CHECK(s_revoke_persistence_pending);
    persist_error = ESP_OK;
    CHECK(client_auth_revoke(client_a) == ESP_OK && persist_calls == 5);
    CHECK(!s_revoke_persistence_pending);
    s_store.count = 1; s_store.spki[0][0] = 1;
    CHECK(client_auth_revoke(client_a) == ESP_OK && s_store.count == 0 && persist_calls == 6);
    return 0;
}
""".replace("GUARDS", guards)
parser = argparse.ArgumentParser()
parser.add_argument("--clang", default="clang")
parser.add_argument("--msvc", help="path to MSVC cl.exe (Windows alternative to clang/lld)")
args = parser.parse_args()
build = root / "build"
build.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="signaling-guards-", dir=build) as temp:
    temp = Path(temp)
    source = temp / "guards.c"
    source.write_text(harness)
    common = [args.clang, "-std=c11", "-Wall", "-Wextra", "-Werror", "-ffreestanding", "-fno-builtin"]
    if sys.platform == "win32":
        obj, library = temp / "guards.obj", temp / "guards.dll"
        if args.msvc:
            subprocess.run([args.msvc, "/nologo", "/std:c11", "/W3", "/WX", "/GS-", "/Zl", "/c",
                            "/TC", "/Fo" + str(obj), str(source)], check=True)
            link = [str(Path(args.msvc).with_name("link.exe")), "/nologo"]
        else:
            subprocess.run(common + ["--target=x86_64-pc-windows-msvc", "-c", str(source), "-o", str(obj)], check=True)
            link = [str(Path(args.clang).with_name("ld.lld.exe")), "-flavor", "link"]
        subprocess.run(link + ["/machine:x64", "/dll", "/noentry",
                        "/nodefaultlib", "/export:run_checks", "/out:" + str(library), str(obj)], check=True)
    else:
        library = temp / "guards.so"
        subprocess.run(common + ["-shared", "-fPIC", "-nostdlib", str(source), "-o", str(library)], check=True)
    loaded = ctypes.CDLL(str(library))
    result = loaded.run_checks()
    if sys.platform == "win32":
        ctypes.windll.kernel32.FreeLibrary(ctypes.c_void_p(loaded._handle))
    if result:
        raise AssertionError(f"production guard regression failed at generated harness line {result}")
print("PASS: transport admission, disconnect status, two-peer isolation, signaling deadlines, and revoke persistence retry")
