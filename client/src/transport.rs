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
        "M1.2 RESOURCE stage={stage} device_state={} internal_free_start_bytes={} internal_free_min_bytes={} internal_free_after_bytes={} psram_free_start_bytes={} psram_free_min_bytes={} psram_free_after_bytes={} device_channel_opens={} device_application_bytes_received={} device_peer_task_cpu_us={} device_total_soc_cpu_us={} device_event_processing_us={} device_response_queue_residence_us={}",
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
        metrics.total_soc_cpu_us,
        metrics.event_processing_us,
        metrics.response_queue_residence_us,
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

pub(crate) fn run_test(mut port: crate::serial::Port) -> Result<()> {
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
    let hid_baseline = port
        .transport_status()
        .map_err(anyhow::Error::msg)?
        .hid_reports;
    let runtime = default_runtime().context("no WebRTC runtime")?;
    let mut result: Option<Result<()>> = None;
    runtime.block_on(Box::pin(async {
        result = Some(
            async {
                let port = exchange(runtime.clone(), port, "initial").await?;
                let mut port = exchange(runtime.clone(), port, "reconnect").await?;
                let hid_after = port
                    .transport_status()
                    .map_err(anyhow::Error::msg)?
                    .hid_reports;
                ensure!(
                    hid_after == hid_baseline,
                    "unexpected HID reports in USB transport test"
                );
                Ok(())
            }
            .await,
        );
    }));
    result.context("runtime did not execute")??;
    eprintln!("M1.2 TRANSPORT COMPLETE sessions=2 reconnect=clean hid_reports=0");
    Ok(())
}

enum MonitorCommand {
    Snapshot(std::sync::mpsc::Sender<Result<crate::TransportMetrics, String>>),
    RevokeClient([u8; 32], std::sync::mpsc::Sender<Result<(), String>>),
    RestartEndpoint(std::sync::mpsc::Sender<Result<(), String>>),
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
                    Ok(MonitorCommand::RestartEndpoint(reply)) => {
                        let _ = reply.send(port.transport_close());
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

    fn restart_endpoint(&self) -> Result<()> {
        let (reply, receiver) = std::sync::mpsc::channel();
        self.stop
            .send(MonitorCommand::RestartEndpoint(reply))
            .map_err(|_| {
                anyhow::anyhow!("USB transport monitor stopped before endpoint restart")
            })?;
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("USB endpoint restart timed out"))?
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScriptMode {
    Transport,
    HidBench,
    HidSustain,
}
const SCRIPTED_HID_REPORTS: u32 = 7;
const SUSTAIN_REPEAT_COUNT: u32 = 100;
const SUSTAIN_DURATION_MS: u64 = 10_000;
const SUSTAIN_HID_REPORTS: u32 = SUSTAIN_REPEAT_COUNT * 5 + 2;

enum TranscriptProofCase<'a> {
    Valid {
        scripted: Option<ScriptMode>,
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

struct SignalingPeer<P> {
    session: crate::signaling::Session,
    pc: P,
    control: Arc<dyn DataChannel>,
    motion: Arc<dyn DataChannel>,
    offer_sdp: String,
    proof: Vec<u8>,
}

impl<P: PeerConnection> SignalingPeer<P> {
    async fn close(mut self, runtime: &dyn Runtime) {
        self.session.close();
        drop(self.session);
        let _ = timeout(runtime, Duration::from_secs(10), self.pc.close()).await;
    }
}

async fn open_signaling_peer(
    runtime: Arc<dyn Runtime>,
    device_status: &AtomicU8,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential: &crate::credentials::ClientCredential,
) -> Result<SignalingPeer<impl PeerConnection>> {
    let started = Instant::now();
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
        wait_open(
            &*runtime,
            &control,
            &motion,
            &peer_failed,
            device_status,
            &connection_states,
            false,
        ).await?;
        eprintln!("M1.3 DTLS_OPEN elapsed_ms={} control=ordered-reliable motion=unordered-zero-retransmit",
                  started.elapsed().as_millis());

        Ok((control, motion, offer_sdp, offer_exchange.client_transcript_proof))
    }.await;
    match result {
        Ok((control, motion, offer_sdp, proof)) => Ok(SignalingPeer {
            session,
            pc,
            control,
            motion,
            offer_sdp,
            proof,
        }),
        Err(error) => {
            session.close();
            drop(session);
            let _ = timeout(&*runtime, Duration::from_secs(10), pc.close()).await;
            Err(error)
        }
    }
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
    let authorized_case = matches!(&proof_case, TranscriptProofCase::Valid { .. });
    let mut peer = open_signaling_peer(
        runtime.clone(),
        &monitor.device_status,
        address,
        hostname,
        status,
        credential,
    )
    .await?;
    let control = &peer.control;
    let motion = &peer.motion;
    let offer_sdp = &peer.offer_sdp;
    let proof = &peer.proof;
    let result = async {
        match proof_case {
            TranscriptProofCase::Valid { scripted, check_isolation, check_resources, revoke_after_authorization } => {
                peer.session.send_transcript_proof(proof)
                    .map_err(anyhow::Error::msg)?;
                eprintln!("M1.3 AUTHORIZED client_proof=valid endpoint_transcript=verified dtls_identity=verified");
                if let Some(mode) = scripted {
                    scripted_transport(
                        &*runtime,
                        monitor,
                        control,
                        motion,
                        peer.session.session_id(),
                        credential.fingerprint,
                        status.identity_fingerprint,
                        mode,
                    )
                    .await?;
                }
                if check_isolation {
                    test_competing_signaling(&*runtime, control, motion, address, hostname, status, credential, offer_sdp).await?;
                }
                if check_resources {
                    run_resource_test(&*runtime, control, motion, monitor, address, hostname, status, credential).await?;
                }
                if revoke_after_authorization {
                    echo_signaling(&*runtime, control, "control-before-active-revoke", 1).await?;
                    echo_signaling(&*runtime, motion, "motion-before-active-revoke", 1).await?;
                    monitor.revoke_client(credential.fingerprint)?;
                    wait_device_closed(&monitor.device_status).await?;
                    send_after_revocation_and_reject_echo(
                        &*runtime,
                        control,
                        motion,
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
                    return Ok(proof.clone());
                }
                echo_signaling(&*runtime, control, "control", 10).await?;
                echo_signaling(&*runtime, motion, "motion", 10).await?;
                Ok(proof.clone())
            }
            TranscriptProofCase::Missing => {
                send_before_authorization_and_reject_echo(&*runtime, control, motion).await?;
                eprintln!("M1.3 PROOF negative=missing accepted_application_bytes=0 echoed=false");
                Ok(proof.clone())
            }
            TranscriptProofCase::Invalid => {
                let mut invalid = proof.clone();
                *invalid.last_mut().ok_or_else(|| anyhow::anyhow!("empty transcript proof"))? ^= 1;
                send_before_authorization_and_reject_echo(&*runtime, control, motion).await?;
                peer.session.send_transcript_proof_frame(&invalid).map_err(anyhow::Error::msg)?;
                let rejection = peer.session.receive_rejection().map_err(anyhow::Error::msg)?;
                ensure!(rejection.contains("unauthorized"), "invalid proof got unexpected error: {rejection}");
                eprintln!("M1.3 PROOF negative=invalid accepted_application_bytes=0 echoed=false result=unauthorized");
                Ok(Vec::new())
            }
            TranscriptProofCase::Replayed(replayed) => {
                send_before_authorization_and_reject_echo(&*runtime, control, motion).await?;
                peer.session.send_transcript_proof_frame(replayed).map_err(anyhow::Error::msg)?;
                let rejection = peer.session.receive_rejection().map_err(anyhow::Error::msg)?;
                ensure!(rejection.contains("unauthorized"), "replayed proof got unexpected error: {rejection}");
                eprintln!("M1.3 PROOF negative=replayed_previous_session accepted_application_bytes=0 echoed=false result=unauthorized");
                Ok(Vec::new())
            }
        }
    }.await;
    peer.close(&*runtime).await;
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

pub(crate) fn run_foreground_session(
    service: &crate::discovery::Service,
    status: crate::Status,
    credential: crate::credentials::ClientCredential,
) -> Result<SocketAddr> {
    install_certificate_log()?;
    let address = crate::discovery::socket_address(service).map_err(anyhow::Error::msg)?;
    let hostname = service.hostname.clone();
    let runtime = default_runtime().context("no WebRTC runtime")?;
    let device_status = AtomicU8::new(1);
    let mut result: Option<Result<()>> = None;
    runtime.block_on(Box::pin(async {
        result = Some(
            async {
                let mut peer = open_signaling_peer(
                    runtime.clone(),
                    &device_status,
                    address,
                    &hostname,
                    &status,
                    &credential,
                )
                .await?;
                let session_result = async {
                    peer.session
                        .send_transcript_proof(&peer.proof)
                        .map_err(anyhow::Error::msg)
                        .context("foreground session authorization")?;
                    let mut responses = ResponseCursor::new(peer.session.session_id());
                    send_hid_event(
                        &peer.control,
                        crate::hid_protocol::Envelope {
                            message_type: crate::hid_protocol::LEASE_REQUEST,
                            session_id: peer.session.session_id(),
                            lease_epoch: 0,
                            sequence: 0,
                            event: crate::hid_protocol::Event::LeaseRequest,
                        },
                    )
                    .await?;
                    let (response, epoch) =
                        receive_endpoint_response_any_epoch(
                            &*runtime,
                            &peer.control,
                            &mut responses,
                        )
                        .await?;
                    require_response(
                        response,
                        EndpointResponse::LeaseState {
                            owner: Some(credential.fingerprint),
                            state: 1,
                        },
                    )?;
                    eprintln!(
                        "M2.1 SESSION authenticated=true input_local=true physical_input_capture=off"
                    );
                    println!("Connected/local. Press Enter to disconnect.");
                    let (done_tx, done_rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let mut line = String::new();
                        let _ = std::io::stdin().read_line(&mut line);
                        let _ = done_tx.send(());
                    });
                    let mut sequence = 1;
                    let mut heartbeat = Instant::now();
                    loop {
                        if done_rx.try_recv().is_ok() {
                            send_hid_event(
                                &peer.control,
                                crate::hid_protocol::Envelope {
                                    message_type: crate::hid_protocol::LEASE_RELEASE,
                                    session_id: peer.session.session_id(),
                                    lease_epoch: epoch,
                                    sequence,
                                    event: crate::hid_protocol::Event::LeaseRelease,
                                },
                            )
                            .await?;
                            require_response(
                                receive_endpoint_response(
                                    &*runtime,
                                    &peer.control,
                                    &mut responses,
                                    next_lease_epoch(epoch),
                                )
                                .await?,
                                EndpointResponse::LeaseState {
                                    owner: None,
                                    state: 0,
                                },
                            )?;
                            break;
                        }
                        if heartbeat.elapsed() >= Duration::from_secs(1) {
                            send_hid_event(
                                &peer.control,
                                crate::hid_protocol::Envelope {
                                    message_type: crate::hid_protocol::HEARTBEAT,
                                    session_id: peer.session.session_id(),
                                    lease_epoch: epoch,
                                    sequence,
                                    event: crate::hid_protocol::Event::Heartbeat,
                                },
                            )
                            .await?;
                            sequence += 1;
                            heartbeat = Instant::now();
                        }
                        if let Ok(Some(DataChannelEvent::OnClose)) =
                            timeout(&*runtime, Duration::from_millis(100), peer.control.poll()).await
                        {
                            bail!("foreground control channel closed");
                        }
                        runtime.sleep(Duration::from_millis(50)).await;
                    }
                    Ok(())
                }
                .await;
                peer.close(&*runtime).await;
                session_result
            }
            .await,
        );
    }));
    result.context("runtime did not execute")??;
    Ok(address)
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

#[derive(Debug, PartialEq, Eq)]
enum EndpointResponse {
    LeaseState {
        owner: Option<[u8; 32]>,
        state: u8,
    },
    TakeoverResponse {
        request_id: [u8; 16],
        accepted: bool,
    },
    TakeoverNotice {
        request_id: [u8; 16],
        requester: [u8; 32],
        response_window_ms: u64,
    },
    Error {
        code: String,
    },
}

struct ResponseCursor {
    session_id: [u8; 16],
    next_sequence: u64,
}

impl ResponseCursor {
    fn new(session_id: [u8; 16]) -> Self {
        Self {
            session_id,
            next_sequence: 0,
        }
    }

    fn decode(&mut self, data: &[u8], expected_epoch: u32) -> Result<EndpointResponse> {
        let envelope = self.decode_envelope(data)?;
        ensure!(
            envelope.lease_epoch == expected_epoch,
            "endpoint response lease epoch mismatch: expected {expected_epoch}, received {} ({:?})",
            envelope.lease_epoch,
            envelope.event
        );
        self.advance()?;
        Self::response(envelope.event)
    }

    fn decode_any_epoch(&mut self, data: &[u8]) -> Result<(EndpointResponse, u32)> {
        let envelope = self.decode_envelope(data)?;
        let epoch = envelope.lease_epoch;
        self.advance()?;
        Ok((Self::response(envelope.event)?, epoch))
    }

    fn decode_envelope(&self, data: &[u8]) -> Result<crate::hid_protocol::Envelope> {
        let envelope = crate::hid_protocol::decode(data)
            .map_err(|error| anyhow::anyhow!("strict event decode failed: {error:?}"))?;
        ensure!(
            envelope.session_id == self.session_id,
            "endpoint response session ID mismatch"
        );
        ensure!(
            envelope.sequence == self.next_sequence,
            "endpoint response sequence mismatch: expected {}, received {}",
            self.next_sequence,
            envelope.sequence
        );
        Ok(envelope)
    }

    fn advance(&mut self) -> Result<()> {
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .context("endpoint response sequence exhausted")?;
        Ok(())
    }

    fn response(event: crate::hid_protocol::Event) -> Result<EndpointResponse> {
        Ok(match event {
            crate::hid_protocol::Event::LeaseState { owner, state } => {
                EndpointResponse::LeaseState { owner, state }
            }
            crate::hid_protocol::Event::TakeoverResponse {
                request_id,
                accepted,
            } => EndpointResponse::TakeoverResponse {
                request_id,
                accepted,
            },
            crate::hid_protocol::Event::TakeoverNotice {
                request_id,
                requester,
                response_window_ms,
            } => EndpointResponse::TakeoverNotice {
                request_id,
                requester,
                response_window_ms,
            },
            crate::hid_protocol::Event::Error { code } => EndpointResponse::Error { code },
            _ => bail!("unexpected endpoint response event"),
        })
    }
}

fn require_response(response: EndpointResponse, expected: EndpointResponse) -> Result<()> {
    match response {
        EndpointResponse::Error { code } => bail!("endpoint HID error: {code}"),
        response => ensure!(response == expected, "unexpected endpoint response"),
    }
    Ok(())
}

async fn receive_endpoint_response(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    cursor: &mut ResponseCursor,
    expected_epoch: u32,
) -> Result<EndpointResponse> {
    loop {
        match timeout(runtime, Duration::from_secs(5), control.poll()).await? {
            Some(DataChannelEvent::OnMessage(message)) => {
                return cursor.decode(&message.data, expected_epoch);
            }
            Some(DataChannelEvent::OnClose) | None => bail!("scripted control channel closed"),
            _ => {}
        }
    }
}

async fn receive_endpoint_response_any_epoch(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    cursor: &mut ResponseCursor,
) -> Result<(EndpointResponse, u32)> {
    loop {
        match timeout(runtime, Duration::from_secs(5), control.poll()).await? {
            Some(DataChannelEvent::OnMessage(message)) => {
                return cursor.decode_any_epoch(&message.data);
            }
            Some(DataChannelEvent::OnClose) | None => bail!("foreground control channel closed"),
            _ => {}
        }
    }
}

async fn send_hid_event(
    channel: &Arc<dyn DataChannel>,
    event: crate::hid_protocol::Envelope,
) -> Result<()> {
    let encoded = crate::hid_protocol::encode(&event);
    channel.send(BytesMut::from(encoded.as_slice())).await?;
    Ok(())
}

async fn send_timed_hid_event(
    channel: &Arc<dyn DataChannel>,
    event: crate::hid_protocol::Envelope,
    timing: Option<&crate::raw_input::SendTiming>,
) -> Result<()> {
    if let Some(timing) = timing {
        timing.begin();
    }
    let result = send_hid_event(channel, event).await;
    if let Some(timing) = timing {
        timing.finish();
    }
    result
}

fn wait_for_scripted_telemetry(
    monitor: &PortMonitor,
    baseline: &crate::TransportMetrics,
    expected_events: u32,
    expected_sequence: u64,
) -> Result<()> {
    let target = baseline
        .scripted_events
        .checked_add(expected_events)
        .context("scripted telemetry event count overflowed")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let metrics = monitor.fresh_snapshot()?;
        ensure!(
            metrics.scripted_rejections == baseline.scripted_rejections,
            "scripted event was rejected before telemetry barrier"
        );
        ensure!(
            metrics.scripted_drops == baseline.scripted_drops
                && metrics.scripted_reorders == baseline.scripted_reorders,
            "scripted event was dropped or reordered before telemetry barrier"
        );
        if metrics.scripted_events >= target && metrics.last_event_sequence == expected_sequence {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "scripted telemetry barrier timed out: events={} target={} last_sequence={} expected_sequence={}",
                metrics.scripted_events,
                target,
                metrics.last_event_sequence,
                expected_sequence
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_scripted_count(
    monitor: &PortMonitor,
    baseline: &crate::TransportMetrics,
    expected_events: u32,
) -> Result<()> {
    let target = baseline
        .scripted_events
        .checked_add(expected_events)
        .context("scripted telemetry event count overflowed")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let metrics = monitor.fresh_snapshot()?;
        ensure!(
            metrics.scripted_rejections == baseline.scripted_rejections
                && metrics.scripted_drops == baseline.scripted_drops
                && metrics.scripted_reorders == baseline.scripted_reorders,
            "sustained HID event was rejected, dropped, or reordered"
        );
        if metrics.scripted_events >= target {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "sustained HID telemetry barrier timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

async fn scripted_transport(
    runtime: &dyn Runtime,
    monitor: &PortMonitor,
    control: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    session_id: [u8; 16],
    client_fingerprint: [u8; 32],
    endpoint_fingerprint: [u8; 32],
    mode: ScriptMode,
) -> Result<()> {
    use crate::hid_protocol::{self, Envelope, Event};

    let event = |message_type, lease_epoch, sequence, event| Envelope {
        message_type,
        session_id,
        lease_epoch,
        sequence,
        event,
    };
    let mut responses = ResponseCursor::new(session_id);
    let epoch = next_lease_epoch(monitor.fresh_snapshot()?.lease_epoch);

    send_hid_event(
        control,
        event(hid_protocol::LEASE_REQUEST, 0, 0, Event::LeaseRequest),
    )
    .await?;
    require_response(
        receive_endpoint_response(runtime, control, &mut responses, epoch).await?,
        EndpointResponse::LeaseState {
            owner: Some(client_fingerprint),
            state: 1,
        },
    )
    .context("lease request response")?;
    let hid_bench = matches!(mode, ScriptMode::HidBench | ScriptMode::HidSustain);
    let capture_hid = mode == ScriptMode::HidBench;
    let offset = u64::from(hid_bench);
    let hid_baseline = monitor.fresh_snapshot()?.hid_reports;
    let mut hid_timing = None;
    let observer = if capture_hid {
        ensure!(
            hid_baseline.is_some(),
            "firmware did not expose HID bench telemetry"
        );
        let serial = crate::hex(&endpoint_fingerprint[..12]);
        let (observer, timing) = crate::raw_input::Observer::start_timed(0x303a, 0x4004, &serial)?;
        hid_timing = Some(timing);
        Some(observer)
    } else {
        None
    };
    if hid_bench {
        send_hid_event(
            control,
            event(hid_protocol::HID_BENCH_ARM, epoch, 1, Event::HidBenchArm),
        )
        .await?;
        require_response(
            receive_endpoint_response(runtime, control, &mut responses, epoch).await?,
            EndpointResponse::LeaseState {
                owner: Some(client_fingerprint),
                state: 1,
            },
        )
        .context("HID bench arm response")?;
    }
    if let Some(observer) = &observer {
        observer.check_focus()?;
    }
    let telemetry_baseline = monitor
        .fresh_snapshot()
        .context("scripted telemetry baseline")?;

    if mode == ScriptMode::HidSustain {
        let started = Instant::now();
        for repeat in 0..SUSTAIN_REPEAT_COUNT {
            let control_sequence = 2 + u64::from(repeat) * 4;
            send_hid_event(
                control,
                event(
                    hid_protocol::HEARTBEAT,
                    epoch,
                    control_sequence,
                    Event::Heartbeat,
                ),
            )
            .await?;
            send_hid_event(
                control,
                event(
                    hid_protocol::KEYBOARD_STATE,
                    epoch,
                    control_sequence + 1,
                    Event::KeyboardState {
                        modifiers: 2,
                        keys: vec![(7, 4)],
                    },
                ),
            )
            .await?;
            send_hid_event(
                control,
                event(
                    hid_protocol::MOUSE_BUTTONS,
                    epoch,
                    control_sequence + 2,
                    Event::MouseButtons { mask: 1 },
                ),
            )
            .await?;
            send_hid_event(
                control,
                event(
                    hid_protocol::WHEEL,
                    epoch,
                    control_sequence + 3,
                    Event::Wheel {
                        vertical: 1,
                        horizontal: -1,
                    },
                ),
            )
            .await?;
            send_hid_event(
                motion,
                event(
                    hid_protocol::MOTION,
                    epoch,
                    u64::from(repeat),
                    Event::Motion { dx: 12, dy: -4 },
                ),
            )
            .await?;
            // Motion uses the production unordered/zero-retransmit channel.  Let
            // each sample reach the board before sending the next sequence so
            // this sustained-path measurement fails on loss, not avoidable
            // test-induced reordering.
            wait_for_scripted_count(monitor, &telemetry_baseline, (repeat + 1) * 5)?;
            let target = started
                + Duration::from_millis(
                    SUSTAIN_DURATION_MS * u64::from(repeat + 1) / u64::from(SUSTAIN_REPEAT_COUNT),
                );
            let delay = target.saturating_duration_since(Instant::now());
            if !delay.is_zero() {
                runtime.sleep(delay).await;
            }
        }
        wait_for_scripted_count(monitor, &telemetry_baseline, SUSTAIN_REPEAT_COUNT * 5)?;
        send_hid_event(
            control,
            event(
                hid_protocol::LEASE_RELEASE,
                epoch,
                2 + u64::from(SUSTAIN_REPEAT_COUNT) * 4,
                Event::LeaseRelease,
            ),
        )
        .await?;
        require_response(
            receive_endpoint_response(runtime, control, &mut responses, next_lease_epoch(epoch))
                .await?,
            EndpointResponse::LeaseState {
                owner: None,
                state: 0,
            },
        )?;
        let final_metrics = wait_for_hid_reports(
            monitor,
            hid_baseline.context("missing report count")?,
            SUSTAIN_HID_REPORTS,
        )?;
        ensure!(
            final_metrics.normalized_keys == 0 && final_metrics.normalized_buttons == 0,
            "sustained HID bench left held state"
        );
        eprintln!(
            "M1.4 SUSTAINED_HID repeat_count={} duration_ms={} scripted_events={} hid_reports_delta={} physical_input_capture=off",
            SUSTAIN_REPEAT_COUNT,
            SUSTAIN_DURATION_MS,
            SUSTAIN_REPEAT_COUNT * 5,
            SUSTAIN_HID_REPORTS,
        );
        return Ok(());
    }

    send_hid_event(
        control,
        event(hid_protocol::HEARTBEAT, epoch, 1 + offset, Event::Heartbeat),
    )
    .await?;
    send_timed_hid_event(
        control,
        event(
            hid_protocol::KEYBOARD_STATE,
            epoch,
            2 + offset,
            Event::KeyboardState {
                modifiers: 2,
                keys: vec![(7, 4)],
            },
        ),
        hid_timing.as_ref(),
    )
    .await?;
    if let Some(observer) = &observer {
        observer.check_focus()?;
    }
    send_timed_hid_event(
        control,
        event(
            hid_protocol::MOUSE_BUTTONS,
            epoch,
            3 + offset,
            Event::MouseButtons { mask: 1 },
        ),
        hid_timing.as_ref(),
    )
    .await?;
    send_timed_hid_event(
        control,
        event(
            hid_protocol::WHEEL,
            epoch,
            4 + offset,
            Event::Wheel {
                vertical: 1,
                horizontal: -1,
            },
        ),
        hid_timing.as_ref(),
    )
    .await?;
    wait_for_scripted_telemetry(monitor, &telemetry_baseline, 4, 4 + offset)
        .context("control telemetry barrier")?;
    if hid_bench {
        wait_for_hid_reports(monitor, hid_baseline.context("missing report count")?, 4)?;
    }
    if let Some(observer) = &observer {
        observer.check_focus()?;
    }
    send_timed_hid_event(
        motion,
        event(
            hid_protocol::MOTION,
            epoch,
            0,
            Event::Motion { dx: 12, dy: -4 },
        ),
        hid_timing.as_ref(),
    )
    .await?;
    wait_for_scripted_telemetry(monitor, &telemetry_baseline, 5, 0)
        .context("motion telemetry barrier")?;
    if hid_bench {
        wait_for_hid_reports(monitor, hid_baseline.context("missing report count")?, 5)?;
    }
    send_hid_event(
        control,
        event(
            hid_protocol::LEASE_RELEASE,
            epoch,
            5 + offset,
            Event::LeaseRelease,
        ),
    )
    .await?;
    require_response(
        receive_endpoint_response(runtime, control, &mut responses, next_lease_epoch(epoch))
            .await?,
        EndpointResponse::LeaseState {
            owner: None,
            state: 0,
        },
    )
    .context("lease release response")?;
    if let Some(before) = hid_baseline {
        let expected = if hid_bench { SCRIPTED_HID_REPORTS } else { 0 };
        let metrics = wait_for_hid_reports(monitor, before, expected)?;
        ensure!(
            metrics.normalized_keys == 0 && metrics.normalized_buttons == 0,
            "lease release left held state"
        );
    }
    if let Some(observer) = observer {
        observer.verify()?;
    } else {
        eprintln!(
            "M1.4 SCRIPTED secure_path=true key=true modifier=true button=true wheel=true motion=true hid_reports=0 physical_input_capture=off"
        );
    }
    Ok(())
}

fn wait_for_hid_reports(
    monitor: &PortMonitor,
    before: u32,
    expected: u32,
) -> Result<crate::TransportMetrics> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let metrics = monitor.fresh_snapshot()?;
        let count = metrics
            .hid_reports
            .context("HID report telemetry disappeared")?
            .wrapping_sub(before);
        if count == expected {
            return Ok(metrics);
        }
        ensure!(
            count < expected && Instant::now() < deadline,
            "expected {expected} HID reports, observed {count}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct TakeoverClient<'a> {
    label: &'static str,
    runtime: &'a dyn Runtime,
    control: &'a Arc<dyn DataChannel>,
    responses: ResponseCursor,
    sequence: u64,
}

impl<'a> TakeoverClient<'a> {
    fn new<P>(label: &'static str, runtime: &'a dyn Runtime, peer: &'a SignalingPeer<P>) -> Self {
        Self {
            label,
            runtime,
            control: &peer.control,
            responses: ResponseCursor::new(peer.session.session_id()),
            sequence: 0,
        }
    }

    async fn send(
        &mut self,
        message_type: u64,
        epoch: u32,
        event: crate::hid_protocol::Event,
    ) -> Result<()> {
        send_hid_event(
            self.control,
            crate::hid_protocol::Envelope {
                message_type,
                session_id: self.responses.session_id,
                lease_epoch: epoch,
                sequence: self.sequence,
                event,
            },
        )
        .await?;
        self.sequence += 1;
        Ok(())
    }

    async fn expect(&mut self, epoch: u32, expected: EndpointResponse) -> Result<()> {
        let received =
            receive_endpoint_response(self.runtime, self.control, &mut self.responses, epoch)
                .await
                .with_context(|| {
                    format!(
                        "client {} expected {expected:?} at epoch {epoch}",
                        self.label
                    )
                })?;
        eprintln!(
            "M1.4 TAKEOVER client={} epoch={epoch} response_sequence={} response={received:?}",
            self.label,
            self.responses.next_sequence - 1
        );
        ensure!(
            received == expected,
            "client {} expected {expected:?}",
            self.label
        );
        Ok(())
    }
}

fn next_lease_epoch(epoch: u32) -> u32 {
    epoch.wrapping_add(1).max(1)
}

async fn takeover_heartbeat_processed(
    runtime: &dyn Runtime,
    monitor: &PortMonitor,
    epoch: u32,
    sequence: u64,
    deadline: Instant,
) -> Result<crate::TransportMetrics> {
    loop {
        let metrics = monitor.snapshot();
        ensure!(
            metrics.lease_epoch == epoch
                && metrics.normalized_keys == 1
                && metrics.normalized_buttons == 1,
            "A lost its lease or held state before the takeover window ended"
        );
        if metrics.last_event_sequence == sequence {
            return Ok(metrics);
        }
        ensure!(
            Instant::now() < deadline,
            "A heartbeat {sequence} was not processed"
        );
        runtime.sleep(Duration::from_millis(20)).await;
    }
}

async fn exercise_takeover(
    monitor: &PortMonitor,
    mut a: TakeoverClient<'_>,
    mut b: TakeoverClient<'_>,
    owner_a: [u8; 32],
    owner_b: [u8; 32],
    endpoint_fingerprint: [u8; 32],
    physical_hid: bool,
) -> Result<u32> {
    use crate::hid_protocol::{self as hid, Event};

    let owned = |owner| EndpointResponse::LeaseState {
        owner: Some(owner),
        state: 1,
    };
    let pending = || EndpointResponse::LeaseState {
        owner: Some(owner_a),
        state: 2,
    };
    let notice = |request_id| EndpointResponse::TakeoverNotice {
        request_id,
        requester: owner_b,
        response_window_ms: 10_000,
    };
    let stale = || EndpointResponse::Error {
        code: "stale_epoch".into(),
    };
    let key = || Event::KeyboardState {
        modifiers: 2,
        keys: vec![(7, 4)],
    };
    let button = || Event::MouseButtons { mask: 1 };
    let baseline = monitor.fresh_snapshot()?;
    let hid_baseline = physical_hid.then_some(baseline.hid_reports).flatten();
    let observer = if hid_baseline.is_some() {
        Some(crate::raw_input::Observer::start_takeover(
            0x303a,
            0x4004,
            &crate::hex(&endpoint_fingerprint[..12]),
        )?)
    } else {
        None
    };
    let a_epoch = next_lease_epoch(baseline.lease_epoch);
    let b_epoch = next_lease_epoch(next_lease_epoch(a_epoch));
    a.send(hid::LEASE_REQUEST, 0, Event::LeaseRequest).await?;
    a.expect(a_epoch, owned(owner_a)).await?;
    if hid_baseline.is_some() {
        a.send(hid::HID_BENCH_ARM, a_epoch, Event::HidBenchArm)
            .await?;
        a.expect(a_epoch, owned(owner_a)).await?;
    }
    a.send(hid::KEYBOARD_STATE, a_epoch, key()).await?;
    a.send(hid::MOUSE_BUTTONS, a_epoch, button()).await?;
    wait_for_scripted_telemetry(
        monitor,
        &baseline,
        3 + u32::from(hid_baseline.is_some()),
        a.sequence - 1,
    )?;
    let held = monitor.fresh_snapshot()?;
    ensure!(
        held.normalized_keys == 1 && held.normalized_buttons == 1,
        "A input did not set held keys and buttons"
    );
    eprintln!("M1.4 TAKEOVER A-held epoch={a_epoch} keys=1 buttons=1");

    let rejected = crate::random_id().map_err(anyhow::Error::msg)?;
    b.send(
        hid::TAKEOVER_REQUEST,
        0,
        Event::TakeoverRequest {
            request_id: rejected,
        },
    )
    .await?;
    b.expect(a_epoch, pending()).await?;
    a.expect(a_epoch, notice(rejected)).await?;
    a.send(
        hid::TAKEOVER_RESPONSE,
        a_epoch,
        Event::TakeoverResponse {
            request_id: rejected,
            accepted: false,
        },
    )
    .await?;
    b.expect(
        a_epoch,
        EndpointResponse::TakeoverResponse {
            request_id: rejected,
            accepted: false,
        },
    )
    .await?;
    a.expect(a_epoch, owned(owner_a)).await?;
    b.expect(a_epoch, owned(owner_a)).await?;

    a.send(hid::HEARTBEAT, a_epoch, Event::Heartbeat).await?;
    takeover_heartbeat_processed(
        a.runtime,
        monitor,
        a_epoch,
        a.sequence - 1,
        Instant::now() + Duration::from_millis(1500),
    )
    .await?;
    let timed = crate::random_id().map_err(anyhow::Error::msg)?;
    let requested = Instant::now();
    b.send(
        hid::TAKEOVER_REQUEST,
        0,
        Event::TakeoverRequest { request_id: timed },
    )
    .await?;
    b.expect(a_epoch, pending()).await?;
    a.expect(a_epoch, notice(timed)).await?;
    let pending_board_ms = monitor.fresh_snapshot()?.board_timestamp_ms;
    eprintln!("M1.4 TAKEOVER pending epoch={a_epoch} board_timestamp_ms={pending_board_ms}");
    for heartbeat in 1..=11 {
        let due = requested + Duration::from_millis(750 * heartbeat);
        a.runtime
            .sleep(due.saturating_duration_since(Instant::now()))
            .await;
        a.send(hid::HEARTBEAT, a_epoch, Event::Heartbeat).await?;
        let metrics = monitor.snapshot();
        ensure!(
            metrics.lease_epoch == a_epoch
                && metrics.normalized_keys == 1
                && metrics.normalized_buttons == 1,
            "A lost its lease or held state before the takeover window ended"
        );
        eprintln!(
            "M1.4 TAKEOVER heartbeat={heartbeat} epoch={a_epoch} sequence_sent={} last_board_sequence={}",
            a.sequence - 1,
            metrics.last_event_sequence,
        );
        if heartbeat == 8 {
            b.send(hid::KEYBOARD_STATE, a_epoch, key()).await?;
            b.expect(a_epoch, stale()).await?;
        }
    }
    let final_sample = requested + Duration::from_secs(9);
    a.runtime
        .sleep(final_sample.saturating_duration_since(Instant::now()))
        .await;
    let last_heartbeat = takeover_heartbeat_processed(
        a.runtime,
        monitor,
        a_epoch,
        a.sequence - 1,
        requested + Duration::from_millis(9700),
    )
    .await?;
    eprintln!(
        "M1.4 TAKEOVER A-still-held epoch={a_epoch} board_timestamp_ms={} last_heartbeat_sequence={}",
        last_heartbeat.board_timestamp_ms, last_heartbeat.last_event_sequence,
    );
    if let Some(before) = hid_baseline {
        wait_for_hid_reports(monitor, before, 4)?;
        eprintln!("M1.4 TAKEOVER A-neutral-reports=2 before=B-grant");
    }
    b.expect(b_epoch, owned(owner_b)).await?;
    let elapsed = requested.elapsed();
    ensure!(
        elapsed >= Duration::from_secs(10),
        "takeover granted before the 10-second window"
    );
    a.expect(b_epoch, owned(owner_b)).await?;
    let cleared = monitor.fresh_snapshot()?;
    ensure!(
        cleared.lease_epoch == b_epoch
            && cleared.normalized_keys == 0
            && cleared.normalized_buttons == 0,
        "held input survived the handoff"
    );
    eprintln!(
        "M1.4 TAKEOVER timeout_ms={} B-owned epoch={b_epoch} keys=0 buttons=0",
        elapsed.as_millis()
    );
    let heartbeat_baseline = monitor.fresh_snapshot()?;
    b.send(hid::HEARTBEAT, b_epoch, Event::Heartbeat).await?;
    wait_for_scripted_telemetry(monitor, &heartbeat_baseline, 1, b.sequence - 1)?;
    if hid_baseline.is_some() {
        b.send(hid::HID_BENCH_ARM, b_epoch, Event::HidBenchArm)
            .await?;
        b.expect(b_epoch, owned(owner_b)).await?;
    }
    a.send(hid::KEYBOARD_STATE, a_epoch, key()).await?;
    a.expect(b_epoch, stale()).await?;
    let before_b = monitor.fresh_snapshot()?;
    ensure!(
        before_b.normalized_keys == 0 && before_b.normalized_buttons == 0,
        "A's stale input changed held state"
    );
    b.send(hid::KEYBOARD_STATE, b_epoch, key()).await?;
    b.send(hid::MOUSE_BUTTONS, b_epoch, button()).await?;
    wait_for_scripted_telemetry(monitor, &before_b, 2, b.sequence - 1)?;
    let accepted = monitor.fresh_snapshot()?;
    ensure!(
        accepted.lease_epoch == b_epoch
            && accepted.normalized_keys == 1
            && accepted.normalized_buttons == 1,
        "B's input did not set held keys and buttons"
    );
    eprintln!("M1.4 TAKEOVER B-input accepted epoch={b_epoch} keys=1 buttons=1");
    b.send(hid::LEASE_RELEASE, b_epoch, Event::LeaseRelease)
        .await?;
    let idle_epoch = next_lease_epoch(b_epoch);
    a.expect(
        idle_epoch,
        EndpointResponse::LeaseState {
            owner: None,
            state: 0,
        },
    )
    .await?;
    b.expect(
        idle_epoch,
        EndpointResponse::LeaseState {
            owner: None,
            state: 0,
        },
    )
    .await?;
    if let Some(before) = hid_baseline {
        wait_for_hid_reports(monitor, before, 8)?;
    }
    if let Some(observer) = observer {
        observer.verify_takeover()?;
    }
    Ok(idle_epoch)
}

async fn run_takeover(
    runtime: Arc<dyn Runtime>,
    monitor: &PortMonitor,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential_a: &crate::credentials::ClientCredential,
    credential_b: &crate::credentials::ClientCredential,
    physical_hid: bool,
) -> Result<()> {
    ensure!(
        credential_a.fingerprint != credential_b.fingerprint,
        "takeover requires two distinct enrolled identities"
    );
    eprintln!(
        "M1.4 TAKEOVER A={} B={}",
        crate::hex(&credential_a.fingerprint),
        crate::hex(&credential_b.fingerprint)
    );
    let initial_hid_reports = monitor.fresh_snapshot()?.hid_reports;
    let mut a = open_signaling_peer(
        runtime.clone(),
        &monitor.device_status,
        address,
        hostname,
        status,
        credential_a,
    )
    .await?;
    let result = async {
        a.session.send_transcript_proof(&a.proof).map_err(anyhow::Error::msg)
            .context("client A transcript authorization")?;
        eprintln!("M1.4 TAKEOVER client=A authorized=true");
        let mut b = open_signaling_peer(runtime.clone(), &monitor.device_status, address, hostname, status, credential_b).await
            .context("client B signaling setup")?;
        let result = async {
            b.session.send_transcript_proof(&b.proof).map_err(anyhow::Error::msg)
                .context("client B transcript authorization")?;
            eprintln!("M1.4 TAKEOVER both_clients_authorized=true endpoint_transcripts=verified dtls_identities=verified");
            exercise_takeover(
                monitor,
                TakeoverClient::new("A", &*runtime, &a),
                TakeoverClient::new("B", &*runtime, &b),
                credential_a.fingerprint,
                credential_b.fingerprint,
                status.identity_fingerprint,
                physical_hid,
            ).await
        }.await;
        b.close(&*runtime).await;
        result
    }.await;
    a.close(&*runtime).await;
    let cleanup = wait_device_closed(&monitor.device_status).await;
    let idle_epoch = result?;
    cleanup?;
    let final_status = monitor.fresh_snapshot()?;
    ensure!(
        final_status.state == 0
            && final_status.lease_epoch == idle_epoch
            && final_status.normalized_keys == 0
            && final_status.normalized_buttons == 0
            && final_status.response_queue_depth == 0,
        "takeover did not leave an idle endpoint and empty held state"
    );
    ensure!(
        final_status.scripted_rejections == 0
            && final_status.scripted_reorders == 0
            && final_status.scripted_drops == 2,
        "expected exactly two stale input drops and no parser rejections or reorders"
    );
    if let Some(before) = initial_hid_reports {
        let expected = if physical_hid { 8 } else { 0 };
        let observed = final_status
            .hid_reports
            .context("missing HID report count")?
            .wrapping_sub(before);
        ensure!(
            observed == expected,
            "takeover HID report delta {observed}, expected {expected}"
        );
    }
    eprintln!(
        "M1.4 TAKEOVER sessions=closed lease=idle epoch={idle_epoch} keys=0 buttons=0 hid_reports_delta={} physical_input_capture=off",
        if physical_hid { 8 } else { 0 }
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum CleanupPath {
    Disconnect,
    HeartbeatExpiry,
    UsbRevoke,
    EndpointRestart,
    TransferFailure,
}

async fn run_cleanup_path(
    runtime: Arc<dyn Runtime>,
    monitor: &PortMonitor,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential: &crate::credentials::ClientCredential,
    path: CleanupPath,
) -> Result<()> {
    let mut peer = open_signaling_peer(
        runtime.clone(),
        &monitor.device_status,
        address,
        hostname,
        status,
        credential,
    )
    .await?;
    peer.session
        .send_transcript_proof(&peer.proof)
        .map_err(anyhow::Error::msg)?;
    let before = monitor.fresh_snapshot()?;
    let observer = if before.hid_reports.is_some() && !matches!(path, CleanupPath::TransferFailure)
    {
        Some(crate::raw_input::Observer::start_cleanup(
            0x303a,
            0x4004,
            &crate::hex(&status.identity_fingerprint[..12]),
        )?)
    } else {
        None
    };
    let epoch = next_lease_epoch(before.lease_epoch);
    {
        let mut client = TakeoverClient::new("cleanup", &*runtime, &peer);
        client
            .send(
                crate::hid_protocol::LEASE_REQUEST,
                0,
                crate::hid_protocol::Event::LeaseRequest,
            )
            .await?;
        client
            .expect(
                epoch,
                EndpointResponse::LeaseState {
                    owner: Some(credential.fingerprint),
                    state: 1,
                },
            )
            .await?;
        let arm = before.hid_reports.is_some();
        if arm {
            client
                .send(
                    crate::hid_protocol::HID_BENCH_ARM,
                    epoch,
                    crate::hid_protocol::Event::HidBenchArm,
                )
                .await?;
            client
                .expect(
                    epoch,
                    EndpointResponse::LeaseState {
                        owner: Some(credential.fingerprint),
                        state: 1,
                    },
                )
                .await?;
        }
        client
            .send(
                crate::hid_protocol::KEYBOARD_STATE,
                epoch,
                crate::hid_protocol::Event::KeyboardState {
                    modifiers: 2,
                    keys: vec![(7, 4)],
                },
            )
            .await?;
        client
            .send(
                crate::hid_protocol::MOUSE_BUTTONS,
                epoch,
                crate::hid_protocol::Event::MouseButtons { mask: 1 },
            )
            .await?;
    }
    wait_for_scripted_telemetry(
        monitor,
        &before,
        3 + u32::from(before.hid_reports.is_some()),
        2 + u64::from(before.hid_reports.is_some()),
    )?;
    match path {
        CleanupPath::Disconnect => peer.close(&*runtime).await,
        CleanupPath::HeartbeatExpiry => {
            runtime.sleep(Duration::from_millis(3_500)).await;
            if let Some(before_reports) = before.hid_reports {
                wait_for_hid_reports(monitor, before_reports, 4)?;
            }
            wait_for_held_cleanup(monitor)?;
            peer.close(&*runtime).await;
        }
        CleanupPath::UsbRevoke => {
            monitor.revoke_client(credential.fingerprint)?;
            wait_device_closed(&monitor.device_status).await?;
            peer.close(&*runtime).await;
        }
        CleanupPath::EndpointRestart => {
            monitor.restart_endpoint()?;
            wait_device_closed(&monitor.device_status).await?;
            peer.close(&*runtime).await;
        }
        CleanupPath::TransferFailure => {
            wait_for_held_cleanup(monitor)?;
            peer.close(&*runtime).await;
        }
    }
    let expected_hid_delta = if matches!(path, CleanupPath::TransferFailure) {
        // One injected transfer fails; bounded neutral cleanup still completes
        // the remaining recovery reports before the lease is released.
        3
    } else {
        4
    };
    wait_device_closed(&monitor.device_status).await?;
    let final_status = if let Some(before_reports) = before.hid_reports {
        wait_for_hid_reports(monitor, before_reports, expected_hid_delta)?
    } else {
        let metrics = monitor.fresh_snapshot()?;
        ensure!(
            metrics.hid_reports == None,
            "zero-HID telemetry disappeared"
        );
        metrics
    };
    ensure!(
        final_status.state == 0
            && final_status.normalized_keys == 0
            && final_status.normalized_buttons == 0
            && final_status.response_queue_depth == 0,
        "cleanup path left endpoint active or held input"
    );
    if let Some(observer) = observer {
        observer.verify_cleanup(match path {
            CleanupPath::Disconnect => "disconnect",
            CleanupPath::HeartbeatExpiry => "heartbeat-expiry",
            CleanupPath::UsbRevoke => "usb-revoke",
            CleanupPath::EndpointRestart => "endpoint-restart",
            CleanupPath::TransferFailure => "transfer-failure",
        })?;
    }
    eprintln!(
        "M1.4 CLEANUP path={} released=true held_keys=0 held_buttons=0 hid_reports_delta={}",
        match path {
            CleanupPath::Disconnect => "disconnect",
            CleanupPath::HeartbeatExpiry => "heartbeat-expiry",
            CleanupPath::UsbRevoke => "usb-revoke",
            CleanupPath::EndpointRestart => "endpoint-restart",
            CleanupPath::TransferFailure => "transfer-failure",
        },
        if before.hid_reports.is_some() {
            expected_hid_delta
        } else {
            0
        },
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

fn wait_for_held_cleanup(monitor: &PortMonitor) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let metrics = monitor.fresh_snapshot()?;
        if metrics.normalized_keys == 0
            && metrics.normalized_buttons == 0
            && metrics.response_queue_depth == 0
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "heartbeat expiry left held input: keys={} buttons={} queue={}",
            metrics.normalized_keys,
            metrics.normalized_buttons,
            metrics.response_queue_depth
        );
        std::thread::sleep(Duration::from_millis(100));
    }
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
    takeover_peer: Option<crate::credentials::ClientCredential>,
    scenario: &str,
) -> Result<SocketAddr> {
    ensure!(
        matches!(
            scenario,
            "success"
                | "missing"
                | "invalid"
                | "replay"
                | "resources"
                | "revoke-active"
                | "scripted"
                | "takeover"
                | "takeover-zero"
                | "hid-bench"
                | "hid-sustain"
                | "cleanup-disconnect"
                | "cleanup-expiry"
                | "cleanup-revoke"
                | "cleanup-restart"
                | "cleanup-transfer-failure"
        ),
        "unknown signaling-test scenario"
    );
    install_certificate_log()?;
    let address = crate::discovery::socket_address(service).map_err(anyhow::Error::msg)?;
    let hostname = service.hostname.clone();
    let monitor = PortMonitor::start(port);
    let initial_hid_reports = monitor.fresh_snapshot()?.hid_reports;
    let runtime = default_runtime().context("no WebRTC runtime")?;
    let mut result: Option<Result<()>> = None;
    runtime.block_on(Box::pin(async {
        result = Some(
            async {
                match scenario {
                    "takeover" | "takeover-zero" => {
                        let peer = takeover_peer
                            .as_ref()
                            .context("missing takeover identity")?;
                        run_takeover(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            peer,
                            scenario == "takeover",
                        )
                        .await?;
                    }
                    "cleanup-disconnect"
                    | "cleanup-expiry"
                    | "cleanup-revoke"
                    | "cleanup-restart"
                    | "cleanup-transfer-failure" => {
                        let path = match scenario {
                            "cleanup-disconnect" => CleanupPath::Disconnect,
                            "cleanup-expiry" => CleanupPath::HeartbeatExpiry,
                            "cleanup-revoke" => CleanupPath::UsbRevoke,
                            "cleanup-restart" => CleanupPath::EndpointRestart,
                            _ => CleanupPath::TransferFailure,
                        };
                        run_cleanup_path(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            path,
                        )
                        .await?;
                    }
                    "scripted" | "hid-bench" | "hid-sustain" => {
                        let _proof = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Valid {
                                scripted: Some(if scenario == "hid-bench" {
                                    ScriptMode::HidBench
                                } else if scenario == "hid-sustain" {
                                    ScriptMode::HidSustain
                                } else {
                                    ScriptMode::Transport
                                }),
                                check_isolation: false,
                                check_resources: false,
                                revoke_after_authorization: false,
                            },
                        )
                        .await?;
                    }
                    "success" => {
                        let _proof = exchange_signaling(
                            runtime.clone(),
                            &monitor,
                            address,
                            &hostname,
                            &status,
                            &credential,
                            TranscriptProofCase::Valid {
                                scripted: None,
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
                                scripted: None,
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
                                scripted: None,
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
                                scripted: None,
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
        if scenario == "takeover" {
            eprintln!(
                "M1.4 TAKEOVER FAILED state={} epoch={} keys={} buttons={} internal_free_min_bytes={} hid_reports=0",
                metrics.state,
                metrics.lease_epoch,
                metrics.normalized_keys,
                metrics.normalized_buttons,
                metrics.internal_free_min,
            );
        }
        let diagnostic = &metrics.tls_diagnostic[..metrics.tls_diagnostic_len];
        if !diagnostic.is_empty() {
            return Err(anyhow::Error::msg(format!(
                "{error:#}; ESP TLS diagnostic: {}",
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
        "M1.4 TELEMETRY board_timestamp_ms={} lease_epoch={} last_event_sequence={} response_queue_depth={} response_queue_residence_us={} event_processing_us={} total_soc_cpu_us={} scripted_events={} scripted_rejections={} scripted_drops={} scripted_reorders={} normalized_keys={} normalized_buttons={}",
        metrics.board_timestamp_ms,
        metrics.lease_epoch,
        metrics.last_event_sequence,
        metrics.response_queue_depth,
        metrics.response_queue_residence_us,
        metrics.event_processing_us,
        metrics.total_soc_cpu_us,
        metrics.scripted_events,
        metrics.scripted_rejections,
        metrics.scripted_drops,
        metrics.scripted_reorders,
        metrics.normalized_keys,
        metrics.normalized_buttons,
    );
    let expected_hid_reports = match scenario {
        "hid-bench" => Some(SCRIPTED_HID_REPORTS),
        "hid-sustain" => Some(SUSTAIN_HID_REPORTS),
        "takeover" => Some(8),
        "cleanup-disconnect" | "cleanup-expiry" | "cleanup-revoke" | "cleanup-restart" => Some(4),
        // One injected transfer fails; three bounded recovery reports complete.
        "cleanup-transfer-failure" => Some(3),
        _ => None,
    };
    let mut hid_reports = 0;
    if let Some(expected) = expected_hid_reports {
        hid_reports = metrics
            .hid_reports
            .context("missing final HID report count")?
            .wrapping_sub(initial_hid_reports.context("missing initial HID report count")?);
        ensure!(
            hid_reports == expected,
            "expected {expected} HID reports, observed {hid_reports}"
        );
        if scenario == "hid-bench" {
            ensure!(
                metrics.state == 0
                    && metrics.normalized_keys == 0
                    && metrics.normalized_buttons == 0,
                "bench left active transport or held input"
            );
        }
    } else {
        ensure!(
            metrics.hid_reports == initial_hid_reports,
            "unexpected HID reports in signaling transport test"
        );
    }
    eprintln!(
        "M1.3 SIGNALING COMPLETE scenario={scenario} hid_reports={hid_reports} physical_input_capture=off"
    );
    Ok(address)
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

    #[test]
    fn endpoint_responses_require_exact_session_sequence_and_epoch() {
        let session_id = [7; 16];
        let state = crate::hid_protocol::Envelope {
            message_type: crate::hid_protocol::LEASE_STATE,
            session_id,
            lease_epoch: 1,
            sequence: 0,
            event: crate::hid_protocol::Event::LeaseState {
                owner: Some([8; 32]),
                state: 1,
            },
        };
        let mut cursor = ResponseCursor::new(session_id);
        assert_eq!(
            cursor
                .decode(&crate::hid_protocol::encode(&state), 1)
                .unwrap(),
            EndpointResponse::LeaseState {
                owner: Some([8; 32]),
                state: 1,
            }
        );
        let mut dynamic_epoch = state.clone();
        dynamic_epoch.lease_epoch = 3;
        let (response, epoch) = ResponseCursor::new(session_id)
            .decode_any_epoch(&crate::hid_protocol::encode(&dynamic_epoch))
            .unwrap();
        assert_eq!(epoch, 3);
        assert_eq!(
            response,
            EndpointResponse::LeaseState {
                owner: Some([8; 32]),
                state: 1
            }
        );

        let takeover_response = crate::hid_protocol::Envelope {
            message_type: crate::hid_protocol::TAKEOVER_RESPONSE,
            session_id,
            lease_epoch: 1,
            sequence: 1,
            event: crate::hid_protocol::Event::TakeoverResponse {
                request_id: [3; 16],
                accepted: true,
            },
        };
        assert_eq!(
            cursor
                .decode(&crate::hid_protocol::encode(&takeover_response), 1)
                .unwrap(),
            EndpointResponse::TakeoverResponse {
                request_id: [3; 16],
                accepted: true,
            }
        );

        for (session_id, sequence, epoch) in
            [([9; 16], 0, 1), (session_id, 1, 1), (session_id, 0, 2)]
        {
            let mut invalid = state.clone();
            invalid.session_id = session_id;
            invalid.sequence = sequence;
            invalid.lease_epoch = epoch;
            assert!(
                ResponseCursor::new(state.session_id)
                    .decode(&crate::hid_protocol::encode(&invalid), 1)
                    .is_err()
            );
        }
    }

    #[test]
    fn endpoint_notice_and_error_are_decoded_without_echo_assumption() {
        let session_id = [7; 16];
        let notice = crate::hid_protocol::Envelope {
            message_type: crate::hid_protocol::TAKEOVER_NOTICE,
            session_id,
            lease_epoch: 4,
            sequence: 0,
            event: crate::hid_protocol::Event::TakeoverNotice {
                request_id: [1; 16],
                requester: [2; 32],
                response_window_ms: 10_000,
            },
        };
        assert_eq!(
            ResponseCursor::new(session_id)
                .decode(&crate::hid_protocol::encode(&notice), 4)
                .unwrap(),
            EndpointResponse::TakeoverNotice {
                request_id: [1; 16],
                requester: [2; 32],
                response_window_ms: 10_000,
            }
        );

        let error = crate::hid_protocol::Envelope {
            message_type: crate::hid_protocol::ERROR,
            session_id,
            lease_epoch: 0,
            sequence: 0,
            event: crate::hid_protocol::Event::Error {
                code: "busy".to_owned(),
            },
        };
        let response = ResponseCursor::new(session_id)
            .decode(&crate::hid_protocol::encode(&error), 0)
            .unwrap();
        assert_eq!(
            response,
            EndpointResponse::Error {
                code: "busy".to_owned()
            }
        );
        assert!(
            require_response(
                response,
                EndpointResponse::LeaseState {
                    owner: None,
                    state: 0
                }
            )
            .is_err()
        );
    }
}
