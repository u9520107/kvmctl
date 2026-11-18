use anyhow::{Context, Result, bail, ensure};
use bytes::BytesMut;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCCertificate,
    RTCConfigurationBuilder, RTCIceConnectionState, RTCIceGatheringState, RTCPeerConnectionState,
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
            let _ =
                RTC_ERROR_CLASS.compare_exchange(0, class, Ordering::Relaxed, Ordering::Relaxed);
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

struct Handler(Sender<()>, Arc<AtomicBool>, Arc<Mutex<ConnectionStates>>);

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

pub(crate) fn parse_sha256_fingerprint(sdp: &str) -> Result<[u8; 32]> {
    let mut result: Option<[u8; 32]> = None;
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
    ensure!(
        count == 1,
        "expected exactly one SDP certificate fingerprint"
    );
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
        ensure!(
            control.label().await? == "control",
            "invalid control channel label"
        );
        ensure!(control.ordered().await?, "control channel must be ordered");
        ensure!(
            control.max_retransmits().await?.is_none()
                && control.max_packet_life_time().await?.is_none(),
            "control channel must be reliable"
        );
        ensure!(
            motion.label().await? == "motion",
            "invalid motion channel label"
        );
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
        ensure!(
            offer_sdp.len() <= 3500,
            "offer exceeds USB signaling frame limit"
        );
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
                        let state = port
                            .transport_status()
                            .map(|status| status.state)
                            .unwrap_or(254);
                        worker_device_status.store(state, Ordering::Release);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break false,
                }
            };
            let invalidation: Result<(), String> = match command {
                true => match port.transport_offer(&invalidation_sdp) {
                    Err(error) if error == "device is busy with another transport session" => {
                        Ok(())
                    }
                    Err(error) => Err(format!(
                        "session invalidation returned unexpected rejection: {error}"
                    )),
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
        pc.set_remote_description(RTCSessionDescription::answer(answer)?)
            .await?;

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
        // Raw USB signaling remains useful for the M1.2 DTLS identity check,
        // but this unauthenticated path must never authorize app traffic.
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
    eprintln!(
        "M1.2 TRANSPORT INVALIDATION stage={stage} {invalidation}=rejected active_session=closed"
    );
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
        result = Some(
            async {
                let port = exchange(runtime.clone(), port, "initial").await?;
                let _port = exchange(runtime.clone(), port, "reconnect").await?;
                Ok(())
            }
            .await,
        );
    }));
    result.context("runtime did not execute")??;
    eprintln!("M1.2 TRANSPORT COMPLETE sessions=2 reconnect=clean");
    Ok(())
}

enum MonitorCommand {
    Snapshot(std::sync::mpsc::Sender<Result<crate::TransportMetrics, String>>),
    RevokeClient([u8; 32], std::sync::mpsc::Sender<Result<(), String>>),
    Stop,
}

struct PortMonitor {
    device_status: Arc<AtomicU8>,
    metrics: Arc<Mutex<crate::TransportMetrics>>,
    usb_admin_ok: Arc<AtomicBool>,
    stop: std::sync::mpsc::Sender<MonitorCommand>,
    finished: std::sync::mpsc::Receiver<crate::serial::Port>,
}

impl PortMonitor {
    fn start(mut port: crate::serial::Port) -> Self {
        let device_status = Arc::new(AtomicU8::new(254));
        let metrics = Arc::new(Mutex::new(crate::TransportMetrics::default()));
        let usb_admin_ok = Arc::new(AtomicBool::new(true));
        let (stop, stop_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished) = std::sync::mpsc::channel();
        let worker_status = device_status.clone();
        let worker_metrics = metrics.clone();
        let worker_admin_ok = usb_admin_ok.clone();
        std::thread::spawn(move || {
            let mut next_admin_check = Instant::now();
            loop {
                if Instant::now() >= next_admin_check {
                    if port.status().is_err() {
                        worker_admin_ok.store(false, Ordering::Release);
                    }
                    next_admin_check = Instant::now() + Duration::from_secs(1);
                }
                match port.transport_status() {
                    Ok(snapshot) => {
                        worker_status.store(snapshot.state, Ordering::Release);
                        *worker_metrics.lock().unwrap() = snapshot;
                    }
                    Err(_) => {
                        worker_status.store(254, Ordering::Release);
                        worker_admin_ok.store(false, Ordering::Release);
                    }
                }
                match stop_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(MonitorCommand::Stop)
                    | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(MonitorCommand::Snapshot(reply)) => {
                        let snapshot = port.transport_status();
                        match &snapshot {
                            Ok(snapshot) => {
                                worker_status.store(snapshot.state, Ordering::Release);
                                *worker_metrics.lock().unwrap() = *snapshot;
                            }
                            Err(_) => {
                                worker_status.store(254, Ordering::Release);
                                worker_admin_ok.store(false, Ordering::Release);
                            }
                        }
                        let _ = reply.send(snapshot);
                    }
                    Ok(MonitorCommand::RevokeClient(fingerprint, reply)) => {
                        let _ = reply.send(port.revoke_client(&fingerprint));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            let _ = finished_tx.send(port);
        });
        Self {
            device_status,
            metrics,
            usb_admin_ok,
            stop,
            finished,
        }
    }

    fn snapshot(&self) -> crate::TransportMetrics {
        *self.metrics.lock().unwrap()
    }

    fn fresh_snapshot(&self) -> Result<crate::TransportMetrics> {
        let (reply, receiver) = std::sync::mpsc::channel();
        self.stop
            .send(MonitorCommand::Snapshot(reply))
            .map_err(|_| anyhow::anyhow!("USB transport monitor stopped before metrics refresh"))?;
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("USB transport metrics refresh timed out"))?
            .map_err(anyhow::Error::msg)
    }

    fn revoke_client(&self, fingerprint: [u8; 32]) -> Result<()> {
        let (reply, receiver) = std::sync::mpsc::channel();
        self.stop
            .send(MonitorCommand::RevokeClient(fingerprint, reply))
            .map_err(|_| {
                anyhow::anyhow!("USB transport monitor stopped before client revocation")
            })?;
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("USB client revocation timed out"))?
            .map_err(anyhow::Error::msg)
    }

    fn finish(self) -> Result<(crate::serial::Port, crate::TransportMetrics, bool)> {
        let _ = self.stop.send(MonitorCommand::Stop);
        let port = self
            .finished
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow::anyhow!("USB transport monitor did not stop"))?;
        Ok((
            port,
            self.snapshot(),
            self.usb_admin_ok.load(Ordering::Acquire),
        ))
    }
}

impl Drop for PortMonitor {
    fn drop(&mut self) {
        let _ = self.stop.send(MonitorCommand::Stop);
    }
}

enum TranscriptProofCase<'a> {
    Valid {
        check_isolation: bool,
        check_resources: bool,
        revoke_after_authorization: bool,
    },
    Missing,
    Invalid,
    Replayed(&'a [u8]),
}

async fn send_before_authorization_and_reject_echo(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
) -> Result<()> {
    let payload = b"kvmctl-m1.3-pre-auth-must-not-echo";
    for channel in [control, motion] {
        channel
            .send(BytesMut::from(&payload[..]))
            .await
            .context("pre-authorization application payload could not be sent")?;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        for channel in [control, motion] {
            match timeout(runtime, Duration::from_millis(200), channel.poll()).await {
                Ok(Some(DataChannelEvent::OnMessage(message))) => {
                    bail!(
                        "ESP32 echoed application payload before authorization: {} bytes",
                        message.data.len()
                    );
                }
                Ok(Some(DataChannelEvent::OnClose)) => {
                    bail!("DataChannel closed before the transcript proof was checked")
                }
                _ => {}
            }
        }
    }
    Ok(())
}

async fn send_after_revocation_and_reject_echo(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    device_status: &AtomicU8,
) -> Result<()> {
    let payload = b"kvmctl-m1.3-revoked-must-not-echo";
    for channel in [control, motion] {
        let _ = channel.send(BytesMut::from(&payload[..])).await;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        ensure!(
            device_status.load(Ordering::Acquire) == 0,
            "ESP32 transport became active again after client revocation"
        );
        for channel in [control, motion] {
            if let Ok(Some(DataChannelEvent::OnMessage(message))) =
                timeout(runtime, Duration::from_millis(200), channel.poll()).await
            {
                bail!(
                    "ESP32 echoed {} application bytes after USB revocation",
                    message.data.len()
                );
            }
        }
    }
    Ok(())
}

fn expect_busy(result: Result<String, String>) -> Result<()> {
    match result {
        Err(error) if error.contains("busy") => Ok(()),
        Err(error) => bail!("competing SDP returned unexpected rejection: {error}"),
        Ok(_) => bail!("competing SDP was accepted while another client was active"),
    }
}

async fn test_competing_signaling(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential: &crate::credentials::ClientCredential,
    offer_sdp: &str,
) -> Result<()> {
    let outsider = crate::credentials::ClientCredential::generate().map_err(anyhow::Error::msg)?;
    match crate::signaling::Session::connect(
        address,
        hostname,
        status.identity_fingerprint,
        status.tls_spki_hash,
        &outsider,
    ) {
        Err(error) if error == "endpoint rejected signaling: unauthorized" => {}
        Err(error) => bail!("unauthorized client returned unexpected rejection: {error}"),
        Ok(mut session) => {
            session.close();
            bail!("un-enrolled client passed WSS authentication");
        }
    }
    echo_signaling(runtime, control, "control-after-unauthorized-client", 1).await?;
    echo_signaling(runtime, motion, "motion-after-unauthorized-client", 1).await?;
    eprintln!(
        "M1.3 ISOLATION unauthorized_client=rejected active_session=preserved control_echo=true motion_echo=true"
    );
    crate::signaling::Session::expect_invalid_client_proof(
        address,
        hostname,
        status.identity_fingerprint,
        status.tls_spki_hash,
        credential,
    )
    .map_err(anyhow::Error::msg)?;
    echo_signaling(runtime, control, "control-after-invalid-client-proof", 1).await?;
    echo_signaling(runtime, motion, "motion-after-invalid-client-proof", 1).await?;
    eprintln!(
        "M1.3 ISOLATION invalid_client_proof=rejected active_session=preserved control_echo=true motion_echo=true"
    );
    let mut competitor = crate::signaling::Session::connect(
        address,
        hostname,
        status.identity_fingerprint,
        status.tls_spki_hash,
        credential,
    )
    .map_err(anyhow::Error::msg)?;
    expect_busy(
        competitor
            .send_offer(credential, offer_sdp)
            .map(|_| String::new()),
    )?;
    competitor.close();
    echo_signaling(runtime, control, "control-after-competing-offer", 1).await?;
    echo_signaling(runtime, motion, "motion-after-competing-offer", 1).await?;
    eprintln!(
        "M1.3 ISOLATION authenticated_competing_offer=busy active_session=preserved control_echo=true motion_echo=true"
    );
    Ok(())
}

async fn exchange_signaling(
    runtime: Arc<dyn Runtime>,
    monitor: &PortMonitor,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential: &crate::credentials::ClientCredential,
    proof_case: TranscriptProofCase<'_>,
) -> Result<Vec<u8>> {
    let started = Instant::now();
    let authorized_case = matches!(&proof_case, TranscriptProofCase::Valid { .. });
    let mut session = crate::signaling::Session::connect(
        address,
        hostname,
        status.identity_fingerprint,
        status.tls_spki_hash,
        credential,
    )
    .map_err(anyhow::Error::msg)?;
    let (gather_tx, mut gather_rx) = channel(1);
    let peer_failed = Arc::new(AtomicBool::new(false));
    let connection_states = Arc::new(Mutex::new(ConnectionStates::default()));
    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![])
        .build();
    let pc = PeerConnectionBuilder::new()
        .with_configuration(configuration)
        .with_handler(Arc::new(Handler(
            gather_tx,
            peer_failed.clone(),
            connection_states.clone(),
        )))
        .with_runtime(runtime.clone())
        .with_udp_addrs(vec!["0.0.0.0:0".to_owned()])
        .build()
        .await?;
    let result = async {
        let control = pc.create_data_channel("control", None).await?;
        let motion = pc.create_data_channel(
            "motion",
            Some(RTCDataChannelInit { ordered: false, max_retransmits: Some(0), ..Default::default() }),
        ).await?;
        ensure!(control.ordered().await? && control.max_retransmits().await?.is_none(),
                "control channel must be reliable and ordered");
        ensure!(!motion.ordered().await? && motion.max_retransmits().await? == Some(0),
                "motion channel must be unordered with zero retransmissions");

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;
        timeout(&*runtime, Duration::from_secs(10), gather_rx.recv())
            .await.map_err(|_| anyhow::anyhow!("ICE gathering timeout"))?;
        let offer = pc.local_description().await.context("no local SDP")?;
        let offer_sdp = offer.sdp;
        let offer_exchange = session.send_offer(credential, &offer_sdp).map_err(anyhow::Error::msg)?;
        pc.set_remote_description(RTCSessionDescription::answer(offer_exchange.answer.clone())?).await?;
        let device_status = monitor.device_status.clone();
        wait_open(
            &*runtime,
            &control,
            &motion,
            &peer_failed,
            &device_status,
            &connection_states,
            false,
        ).await?;
        eprintln!("M1.3 DTLS_OPEN elapsed_ms={} control=ordered-reliable motion=unordered-zero-retransmit",
                  started.elapsed().as_millis());

        match proof_case {
            TranscriptProofCase::Valid { check_isolation, check_resources, revoke_after_authorization } => {
                session.send_transcript_proof(&offer_exchange.client_transcript_proof)
                    .map_err(anyhow::Error::msg)?;
                eprintln!("M1.3 AUTHORIZED client_proof=valid endpoint_transcript=verified dtls_identity=verified");
                if check_isolation {
                    test_competing_signaling(&*runtime, &control, &motion, address, hostname, status, credential, &offer_sdp).await?;
                }
                if check_resources {
                    run_resource_test(&*runtime, &control, &motion, monitor, address, hostname, status, credential).await?;
                }
                if revoke_after_authorization {
                    echo_signaling(&*runtime, &control, "control-before-active-revoke", 1).await?;
                    echo_signaling(&*runtime, &motion, "motion-before-active-revoke", 1).await?;
                    monitor.revoke_client(credential.fingerprint)?;
                    wait_device_closed(&monitor.device_status).await?;
                    send_after_revocation_and_reject_echo(
                        &*runtime,
                        &control,
                        &motion,
                        &monitor.device_status,
                    )
                    .await?;
                    match crate::signaling::Session::connect(
                        address, hostname, status.identity_fingerprint, status.tls_spki_hash, credential,
                    ) {
                        Err(error) if error == "endpoint rejected signaling: unauthorized" => {}
                        Err(error) => bail!("revoked client got unexpected signaling result: {error}"),
                        Ok(mut session) => {
                            session.close();
                            bail!("revoked client reopened a signaling session");
                        }
                    }
                    eprintln!("M1.3 REVOCATION usb_revoke=active_session_closed reconnect=unauthorized");
                    return Ok(offer_exchange.client_transcript_proof);
                }
                echo_signaling(&*runtime, &control, "control", 10).await?;
                echo_signaling(&*runtime, &motion, "motion", 10).await?;
                Ok(offer_exchange.client_transcript_proof)
            }
            TranscriptProofCase::Missing => {
                send_before_authorization_and_reject_echo(&*runtime, &control, &motion).await?;
                eprintln!("M1.3 PROOF negative=missing accepted_application_bytes=0 echoed=false");
                Ok(offer_exchange.client_transcript_proof)
            }
            TranscriptProofCase::Invalid => {
                let mut invalid = offer_exchange.client_transcript_proof;
                *invalid.last_mut().ok_or_else(|| anyhow::anyhow!("empty transcript proof"))? ^= 1;
                send_before_authorization_and_reject_echo(&*runtime, &control, &motion).await?;
                session.send_transcript_proof_frame(&invalid).map_err(anyhow::Error::msg)?;
                let rejection = session.receive_rejection().map_err(anyhow::Error::msg)?;
                ensure!(rejection.contains("unauthorized"), "invalid proof got unexpected error: {rejection}");
                eprintln!("M1.3 PROOF negative=invalid accepted_application_bytes=0 echoed=false result=unauthorized");
                Ok(Vec::new())
            }
            TranscriptProofCase::Replayed(replayed) => {
                send_before_authorization_and_reject_echo(&*runtime, &control, &motion).await?;
                session.send_transcript_proof_frame(replayed).map_err(anyhow::Error::msg)?;
                let rejection = session.receive_rejection().map_err(anyhow::Error::msg)?;
                ensure!(rejection.contains("unauthorized"), "replayed proof got unexpected error: {rejection}");
                eprintln!("M1.3 PROOF negative=replayed_previous_session accepted_application_bytes=0 echoed=false result=unauthorized");
                Ok(Vec::new())
            }
        }
    }.await;
    session.close();
    drop(session);
    let _ = timeout(&*runtime, Duration::from_secs(10), pc.close()).await;
    let cleanup = wait_device_closed(&monitor.device_status).await;
    let proof = result?;
    cleanup?;
    let metrics = monitor.fresh_snapshot()?;
    // Firmware resets this counter for each offer; subtracting the previous
    // session's count can hide acceptance in the replay-negative session.
    let accepted_bytes = metrics.application_bytes_received;
    if authorized_case {
        ensure!(
            accepted_bytes > 0,
            "authorized M1.3 session did not deliver application test payloads"
        );
    } else {
        ensure!(
            accepted_bytes == 0,
            "ESP32 accepted application data before successful authorization"
        );
    }
    Ok(proof)
}

async fn echo_signaling(
    runtime: &dyn Runtime,
    channel: &Arc<dyn DataChannel>,
    label: &str,
    count: u32,
) -> Result<()> {
    let mut rtts = Vec::with_capacity(count as usize);
    for sequence in 0..count {
        let payload = format!("kvmctl-m1.3-{label}:{sequence:06}");
        let sent = Instant::now();
        channel.send(BytesMut::from(payload.as_bytes())).await?;
        loop {
            match timeout(runtime, Duration::from_secs(5), channel.poll())
                .await
                .map_err(|_| anyhow::anyhow!("{label} M1.3 echo timeout"))?
            {
                Some(DataChannelEvent::OnMessage(message)) => {
                    ensure!(
                        message.data.as_ref() == payload.as_bytes(),
                        "{label} M1.3 echo mismatch"
                    );
                    rtts.push(sent.elapsed().as_micros());
                    break;
                }
                Some(DataChannelEvent::OnClose) | None => bail!("{label} M1.3 channel closed"),
                _ => ensure!(
                    sent.elapsed() < Duration::from_secs(5),
                    "{label} M1.3 echo timeout"
                ),
            }
        }
    }
    eprintln!(
        "M1.3 CHANNEL label={label} reliable={} ordered={} messages={} rtt_min_us={} rtt_max_us={}",
        channel.max_retransmits().await?.is_none()
            && channel.max_packet_life_time().await?.is_none(),
        channel.ordered().await?,
        count,
        rtts.iter().min().unwrap(),
        rtts.iter().max().unwrap()
    );
    Ok(())
}

async fn wait_device_closed(device_status: &AtomicU8) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if device_status.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "ESP32 transport did not return to idle after WSS close (state={})",
        device_status.load(Ordering::Acquire)
    );
}

fn read_until_closed(stream: &mut TcpStream, deadline: Instant, label: &str) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut byte = [0u8; 64];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= deadline {
                    bail!("stalled TLS socket `{label}` exceeded its handshake deadline");
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::NotConnected
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn run_resource_test(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    monitor: &PortMonitor,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential: &crate::credentials::ClientCredential,
) -> Result<()> {
    echo_signaling(runtime, control, "control-before-resource-test", 1).await?;
    echo_signaling(runtime, motion, "motion-before-resource-test", 1).await?;
    let mut stalled = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    std::thread::sleep(Duration::from_millis(150));

    let endpoint = status.identity_fingerprint;
    let tls_pin = status.tls_spki_hash;
    let address_copy = address;
    let host_copy = hostname.to_owned();
    let (probe_tx, probe_rx) = std::sync::mpsc::channel();
    let cap_probe = std::thread::spawn(move || {
        let result = match crate::credentials::ClientCredential::generate() {
            Ok(outsider) => crate::signaling::Session::connect(
                address_copy,
                &host_copy,
                endpoint,
                tls_pin,
                &outsider,
            )
            .map(|mut session| {
                session.close();
            })
            .map_err(anyhow::Error::msg),
            Err(error) => Err(anyhow::Error::msg(error)),
        };
        let _ = probe_tx.send(result);
    });
    match probe_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => {
            bail!(
                "a third WSS handshake completed while an active session and stalled TLS socket occupied both slots"
            )
        }
        Ok(Err(error)) if error.to_string().contains("unauthorized") => {
            bail!(
                "a third WSS handshake reached client authorization while an active session and stalled TLS socket occupied both slots"
            )
        }
        Ok(Err(_)) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            bail!("TLS socket-cap probe stopped unexpectedly")
        }
    }

    let deadline = Instant::now() + Duration::from_secs(8);
    read_until_closed(&mut stalled, deadline, "B")?;
    let probe = probe_rx.recv_timeout(Duration::from_secs(2)).map_err(|_| {
        anyhow::anyhow!("third TLS/WSS connection did not recover after stalled handshakes expired")
    })?;
    ensure!(
        probe.is_err(),
        "third WSS probe unexpectedly authenticated without a client credential"
    );
    cap_probe
        .join()
        .map_err(|_| anyhow::anyhow!("TLS socket-cap probe panicked"))?;
    eprintln!(
        "M1.3 RESOURCE active_wss=1 stalled_tls=1 deadline_closed=true concurrent_socket_cap=2 third_completed_while_stalled=false"
    );
    echo_signaling(runtime, control, "control-after-stalled-tls", 1).await?;
    echo_signaling(runtime, motion, "motion-after-stalled-tls", 1).await?;

    let rate_limit_baseline = monitor.fresh_snapshot()?.tls_rate_limited;
    let mut completed_unauthorized = 0usize;
    let mut throttled = None;
    for attempt in 0..8 {
        let outsider =
            crate::credentials::ClientCredential::generate().map_err(anyhow::Error::msg)?;
        match crate::signaling::Session::connect(address, hostname, endpoint, tls_pin, &outsider) {
            Err(error) if error.contains("unauthorized") => completed_unauthorized += 1,
            Err(error) => {
                let rate_limited = monitor.fresh_snapshot()?.tls_rate_limited;
                let count =
                    checked_rate_limit_delta(rate_limit_baseline, rate_limited, attempt + 1)
                        .with_context(|| {
                            format!(
                                "WSS flood failed without firmware rate-limit evidence: {error}"
                            )
                        })?;
                throttled = Some((Instant::now(), error, count));
                break;
            }
            Ok(mut session) => {
                session.close();
                bail!("un-enrolled resource-flood client authenticated");
            }
        }
        if attempt < 7 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    ensure!(
        completed_unauthorized > 0,
        "handshake flood did not complete any baseline WSS authentication attempts"
    );
    let (throttled_at, throttle_error, rate_limit_count) = throttled.ok_or_else(|| {
        anyhow::anyhow!(
            "eight rapid WSS handshakes were not rate limited; last attempt remained unauthorized"
        )
    })?;
    eprintln!(
        "M1.3 RESOURCE handshake_flood_attempts={} completed_before_limit={} throttled=true firmware_rate_limit_delta={} diagnostic={}",
        completed_unauthorized + 1,
        completed_unauthorized,
        rate_limit_count,
        throttle_error
    );
    echo_signaling(runtime, control, "control-after-handshake-flood", 1).await?;
    echo_signaling(runtime, motion, "motion-after-handshake-flood", 1).await?;

    let recovery_deadline = throttled_at + Duration::from_secs(15);
    let mut recovered_session = None;
    while Instant::now() < recovery_deadline {
        match crate::signaling::Session::connect(address, hostname, endpoint, tls_pin, credential) {
            Ok(session) => {
                recovered_session = Some(session);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
    let mut recovered_session = recovered_session
        .ok_or_else(|| anyhow::anyhow!("WSS admission did not recover after token refill"))?;
    recovered_session
        .probe_oversized_reassembly()
        .map_err(anyhow::Error::msg)?;
    recovered_session.close();
    echo_signaling(runtime, control, "control-after-oversized-wss", 1).await?;
    echo_signaling(runtime, motion, "motion-after-oversized-wss", 1).await?;
    eprintln!("M1.3 RESOURCE handshake_recovery=true partial_message_over_limit=rejected");
    Ok(())
}

fn checked_rate_limit_delta(before: u32, after: u32, attempts: u32) -> Result<u32> {
    let delta = after.wrapping_sub(before);
    ensure!(
        delta > 0 && delta <= attempts,
        "firmware TLS rate-limit counter did not advance within the flood attempt count"
    );
    Ok(delta)
}

pub(crate) fn run_signaling_test(
    port: crate::serial::Port,
    service: &crate::discovery::Service,
    status: crate::Status,
    credential: crate::credentials::ClientCredential,
    scenario: &str,
) -> Result<()> {
    ensure!(
        matches!(
            scenario,
            "success" | "missing" | "invalid" | "replay" | "resources" | "revoke-active"
        ),
        "unknown signaling-test scenario"
    );
    install_certificate_log()?;
    let address = crate::discovery::socket_address(service).map_err(anyhow::Error::msg)?;
    let hostname = service.hostname.clone();
    let monitor = PortMonitor::start(port);
    let runtime = default_runtime().context("no WebRTC runtime")?;
    let mut result: Option<Result<()>> = None;
    runtime.block_on(Box::pin(async {
        result = Some(
            async {
                match scenario {
                    "success" => {
                        let _proof = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Valid {
                                check_isolation: true,
                                check_resources: false,
                                revoke_after_authorization: false,
                            },
                        )
                        .await?;
                    }
                    "missing" => {
                        let _proof = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Missing,
                        )
                        .await?;
                    }
                    "invalid" => {
                        let _proof = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Invalid,
                        )
                        .await?;
                    }
                    "replay" => {
                        let valid = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Valid {
                                check_isolation: false,
                                check_resources: false,
                                revoke_after_authorization: false,
                            },
                        )
                        .await?;
                        let _ = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Replayed(&valid),
                        )
                        .await?;
                    }
                    "resources" => {
                        let _proof = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Valid {
                                check_isolation: false,
                                check_resources: true,
                                revoke_after_authorization: false,
                            },
                        )
                        .await?;
                    }
                    "revoke-active" => {
                        let _proof = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Valid {
                                check_isolation: false,
                                check_resources: false,
                                revoke_after_authorization: true,
                            },
                        )
                        .await?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            }
            .await,
        );
    }));
    let result = result.context("runtime did not execute")?;
    let (port, metrics, usb_admin_ok) = monitor.finish()?;
    let _ = port;
    if let Err(error) = result {
        let diagnostic = &metrics.tls_diagnostic[..metrics.tls_diagnostic_len];
        if !diagnostic.is_empty() {
            return Err(anyhow::Error::msg(format!(
                "{error}; ESP TLS diagnostic: {}",
                String::from_utf8_lossy(diagnostic)
            )));
        }
        return Err(error);
    }
    ensure!(
        usb_admin_ok,
        "USB status administration stopped responding during signaling checks"
    );
    eprintln!(
        "M1.3 RESOURCE scenario={scenario} state={} internal_free_start_bytes={} internal_free_min_bytes={} internal_free_after_bytes={} psram_free_start_bytes={} psram_free_min_bytes={} psram_free_after_bytes={} device_application_bytes_received={}",
        metrics.state,
        metrics.internal_free_start,
        metrics.internal_free_min,
        metrics.internal_free_end,
        metrics.psram_free_start,
        metrics.psram_free_min,
        metrics.psram_free_end,
        metrics.application_bytes_received,
    );
    eprintln!(
        "M1.3 SIGNALING COMPLETE scenario={scenario} hid_reports=0 physical_input_capture=off"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_evidence_requires_counter_advance_and_handles_wrap() {
        assert_eq!(checked_rate_limit_delta(7, 8, 1).unwrap(), 1);
        assert_eq!(checked_rate_limit_delta(u32::MAX, 0, 1).unwrap(), 1);
        assert!(checked_rate_limit_delta(7, 7, 8).is_err());
        assert!(checked_rate_limit_delta(7, 0, 8).is_err());
        assert!(checked_rate_limit_delta(7, 9, 1).is_err());
    }

    #[test]
    fn fingerprint_parser_fails_closed() {
        let digest = [0xab; 32];
        let line = format!(
            "a=fingerprint:sha-256 {}",
            digest
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(":")
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
        assert!(
            apply_test_fingerprint_fault(sdp, Some("malformed"))
                .unwrap()
                .contains("a=fingerprint:sha-256 invalid\r\n")
        );
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
