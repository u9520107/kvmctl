"""Run with python3 firmware/test_usb_admin.py on a host with a C compiler.

Compile the production decoder, without ESP-IDF dependencies, in a temporary
directory. No firmware image or Windows executable is produced.
"""
import ctypes
from pathlib import Path
import subprocess
import sys
import tempfile


root = Path(__file__).resolve().parent
source = (root / "main/usb_admin.c").read_text()
decoder = source[source.index("#define ADMIN_UART"):source.index("static uint8_t s_request")]
decoder += source[source.index("static bool read_head"):source.index("static bool put_head")]
harness = r"""
#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#define CLIENT_AUTH_SPKI_BYTES 91
#define CLIENT_AUTH_FINGERPRINT_BYTES 32
DECODER
static void check_command(const uint8_t *payload, size_t len, uint8_t command,
                          request_decode_result_t expected, uint8_t major, uint8_t minor)
{
    uint8_t frame[4096] = {0xa5, 0, major, 1, minor, 2, 0x18, 0x61, 3, 0x50};
    frame[7] = command;
    frame[26] = 4;
    memcpy(frame + 27, payload, len);
    admin_request_t request = {0};
    assert(decode_request(frame, 27 + len, &request) == expected);
}
static void check(const uint8_t *payload, size_t len, request_decode_result_t expected, uint8_t major, uint8_t minor)
{
    check_command(payload, len, 0x61, expected, major, minor);
}
#define CHECK(expected, ...) do { const uint8_t p[] = {__VA_ARGS__}; \
    check(p, sizeof(p), expected, 2, 0); check(p, sizeof(p), expected, 1, 1); } while (0)
#ifdef _WIN32
__declspec(dllexport) void run_checks(void)
#else
int main(void)
#endif
{
    const uint8_t empty_map[] = {0xa0};
    check(empty_map, 1, REQUEST_SUPPORTED, 1, 0);
    CHECK(REQUEST_UNSUPPORTED_VERSION, 0xa0);
    CHECK(REQUEST_UNSUPPORTED_VERSION, 0xa1, 0, 0x82, 0xf5, 0x62, 0xc2, 0xa2);
    CHECK(REQUEST_UNSUPPORTED_VERSION, 0xa2, 0, 0x43, 0, 0xff, 0x80, 1, 0x64, 0xf0, 0x9f, 0x98, 0x80);
    CHECK(REQUEST_UNSUPPORTED_VERSION, 0x1a, 0, 1, 0, 0);
    CHECK(REQUEST_INVALID, 0x1a);
    CHECK(REQUEST_INVALID, 0xa0, 0);
    CHECK(REQUEST_INVALID, 0x18, 0);
    CHECK(REQUEST_INVALID, 0x9f, 0xff);
    CHECK(REQUEST_INVALID, 0x42, 0);
    CHECK(REQUEST_INVALID, 0x81);
    CHECK(REQUEST_INVALID, 0xa1, 0);
    CHECK(REQUEST_INVALID, 0xa2, 0, 0, 0, 1);
    CHECK(REQUEST_INVALID, 0xa2, 1, 0, 0, 1);
    CHECK(REQUEST_INVALID, 0xa1, 0x60, 0);
    CHECK(REQUEST_INVALID, 0xc0, 0);
    CHECK(REQUEST_INVALID, 0xf9, 0x3c, 0);
    uint8_t enroll[95] = {0xa1, 0x00, 0x58, 0x5b};
    check_command(enroll, sizeof(enroll), 0x62, REQUEST_SUPPORTED, 1, 0);
    check_command(enroll, sizeof(enroll) - 1, 0x62, REQUEST_INVALID, 1, 0);
    check_command(empty_map, sizeof(empty_map), 0x63, REQUEST_SUPPORTED, 1, 0);
    check_command(empty_map, sizeof(empty_map), 0x65, REQUEST_SUPPORTED, 1, 0);
    uint8_t revoke[36] = {0xa1, 0x00, 0x58, 0x20};
    check_command(revoke, sizeof(revoke), 0x64, REQUEST_SUPPORTED, 1, 0);
    check_command(revoke, sizeof(revoke) - 1, 0x64, REQUEST_INVALID, 1, 0);
    CHECK(REQUEST_INVALID, 0x62, 0xc0, 0x80);
    CHECK(REQUEST_INVALID, 0x63, 0xed, 0xa0, 0x80);
    CHECK(REQUEST_INVALID, 0x64, 0xf4, 0x90, 0x80, 0x80);
    uint8_t nested[20];
    memset(nested, 0x81, sizeof(nested));
    nested[19] = 0;
    check(nested, sizeof(nested), REQUEST_INVALID, 2, 0);
    check(empty_map, 0, REQUEST_INVALID, 2, 0);
}
""".replace("DECODER", decoder)
build = root / "build"
build.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="usb-admin-test-", dir=build) as temp:
    path = Path(temp)
    (path / "test.c").write_text(harness)
    if sys.platform == "win32":
        library = path / "test.dll"
        subprocess.run(["cl", "/nologo", "/std:c11", "/LD", "test.c",
                        "/Fe:" + str(library)], cwd=path, check=True)
        loaded = ctypes.CDLL(str(library))
        loaded.run_checks.restype = None
        loaded.run_checks()
        ctypes.windll.kernel32.FreeLibrary(ctypes.c_void_p(loaded._handle))
    else:
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror",
                        "test.c", "-o", "test"], cwd=path, check=True)
        subprocess.run([str(path / "test")], check=True)
print("USB decoder regression checks passed")
