"""Run with python3 firmware/test_usb_admin.py on a host with a C compiler.

Compile the production decoder, without ESP-IDF dependencies, in a temporary
directory. No firmware image or Windows executable is produced.
"""
from pathlib import Path
import subprocess
import tempfile


source = (Path(__file__).parent / "main/usb_admin.c").read_text()
decoder = source[source.index("#define ADMIN_UART"):source.index("static uint8_t s_request")]
decoder += source[source.index("static bool read_head"):source.index("static bool put_head")]
harness = r"""
#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
DECODER
static void check(const uint8_t *payload, size_t len, request_decode_result_t expected, uint8_t major, uint8_t minor)
{
    uint8_t frame[4096] = {0xa5, 0, major, 1, minor, 2, 0x18, 0x61, 3, 0x50};
    frame[26] = 4;
    memcpy(frame + 27, payload, len);
    admin_request_t request = {0};
    assert(decode_request(frame, 27 + len, &request) == expected);
}
#define CHECK(expected, ...) do { const uint8_t p[] = {__VA_ARGS__}; \
    check(p, sizeof(p), expected, 2, 0); check(p, sizeof(p), expected, 1, 1); } while (0)
int main(void)
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
with tempfile.TemporaryDirectory() as temp:
    path = Path(temp)
    (path / "test.c").write_text(harness)
    subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", str(path / "test.c"), "-o", str(path / "test")], check=True)
    subprocess.run([str(path / "test")], check=True)
print("USB decoder regression checks passed")
