"""Direct-USB SDP bridge for the M0 Rust/ESP32 DataChannel bench. No credentials."""
import argparse
import pathlib
import queue
import subprocess
import threading
import time

import serial

ROOT = pathlib.Path(__file__).resolve().parent


def sdp_hex(text):
    if not text or len(text) > 16384 or len(text) % 2:
        raise ValueError("SDP exceeds firmware framing limit")
    raw = bytes.fromhex(text)
    if len(raw) * 2 != len(text) or not raw.startswith(b"v=0") or b"\0" in raw:
        raise ValueError("Malformed SDP framing")
    return text


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", default="COM6")
    parser.add_argument("--bind", default="192.168.50.2")
    parser.add_argument("--messages", type=int, default=100)
    parser.add_argument("--timeout", type=float, default=180)
    args = parser.parse_args()
    if not 1 <= args.messages <= 100000 or args.timeout <= 0:
        parser.error("messages must be 1..100000 and timeout positive")
    logs = ROOT / "logs"
    logs.mkdir(exist_ok=True)
    log_path = logs / (time.strftime("%Y%m%d-%H%M%S") + ".log")
    uart = serial.Serial(port=None, baudrate=115200, timeout=0.1, write_timeout=10)
    uart.port = args.port
    uart.dtr = uart.rts = False
    process = None
    with log_path.open("w", encoding="utf-8") as log, uart:
        def report(line):
            print(line, flush=True)
            log.write(line + "\n")
            log.flush()

        uart.reset_input_buffer()
        try:
            # Closes only an old bench peer; does not reboot or change Wi-Fi.
            uart.write(b"PEER STOP\n")
            uart.flush()
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if uart.readline().strip() == b"M0 PEER STOPPED":
                    break
            else:
                raise RuntimeError("No PEER STOPPED reply; flash the serial-peer bench image first")
            process = subprocess.Popen(
                [str(ROOT / "target/debug/m0-rust-peer.exe"), args.bind, str(args.messages)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True, encoding="utf-8", bufsize=1,
            )
            lines = queue.Queue()

            def read_host():
                try:
                    for line in process.stdout:
                        lines.put(line.rstrip("\r\n"))
                finally:
                    lines.put(None)

            threading.Thread(target=read_host, daemon=True).start()
            deadline = time.monotonic() + args.timeout
            pending = b""
            offer_sent = answer_sent = passed = False
            host_eof = False
            while time.monotonic() < deadline:
                while not lines.empty():
                    line = lines.get_nowait()
                    if line is None:
                        host_eof = True
                        continue
                    if line.startswith("M0 RUST OFFER "):
                        if offer_sent:
                            raise RuntimeError("Unexpected second offer")
                        encoded = sdp_hex(line.removeprefix("M0 RUST OFFER "))
                        uart.write(("PEER OFFER " + encoded + "\n").encode("ascii"))
                        uart.flush()
                        offer_sent = True
                        report(f"M0 BRIDGE offer_bytes={len(encoded) // 2}")
                    else:
                        report(line)
                        passed |= line.startswith("M0 RUST PASS ")
                pending += uart.read(uart.in_waiting or 1)
                if len(pending) > 65536:
                    raise RuntimeError("Oversized UART line")
                while b"\n" in pending:
                    raw, pending = pending.split(b"\n", 1)
                    line = raw.decode("utf-8", errors="replace").rstrip("\r")
                    if line.startswith("M0 PEER SDP "):
                        if not offer_sent or answer_sent:
                            raise RuntimeError("Unexpected SDP answer")
                        encoded = sdp_hex(line.removeprefix("M0 PEER SDP "))
                        process.stdin.write("ANSWER " + encoded + "\n")
                        process.stdin.flush()
                        answer_sent = True
                        report(f"M0 BRIDGE answer_bytes={len(encoded) // 2}")
                    elif line.startswith("M0 ") or "PEER_FP_REJECT" in line:
                        report(line)
                    elif "Guru Meditation Error" in line or "stack overflow" in line or "abort() was called" in line:
                        raise RuntimeError("Board crashed during peer bench")
                code = process.poll()
                if code is not None and host_eof:
                    if code != 0 or not passed:
                        raise RuntimeError(f"Rust peer failed (exit {code}); inspect {log_path}")
                    report(f"M0 BRIDGE PASS log={log_path}")
                    return
            raise TimeoutError(f"Interop did not finish; inspect {log_path}")
        finally:
            if process and process.poll() is None:
                process.terminate()
                process.wait(timeout=10)
            uart.write(b"PEER STOP\n")
            uart.flush()


if __name__ == "__main__":
    main()
