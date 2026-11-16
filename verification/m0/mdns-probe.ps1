param(
    [System.Net.IPAddress]$LocalAddress = '192.168.50.2',
    [System.Net.IPAddress]$BoardAddress = '192.168.50.137',
    [string]$Hostname = 'kvmctl-m0-probe.local',
    [ValidateRange(100, 10000)][int]$TimeoutMs = 2500
)

# Read-only network probe. No firewall, VPN, router, serial, or firmware changes.
$ErrorActionPreference = 'Stop'
if ($LocalAddress.AddressFamily -ne 'InterNetwork' -or $BoardAddress.AddressFamily -ne 'InterNetwork') {
    throw 'This probe requires IPv4 addresses.'
}
$query = [Collections.Generic.List[byte]]::new()
$query.AddRange([byte[]](0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0))
foreach ($label in $Hostname.TrimEnd('.').Split('.')) {
    if ($label -notmatch '\A[A-Za-z0-9_-]{1,63}\z') { throw 'Invalid DNS label.' }
    $query.Add([byte]$label.Length)
    $query.AddRange([Text.Encoding]::ASCII.GetBytes($label))
}
if ($query.Count -gt 266) { throw 'DNS name exceeds 255 wire bytes.' }
$query.AddRange([byte[]](0, 0, 1, 0, 1)) # A, class IN

foreach ($mode in 'unicast', 'multicast-QU', 'multicast-QM', 'multicast-loopback') {
    $udp = [Net.Sockets.UdpClient]::new([Net.Sockets.AddressFamily]::InterNetwork)
    try {
        $udp.ExclusiveAddressUse = $false
        $udp.Client.SetSocketOption('Socket', 'ReuseAddress', $true)
        $port = if ($mode -eq 'multicast-QU' -or $mode -eq 'multicast-QM') { 5353 } else { 0 }
        $bindAddress = if ($mode -eq 'unicast') { $LocalAddress } else { [Net.IPAddress]::Any }
        $udp.Client.Bind([Net.IPEndPoint]::new($bindAddress, $port))
        $udp.Client.SetSocketOption('IP', 'MulticastInterface', $LocalAddress.GetAddressBytes())
        $udp.Client.SetSocketOption('IP', 'MulticastTimeToLive', 255)
        $udp.MulticastLoopback = $true
        $group = if ($mode -eq 'multicast-loopback') { '239.255.77.77' } else { '224.0.0.251' }
        $udp.JoinMulticastGroup([Net.IPAddress]::Parse($group), $LocalAddress)
        $udp.Client.ReceiveTimeout = 200
        $packet = $query.ToArray()
        if ($mode -eq 'multicast-QU') { $packet[$packet.Length - 2] = 128 }
        if ($mode -eq 'multicast-loopback') { $packet = [Text.Encoding]::ASCII.GetBytes('kvmctl multicast loopback check') }
        $target = if ($mode -eq 'unicast') { $BoardAddress } else { [Net.IPAddress]::Parse($group) }
        $targetPort = if ($mode -eq 'multicast-loopback') { $udp.Client.LocalEndPoint.Port } else { 5353 }
        [void]$udp.Send($packet, $packet.Length, [Net.IPEndPoint]::new($target, $targetPort))
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $result = 'timeout'
        $replyHex = ''
        while ($watch.ElapsedMilliseconds -lt $TimeoutMs) {
            $sender = [Net.IPEndPoint]::new([Net.IPAddress]::Any, 0)
            try {
                $reply = $udp.Receive([ref]$sender)
                if ($mode -eq 'multicast-loopback') {
                    if ($sender.Address.Equals($LocalAddress) -and [Text.Encoding]::ASCII.GetString($reply) -eq 'kvmctl multicast loopback check') {
                        $result = 'local multicast received'
                        break
                    }
                } elseif ($sender.Address.Equals($BoardAddress) -and $reply.Length -ge 12 -and ($reply[2] -band 128)) {
                    $result = "board DNS response; answers=$(256 * $reply[6] + $reply[7])"
                    $replyHex = [BitConverter]::ToString($reply)
                    break
                }
            } catch [Net.Sockets.SocketException] {
                if ($_.Exception.SocketErrorCode -ne [Net.Sockets.SocketError]::TimedOut) { throw }
            }
        }
        [pscustomobject]@{ Mode = $mode; Result = $result; ReplyHex = $replyHex }
    } finally {
        $udp.Dispose()
    }
}
