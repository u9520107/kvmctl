"""Back up the factory app, run the isolated probe, restore and verify the backup."""
import argparse
import hashlib
import pathlib
import re
import subprocess
import sys
import time

import serial
from esptool.reset import HardReset

ROOT = pathlib.Path(__file__).resolve().parent
APP_OFFSET = 0x10000
APP_CAPACITY = 0x1F0000  # Board 2's already-read factory partition.


def run_tool(port, *args, after="no-reset"):
    command = [sys.executable, "-m", "esptool", "--chip", "esp32s3", "--port", port,
               "--baud", "115200", "--before", "default-reset", "--after", after, *map(str, args)]
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", default="COM6")
    parser.add_argument("--backup", type=pathlib.Path,
                        help="Reuse an app backup only after verifying it matches current flash")
    args = parser.parse_args()
    image = ROOT / "build/m0_peer_fingerprint.bin"
    if not 0 < image.stat().st_size <= APP_CAPACITY:
        raise ValueError("Probe image does not fit known factory partition")
    logs = ROOT / "logs" / time.strftime("%Y%m%d-%H%M%S")
    logs.mkdir(parents=True)
    (logs / "probe.sha256").write_text(hashlib.sha256(image.read_bytes()).hexdigest() + "\n", encoding="ascii")
    backup = logs / "factory-app-backup.bin"
    # Confirm partition layout before using any hard-coded write address.
    partitions = logs / "partitions.bin"
    run_tool(args.port, "read-flash", "0x8000", "0x1000", partitions)
    import struct
    entries = [struct.unpack("<HBBII16sI", partitions.read_bytes()[i:i + 32])
               for i in range(0, 0xC00, 32)]
    factory = [entry for entry in entries if entry[0] == 0x50AA and entry[1:3] == (0, 0)]
    if len(factory) != 1 or factory[0][3:5] != (APP_OFFSET, APP_CAPACITY):
        raise ValueError("Partition layout differs from recorded Board 2 layout; no write performed")
    if args.backup:
        if args.backup.stat().st_size != APP_CAPACITY:
            raise ValueError("Supplied backup has wrong size")
        run_tool(args.port, "verify-flash", hex(APP_OFFSET), args.backup)
        backup.write_bytes(args.backup.read_bytes())
    else:
        run_tool(args.port, "read-flash", hex(APP_OFFSET), hex(APP_CAPACITY), backup)
    if backup.stat().st_size != APP_CAPACITY:
        raise ValueError("Incomplete backup; no write performed")
    digest = hashlib.sha256(backup.read_bytes()).hexdigest()
    (logs / "backup.sha256").write_text(digest + "\n", encoding="ascii")
    print(f"M0 BACKUP bytes={APP_CAPACITY} sha256={digest} path={backup}", flush=True)
    try:
        run_tool(args.port, "write-flash", "--flash-mode", "dio", "--flash-size", "16MB",
                 "--flash-freq", "80m", hex(APP_OFFSET), image)
        run_tool(args.port, "verify-flash", hex(APP_OFFSET), image)
        print("M0 FLASH verified; collecting UART", flush=True)
        uart = serial.Serial(port=None, baudrate=115200, timeout=0.25)
        uart.port = args.port
        uart.dtr = uart.rts = False
        with uart:
            HardReset(uart)()
            deadline = time.monotonic() + 120
            pending = b""
            cases = {}
            current_case = None
            with (logs / "uart.log").open("w", encoding="utf-8") as log:
                while time.monotonic() < deadline:
                    pending += uart.read(uart.in_waiting or 1)
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        text = line.decode("utf-8", errors="replace").rstrip("\r")
                        text = re.sub(r"(a=ice-(?:pwd|ufrag):).*", r"\1[redacted]", text)
                        log.write(text + "\n")
                        log.flush()
                        case = re.match(r"M0 CASE source=(-?\d+) begin", text)
                        if case:
                            current_case = int(case[1])
                            cases[current_case] = {"rejections": set(), "setup": {}}
                        if current_case is not None:
                            rejection = re.search(r"PEER_FP_REJECT role=(client|server) reason=pin_mismatch\b", text)
                            if rejection:
                                cases[current_case]["rejections"].add(rejection[1])
                            setup = re.match(r"M0 SDP source=(\d+) setup=a=setup:(\w+) bytes=", text)
                            if setup:
                                cases[current_case]["setup"][int(setup[1])] = setup[2]
                            result = re.match(r"M0 RESULT source=(-?\d+) result=(\w+) mutations=(\d+) errors=(\d+) opened=(\d+),(\d+) received=(\d+),(\d+) failures=(\d+),(\d+)", text)
                            if result:
                                cases[int(result[1])]["result"] = result[2]
                                cases[int(result[1])]["counts"] = tuple(map(int, result.groups()[2:]))
                        if text.startswith("M0 "):
                            print(text, flush=True)
                        if "stack overflow" in text or "Guru Meditation Error" in text or "abort() was called" in text:
                            raise RuntimeError(f"Board crashed: {text}; inspect {logs / 'uart.log'}")
                        if text.startswith("M0 SUITE "):
                            baseline = cases.get(-1, {})
                            accepted = baseline.get("result") == "PASS"
                            for source, role in ((0, "client"), (1, "server")):
                                case = cases.get(source, {})
                                # mutations, errors, opens, receives, failures
                                counts = case.get("counts", ())
                                rejected = (len(counts) == 8 and counts[0] > 0 and
                                            counts[1:6] == (0, 0, 0, 0, 0) and
                                            case.get("result") == "INCONCLUSIVE" and
                                            role in case.get("rejections", set()) and
                                            case.get("setup") == {0: "actpass", 1: "active"})
                                accepted = accepted and rejected
                            if not accepted:
                                raise RuntimeError(f"{text}; inspect {logs / 'uart.log'}")
                            print("M0 VERIFIED_SUITE PASS baseline_and_both_fingerprint_rejections", flush=True)
                            log.write("M0 VERIFIED_SUITE PASS baseline_and_both_fingerprint_rejections\n")
                            return
                raise TimeoutError(f"Probe suite did not finish; inspect {logs / 'uart.log'}")
    finally:
        print("M0 RESTORING original app", flush=True)
        run_tool(args.port, "write-flash", "--flash-mode", "dio", "--flash-size", "16MB",
                 "--flash-freq", "80m", hex(APP_OFFSET), backup)
        run_tool(args.port, "verify-flash", hex(APP_OFFSET), backup, after="hard-reset")
        print(f"M0 RESTORED verified sha256={digest}", flush=True)


if __name__ == "__main__":
    main()
