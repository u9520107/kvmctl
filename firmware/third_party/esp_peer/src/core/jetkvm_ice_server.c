/*
 * SPDX-License-Identifier: MIT
 *
 * Minimal application adapter required by JetKVM Mini's ICE source.
 * Server-reply reads get a bounded blocking wait; peer traffic keeps the
 * vendor's normal receive policy.
 */

#include "esp_peer_udp_policy.h"

extern int udp_socket_recvfrom_nowait(udp_socket_t *, esp_peer_addr_t *,
                                      uint8_t *, int, bool);

int jetkvm_ice_server_recv(udp_socket_t *udp_socket, esp_peer_addr_t *addr,
                           uint8_t *buf, int len, bool nowait)
{
    (void)nowait;
    const long long int timeout_sec = udp_socket->timeout_sec;
    const long int timeout_usec = udp_socket->timeout_usec;
    udp_blocking_timeout(udp_socket, 100);
    int ret = udp_socket_recvfrom_nowait(udp_socket, addr, buf, len, false);
    udp_socket->timeout_sec = timeout_sec;
    udp_socket->timeout_usec = timeout_usec;
    return ret;
}
