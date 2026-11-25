"""Static guards for Wi-Fi ping lifetime and association accounting."""

from pathlib import Path


SOURCE = (Path(__file__).parent / "main" / "wifi_setup.c").read_text()


def function(source: str, name: str) -> str:
    start = source.index(name + "(")
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


handler = function(SOURCE, "wifi_event_handler")
ping_end = function(SOURCE, "ping_end")
quality = function(SOURCE, "wifi_setup_quality")

disconnect = handler[handler.index("WIFI_EVENT_STA_DISCONNECTED"):]
got_ip = handler[handler.index("IP_EVENT_STA_GOT_IP"):]
assert "s_ip_counted_for_association = false;" in disconnect
assert "if (!s_ip_counted_for_association)" in got_ip
assert got_ip.index("if (!s_ip_counted_for_association)") < got_ip.index(
    "s_ip_counted_for_association = true;"
)

assert "ping_state_t *state = calloc(1, sizeof(*state));" in quality
assert ".cb_args = state" in quality
assert "ping_state_retain(state);" in quality
assert "xSemaphoreGive(state->done);" in ping_end
assert ping_end.index("xSemaphoreGive(state->done);") < ping_end.index(
    "ping_state_release(state);"
)
assert "vSemaphoreDelete(state.done)" not in quality
assert "if (ping_started && !ping_finished) esp_ping_stop(handle);" in quality
assert "ping_state_release(state);" in quality

print("Wi-Fi ping lifetime and association-count guards passed")
