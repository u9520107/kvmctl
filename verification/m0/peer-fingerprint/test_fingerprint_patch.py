"""Check the C SDP parser and per-peer rejection tracking with MSVC."""
import ctypes
import os
from pathlib import Path
import random
import shutil
import subprocess
import tempfile


def main():
    here = Path(__file__).resolve().parent
    patch = (here / "esp-peer-fingerprint.patch").read_text()
    additions = "\n".join(line[1:] for line in patch.splitlines()
                          if line.startswith("+") and not line.startswith("+++"))
    parser = additions[additions.index("static int fingerprint_hex("):
                       additions.index("static void clear_remote_fingerprint(")]
    source = "#include <stdbool.h>\n#include <stddef.h>\n#include <string.h>\n" + parser
    source += "\n__declspec(dllexport) int parse_test(const unsigned char *p, size_t n, unsigned char *out) { return parse_remote_fingerprint(p, n, out); }\n"
    source += r"""
typedef void *esp_peer_handle_t;
typedef struct peer_wrapper {
    struct peer_wrapper *next;
    esp_peer_handle_t handle;
    unsigned char expected_fingerprint[32];
    bool fingerprint_set, fingerprint_invalid, fingerprint_rejected;
} peer_wrapper_t;
static peer_wrapper_t *fingerprint_peers;
#define portENTER_CRITICAL(lock) ((void)0)
#define portEXIT_CRITICAL(lock) ((void)0)
"""
    getter = additions.index("bool esp_peer_remote_fingerprint_rejected(")
    source += additions[additions.index("static void clear_remote_fingerprint("):
                        additions.index("\n}", getter) + 2]
    source += r"""
__declspec(dllexport) int rejection_test(void)
{
    int first_backend, second_backend;
    peer_wrapper_t second = {.handle = &second_backend};
    peer_wrapper_t first = {.next = &second, .handle = &first_backend};
    fingerprint_peers = &first;
    esp_peer_note_remote_fingerprint_rejection(&second_backend);
    if (esp_peer_remote_fingerprint_rejected(&first) ||
        !esp_peer_remote_fingerprint_rejected(&second)) return 0;
    clear_remote_fingerprint(&second);
    return !esp_peer_remote_fingerprint_rejected(&second);
}
"""
    vcvars = Path("C:/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/VC/Auxiliary/Build/vcvars64.bat")
    environment = os.environ.copy()
    # cmd.exe uses its own quoting rules, unlike subprocess's argv quoting.
    setup = subprocess.check_output(f'cmd.exe /d /s /c ""{vcvars}" >nul && set"', text=True)
    for line in setup.splitlines():
        if "=" in line and not line.startswith("="):
            key, value = line.split("=", 1)
            environment[key] = value
    compiler = shutil.which("cl.exe", path=environment.get("PATH"))
    assert compiler, "MSVC cl.exe not found after vcvars64.bat"
    (here / "build").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=here / "build", prefix="parser-check-") as directory:
        directory = Path(directory)
        c_file, dll_file = directory / "parser.c", directory / "parser.dll"
        c_file.write_text(source)
        subprocess.run([compiler, "/nologo", "/LD", "/O1", "/W4", "/WX", "/std:c11",
                        str(c_file), "/link", f"/OUT:{dll_file}"],
                       check=True, env=environment, cwd=directory)
        library = ctypes.CDLL(str(dll_file))
        parse = library.parse_test
        parse.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_ubyte)]
        parse.restype = ctypes.c_int
        checks = 0

        def check(text, expected, digest=None):
            nonlocal checks
            output = (ctypes.c_ubyte * 32)()
            assert bool(parse(text, len(text), output)) == expected, repr(text)
            if digest is not None:
                assert bytes(output) == digest
            checks += 1

        rng = random.Random(0)
        for _ in range(100):
            digest = rng.randbytes(32)
            fingerprint = b":".join(f"{byte:02X}".encode() for byte in digest)
            line = b"a=fingerprint:sha-256 " + fingerprint
            valid = b"v=0\r\n" + line + b"\r\n"
            check(valid, True, digest)
            check(valid.lower(), True, digest)
            check(valid + b"\0", True, digest)
            check(line, True, digest)
            check(valid + line + b"\r\n", False)
            check(valid.replace(b"sha-256", b"sha-1"), False)
            check(valid.replace(b"sha-256", b"SHA-256"), False)
            check(line[:-1], False)
            check(line + b" ", False)
            check(line[:21] + b"GG" + line[23:], False)
            check(line[:23] + b"-" + line[24:], False)
            check(b"v=0\0\r\n" + line, False)
            check(b"v=0\r\n" + line + b"\0x", False)
        for invalid in (b"", b"\0", b"v=0\r\n", b"a=fingerprint:\r\n", b"x" * 8193):
            check(invalid, False)
        assert not parse(None, 0, (ctypes.c_ubyte * 32)())
        assert library.rejection_test() == 1
        # Unload before TemporaryDirectory removes the Windows DLL.
        ctypes.windll.kernel32.FreeLibrary.argtypes = [ctypes.c_void_p]
        ctypes.windll.kernel32.FreeLibrary(library._handle)
    print(f"PASS: {checks + 1} actual-C fingerprint parser checks")
    print("PASS: per-peer fingerprint rejection and reset")


if __name__ == "__main__":
    main()
