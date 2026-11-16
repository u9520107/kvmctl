use anyhow::{Context, Result, bail, ensure};
use bytes::BytesMut;
use std::io::{self, BufRead, Write};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use webrtc::data_channel::DataChannelEvent;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceGatheringState, RTCPeerConnectionState, RTCSessionDescription,
};
use webrtc::runtime::{Runtime, Sender, channel, default_runtime, timeout};

struct Handler(Sender<()>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete { let _ = self.0.try_send(()); }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        eprintln!("M0 RUST STATE {state}");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    ensure!(!text.is_empty() && text.len() <= 16384 && text.len() % 2 == 0, "invalid SDP length");
    text.as_bytes().chunks_exact(2).map(|pair| {
        let hi = (pair[0] as char).to_digit(16).context("invalid hex")?;
        let lo = (pair[1] as char).to_digit(16).context("invalid hex")?;
        Ok(((hi << 4) | lo) as u8)
    }).collect()
}

// Bench uses one explicit private /24 and UDP host candidates only.
fn check_candidates(sdp: &str, bind: Ipv4Addr, local: bool) -> Result<()> {
    let mut count = 0;
    for line in sdp.lines().filter(|line| line.starts_with("a=candidate:")) {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() >= 8 && fields[2].eq_ignore_ascii_case("udp") &&
                fields[6] == "typ" && fields[7] == "host", "non-host/UDP candidate rejected");
        let ip: Ipv4Addr = fields[4].parse().context("candidate is not IPv4")?;
        ensure!(ip.octets()[..3] == bind.octets()[..3] && (!local || ip == bind),
                "candidate outside selected private interface");
        count += 1;
    }
    ensure!(count > 0, "no usable host candidates");
    Ok(())
}

async fn run(runtime: Arc<dyn Runtime>, bind: Ipv4Addr, count: u32) -> Result<()> {
    let start = Instant::now();
    let (gather_tx, mut gather_rx) = channel(1);
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().with_ice_servers(vec![]).build())
        .with_handler(Arc::new(Handler(gather_tx)))
        .with_runtime(runtime.clone())
        .with_udp_addrs(vec![format!("{bind}:0")])
        .build().await?;
    let result = async {
        let dc = pc.create_data_channel("control", None).await?;
        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;
        timeout(&*runtime, Duration::from_secs(10), gather_rx.recv()).await
            .map_err(|_| anyhow::anyhow!("ICE gathering timeout"))?;
        let offer = pc.local_description().await.context("no local SDP")?;
        ensure!(offer.sdp.len() <= 8192, "offer exceeds firmware limit");
        check_candidates(&offer.sdp, bind, true)?;
        println!("M0 RUST OFFER {}", hex(offer.sdp.as_bytes()));
        io::stdout().flush()?;

        let (answer_tx, mut answer_rx) = channel(1);
        std::thread::spawn(move || {
            let line = io::stdin().lock().lines().next().transpose();
            let _ = answer_tx.try_send(line);
        });
        let line = timeout(&*runtime, Duration::from_secs(45), answer_rx.recv()).await
            .map_err(|_| anyhow::anyhow!("serial SDP answer timeout"))?
            .context("answer reader closed")??.context("answer input EOF")?;
        let raw = unhex(line.strip_prefix("ANSWER ").context("expected ANSWER hex")?.trim())?;
        let answer = String::from_utf8(raw)?;
        check_candidates(&answer, bind, false)?;
        pc.set_remote_description(RTCSessionDescription::answer(answer)?).await?;
        let open_start = Instant::now();
        loop {
            match timeout(&*runtime, Duration::from_secs(25), dc.poll()).await
                .map_err(|_| anyhow::anyhow!("DataChannel open timeout"))? {
                Some(DataChannelEvent::OnOpen) => break,
                Some(DataChannelEvent::OnClose) | None => bail!("DataChannel closed before open"),
                _ => {},
            }
            ensure!(open_start.elapsed() < Duration::from_secs(25), "DataChannel open timeout");
        }
        let open_ms = start.elapsed().as_millis();
        eprintln!("M0 RUST OPEN elapsed_ms={open_ms}");
        let mut rtts = Vec::with_capacity(count as usize);
        for sequence in 0..count {
            let payload = format!("kvmctl-m0-echo:{sequence:06}");
            let sent = Instant::now();
            dc.send(BytesMut::from(payload.as_bytes())).await?;
            loop {
                match timeout(&*runtime, Duration::from_secs(5), dc.poll()).await
                    .map_err(|_| anyhow::anyhow!("echo timeout at sequence {sequence}"))? {
                    Some(DataChannelEvent::OnMessage(message)) => {
                        ensure!(message.data.as_ref() == payload.as_bytes(), "echo mismatch at {sequence}");
                        rtts.push(sent.elapsed().as_micros());
                        break;
                    },
                    Some(DataChannelEvent::OnClose) | None => bail!("channel closed at {sequence}"),
                    _ => ensure!(sent.elapsed() < Duration::from_secs(5), "echo timeout"),
                }
            }
        }
        println!("M0 RUST PASS messages={count} open_ms={open_ms} rtt_min_us={} rtt_max_us={} rtt_mean_us={}",
                 rtts.iter().min().unwrap(), rtts.iter().max().unwrap(), rtts.iter().sum::<u128>() / count as u128);
        io::stdout().flush()?;
        Ok(())
    }.await;
    pc.close().await?;
    result
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let bind: Ipv4Addr = args.get(1).map(String::as_str).unwrap_or("192.168.50.2").parse()?;
    ensure!(bind.is_private() || bind.is_loopback(), "bind to private LAN or loopback only");
    let count: u32 = args.get(2).map(String::as_str).unwrap_or("100").parse()?;
    ensure!((1..=100_000).contains(&count), "message count outside 1..100000");
    let runtime = default_runtime().context("no runtime")?;
    let rt = runtime.clone();
    let mut result = None;
    runtime.block_on(Box::pin(async { result = Some(run(rt, bind, count).await); }));
    result.context("runtime did not execute")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_and_candidate_boundary() {
        assert_eq!(unhex(&hex(b"v=0\r\n")).unwrap(), b"v=0\r\n");
        assert!(unhex("+1").is_err());
        assert!(unhex("0").is_err());
        let bind = "192.168.50.2".parse().unwrap();
        assert!(check_candidates("a=candidate:1 1 UDP 1 192.168.50.2 12345 typ host\r\n", bind, true).is_ok());
        assert!(check_candidates("a=candidate:1 1 UDP 1 10.14.0.2 12345 typ host\r\n", bind, false).is_err());
        assert!(check_candidates("a=candidate:1 1 UDP 1 192.168.50.2 12345 typ srflx\r\n", bind, false).is_err());
    }
}
