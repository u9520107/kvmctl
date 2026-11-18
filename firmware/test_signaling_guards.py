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


root = Path(__file__).parent
transport = (root / "main/transport.c").read_text()
signaling = (root / "main/signaling.c").read_text()
client_auth = (root / "main/client_auth.c").read_text()
guards = "\n".join(function(transport, name) for name in (
    "matches_client_generation", "transport_client_session_active", "transport_close_for_client", "transport_authorize_client"
)) + "\n" + function(signaling, "session_deadline_expired")
guards += "\n" + "\n".join(function(client_auth, name) for name in (
    "client_auth_revoke", "client_auth_revoke_all"
))
harness = r"""
typedef unsigned char uint8_t;
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;
typedef unsigned int EventBits_t;
typedef int esp_err_t;
typedef _Bool bool;
#define true 1
#define false 0
#define NULL ((void *)0)
#define ESP_OK 0
#define ESP_ERR_INVALID_ARG 1
#define ESP_ERR_INVALID_STATE 2
#define ESP_ERR_TIMEOUT 3
#define ESP_ERR_NOT_FOUND 4
#define ESP_FAIL 5
#define BIT_CLOSED 1
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
#define portMAX_DELAY 0
#define xSemaphoreTake(lock, ticks) ((void)(lock), (void)(ticks))
#define xSemaphoreGive(lock) ((void)(lock))
static int s_client_fingerprint_mux;
static bool s_active, s_failed, s_identity_rejected, s_application_authorized, s_close_requested;
static unsigned int s_open_channels;
static uint32_t s_generation;
static uint8_t s_active_client_fingerprint[32];
static void *s_events = (void *)1;
static bool enrolled = true;
static unsigned int waits;
static bool s_ready = true;
static int s_lock;
static struct { unsigned int count; uint8_t spki[CLIENT_AUTH_MAX_CLIENTS][CLIENT_AUTH_SPKI_BYTES]; } s_store;
static esp_err_t persist_error = ESP_FAIL;
static unsigned int persist_calls, transport_invalidations, signaling_invalidations;
typedef struct { unsigned int phase; uint64_t phase_deadline_us, message_deadline_us; } signaling_session_t;
int memcmp(const void *a, const void *b, unsigned long long len) {
    const uint8_t *left = a, *right = b;
    for (unsigned long long i = 0; i < len; ++i) if (left[i] != right[i]) return left[i] - right[i];
    return 0;
}
void *memset(void *out, int value, unsigned long long len) {
    uint8_t *bytes = out;
    for (unsigned long long i = 0; i < len; ++i) bytes[i] = (uint8_t)value;
    return out;
}
void *memmove(void *out, const void *in, unsigned long long len) {
    uint8_t *to = out;
    const uint8_t *from = in;
    if (to < from) for (unsigned long long i = 0; i < len; ++i) to[i] = from[i];
    else while (len) { --len; to[len] = from[len]; }
    return out;
}
static EventBits_t xEventGroupWaitBits(void *event, EventBits_t bits, bool clear, bool all, unsigned int ticks) {
    (void)event; (void)clear; (void)all; (void)ticks; ++waits; return bits;
}
static bool client_auth_is_enrolled(const uint8_t *fingerprint) { (void)fingerprint; return enrolled; }
static void transport_invalidate_client(const uint8_t *fingerprint) {
    (void)fingerprint; s_application_authorized = false; s_close_requested = true; ++transport_invalidations;
}
static void transport_invalidate_all_clients(void) { transport_invalidate_client(NULL); }
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
    s_active = true; s_generation = 2; s_active_client_fingerprint[0] = 2;
    s_application_authorized = true; s_open_channels = 2;
    CHECK(transport_client_session_active(client_b, 2));
    CHECK(!transport_client_session_active(client_a, 1));
    CHECK(!transport_client_session_active(client_b, 1));
    s_failed = true;
    CHECK(!transport_client_session_active(client_b, 2));
    s_failed = false; s_active = false;
    CHECK(!transport_client_session_active(client_b, 2));
    s_active = true;
    CHECK(transport_close_for_client(client_a, 1) == ESP_OK);
    CHECK(s_application_authorized && !s_close_requested && waits == 0);
    CHECK(transport_close_for_client(client_b, 1) == ESP_OK);
    CHECK(s_application_authorized && !s_close_requested && waits == 0);
    CHECK(transport_close_for_client(client_a, 2) == ESP_OK);
    CHECK(s_application_authorized && !s_close_requested && waits == 0);
    s_application_authorized = false;
    CHECK(transport_authorize_client(client_a, 1) == ESP_ERR_INVALID_STATE);
    CHECK(transport_authorize_client(client_b, 1) == ESP_ERR_INVALID_STATE);
    CHECK(!s_application_authorized);
    CHECK(transport_authorize_client(client_b, 2) == ESP_OK && s_application_authorized);
    s_application_authorized = false; s_open_channels = 1;
    CHECK(transport_authorize_client(client_b, 2) == ESP_ERR_INVALID_STATE && !s_application_authorized);
    s_open_channels = 2; s_identity_rejected = true;
    CHECK(transport_authorize_client(client_b, 2) == ESP_ERR_INVALID_STATE && !s_application_authorized);
    s_identity_rejected = false; enrolled = false;
    CHECK(transport_authorize_client(client_b, 2) == ESP_ERR_INVALID_STATE && !s_application_authorized);
    enrolled = true; s_application_authorized = true;
    CHECK(transport_close_for_client(client_b, 2) == ESP_OK);
    CHECK(!s_application_authorized && s_close_requested && waits == 1);
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
    transport_invalidations = signaling_invalidations = persist_calls = 0;
    s_store.count = 2; s_store.spki[0][0] = 1; s_store.spki[1][0] = 2;
    s_application_authorized = true; s_close_requested = false;
    CHECK(client_auth_revoke(client_a) == ESP_FAIL);
    CHECK(s_store.count == 1 && s_store.spki[0][0] == 2 && s_store.spki[1][0] == 0);
    CHECK(!s_application_authorized && s_close_requested);
    CHECK(transport_invalidations == 1 && signaling_invalidations == 1 && persist_calls == 1);
    CHECK(client_auth_revoke(client_a) == ESP_ERR_NOT_FOUND && persist_calls == 1);
    s_application_authorized = true; s_close_requested = false;
    CHECK(client_auth_revoke_all() == ESP_FAIL && s_store.count == 0 && s_store.spki[0][0] == 0);
    CHECK(!s_application_authorized && s_close_requested);
    CHECK(transport_invalidations == 2 && signaling_invalidations == 2 && persist_calls == 2);
    persist_error = ESP_OK; s_store.count = 1; s_store.spki[0][0] = 1;
    CHECK(client_auth_revoke(client_a) == ESP_OK && s_store.count == 0);
    return 0;
}
""".replace("GUARDS", guards)
parser = argparse.ArgumentParser()
parser.add_argument("--clang", default="clang")
parser.add_argument("--msvc", help="path to MSVC cl.exe (Windows alternative to clang/lld)")
args = parser.parse_args()
with tempfile.TemporaryDirectory(prefix="kvmctl-signaling-guards-") as temp:
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
print("PASS: stale transport generations cannot close/authorize current peer; deadlines expire; failed revoke persistence stays blocked and returns error")
