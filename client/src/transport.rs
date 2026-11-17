use anyhow::{Context, Result, bail, ensure};
use bytes::BytesMut;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCCertificate,
    RTCConfigurationBuilder, RTCIceConnectionState, RTCIceGatheringState,
    RTCPeerConnectionState,
    RTCSessionDescription,
};
use webrtc::runtime::{Runtime, Sender, channel, default_runtime, timeout};

static CERTIFICATE_REJECTED: AtomicBool = AtomicBool::new(false);
static RTC_ERROR_LOGS: AtomicUsize = AtomicUsize::new(0);
static RTC_ERROR_CLASS: AtomicU8 = AtomicU8::new(0);
static RTC_ERROR_LOCATION: OnceLock<(String, Option<u32>)> = OnceLock::new();

#[derive(Default)]
struct ConnectionStates {
    ice: String,
    peer: String,
}

struct CertificateLog;

impl Log for CertificateLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        matches!(metadata.level(), Level::Error | Level::Warn)
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            RTC_ERROR_LOGS.fetch_add(1, Ordering::Relaxed);
            let _ = RTC_ERROR_LOCATION.set((
                record.module_path().unwrap_or(record.target()).to_owned(),
                record.line(),
            ));
            let message = record.args().to_string().to_ascii_lowercase();
            let class = if message.contains("fingerprint") {
                1
            } else if message.contains("certificate") {
                2
            } else if message.contains("dtls") {
                3
            } else {
                4
            };
            let _ = RTC_ERROR_CLASS.compare_exchange(0, class, Ordering::Relaxed, Ordering::Relaxed);
            if message.contains("remote certificate does not match any fingerprint") {
                CERTIFICATE_REJECTED.store(true, Ordering::Release);
            }
        }
    }

    fn flush(&self) {}
}

static CERTIFICATE_LOG: CertificateLog = CertificateLog;

fn install_certificate_log() -> Result<()> {
    log::set_logger(&CERTIFICATE_LOG)
        .map_err(|_| anyhow::anyhow!("cannot capture DTLS certificate rejection events"))?;
    log::set_max_level(LevelFilter::Warn);
    Ok(())
}

struct Handler(
    Sender<()>,
    Arc<AtomicBool>,
    Arc<Mutex<ConnectionStates>>,
);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        self.2.lock().unwrap().ice = format!("{state:?}");
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.0.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.2.lock().unwrap().peer = format!("{state:?}");
        if state == RTCPeerConnectionState::Failed {
            self.1.store(true, Ordering::Release);
        }
    }
}

fn parse_sha256_fingerprint(sdp: &str) -> Result<[u8; 32]> {
    let mut result = None;
    for line in sdp.lines() {
        if !line.starts_with("a=fingerprint:") {
            continue;
        }
        let value = line
            .strip_prefix("a=fingerprint:sha-256 ")
            .context("missing or unsupported SDP certificate fingerprint")?;
        ensure!(result.is_none(), "duplicate SDP certificate fingerprint");
        ensure!(value.len() == 95, "malformed SDP certificate fingerprint");
        let mut digest = [0u8; 32];
        for (index, byte) in digest.iter_mut().enumerate() {
            let offset = index * 3;
            ensure!(
                value.is_char_boundary(offset + 2)
                    && (index == 31 || value.as_bytes()[offset + 2] == b':'),
                "malformed SDP certificate fingerprint"
            );
            *byte = u8::from_str_radix(&value[offset..offset + 2], 16)
                .context("malformed SDP certificate fingerprint")?;
        }
        result = Some(digest);
    }
    result.context("missing SDP certificate fingerprint")
}

fn override_test_fingerprint(sdp: &str, expected: Option<&str>) -> Result<String> {
    let Some(expected) = expected else {
        return Ok(sdp.to_owned());
    };
    parse_sha256_fingerprint(&format!("a=fingerprint:sha-256 {expected}"))
        .context("test fingerprint override must be one SHA-256 fingerprint")?;
    let mut result = String::with_capacity(sdp.len());
    let mut count = 0;
    for line in sdp.split_inclusive('\n') {
        let content = line
            .strip_suffix("\r\n")
            .or_else(|| line.strip_suffix('\n'))
            .unwrap_or(line);
        if content.starts_with("a=fingerprint:sha-256 ") {
            result.push_str("a=fingerprint:sha-256 ");
            result.push_str(&expected);
            result.push_str(&line[content.len()..]);
            count += 1;
        } else {
            result.push_str(line);
        }
    }
    ensure!(count == 1, "expected exactly one SHA-256 SDP fingerprint");
    Ok(result)
}

fn apply_test_fingerprint_fault(sdp: &str, fault: Option<&str>) -> Result<String> {
    let Some(fault) = fault else {
        return Ok(sdp.to_owned());
    };
    ensure!(
        matches!(fault, "missing" | "malformed" | "duplicate"),
        "fingerprint fault must be missing, malformed, or duplicate"
    );
    let mut result = String::with_capacity(sdp.len());
    let mut count = 0;
    for line in sdp.split_inclusive('\n') {
        let content = line
            .strip_suffix("\r\n")
            .or_else(|| line.strip_suffix('\n'))
            .unwrap_or(line);
        if content.starts_with("a=fingerprint:") {
            ensure!(
                content.starts_with("a=fingerprint:sha-256 "),
                "test SDP did not contain one SHA-256 fingerprint"
            );
            count += 1;
            match fault {
                "missing" => {}
                "malformed" => {
                    result.push_str("a=fingerprint:sha-256 invalid");
                    result.push_str(&line[content.len()..]);
                }
                "duplicate" => {
                    result.push_str(line);
                    result.push_str(line);
                }
                _ => unreachable!(),
            }
        } else {
            result.push_str(line);
        }
    }
    ensure!(count == 1, "expected exactly one SDP certificate fingerprint");
    Ok(result)
}

fn test_certificate() -> Result<Option<RTCCertificate>> {
    let Some(path) = std::env::var_os("KVMCTL_TEST_CERT") else {
        return Ok(None);
    };
    let pem = std::fs::read_to_string(path).context("could not read test certificate fixture")?;
    let provider = rtc::crypto::default_provider().context("no RTC crypto provider")?;
    Ok(Some(RTCCertificate::from_pem(&pem, provider.crypto())?))
}

async fn exchange(
    runtime: Arc<dyn Runtime>,
    mut port: crate::serial::Port,
    stage: &'static str,
) -> Result<crate::serial::Port> {
    let started = Instant::now();
    let (gather_tx, mut gather_rx) = channel(1);
    let peer_failed = Arc::new(AtomicBool::new(false));
    let connection_states = Arc::new(Mutex::new(ConnectionStates::default()));
    let mut configuration = RTCConfigurationBuilder::new().with_ice_servers(vec![]);
    if let Some(certificate) = test_certificate()? {
        configuration = configuration.with_certificates(vec![certificate]);
    }
    let pc = PeerConnectionBuilder::new()
        .with_configuration(configuration.build())
        .with_handler(Arc::new(Handler(
            gather_tx,
            peer_failed.clone(),
            connection_states.clone(),
        )))
        .with_runtime(runtime.clone())
        .with_udp_addrs(vec!["0.0.0.0:0".to_owned()])
        .build()
        .await?;
    let (command_tx, command_rx) = std::sync::mpsc::channel();
    let (closed_tx, mut closed_rx) = channel(1);
    let result = async {
        let control = pc.create_data_channel("control", None).await?;
        let motion = pc
            .create_data_channel(
                "motion",
                Some(RTCDataChannelInit {
                    ordered: false,
                    max_retransmits: Some(0),
                    ..Default::default()
                }),
            )
            .await?;
        ensure!(control.label().await? == "control", "invalid control channel label");
        ensure!(control.ordered().await?, "control channel must be ordered");
        ensure!(
            control.max_retransmits().await?.is_none()
                && control.max_packet_life_time().await?.is_none(),
            "control channel must be reliable"
        );
        ensure!(motion.label().await? == "motion", "invalid motion channel label");
        ensure!(!motion.ordered().await?, "motion channel must be unordered");
        ensure!(
            motion.max_retransmits().await? == Some(0)
                && motion.max_packet_life_time().await?.is_none(),
            "motion channel must use zero retransmissions"
        );

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;
        timeout(&*runtime, Duration::from_secs(10), gather_rx.recv())
            .await
            .map_err(|_| anyhow::anyhow!("ICE gathering timeout"))?;
        let offer = pc.local_description().await.context("no local SDP")?;
        let offer_fault = std::env::var("KVMCTL_TEST_OFFER_FINGERPRINT_FAULT").ok();
        let offer_fingerprint = std::env::var("KVMCTL_TEST_EXPECTED_FINGERPRINT").ok();
        ensure!(
            offer_fault.is_none() || offer_fingerprint.is_none(),
            "choose either an offer fingerprint fault or fingerprint override"
        );
        let offer_sdp = apply_test_fingerprint_fault(&offer.sdp, offer_fault.as_deref())?;
        let offer_sdp = override_test_fingerprint(&offer_sdp, offer_fingerprint.as_deref())?;
        ensure!(offer_sdp.len() <= 3500, "offer exceeds USB signaling frame limit");
        let invalidation_sdp = if stage == "initial" && offer_fault.is_none() {
            let mut fingerprint = parse_sha256_fingerprint(&offer_sdp)?;
            fingerprint[0] ^= 1;
            let fingerprint = fingerprint
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(":");
            override_test_fingerprint(&offer_sdp, Some(&fingerprint))?
        } else {
            "v=0\r\n".to_owned()
        };

        let (answer_tx, mut answer_rx) = channel(1);
        let device_status = Arc::new(AtomicU8::new(u8::MAX));
        let worker_device_status = device_status.clone();
        std::thread::spawn(move || {
            let answer = port.transport_offer(&offer_sdp);
            let _ = answer_tx.try_send(answer);
            let command = loop {
                match command_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(command) => break command,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        let status = port.transport_status().unwrap_or_default();
                        worker_device_status.store(status.state, Ordering::Release);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break false,
                }
            };
            let invalidation: Result<(), String> = match command {
                true => match port.transport_offer(&invalidation_sdp) {
                    Err(error) if error == "device rejected SDP offer with code 3" => Ok(()),
                    Err(error) => Err(format!("session invalidation returned unexpected rejection: {error}")),
                    Ok(_) => Err("session invalidation unexpectedly started a session".to_owned()),
                },
                false => Ok(()),
            };
            if let Err(error) = invalidation {
                let _ = closed_tx.try_send(Err(error));
                return;
            }
            if let Err(error) = port.transport_close() {
                let _ = closed_tx.try_send(Err(error));
                return;
            }
            let _ = closed_tx.try_send(Ok(port));
        });
        let answer = timeout(&*runtime, Duration::from_secs(45), answer_rx.recv())
            .await
            .map_err(|_| anyhow::anyhow!("USB SDP exchange timeout"))?
            .context("USB transport worker stopped")?
            .map_err(anyhow::Error::msg)?;
        let presented_answer_fingerprint = parse_sha256_fingerprint(&answer)?;
        let answer_fault = std::env::var("KVMCTL_TEST_ANSWER_FINGERPRINT_FAULT").ok();
        let answer_fingerprint = std::env::var("KVMCTL_TEST_REMOTE_FINGERPRINT").ok();
        ensure!(
            answer_fault.is_none() || answer_fingerprint.is_none(),
            "choose either an answer fingerprint fault or fingerprint override"
        );
        let answer = apply_test_fingerprint_fault(&answer, answer_fault.as_deref())?;
        let answer = override_test_fingerprint(&answer, answer_fingerprint.as_deref())?;
        parse_sha256_fingerprint(&answer)?;
        let expected_fingerprint_mismatch = answer_fingerprint
            .as_deref()
            .map(|expected| {
                parse_sha256_fingerprint(&format!("a=fingerprint:sha-256 {expected}"))
                    .map(|expected| expected != presented_answer_fingerprint)
            })
            .transpose()?
            .unwrap_or(false);
        pc.set_remote_description(RTCSessionDescription::answer(answer)?).await?;

        wait_open(
            &*runtime,
            &control,
            &motion,
            &peer_failed,
            &device_status,
            &connection_states,
            expected_fingerprint_mismatch,
        )
        .await?;
        let open_ms = started.elapsed().as_millis();
        eprintln!("M1.2 TRANSPORT OPEN stage={stage} elapsed_ms={open_ms}");
        echo(&*runtime, &control, "control", 10).await?;
        echo(&*runtime, &motion, "motion", 10).await?;
        Ok(())
    }
    .await;
    let _ = command_tx.send(result.is_ok());
    let cleanup = timeout(&*runtime, Duration::from_secs(10), closed_rx.recv()).await;
    let _ = pc.close().await;
    let cleanup = cleanup
        .map_err(|_| anyhow::anyhow!("session close timeout"))?
        .context("transport worker stopped before close")?
        .map_err(anyhow::Error::msg);
    let mut port = cleanup?;
    let metrics = port.transport_status().map_err(anyhow::Error::msg)?;
    eprintln!(
        "M1.2 RESOURCE stage={stage} device_state={} internal_free_start_bytes={} internal_free_min_bytes={} internal_free_after_bytes={} psram_free_start_bytes={} psram_free_min_bytes={} psram_free_after_bytes={} device_channel_opens={} device_application_bytes_received={} device_peer_task_cpu_us={}",
        metrics.state,
        metrics.internal_free_start,
        metrics.internal_free_min,
        metrics.internal_free_end,
        metrics.psram_free_start,
        metrics.psram_free_min,
        metrics.psram_free_end,
        metrics.channel_opens,
        metrics.application_bytes_received,
        metrics.peer_task_cpu_us,
    );
    result?;
    let invalidation = if stage == "initial" {
        "conflicting_sdp"
    } else {
        "malformed_sdp"
    };
    eprintln!("M1.2 TRANSPORT INVALIDATION stage={stage} {invalidation}=rejected active_session=closed");
    Ok(port)
}

async fn wait_open(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    peer_failed: &AtomicBool,
    device_status: &AtomicU8,
    connection_states: &Mutex<ConnectionStates>,
    expected_fingerprint_mismatch: bool,
) -> Result<()> {
    let started = Instant::now();
    let mut open = [false, false];
    while !open[0] || !open[1] {
        if expected_fingerprint_mismatch && CERTIFICATE_REJECTED.load(Ordering::Acquire) {
            let opened = open.into_iter().filter(|value| *value).count();
            if opened != 0 {
                bail!(
                    "Windows rejected the remote DTLS certificate fingerprint after {opened} DataChannels opened"
                )
            }
            bail!(
                "Windows rejected the remote DTLS certificate fingerprint (DataChannels opened=0, application messages sent=0)"
            );
        }
        if peer_failed.load(Ordering::Acquire) {
            if expected_fingerprint_mismatch {
                bail!("Windows rejected the remote DTLS certificate fingerprint");
            }
            bail!("Windows peer connection failed before DataChannel open");
        }
        match device_status.load(Ordering::Acquire) {
            3 => bail!("ESP32 rejected the remote DTLS certificate fingerprint"),
            4 => bail!("ESP32 transport failed before DataChannel open"),
            0 => bail!("ESP32 transport session closed before DataChannel open"),
            254 => bail!("could not query ESP32 transport status"),
            _ => {}
        }
        if started.elapsed() >= Duration::from_secs(25) {
            let states = connection_states.lock().unwrap();
            let error_location = RTC_ERROR_LOCATION
                .get()
                .map(|(module, line)| {
                    line.map(|line| format!("{module}:{line}"))
                        .unwrap_or_else(|| module.clone())
                })
                .unwrap_or_else(|| "none".to_owned());
            bail!(
                "no DataChannel opened within 25 seconds (ICE={}, peer={}, RTC errors={}, first error class={}, error location={})",
                states.ice,
                states.peer,
                RTC_ERROR_LOGS.load(Ordering::Relaxed),
                match RTC_ERROR_CLASS.load(Ordering::Relaxed) {
                    1 => "fingerprint",
                    2 => "certificate",
                    3 => "dtls",
                    4 => "other",
                    _ => "none",
                },
                error_location,
            );
        }
        for (index, channel) in [control, motion].into_iter().enumerate() {
            if open[index] {
                continue;
            }
            let event = match timeout(runtime, Duration::from_secs(1), channel.poll()).await {
                Ok(event) => event,
                Err(_) => None,
            };
            if let Some(event) = event {
                match event {
                    DataChannelEvent::OnOpen => open[index] = true,
                    DataChannelEvent::OnClose => {
                        if peer_failed.load(Ordering::Acquire) && expected_fingerprint_mismatch {
                            bail!("Windows rejected the remote DTLS certificate fingerprint");
                        }
                        match device_status.load(Ordering::Acquire) {
                            3 => bail!("ESP32 rejected the remote DTLS certificate fingerprint"),
                            4 => bail!("ESP32 transport failed before DataChannel open"),
                            _ => bail!("DataChannel closed before open"),
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn echo(
    runtime: &dyn Runtime,
    channel: &Arc<dyn DataChannel>,
    label: &str,
    count: u32,
) -> Result<()> {
    let mut rtts = Vec::with_capacity(count as usize);
    for sequence in 0..count {
        let payload = format!("kvmctl-m1.2-{label}:{sequence:06}");
        let sent = Instant::now();
        channel.send(BytesMut::from(payload.as_bytes())).await?;
        loop {
            match timeout(runtime, Duration::from_secs(5), channel.poll())
                .await
                .map_err(|_| anyhow::anyhow!("{label} echo timeout"))?
            {
                Some(DataChannelEvent::OnMessage(message)) => {
                    ensure!(message.data.as_ref() == payload.as_bytes(), "{label} echo mismatch");
                    rtts.push(sent.elapsed().as_micros());
                    break;
                }
                Some(DataChannelEvent::OnClose) | None => bail!("{label} channel closed"),
                _ => ensure!(sent.elapsed() < Duration::from_secs(5), "{label} echo timeout"),
            }
        }
    }
    eprintln!(
        "M1.2 TRANSPORT CHANNEL label={label} reliable={} ordered={} messages={} rtt_min_us={} rtt_max_us={}",
        channel.max_retransmits().await?.is_none() && channel.max_packet_life_time().await?.is_none(),
        channel.ordered().await?,
        count,
        rtts.iter().min().unwrap(),
        rtts.iter().max().unwrap()
    );
    Ok(())
}

pub(crate) fn run_test(port: crate::serial::Port) -> Result<()> {
    install_certificate_log()?;
    CERTIFICATE_REJECTED.store(false, Ordering::Release);
    RTC_ERROR_LOGS.store(0, Ordering::Relaxed);
    RTC_ERROR_CLASS.store(0, Ordering::Relaxed);
    if std::env::var_os("KVMCTL_TEST_CERT").is_some()
        || std::env::var_os("KVMCTL_TEST_EXPECTED_FINGERPRINT").is_some()
        || std::env::var_os("KVMCTL_TEST_REMOTE_FINGERPRINT").is_some()
        || std::env::var_os("KVMCTL_TEST_OFFER_FINGERPRINT_FAULT").is_some()
        || std::env::var_os("KVMCTL_TEST_ANSWER_FINGERPRINT_FAULT").is_some()
    {
        eprintln!("M1.2 TRANSPORT TEST_FIXTURE mode enabled");
    }
    let runtime = default_runtime().context("no WebRTC runtime")?;
    let mut result: Option<Result<()>> = None;
    runtime.block_on(Box::pin(async {
        result = Some(async {
            let port = exchange(runtime.clone(), port, "initial").await?;
            let _port = exchange(runtime.clone(), port, "reconnect").await?;
            Ok(())
        }
        .await);
    }));
    result.context("runtime did not execute")??;
    eprintln!("M1.2 TRANSPORT COMPLETE sessions=2 reconnect=clean");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_parser_fails_closed() {
        let digest = [0xab; 32];
        let line = format!(
            "a=fingerprint:sha-256 {}",
            digest.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(":")
        );
        let sdp = format!("v=0\r\n{line}\r\n");
        assert_eq!(parse_sha256_fingerprint(&sdp).unwrap(), digest);
        assert!(parse_sha256_fingerprint("v=0\r\n").is_err());
        assert!(parse_sha256_fingerprint(&format!("{sdp}{line}\r\n")).is_err());
        assert!(parse_sha256_fingerprint(&sdp.replace("sha-256", "sha-1")).is_err());
        assert!(parse_sha256_fingerprint(&sdp.replace("AB", "GG")).is_err());
    }

    #[test]
    fn test_fingerprint_override_changes_only_one_valid_fingerprint() {
        let fingerprint = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF";
        let sdp = "v=0\r\na=fingerprint:sha-256 old\r\n";
        let result = override_test_fingerprint(sdp, Some(fingerprint)).unwrap();
        assert!(result.contains(&format!("a=fingerprint:sha-256 {fingerprint}\r\n")));
        assert_eq!(override_test_fingerprint(sdp, None).unwrap(), sdp);
        assert!(override_test_fingerprint(sdp, Some("invalid")).is_err());
    }

    #[test]
    fn test_fingerprint_fault_modes_are_single_and_explicit() {
        let sdp = "v=0\r\na=fingerprint:sha-256 00:11\r\n";
        let missing = apply_test_fingerprint_fault(sdp, Some("missing")).unwrap();
        assert!(!missing.contains("a=fingerprint:"));
        assert!(apply_test_fingerprint_fault(sdp, Some("malformed"))
            .unwrap()
            .contains("a=fingerprint:sha-256 invalid\r\n"));
        assert_eq!(
            apply_test_fingerprint_fault(sdp, Some("duplicate"))
                .unwrap()
                .matches("a=fingerprint:")
                .count(),
            2
        );
        assert!(apply_test_fingerprint_fault(sdp, Some("unknown")).is_err());
    }
}
