"""Source guards for M1.4 cleanup, HID recovery, and lease compatibility."""
import re
from pathlib import Path


root = Path(__file__).parent
transport = (root / "main/transport.c").read_text()
hid = (root / "hid_bench/hid_bench.c").read_text()
protocol = (root / "main/hid_protocol.c").read_text()


def block(source: str, name: str) -> str:
    start = source.index(name + "(")
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


release = block(transport, "accept_scripted_event_locked")
cleanup = release.index("begin_lease_cleanup_locked(")
assert release.index("lease_release(") < cleanup
assert cleanup < release.index("result = 2", cleanup)
assert "if (!s_lease_cleanup_pending) return;" in block(transport, "finish_lease_cleanup_locked")
assert "pending_requester_eligible_locked" in block(transport, "finish_lease_cleanup_locked")

service = block(hid, "hid_bench_service")
assert "s_transfer_deadline_ms" in service
assert "s_cleanup_deadline_ms" in service
assert "s_cleanup_failed = true" in service
assert "CLEANUP_RETRIES" in service
failed_recovery = service[service.index("if (failed)"):service.index("if (!failed && s_releasing)")]
assert failed_recovery.count("return false;") == 1
assert "hid_bench_cleanup_complete" in hid
assert "KVMCTL_HID_BENCH_FAIL_TRANSFER_ONCE" in hid

lease_request = protocol.split("case KVMCTL_HID_LEASE_REQUEST:", 1)[1].split("case ", 1)[0]
assert re.search(
    r"if \(field_count == 1\) \{[^{}]*event->heartbeat_ack_supported = true;\s*\}\s*return true;",
    lease_request,
)
assert "slot->heartbeat_ack_supported = false;" in block(transport, "transport_offer_internal")

print("M1.4 cleanup, transfer-failure, and lease compatibility guards passed")
