use anyhow::{Context, Result, bail, ensure};
use bytes::BytesMut;
use log::{LevelFilter, Log, Metadata, Record};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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
static RTC_SCTP_T3_TIMEOUTS: AtomicUsize = AtomicUsize::new(0);
static RTC_SCTP_ACK_TIMEOUTS: AtomicUsize = AtomicUsize::new(0);
static RTC_ERROR_LOCATION: OnceLock<(String, Option<u32>)> = OnceLock::new();
static LOG_LEVEL: OnceLock<LevelFilter> = OnceLock::new();

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);
const LEGACY_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const MINI_DEGRADE_WARN_MISSES: u8 = 2;
const LEGACY_HEARTBEAT_RESPONSE_TIMEOUT: Duration = Duration::from_millis(100);
const INPUT_SEND_BUDGET: Duration = Duration::from_millis(5);
// Responses carry only the originating request sequence, not its channel.
// Keep input-report credits disjoint from control replies while both can be in flight.
const INPUT_SEQUENCE_BASE: u64 = 1 << 63;

#[derive(Default)]
struct ConnectionStates {
    ice: String,
    peer: String,
}

struct CertificateLog;

impl Log for CertificateLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let Some(level) = LOG_LEVEL.get().and_then(LevelFilter::to_level) else {
            return false;
        };
        if metadata.level() > level {
            return false;
        }
        metadata.level() <= log::Level::Info
            || level == log::Level::Trace
            || metadata.target() == "kvmctl"
            || metadata.target().starts_with("kvmctl::")
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let message = record.args().to_string();
            let lower = message.to_ascii_lowercase();
            if lower.contains("nt3timeouts") || lower.contains("t3 timeout") {
                RTC_SCTP_T3_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
            }
            if lower.contains("nacktimeouts") || lower.contains("ack timeout") {
                RTC_SCTP_ACK_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(class) = rtc_error_class(record.level(), &message) {
                RTC_ERROR_LOGS.fetch_add(1, Ordering::Relaxed);
                let _ = RTC_ERROR_LOCATION.set((
                    record.module_path().unwrap_or(record.target()).to_owned(),
                    record.line(),
                ));
                let _ = RTC_ERROR_CLASS.compare_exchange(
                    0,
                    class,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
                if lower.contains("remote certificate does not match any fingerprint") {
                    CERTIFICATE_REJECTED.store(true, Ordering::Release);
                }
            }
            let line = format!(
                "{} [{}] {}: {}\n",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|value| value.as_millis())
                    .unwrap_or_default(),
                record.level(),
                record.target(),
                record.args()
            );
            eprint!("{line}");
            if let Some(path) = diagnostic_log_path() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    }

    fn flush(&self) {}
}

static CERTIFICATE_LOG: CertificateLog = CertificateLog;

fn rtc_error_class(level: log::Level, message: &str) -> Option<u8> {
    if !matches!(level, log::Level::Error | log::Level::Warn) {
        return None;
    }
    let message = message.to_ascii_lowercase();
    Some(if message.contains("fingerprint") {
        1
    } else if message.contains("certificate") {
        2
    } else if message.contains("dtls") {
        3
    } else {
        4
    })
}

fn diagnostic_log_level() -> LevelFilter {
    std::env::var("KVMCTL_LOG")
        .ok()
        .and_then(|value| LevelFilter::from_str(&value).ok())
        .unwrap_or(LevelFilter::Info)
}

fn diagnostic_log_path() -> Option<PathBuf> {
    std::env::var_os("KVMCTL_LOG_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|root| root.join("kvmctl").join("kvmctl.log"))
        })
}

pub(crate) fn init_logging() {
    let level = *LOG_LEVEL.get_or_init(diagnostic_log_level);
    let _ = log::set_logger(&CERTIFICATE_LOG);
    log::set_max_level(level);
}

fn install_certificate_log() -> Result<()> {
    // The process logger is global. Tray reconnects and other callers may have
    // installed it already; that is not a transport failure.
    init_logging();
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
        // Browser-created Mini channels consume even SCTP stream IDs in this
        // order: rpc=0, hidrpc=2, ordered-unreliable=4, unordered=6.
        let control = pc.create_data_channel("rpc", None).await?;
        let input = pc.create_data_channel("hidrpc", None).await?;
        let motion_ordered = pc
            .create_data_channel(
                "hidrpc-unreliable-ordered",
                Some(RTCDataChannelInit {
                    ordered: true,
                    max_retransmits: Some(0),
                    ..Default::default()
                }),
            )
            .await?;
        let motion = pc
            .create_data_channel(
                "hidrpc-unreliable-nonordered",
                Some(RTCDataChannelInit {
                    ordered: false,
                    max_retransmits: Some(0),
                    ..Default::default()
                }),
            )
            .await?;
        ensure!(control.label().await? == "rpc", "invalid rpc channel label");
        ensure!(control.ordered().await?, "rpc channel must be ordered");
        ensure!(
            control.max_retransmits().await?.is_none()
                && control.max_packet_life_time().await?.is_none(),
            "rpc channel must be reliable"
        );
        ensure!(
            input.label().await? == "hidrpc",
            "invalid hidrpc channel label"
        );
        ensure!(input.ordered().await?, "hidrpc channel must be ordered");
        ensure!(
            input.max_retransmits().await?.is_none()
                && input.max_packet_life_time().await?.is_none(),
            "hidrpc channel must be reliable"
        );
        ensure!(
            motion_ordered.label().await? == "hidrpc-unreliable-ordered",
            "invalid ordered unreliable HID channel label"
        );
        ensure!(
            motion_ordered.ordered().await?,
            "ordered unreliable HID channel must be ordered"
        );
        ensure!(
            motion_ordered.max_retransmits().await? == Some(0),
            "ordered unreliable HID channel must use zero retransmissions"
        );
        ensure!(
            motion.label().await? == "hidrpc-unreliable-nonordered",
            "invalid unordered unreliable HID channel label"
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
            &input,
            &motion_ordered,
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
    input: &Arc<dyn DataChannel>,
    motion_ordered: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    peer_failed: &AtomicBool,
    device_status: &AtomicU8,
    connection_states: &Mutex<ConnectionStates>,
    expected_fingerprint_mismatch: bool,
) -> Result<()> {
    let started = Instant::now();
    let mut open = [false, false, false, false];
    while !open.iter().all(|value| *value) {
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
        for (index, channel) in [control, input, motion_ordered, motion]
            .into_iter()
            .enumerate()
        {
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
    input: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
) -> Result<()> {
    let payload = b"kvmctl-m1.3-pre-auth-must-not-echo";
    for channel in [control, input, motion] {
        channel
            .send(BytesMut::from(&payload[..]))
            .await
            .context("pre-authorization application payload could not be sent")?;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        for channel in [control, input, motion] {
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
    input: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    device_status: &AtomicU8,
) -> Result<()> {
    let payload = b"kvmctl-m1.3-revoked-must-not-echo";
    for channel in [control, input, motion] {
        let _ = channel.send(BytesMut::from(&payload[..])).await;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        ensure!(
            device_status.load(Ordering::Acquire) == 0,
            "ESP32 transport became active again after client revocation"
        );
        for channel in [control, input, motion] {
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
    input: &Arc<dyn DataChannel>,
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
    echo_signaling(runtime, input, "input-after-unauthorized-client", 1).await?;
    echo_signaling(runtime, motion, "motion-after-unauthorized-client", 1).await?;
    eprintln!(
        "M1.3 ISOLATION unauthorized_client=rejected active_session=preserved control_echo=true input_echo=true motion_echo=true"
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
    echo_signaling(runtime, input, "input-after-invalid-client-proof", 1).await?;
    echo_signaling(runtime, motion, "motion-after-invalid-client-proof", 1).await?;
    eprintln!(
        "M1.3 ISOLATION invalid_client_proof=rejected active_session=preserved control_echo=true input_echo=true motion_echo=true"
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
    echo_signaling(runtime, input, "input-after-competing-offer", 1).await?;
    echo_signaling(runtime, motion, "motion-after-competing-offer", 1).await?;
    eprintln!(
        "M1.3 ISOLATION authenticated_competing_offer=busy active_session=preserved control_echo=true input_echo=true motion_echo=true"
    );
    Ok(())
}

struct SignalingPeer<P> {
    session: crate::signaling::Session,
    pc: P,
    peer_failed: Arc<AtomicBool>,
    rpc: Arc<dyn DataChannel>,
    hidrpc: Arc<dyn DataChannel>,
    hidrpc_unreliable_nonordered: Arc<dyn DataChannel>,
    // Compatibility aliases for the existing diagnostics/bench paths.
    control: Arc<dyn DataChannel>,
    input: Arc<dyn DataChannel>,
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
        let control = pc.create_data_channel("rpc", None).await?;
        let input = pc.create_data_channel("hidrpc", None).await?;
        let motion_ordered = pc.create_data_channel(
            "hidrpc-unreliable-ordered",
            Some(RTCDataChannelInit { ordered: true, max_retransmits: Some(0), ..Default::default() }),
        ).await?;
        let motion = pc.create_data_channel(
            "hidrpc-unreliable-nonordered",
            Some(RTCDataChannelInit { ordered: false, max_retransmits: Some(0), ..Default::default() }),
        ).await?;
        ensure!(control.ordered().await? && control.max_retransmits().await?.is_none(),
                "rpc channel must be reliable and ordered");
        ensure!(input.ordered().await? && input.max_retransmits().await?.is_none(),
                "hidrpc channel must be reliable and ordered");
        ensure!(motion_ordered.ordered().await? && motion_ordered.max_retransmits().await? == Some(0),
                "ordered unreliable HID channel must be ordered with zero retransmissions");
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
            &input,
            &motion_ordered,
            &motion,
            &peer_failed,
            device_status,
            &connection_states,
            false,
        ).await?;
        eprintln!("M1.3 DTLS_OPEN elapsed_ms={} rpc=ordered-reliable hidrpc=ordered-reliable hidrpc_unreliable_ordered=ordered-zero-retransmit hidrpc_unreliable_nonordered=unordered-zero-retransmit",
                  started.elapsed().as_millis());

        Ok((control, input, motion_ordered, motion, offer_sdp, offer_exchange.client_transcript_proof))
    }.await;
    match result {
        Ok((control, input, _motion_ordered, motion, offer_sdp, proof)) => Ok(SignalingPeer {
            session,
            pc,
            peer_failed,
            rpc: control.clone(),
            hidrpc: input.clone(),
            hidrpc_unreliable_nonordered: motion.clone(),
            control,
            input,
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
    heartbeat_ack_supported: bool,
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
    let input = &peer.input;
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
                        input,
                        motion,
                        peer.session.session_id(),
                        credential.fingerprint,
                        status.identity_fingerprint,
                        mode,
                        heartbeat_ack_supported,
                    )
                    .await?;
                }
                if check_isolation {
                    test_competing_signaling(&*runtime, control, input, motion, address, hostname, status, credential, offer_sdp).await?;
                }
                if check_resources {
                    run_resource_test(&*runtime, control, motion, monitor, address, hostname, status, credential).await?;
                }
                if revoke_after_authorization {
                    echo_signaling(&*runtime, control, "control-before-active-revoke", 1).await?;
                    echo_signaling(&*runtime, input, "input-before-active-revoke", 1).await?;
                    echo_signaling(&*runtime, motion, "motion-before-active-revoke", 1).await?;
                    monitor.revoke_client(credential.fingerprint)?;
                    wait_device_closed(&monitor.device_status).await?;
                    send_after_revocation_and_reject_echo(
                        &*runtime,
                        control,
                        input,
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
                echo_signaling(&*runtime, input, "input", 10).await?;
                echo_signaling(&*runtime, motion, "motion", 10).await?;
                Ok(proof.clone())
            }
            TranscriptProofCase::Missing => {
                send_before_authorization_and_reject_echo(&*runtime, control, input, motion).await?;
                eprintln!("M1.3 PROOF negative=missing accepted_application_bytes=0 echoed=false");
                Ok(proof.clone())
            }
            TranscriptProofCase::Invalid => {
                let mut invalid = proof.clone();
                *invalid.last_mut().ok_or_else(|| anyhow::anyhow!("empty transcript proof"))? ^= 1;
                send_before_authorization_and_reject_echo(&*runtime, control, input, motion).await?;
                peer.session.send_transcript_proof_frame(&invalid).map_err(anyhow::Error::msg)?;
                let rejection = peer.session.receive_rejection().map_err(anyhow::Error::msg)?;
                ensure!(rejection.contains("unauthorized"), "invalid proof got unexpected error: {rejection}");
                eprintln!("M1.3 PROOF negative=invalid accepted_application_bytes=0 echoed=false result=unauthorized");
                Ok(Vec::new())
            }
            TranscriptProofCase::Replayed(replayed) => {
                send_before_authorization_and_reject_echo(&*runtime, control, input, motion).await?;
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
    service: crate::discovery::Service,
    status: crate::Status,
    credential: crate::credentials::ClientCredential,
    rediscover: bool,
) -> Result<SocketAddr> {
    let heartbeat_mode =
        crate::discovery::compatibility_mode(&service).map_err(anyhow::Error::msg)?;
    run_foreground_session_with_heartbeat_ack(
        service,
        status,
        credential,
        rediscover,
        heartbeat_mode == crate::discovery::HeartbeatMode::Acknowledged,
    )
}

pub(crate) fn run_foreground_session_with_heartbeat_ack(
    service: crate::discovery::Service,
    status: crate::Status,
    credential: crate::credentials::ClientCredential,
    rediscover: bool,
    heartbeat_ack_supported: bool,
) -> Result<SocketAddr> {
    let control = SessionControl::new_live(SessionMode::Cli)?;
    let input = control.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            if std::io::stdin().read_line(&mut line).is_err() {
                input.cancel();
                break;
            }
            if let Some(request_id) = input.pending_takeover() {
                input.respond_to_takeover(request_id, line.trim().eq_ignore_ascii_case("yes"));
            } else {
                input.cancel();
                break;
            }
        }
    });
    run_session_with_heartbeat_ack(
        service,
        status,
        credential,
        control,
        rediscover,
        heartbeat_ack_supported,
        |event| match event {
            SessionEvent::Connecting => println!("Connecting/local…"),
            SessionEvent::ConnectedLocal => {
                println!("Connected/local. Press Enter to disconnect.")
            }
            SessionEvent::InputLocalConfirmed => println!("Input local."),
            SessionEvent::InputRemoteConfirmed => {
                println!("Remote input active. Press Ctrl+Shift+F10 to toggle local.")
            }
            SessionEvent::ConnectionDegraded { missed } => {
                println!(
                    "Connection degraded; heartbeat misses={missed}; remote input may lag; press Escape for local control"
                )
            }
            SessionEvent::ConnectionRecovered { rtt_ms } => {
                println!("Connection recovered; heartbeat RTT={rtt_ms}ms")
            }
            SessionEvent::TakeoverPending { .. } => {
                println!("Takeover requested. Input remains local. Accept? type yes or no:")
            }
            SessionEvent::Recovering { seconds_remaining } => {
                println!("Connection lost. Recovering/local ({seconds_remaining}s remaining)…")
            }
            SessionEvent::Unavailable { reason } => println!("Unavailable: {reason}"),
            SessionEvent::OwnershipLost { reason } => println!("Ownership lost: {reason}"),
            SessionEvent::Notice(message) => println!("{message}"),
            SessionEvent::Disconnected => {}
        },
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionEvent {
    Connecting,
    ConnectedLocal,
    InputLocalConfirmed,
    InputRemoteConfirmed,
    ConnectionDegraded {
        missed: u8,
    },
    ConnectionRecovered {
        rtt_ms: u64,
    },
    TakeoverPending {
        request_id: [u8; 16],
        requester: [u8; 32],
        response_window_ms: u64,
    },
    Recovering {
        seconds_remaining: u64,
    },
    Unavailable {
        reason: String,
    },
    OwnershipLost {
        reason: String,
    },
    Notice(String),
    Disconnected,
}

#[derive(Clone)]
pub(crate) struct SessionControl {
    cancelled: Arc<AtomicBool>,
    takeover: Arc<Mutex<Option<([u8; 16], Option<bool>, Instant)>>>,
    input: Arc<Mutex<Option<InputState>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionMode {
    Cli,
    Tray,
}

struct InputState {
    capture: crate::input_capture::Capture,
    events: crate::input_capture::CapturedEvents,
    motion_rate_hz: u32,
    mode: SessionMode,
    remote: bool,
    epoch: Option<u32>,
    keyboard: HeldKeyboard,
    buttons: u8,
}

#[derive(Default)]
struct HeldKeyboard {
    modifiers: u8,
    keys: Vec<(u16, u16)>,
}

impl HeldKeyboard {
    fn update(&mut self, page: u16, usage: u16, down: bool) -> Option<crate::hid_protocol::Event> {
        let previous_modifiers = self.modifiers;
        let previous_keys = self.reported_keys();
        if page == 7 && (0xe0..=0xe7).contains(&usage) {
            let bit = 1 << (usage - 0xe0);
            if down {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
        } else if down {
            if !self.keys.contains(&(page, usage)) {
                self.keys.push((page, usage));
            }
        } else {
            self.keys.retain(|key| *key != (page, usage));
        }
        let keys = self.reported_keys();
        (keys != previous_keys || self.modifiers != previous_modifiers).then_some(
            crate::hid_protocol::Event::KeyboardState {
                modifiers: self.modifiers,
                keys,
            },
        )
    }

    fn reported_keys(&self) -> Vec<(u16, u16)> {
        // Choose slots by press order, then serialize in canonical wire order.
        let mut keys: Vec<_> = self.keys.iter().copied().take(6).collect();
        keys.sort_unstable();
        keys
    }
}

impl SessionControl {
    pub(crate) fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            takeover: Arc::new(Mutex::new(None)),
            input: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn new_live(mode: SessionMode) -> Result<Self> {
        let control = Self::new();
        let bindings = crate::settings::load_escape_bindings().map_err(anyhow::Error::msg)?;
        let motion_rate_hz = crate::settings::load_motion_rate_hz().map_err(anyhow::Error::msg)?;
        let (capture, events) =
            crate::input_capture::Capture::start(bindings).map_err(anyhow::Error::msg)?;
        *control.input.lock().unwrap() = Some(InputState {
            capture,
            events,
            motion_rate_hz,
            mode,
            remote: false,
            epoch: None,
            keyboard: HeldKeyboard::default(),
            buttons: 0,
        });
        Ok(control)
    }

    fn input_enabled(&self) -> bool {
        self.input.lock().unwrap().is_some()
    }

    fn input_motion_rate_hz(&self) -> u32 {
        self.input
            .lock()
            .unwrap()
            .as_ref()
            .map_or(crate::settings::DEFAULT_MOTION_RATE_HZ, |input| {
                input.motion_rate_hz
            })
    }

    fn input_motion_interval(&self) -> Duration {
        Duration::from_micros(1_000_000 / u64::from(self.input_motion_rate_hz()))
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn input_ready(&self, epoch: u32) -> Result<()> {
        let mut input = self.input.lock().unwrap();
        let Some(input) = input.as_mut() else {
            return Ok(());
        };
        input.remote = false;
        input.epoch = Some(epoch);
        input.keyboard = HeldKeyboard::default();
        input.buttons = 0;
        input
            .capture
            .set_remote(false)
            .map_err(anyhow::Error::msg)?;
        input.capture.set_ready(true).map_err(anyhow::Error::msg)
    }

    fn input_epoch_matches(&self, epoch: u32) -> bool {
        match self
            .input
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|input| input.epoch)
        {
            Some(current) => current == epoch,
            None => true,
        }
    }

    fn input_fail_closed(&self) {
        if let Some(input) = self.input.lock().unwrap().as_mut() {
            let _ = input.capture.set_remote(false);
            for _ in 0..256 {
                if input.events.try_recv().is_err() {
                    break;
                }
            }
            input.remote = false;
            input.epoch = None;
            input.keyboard = HeldKeyboard::default();
            input.buttons = 0;
        }
    }

    pub(crate) fn input_is_remote(&self) -> bool {
        self.input
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|input| input.remote)
    }

    fn input_buttons(&self) -> u8 {
        self.input
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |input| input.buttons)
    }

    fn input_keyboard_active(&self) -> bool {
        self.input.lock().unwrap().as_ref().is_some_and(|input| {
            input.keyboard.modifiers != 0 || !input.keyboard.reported_keys().is_empty()
        })
    }

    fn input_faulted(&self) -> bool {
        self.input
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|input| input.capture.is_faulted())
    }

    fn check_input_health(&self) -> Result<()> {
        ensure!(
            !self.input_faulted(),
            "Input capture stopped or overflowed. Input is local."
        );
        Ok(())
    }

    fn input_mode(&self) -> Option<SessionMode> {
        self.input.lock().unwrap().as_ref().map(|input| input.mode)
    }

    fn take_escape(&self) -> bool {
        let detected = self
            .input
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|input| input.events.take_escape());
        if detected {
            log::info!("Escape shortcut detected");
        }
        detected
    }

    fn discard_pre_lease_shortcuts(&self) {
        // Each capture queue limits the count of waiting presses to u8.
        for _ in 0..u8::MAX {
            if !self.take_escape() {
                break;
            }
            if self.input_mode() == Some(SessionMode::Cli) {
                self.cancel();
                break;
            }
        }
    }

    fn next_input_event(&self) -> Result<Option<crate::input_capture::InputEvent>> {
        let input = self.input.lock().unwrap();
        let Some(input) = input.as_ref() else {
            return Ok(None);
        };
        match input.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => bail!("input capture worker exited"),
        }
    }

    fn request_remote(&self, attempt: u64) -> Result<()> {
        let input = self.input.lock().unwrap();
        input
            .as_ref()
            .context("capture is unavailable")?
            .capture
            .request_remote(attempt)
            .map_err(anyhow::Error::msg)
    }

    fn confirm_remote(&self, epoch: u32) -> bool {
        if self.is_cancelled() {
            return false;
        }
        let mut input = self.input.lock().unwrap();
        let Some(input) = input.as_mut() else {
            return false;
        };
        if input.epoch != Some(epoch) || input.capture.is_faulted() {
            return false;
        }
        input.remote = true;
        true
    }

    fn input_set_local(&self) -> Result<bool> {
        let mut input = self.input.lock().unwrap();
        let Some(input) = input.as_mut() else {
            return Ok(false);
        };
        let changed = std::mem::take(&mut input.remote);
        input.keyboard = HeldKeyboard::default();
        input.buttons = 0;
        let handoff = input.capture.set_remote(false).map_err(anyhow::Error::msg);
        for _ in 0..256 {
            if input.events.try_recv().is_err() {
                break;
            }
        }
        // An exited worker has already removed its hooks.
        if !input.capture.is_faulted() {
            handoff?;
        }
        Ok(changed)
    }

    fn input_update(
        &self,
        event: crate::input_capture::InputEvent,
    ) -> Option<crate::hid_protocol::Event> {
        use crate::{hid_protocol::Event, input_capture::InputEvent};
        let mut input = self.input.lock().unwrap();
        let input = input.as_mut()?;
        if !input.remote {
            return None;
        }
        match event {
            InputEvent::Key { page, usage, down } => input.keyboard.update(page, usage, down),
            InputEvent::MouseButton { mask, down } => {
                let before = input.buttons;
                if down {
                    input.buttons |= mask;
                } else {
                    input.buttons &= !mask;
                }
                (before != input.buttons).then_some(Event::MouseButtons {
                    mask: input.buttons,
                })
            }
            InputEvent::Wheel {
                vertical,
                horizontal,
            } => Some(Event::Wheel {
                vertical,
                horizontal,
            }),
            _ => None,
        }
    }

    pub(crate) fn respond_to_takeover(&self, request_id: [u8; 16], accepted: bool) {
        if let Some((pending, decision, _)) = self.takeover.lock().unwrap().as_mut()
            && *pending == request_id
            && decision.is_none()
        {
            *decision = Some(accepted);
        }
    }

    pub(crate) fn pending_takeover(&self) -> Option<[u8; 16]> {
        self.takeover
            .lock()
            .unwrap()
            .as_ref()
            .map(|(request_id, _, _)| *request_id)
    }

    fn start_takeover(&self, request_id: [u8; 16], response_window_ms: u64) {
        *self.takeover.lock().unwrap() = Some((
            request_id,
            None,
            Instant::now() + Duration::from_millis(response_window_ms),
        ));
    }

    fn takeover_expired(&self) -> bool {
        let mut takeover = self.takeover.lock().unwrap();
        if takeover
            .as_ref()
            .is_some_and(|(_, _, deadline)| Instant::now() >= *deadline)
        {
            *takeover = None;
            true
        } else {
            false
        }
    }

    fn take_takeover_decision(&self, request_id: [u8; 16]) -> Option<bool> {
        let mut takeover = self.takeover.lock().unwrap();
        let Some((pending, decision, deadline)) = takeover.as_ref() else {
            return None;
        };
        if *pending != request_id {
            return None;
        }
        if Instant::now() >= *deadline {
            *takeover = None;
            return None;
        }
        let decision = (*decision)?;
        *takeover = None;
        Some(decision)
    }

    fn clear_takeover(&self) {
        *self.takeover.lock().unwrap() = None;
    }
}

enum SessionExit {
    Disconnected,
    Lost(String),
    Unavailable(String),
    OwnershipLost(String),
}

fn session_error_exit(code: &str, response_epoch: u32) -> SessionExit {
    match code {
        "lease_expired" => SessionExit::Lost(format!(
            "endpoint lease expired (endpoint epoch {response_epoch})"
        )),
        "invalid_capability" | "resource_exhausted" => SessionExit::Lost(format!(
            "endpoint HID unavailable: {code} (endpoint epoch {response_epoch})"
        )),
        _ => SessionExit::OwnershipLost(format!("{code} (endpoint epoch {response_epoch})")),
    }
}

fn resolve_for_recovery(
    control: &SessionControl,
    resolve: impl FnOnce() -> Result<Option<crate::discovery::Service>, String> + Send + 'static,
) -> Option<(crate::discovery::Service, SocketAddr)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    // Dropping the receiver lets the session exit while the DNS calls finish.
    std::thread::Builder::new()
        .name("session-discovery".into())
        .spawn(move || {
            let resolved = resolve().ok().flatten().and_then(|service| {
                let address = crate::discovery::socket_address(&service).ok()?;
                Some((service, address))
            });
            let _ = sender.send(resolved);
        })
        .ok()?;
    while !control.is_cancelled() {
        if control.take_escape() && control.input_mode() == Some(SessionMode::Cli) {
            control.cancel();
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(resolved) => return resolved,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    None
}

pub(crate) fn run_session<F>(
    initial_service: crate::discovery::Service,
    status: crate::Status,
    credential: crate::credentials::ClientCredential,
    control: SessionControl,
    rediscover: bool,
    on_event: F,
) -> Result<SocketAddr>
where
    F: FnMut(SessionEvent),
{
    let heartbeat_mode =
        crate::discovery::compatibility_mode(&initial_service).map_err(anyhow::Error::msg)?;
    run_session_with_heartbeat_ack(
        initial_service,
        status,
        credential,
        control,
        rediscover,
        heartbeat_mode == crate::discovery::HeartbeatMode::Acknowledged,
        on_event,
    )
}

pub(crate) fn run_session_with_heartbeat_ack<F>(
    initial_service: crate::discovery::Service,
    status: crate::Status,
    credential: crate::credentials::ClientCredential,
    control: SessionControl,
    rediscover: bool,
    heartbeat_ack_supported: bool,
    mut on_event: F,
) -> Result<SocketAddr>
where
    F: FnMut(SessionEvent),
{
    let advertised_mode =
        crate::discovery::compatibility_mode(&initial_service).map_err(anyhow::Error::msg)?;
    ensure!(
        advertised_mode
            == if heartbeat_ack_supported {
                crate::discovery::HeartbeatMode::Acknowledged
            } else {
                crate::discovery::HeartbeatMode::Legacy
            },
        "service compatibility mode changed before session start"
    );
    let mut heartbeat_ack_supported = heartbeat_ack_supported;
    install_certificate_log()?;
    log::info!("session connecting to {}", initial_service.hostname);
    let mut service = initial_service;
    let runtime = default_runtime().context("no WebRTC runtime")?;
    let mut recovery_started: Option<Instant> = None;
    let mut retry_delay = Duration::from_secs(1);
    let mut last_address =
        crate::discovery::socket_address(&service).map_err(anyhow::Error::msg)?;
    loop {
        if let Err(error) = control.check_input_health() {
            on_event(SessionEvent::Unavailable {
                reason: error.to_string(),
            });
            return Err(error);
        }
        if recovery_started.is_some() {
            control.discard_pre_lease_shortcuts();
        }
        if control.is_cancelled() {
            on_event(SessionEvent::Disconnected);
            return Ok(last_address);
        }
        on_event(if recovery_started.is_some() {
            SessionEvent::Recovering {
                seconds_remaining: 60u64
                    .saturating_sub(recovery_started.unwrap().elapsed().as_secs()),
            }
        } else {
            SessionEvent::Connecting
        });
        let device_status = AtomicU8::new(1);
        let mut connected_in_attempt = false;
        let mut exit = None;
        runtime.block_on(Box::pin(async {
            exit = Some(
                run_session_once(
                    runtime.clone(),
                    last_address,
                    &service.hostname,
                    &status,
                    &credential,
                    &control,
                    heartbeat_ack_supported,
                    service
                        .properties
                        .get("live")
                        .is_some_and(|value| value == "1"),
                    &mut |event| {
                        if matches!(event, SessionEvent::ConnectedLocal) {
                            connected_in_attempt = true;
                        }
                        on_event(event);
                    },
                    &device_status,
                )
                .await,
            );
        }));
        let exit = exit.context("runtime did not execute")??;
        if let Err(error) = control.check_input_health() {
            on_event(SessionEvent::Unavailable {
                reason: error.to_string(),
            });
            return Err(error);
        }
        if connected_in_attempt {
            recovery_started = None;
            retry_delay = Duration::from_secs(1);
        }
        match exit {
            SessionExit::Disconnected => {
                on_event(SessionEvent::Disconnected);
                return Ok(last_address);
            }
            SessionExit::OwnershipLost(reason) => {
                log::warn!("session ownership lost: {reason}");
                on_event(SessionEvent::OwnershipLost { reason });
                return Ok(last_address);
            }
            SessionExit::Unavailable(reason) if recovery_started.is_none() => {
                on_event(SessionEvent::Unavailable {
                    reason: reason.clone(),
                });
                return Err(anyhow::anyhow!(reason));
            }
            SessionExit::Unavailable(reason) => {
                on_event(SessionEvent::Notice(format!(
                    "Recovery attempt unavailable: {reason}"
                )));
            }
            SessionExit::Lost(reason) => {
                if control.is_cancelled() {
                    bail!("Old lease cleanup was not confirmed: {reason}");
                }
                log::warn!("session transport lost: {reason}");
                recovery_started.get_or_insert_with(Instant::now);
                on_event(SessionEvent::Notice(format!("Connection lost: {reason}")));
            }
        }
        let started = recovery_started.expect("recovery start set before retry");
        if control.is_cancelled() {
            on_event(SessionEvent::Disconnected);
            return Ok(last_address);
        }
        if started.elapsed() >= Duration::from_secs(60) {
            let reason = "recovery window expired".to_owned();
            on_event(SessionEvent::Unavailable {
                reason: reason.clone(),
            });
            return Err(anyhow::anyhow!(reason));
        }
        let fingerprint = status.identity_fingerprint;
        if rediscover
            && let Some((next, address)) = resolve_for_recovery(&control, move || {
                crate::discovery::discover_target(&fingerprint)
            })
            && !control.is_cancelled()
        {
            heartbeat_ack_supported = crate::discovery::compatibility_mode(&next)
                .map_err(anyhow::Error::msg)?
                == crate::discovery::HeartbeatMode::Acknowledged;
            service = next;
            last_address = address;
        }
        let wait_until = Instant::now() + retry_delay;
        while Instant::now() < wait_until {
            if control.take_escape() && control.input_mode() == Some(SessionMode::Cli) {
                control.cancel();
            }
            if control.is_cancelled() {
                on_event(SessionEvent::Disconnected);
                return Ok(last_address);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        retry_delay = (retry_delay * 2).min(Duration::from_secs(5));
    }
}

pub(crate) fn run_takeover_request(
    service: &crate::discovery::Service,
    status: crate::Status,
    credential: &crate::credentials::ClientCredential,
) -> Result<()> {
    crate::discovery::compatibility_mode(service).map_err(anyhow::Error::msg)?;
    install_certificate_log()?;
    let address = crate::discovery::socket_address(service).map_err(anyhow::Error::msg)?;
    let runtime = default_runtime().context("no WebRTC runtime")?;
    let device_status = AtomicU8::new(1);
    let mut result = None;
    runtime.block_on(Box::pin(async {
        let opened = open_signaling_peer(
            runtime.clone(),
            &device_status,
            address,
            &service.hostname,
            &status,
            credential,
        )
        .await;
        let mut peer = match opened {
            Ok(peer) => peer,
            Err(error) => {
                result = Some(Err(error));
                return;
            }
        };
        result = Some(
            async {
                peer.session
                    .send_transcript_proof(&peer.proof)
                    .map_err(anyhow::Error::msg)?;
                let request_id = crate::random_id().map_err(anyhow::Error::msg)?;
                let mut responses = ResponseCursor::new(peer.session.session_id());
                send_hid_event(
                    &peer.control,
                    crate::hid_protocol::Envelope {
                        message_type: crate::hid_protocol::TAKEOVER_REQUEST,
                        session_id: peer.session.session_id(),
                        lease_epoch: 0,
                        sequence: 0,
                        event: crate::hid_protocol::Event::TakeoverRequest { request_id },
                    },
                )
                .await?;
                eprintln!("M2.2 TAKEOVER_REQUEST sent; waiting for local-owner decision");
                let deadline = Instant::now() + Duration::from_secs(60);
                let mut pending_epoch = None;
                loop {
                    ensure!(Instant::now() < deadline, "takeover request timed out");
                    match timeout(&*runtime, Duration::from_millis(250), peer.control.poll()).await
                    {
                        Err(_) => continue,
                        Ok(Some(DataChannelEvent::OnMessage(message))) => {
                            let (response, epoch) = responses.decode_any_epoch(&message.data)?;
                            log::debug!(
                                "takeover requester response epoch={epoch} value={response:?}"
                            );
                            match response {
                                EndpointResponse::LeaseState { owner, state }
                                    if owner == Some(credential.fingerprint) && state == 1 =>
                                {
                                    eprintln!("M2.2 TAKEOVER_REQUEST granted=true epoch={epoch}");
                                    return Ok(());
                                }
                                EndpointResponse::LeaseState {
                                    owner: Some(owner),
                                    state: 2,
                                } if owner != credential.fingerprint => {
                                    pending_epoch.get_or_insert(epoch);
                                    eprintln!("M2.2 TAKEOVER_REQUEST pending=true");
                                }
                                EndpointResponse::TakeoverResponse {
                                    request_id: response_id,
                                    accepted,
                                } if response_id == request_id => {
                                    eprintln!("M2.2 TAKEOVER_REQUEST accepted={accepted}");
                                    if !accepted {
                                        return Ok(());
                                    }
                                }
                                EndpointResponse::Error { code } => {
                                    bail!("takeover request rejected: {code}");
                                }
                                _ => {}
                            }
                        }
                        Ok(Some(DataChannelEvent::OnClose)) | Ok(None) => {
                            bail!("takeover requester control channel closed")
                        }
                        Ok(_) => {}
                    }
                }
            }
            .await,
        );
        peer.close(&*runtime).await;
    }));
    result.context("runtime did not execute")?
}

async fn run_session_once<F>(
    runtime: Arc<dyn Runtime>,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential: &crate::credentials::ClientCredential,
    control: &SessionControl,
    heartbeat_ack_supported: bool,
    live_protocol_supported: bool,
    on_event: &mut F,
    device_status: &AtomicU8,
) -> Result<SessionExit>
where
    F: FnMut(SessionEvent),
{
    if std::env::var_os("KVMCTL_LEGACY_PROTOCOL").is_none() {
        return run_mini_session_once(
            runtime,
            address,
            hostname,
            status,
            credential,
            control,
            on_event,
            device_status,
        )
        .await;
    }
    let mut peer = match open_signaling_peer(
        runtime.clone(),
        device_status,
        address,
        hostname,
        status,
        credential,
    )
    .await
    {
        Ok(peer) => peer,
        Err(error) => return Ok(SessionExit::Unavailable(error.to_string())),
    };
    log::debug!("session signaling authenticated for {hostname}");
    let mut connected = false;
    // The endpoint validates each DataChannel sequence independently. Input
    // reports use a reserved sequence namespace because their completion ACKs
    // share the control response stream with heartbeats.
    let mut control_sequence = 0u64;
    let mut input_sequence = INPUT_SEQUENCE_BASE;
    let mut motion_sequence = 0u64;
    let mut takeover_epoch = 0u32;
    let session_result: Result<SessionExit> = async {
        peer.session
            .send_transcript_proof(&peer.proof)
            .map_err(anyhow::Error::msg)
            .context("foreground session authorization")?;
        control.discard_pre_lease_shortcuts();
        if control.is_cancelled() {
            return Ok(SessionExit::Disconnected);
        }
        let mut responses = ResponseCursor::new(peer.session.session_id());
        send_hid_event(
            &peer.control,
            crate::hid_protocol::Envelope {
                message_type: crate::hid_protocol::LEASE_REQUEST,
                session_id: peer.session.session_id(),
                lease_epoch: 0,
                sequence: 0,
                event: crate::hid_protocol::Event::LeaseRequest {
                    heartbeat_ack_supported,
                },
            },
        )
        .await?;
        let (response, epoch) =
            receive_endpoint_response_any_epoch(&*runtime, &peer.control, &mut responses).await?;
        require_response(
            response,
            EndpointResponse::LeaseState {
                owner: Some(credential.fingerprint),
                state: 1,
            },
        )?;
        log::info!("session lease granted epoch={epoch}");
        control.discard_pre_lease_shortcuts();
        control.input_ready(epoch)?;
        let motion_rate_hz = if control.input_enabled() {
            control.input_motion_rate_hz().to_string()
        } else {
            "off".into()
        };
        eprintln!(
            "M2.3 SESSION authenticated=true input_local=true physical_input_capture={} input_channels={} motion_rate_hz={} escape={}",
            if control.input_enabled() {
                "armed-local"
            } else {
                "off"
            },
            if control.input_enabled() {
                "ordered-input+unordered-motion"
            } else {
                "off"
            },
            motion_rate_hz,
            if control.input_enabled() { "ready" } else { "off" },
        );
        connected = true;
        on_event(SessionEvent::ConnectedLocal);
        control_sequence = 1;

        let heartbeat_interval = if heartbeat_ack_supported {
            HEARTBEAT_INTERVAL
        } else {
            LEGACY_HEARTBEAT_INTERVAL
        };
        let mut heartbeat_due = Instant::now()
            + if heartbeat_ack_supported {
                Duration::ZERO
            } else {
                heartbeat_interval
            };
        let mut heartbeat_sent_at = None;
        let mut heartbeat_request_sequence = None;
        let mut missed_heartbeats = 0u8;
        let mut degraded = false;
        let mut live = LiveInput::default();
        let poll_interval = if control.input_enabled() {
            control
                .input_motion_interval()
                .min(Duration::from_millis(5))
        } else {
            Duration::from_millis(100)
        };
        let session_id = peer.session.session_id();
        takeover_epoch = epoch;
        loop {
            if peer.peer_failed.load(Ordering::Acquire) {
                return Ok(SessionExit::Lost("peer connection failed".into()));
            }
            if control.is_cancelled() {
                if control.input_set_local()? {
                    on_event(SessionEvent::InputLocalConfirmed);
                }
                send_hid_event(
                    &peer.control,
                    crate::hid_protocol::Envelope {
                        message_type: crate::hid_protocol::LEASE_RELEASE,
                        session_id,
                        lease_epoch: takeover_epoch,
                        sequence: control_sequence,
                        event: crate::hid_protocol::Event::LeaseRelease,
                    },
                )
                .await.context("send old lease release")?;
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let (response, epoch) = timeout(
                        &*runtime, deadline.saturating_duration_since(Instant::now()),
                        receive_endpoint_response_any_epoch(&*runtime, &peer.control, &mut responses),
                    ).await.context("old lease cleanup timed out")??;
                    let released_epoch = next_lease_epoch(takeover_epoch);
                    let released = responses.take_request_sequence().is_none()
                        && match response {
                            EndpointResponse::LeaseState { owner: None, state: 0 } => epoch == released_epoch,
                            EndpointResponse::LeaseState { owner: Some(owner), state: 1 }
                                if owner != credential.fingerprint => epoch == next_lease_epoch(released_epoch),
                            _ => false,
                        };
                    if released {
                        break;
                    }
                    if let EndpointResponse::Error { code } = response {
                        bail!("old lease release rejected: {code}");
                    }
                }
                return Ok(SessionExit::Disconnected);
            }
            let now = Instant::now();
            if heartbeat_ack_supported
                && heartbeat_sent_at
                    .is_some_and(|sent_at| now.duration_since(sent_at) >= heartbeat_interval)
            {
                heartbeat_sent_at = None;
                heartbeat_request_sequence = None;
                missed_heartbeats = missed_heartbeats.saturating_add(1);
                log::warn!("session heartbeat missed count={missed_heartbeats}");
                if missed_heartbeats >= 3 {
                    return Ok(SessionExit::Lost("heartbeat response timeout".into()));
                }
                if !degraded {
                    degraded = true;
                    on_event(SessionEvent::ConnectionDegraded {
                        missed: missed_heartbeats,
                    });
                    // Keep the established remote route alive while the
                    // connection is only degraded. Escape or confirmed loss
                    // remains responsible for returning control locally.
                }
            }
            let heartbeat_can_send = !heartbeat_ack_supported || heartbeat_sent_at.is_none();
            if heartbeat_can_send && now >= heartbeat_due {
                let send_started = Instant::now();
                log::debug!(
                    "session heartbeat send epoch={} sequence={control_sequence}",
                    takeover_epoch
                );
                send_hid_event(
                    &peer.control,
                    crate::hid_protocol::Envelope {
                        message_type: crate::hid_protocol::HEARTBEAT,
                        session_id: peer.session.session_id(),
                        lease_epoch: takeover_epoch,
                        sequence: control_sequence,
                        event: crate::hid_protocol::Event::Heartbeat,
                    },
                )
                .await?;
                let send_completed = Instant::now();
                control_sequence += 1;
                heartbeat_due += heartbeat_interval;
                if heartbeat_ack_supported {
                    heartbeat_request_sequence = Some(control_sequence - 1);
                    heartbeat_sent_at = Some(send_completed);
                }
                log::debug!(
                    "session heartbeat send-complete sequence={} enqueue_ms={}",
                    control_sequence - 1,
                    send_completed.duration_since(send_started).as_millis()
                );
            }
            // Heartbeat ACKs and live input use independent channels/sequence
            // state. Keep capture and forwarding active while ACK is pending;
            // Escape still returns input local through process_input_events.
            process_input_events(
                &*runtime,
                &peer.control,
                &peer.input,
                &peer.motion,
                control,
                session_id,
                takeover_epoch,
                &mut control_sequence,
                &mut input_sequence,
                &mut motion_sequence,
                &mut live,
                // A missed heartbeat is degradation, not confirmed loss. The
                // control channel can still carry an arm/disarm transition;
                // keep the toggle responsive and let its correlated reply
                // decide whether the endpoint is reachable.
                control.pending_takeover().is_none(),
                live_protocol_supported, on_event,
            ).await?;
            if control.is_cancelled() { continue; }
            if live.arm.as_ref().is_some_and(|arm| !arm.acknowledged && now >= arm.deadline) {
                bail!("input arm response timed out");
            }
            if live.report.is_some_and(|(_, deadline)| now >= deadline) {
                bail!("input report completion timed out");
            }
            if live.disarm.is_some_and(|(_, deadline)| now >= deadline) {
                bail!("input disarm completion timed out");
            }
            match timeout(&*runtime, poll_interval, peer.control.poll()).await {
                Ok(Some(DataChannelEvent::OnMessage(message))) => {
                    let (response, response_epoch) = responses.decode_any_epoch(&message.data)?;
                    let response_request_sequence = responses.take_request_sequence();
                    log::debug!("session response epoch={response_epoch} value={response:?}");
                    match response {
                        EndpointResponse::TakeoverNotice {
                            request_id,
                            requester,
                            response_window_ms,
                        } if response_epoch == takeover_epoch => {
                            log::warn!(
                                "session takeover notice epoch={} requester={} request={} window_ms={}",
                                response_epoch,
                                crate::hex(&requester[..4]),
                                crate::hex(&request_id[..4]),
                                response_window_ms
                            );
                            stop_live_input(control, &peer.control, session_id, takeover_epoch,
                                &mut control_sequence, &mut live, on_event).await?;
                            control.start_takeover(request_id, response_window_ms);
                            on_event(SessionEvent::TakeoverPending {
                                request_id, requester, response_window_ms,
                            });
                        }
                        response if live.arm.as_ref().is_some_and(|arm| {
                            arm.matches_reply(&response, response_epoch, response_request_sequence,
                                credential.fingerprint)
                        }) => {
                            // An escape queued while polling still belongs to
                            // this attempt, even if its rejection arrived first.
                            if live.arm.as_ref().is_some_and(|arm| !arm.cancelled) && control.take_escape() {
                                stop_live_input(control, &peer.control, session_id, takeover_epoch,
                                    &mut control_sequence, &mut live, on_event).await?;
                            }
                            let accepted = matches!(response, EndpointResponse::LeaseState { .. });
                            let declined = live.arm_reply(accepted);
                            log::debug!(
                                "live input arm reply epoch={} request_sequence={:?} accepted={} declined={}",
                                response_epoch, response_request_sequence, accepted, declined
                            );
                            if declined {
                                on_event(SessionEvent::Notice(
                                    "Endpoint declined remote input. Input remains local.".into(),
                                ));
                            }
                        }
                        EndpointResponse::LeaseState { owner: Some(owner), state }
                            if response_epoch == takeover_epoch
                                && owner == credential.fingerprint && matches!(state, 1 | 2) =>
                        {
                            if live.credit_reply(response_request_sequence) {
                                log::debug!(
                                    "live input completion acknowledged epoch={} request_sequence={:?}",
                                    response_epoch, response_request_sequence
                                );
                                send_pending_disarm(
                                    &peer.control,
                                    session_id,
                                    takeover_epoch,
                                    &mut control_sequence,
                                    &mut live,
                                    false,
                                    false,
                                    false,
                                )
                                .await?;
                            }
                        }
                        EndpointResponse::Error { code }
                            if live.disarm.is_some_and(|(sent, _)| Some(sent) == response_request_sequence) =>
                        {
                            return Ok(SessionExit::Lost(format!("endpoint rejected input disarm: {code}")));
                        }
                        EndpointResponse::Error { .. } if response_request_sequence.is_some() => {}
                        EndpointResponse::LeaseState { owner, state }
                            if owner == Some(credential.fingerprint)
                                && (state == 1 || state == 2) =>
                        {
                            if response_epoch != takeover_epoch {
                                if response_request_sequence.is_some() { continue; }
                                return Ok(SessionExit::Lost("endpoint lease epoch changed".into()));
                            }
                            let heartbeat_acknowledged = matches!(
                                (response_request_sequence, heartbeat_request_sequence),
                                (Some(received), Some(pending)) if received == pending
                            );
                            if heartbeat_acknowledged {
                                if let Some(sent_at) = heartbeat_sent_at.take() {
                                    let rtt_ms = Instant::now()
                                        .duration_since(sent_at)
                                        .as_millis()
                                        .min(u128::from(u64::MAX))
                                        as u64;
                                    log::debug!(
                                        "session heartbeat acknowledged epoch={response_epoch} rtt_ms={rtt_ms}"
                                    );
                                    if missed_heartbeats != 0 && degraded {
                                        on_event(SessionEvent::ConnectionRecovered { rtt_ms });
                                    }
                                    missed_heartbeats = 0;
                                    degraded = false;
                                    heartbeat_request_sequence = None;
                                }
                            } else {
                                log::debug!(
                                    "session state ignored as heartbeat ack response_request_sequence={response_request_sequence:?} pending_request_sequence={heartbeat_request_sequence:?}"
                                );
                            }
                        }
                        EndpointResponse::Error { code } => {
                            return Ok(session_error_exit(&code, response_epoch));
                        }
                        EndpointResponse::LeaseState { owner, state } => match owner {
                            Some(owner) if owner != credential.fingerprint => {
                                return Ok(SessionExit::OwnershipLost(format!(
                                    "another enrolled identity owns target (owner={} epoch={} state={})",
                                    crate::hex(&owner[..4]), response_epoch, state
                                )));
                            }
                            None => {
                                return Ok(SessionExit::Lost(format!(
                                    "endpoint lease expired or revoked (endpoint epoch {response_epoch})"
                                )));
                            }
                            Some(_) => {
                                return Ok(SessionExit::Lost(format!(
                                    "unexpected endpoint lease state {state} (endpoint epoch {response_epoch})"
                                )));
                            }
                        }
                        EndpointResponse::TakeoverResponse { .. }
                        | EndpointResponse::TakeoverNotice { .. } => {}
                    }
                }
                Ok(Some(DataChannelEvent::OnClose)) | Ok(None) => {
                    return Ok(SessionExit::Lost(
                        "foreground control channel closed".into(),
                    ));
                }
                Ok(_) | Err(_) => {}
            }
            if let Some(request_id) = control.pending_takeover()
                && let Some(accepted) = control.take_takeover_decision(request_id)
            {
                send_hid_event(
                    &peer.control,
                    crate::hid_protocol::Envelope {
                        message_type: crate::hid_protocol::TAKEOVER_RESPONSE,
                        session_id: peer.session.session_id(),
                        lease_epoch: takeover_epoch,
                        sequence: control_sequence,
                        event: crate::hid_protocol::Event::TakeoverResponse {
                            request_id,
                            accepted,
                        },
                    },
                )
                .await?;
                control_sequence += 1;
                on_event(SessionEvent::Notice(if accepted {
                    "Takeover accepted; input remains local until ownership changes.".into()
                } else {
                    "Takeover rejected; input remains local.".into()
                }));
            }
            if control.takeover_expired() {
                on_event(SessionEvent::Notice(
                    "Takeover request expired; input remains local.".into(),
                ));
            }
        }
    }
    .await;
    if control.input_set_local().unwrap_or(false) {
        on_event(SessionEvent::InputLocalConfirmed);
    }
    control.input_fail_closed();
    peer.close(&*runtime).await;
    // Switching must wait for confirmed lease cleanup. On failure the caller
    // stays local and the endpoint's peer-close/lease-expiry path cleans up.
    if control.is_cancelled() {
        return session_result;
    }
    match session_result {
        Ok(exit) => Ok(exit),
        Err(error) => {
            control.clear_takeover();
            if connected {
                Ok(SessionExit::Lost(error.to_string()))
            } else {
                Ok(SessionExit::Unavailable(error.to_string()))
            }
        }
    }
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
    last_request_sequence: Option<u64>,
}

impl ResponseCursor {
    fn new(session_id: [u8; 16]) -> Self {
        Self {
            session_id,
            next_sequence: 0,
            last_request_sequence: None,
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
        self.last_request_sequence = Self::request_sequence(&envelope.event);
        self.advance()?;
        Self::response(envelope.event)
    }

    fn decode_any_epoch(&mut self, data: &[u8]) -> Result<(EndpointResponse, u32)> {
        let envelope = self.decode_envelope(data)?;
        let epoch = envelope.lease_epoch;
        self.last_request_sequence = Self::request_sequence(&envelope.event);
        self.advance()?;
        Ok((Self::response(envelope.event)?, epoch))
    }

    fn take_request_sequence(&mut self) -> Option<u64> {
        self.last_request_sequence.take()
    }

    fn request_sequence(event: &crate::hid_protocol::Event) -> Option<u64> {
        match event {
            crate::hid_protocol::Event::LeaseState {
                request_sequence, ..
            }
            | crate::hid_protocol::Event::Error {
                request_sequence, ..
            } => *request_sequence,
            _ => None,
        }
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
            "endpoint response sequence mismatch: expected {}, received {} ({:?})",
            self.next_sequence,
            envelope.sequence,
            envelope.event
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
            crate::hid_protocol::Event::LeaseState { owner, state, .. } => {
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
            crate::hid_protocol::Event::Error { code, .. } => EndpointResponse::Error { code },
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
    Ok(
        receive_endpoint_response_with_sequence(runtime, control, cursor, expected_epoch)
            .await?
            .0,
    )
}

async fn receive_endpoint_response_with_sequence(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    cursor: &mut ResponseCursor,
    expected_epoch: u32,
) -> Result<(EndpointResponse, Option<u64>)> {
    loop {
        match timeout(runtime, Duration::from_secs(5), control.poll()).await? {
            Some(DataChannelEvent::OnMessage(message)) => {
                let response = cursor.decode(&message.data, expected_epoch)?;
                return Ok((response, cursor.take_request_sequence()));
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

async fn run_mini_session_once<F: FnMut(SessionEvent)>(
    runtime: Arc<dyn Runtime>,
    address: SocketAddr,
    hostname: &str,
    status: &crate::Status,
    credential: &crate::credentials::ClientCredential,
    control: &SessionControl,
    on_event: &mut F,
    device_status: &AtomicU8,
) -> Result<SessionExit> {
    let mut peer = match open_signaling_peer(
        runtime.clone(),
        device_status,
        address,
        hostname,
        status,
        credential,
    )
    .await
    {
        Ok(peer) => peer,
        Err(error) => return Ok(SessionExit::Unavailable(error.to_string())),
    };
    let sctp_t3_start = RTC_SCTP_T3_TIMEOUTS.load(Ordering::Relaxed);
    let sctp_ack_start = RTC_SCTP_ACK_TIMEOUTS.load(Ordering::Relaxed);
    let mut input = MiniInput::default();
    let mut max_missed = 0u8;
    let mut degradation_count = 0u32;
    let session_result: Result<SessionExit> = async {
        peer.session
            .send_transcript_proof(&peer.proof)
            .map_err(anyhow::Error::msg)
            .context("foreground session authorization")?;
        control.discard_pre_lease_shortcuts();
        if control.is_cancelled() {
            return Ok(SessionExit::Disconnected);
        }
        mini_hid_handshake(&*runtime, &peer.hidrpc).await?;
        // Mini has no kvmctl lease frame.  Capture authority begins at the
        // authenticated peer generation and is invalidated on close.
        let epoch = 1;
        control.input_ready(epoch)?;
        eprintln!(
            "M2.3 SESSION authenticated=true input_local=true physical_input_capture={} input_channels=hidrpc+hidrpc-unreliable-nonordered motion_rate_hz={} escape=ready",
            if control.input_enabled() { "armed-local" } else { "off" },
            if control.input_enabled() {
                control.input_motion_rate_hz().to_string()
            } else {
                "off".to_owned()
            },
        );
        on_event(SessionEvent::ConnectedLocal);

        let mut next_ping = Instant::now();
        let mut ping_sent_at = None;
        let mut missed = 0u8;
        let mut degraded = false;
        let mut ping_id = 0u64;
        let mut mini_responses = ResponseCursor::new(peer.session.session_id());
        let mut mini_control_sequence = 0u64;
        let mut mini_takeover_epoch = None;
        let mut mini_lease_epoch = None;
        loop {
            if peer.peer_failed.load(Ordering::Acquire) {
                return Ok(SessionExit::Lost("peer connection failed".into()));
            }
            if control.is_cancelled() {
                if control.input_is_remote() {
                    mini_send_neutral(&peer.hidrpc).await?;
                }
                if control.input_set_local()? {
                    on_event(SessionEvent::InputLocalConfirmed);
                }
                if let Some(lease_epoch) = mini_lease_epoch {
                    mini_release(
                        &*runtime,
                        &peer.rpc,
                        &mut mini_responses,
                        peer.session.session_id(),
                        lease_epoch,
                        &mut mini_control_sequence,
                    )
                    .await?;
                }
                return Ok(SessionExit::Disconnected);
            }
            let now = Instant::now();
            if ping_sent_at.is_some_and(|sent| now.duration_since(sent) >= HEARTBEAT_INTERVAL) {
                // A missed ping is a warning only. Switching local while the
                // user is still moving can inject those inputs into the host;
                // Escape or an actual channel close remains the handoff path.
                ping_sent_at = None;
                missed = missed.saturating_add(1);
                max_missed = max_missed.max(missed);
                if missed >= 3 {
                    return Ok(SessionExit::Lost("heartbeat response timeout".into()));
                }
                if !degraded && missed >= MINI_DEGRADE_WARN_MISSES {
                    degraded = true;
                    degradation_count += 1;
                    on_event(SessionEvent::ConnectionDegraded { missed });
                    eprintln!(
                        "M2.3 MINI DEGRADE missed={} input_remote={} action=warn_only sctp_t3_timeouts={} sctp_ack_timeouts={}",
                        missed,
                        control.input_is_remote(),
                        RTC_SCTP_T3_TIMEOUTS
                            .load(Ordering::Relaxed)
                            .saturating_sub(sctp_t3_start),
                        RTC_SCTP_ACK_TIMEOUTS
                            .load(Ordering::Relaxed)
                            .saturating_sub(sctp_ack_start),
                    );
                }
            }
            if now >= next_ping && ping_sent_at.is_none() {
                ping_id = ping_id.wrapping_add(1);
                let request = format!(
                    "{{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"params\":{{}},\"id\":{ping_id}}}"
                );
                peer.rpc.send_text(&request).await?;
                ping_sent_at = Some(Instant::now());
                next_ping += HEARTBEAT_INTERVAL;
            }
            process_mini_input_events(
                &peer.hidrpc,
                &peer.hidrpc_unreliable_nonordered,
                control,
                epoch,
                &mut input,
                on_event,
            ).await?;
            let motion_wait = input
                .last_motion_sent
                .map(|sent| control.input_motion_interval().saturating_sub(sent.elapsed()))
                .unwrap_or_default();
            let rpc_poll_timeout = if input.motion != (0, 0) {
                motion_wait.min(Duration::from_millis(5))
            } else {
                Duration::from_millis(5)
            };
            match timeout(&*runtime, rpc_poll_timeout, peer.rpc.poll()).await {
                Ok(Some(DataChannelEvent::OnMessage(message))) => {
                    if message.data.first() == Some(&b'{') {
                        if let Some(sent) = ping_sent_at.take() {
                            let rtt_ms = Instant::now().duration_since(sent).as_millis() as u64;
                            if degraded {
                                on_event(SessionEvent::ConnectionRecovered { rtt_ms });
                                eprintln!(
                                    "M2.3 MINI RECOVER rtt_ms={} missed={} sctp_t3_timeouts={} sctp_ack_timeouts={}",
                                    rtt_ms,
                                    missed,
                                    RTC_SCTP_T3_TIMEOUTS
                                        .load(Ordering::Relaxed)
                                        .saturating_sub(sctp_t3_start),
                                    RTC_SCTP_ACK_TIMEOUTS
                                        .load(Ordering::Relaxed)
                                        .saturating_sub(sctp_ack_start),
                                );
                            }
                            missed = 0;
                            degraded = false;
                        }
                    } else {
                        let (response, response_epoch) =
                            mini_responses.decode_any_epoch(&message.data)?;
                        match response {
                            EndpointResponse::LeaseState { .. } => {
                                mini_lease_epoch = Some(response_epoch);
                            }
                            EndpointResponse::TakeoverNotice {
                                request_id,
                                requester,
                                response_window_ms,
                            } if control.pending_takeover().is_none() => {
                                mini_takeover_epoch = Some(response_epoch);
                                if control.input_is_remote() {
                                    mini_send_neutral(&peer.hidrpc).await?;
                                    if control.input_set_local()? {
                                        on_event(SessionEvent::InputLocalConfirmed);
                                    }
                                }
                                control.start_takeover(request_id, response_window_ms);
                                on_event(SessionEvent::TakeoverPending {
                                    request_id,
                                    requester,
                                    response_window_ms,
                                });
                            }
                            EndpointResponse::TakeoverNotice { .. }
                            | EndpointResponse::TakeoverResponse { .. }
                            | EndpointResponse::Error { .. } => {}
                        }
                    }
                }
                Ok(Some(DataChannelEvent::OnClose)) | Ok(None) => {
                    return Ok(SessionExit::Lost("Mini rpc channel closed".into()));
                }
                Ok(_) | Err(_) => {}
            }
            if let Some(request_id) = control.pending_takeover()
                && let Some(accepted) = control.take_takeover_decision(request_id)
            {
                send_hid_event(
                    &peer.rpc,
                    crate::hid_protocol::Envelope {
                        message_type: crate::hid_protocol::TAKEOVER_RESPONSE,
                        session_id: peer.session.session_id(),
                        lease_epoch: mini_takeover_epoch.unwrap_or(epoch),
                        sequence: mini_control_sequence,
                        event: crate::hid_protocol::Event::TakeoverResponse {
                            request_id,
                            accepted,
                        },
                    },
                )
                .await?;
                mini_control_sequence += 1;
                on_event(SessionEvent::Notice(if accepted {
                    "Takeover accepted; input remains local until ownership changes.".into()
                } else {
                    "Takeover rejected; input remains local.".into()
                }));
            }
            if control.takeover_expired() {
                on_event(SessionEvent::Notice(
                    "Takeover request expired; input remains local.".into(),
                ));
            }
        }
    }.await;

    if control.input_is_remote() {
        let _ = mini_send_neutral(&peer.hidrpc).await;
    }
    if control.input_set_local().unwrap_or(false) {
        on_event(SessionEvent::InputLocalConfirmed);
    }
    eprintln!(
        "M2.3 MINI TELEMETRY motion_sent={} cadence_min_us={} cadence_max_us={} degradation_count={} max_missed={} sctp_t3_timeouts={} sctp_ack_timeouts={}",
        input.motion_sent,
        input.motion_interval_min_us.unwrap_or_default(),
        input.motion_interval_max_us.unwrap_or_default(),
        degradation_count,
        max_missed,
        RTC_SCTP_T3_TIMEOUTS
            .load(Ordering::Relaxed)
            .saturating_sub(sctp_t3_start),
        RTC_SCTP_ACK_TIMEOUTS
            .load(Ordering::Relaxed)
            .saturating_sub(sctp_ack_start),
    );
    control.input_fail_closed();
    peer.close(&*runtime).await;
    match session_result {
        Ok(exit) => Ok(exit),
        Err(error) => Ok(SessionExit::Lost(error.to_string())),
    }
}

#[derive(Default)]
struct MiniInput {
    motion: (i32, i32),
    last_motion_sent: Option<Instant>,
    motion_sent: u64,
    motion_interval_min_us: Option<u64>,
    motion_interval_max_us: Option<u64>,
    last_keyboard_keepalive: Option<Instant>,
}

impl MiniInput {
    fn note_motion_sent(&mut self) {
        let sent = Instant::now();
        if let Some(previous) = self.last_motion_sent {
            let interval_us = sent.duration_since(previous).as_micros() as u64;
            self.motion_interval_min_us = Some(
                self.motion_interval_min_us
                    .map_or(interval_us, |value| value.min(interval_us)),
            );
            self.motion_interval_max_us = Some(
                self.motion_interval_max_us
                    .map_or(interval_us, |value| value.max(interval_us)),
            );
        }
        self.motion_sent += 1;
        self.last_motion_sent = Some(sent);
    }
}

async fn mini_hid_handshake(runtime: &dyn Runtime, channel: &Arc<dyn DataChannel>) -> Result<()> {
    channel
        .send(BytesMut::from(crate::mini_hid::HANDSHAKE.as_slice()))
        .await?;
    loop {
        match timeout(runtime, Duration::from_secs(5), channel.poll()).await? {
            Some(DataChannelEvent::OnMessage(message)) if message.data.as_ref() == [1, 1] => {
                return Ok(());
            }
            Some(DataChannelEvent::OnClose) | None => bail!("Mini hidrpc closed during handshake"),
            Some(_) => {}
        }
    }
}

async fn mini_send_event(
    channel: &Arc<dyn DataChannel>,
    event: &crate::hid_protocol::Event,
    buttons: u8,
) -> Result<()> {
    let frame = crate::mini_hid::encode(event, buttons)
        .context("event has no Mini HIDRPC representation")?;
    channel.send(BytesMut::from(frame.as_slice())).await?;
    Ok(())
}

async fn mini_send_neutral(channel: &Arc<dyn DataChannel>) -> Result<()> {
    mini_send_event(
        channel,
        &crate::hid_protocol::Event::KeyboardState {
            modifiers: 0,
            keys: Vec::new(),
        },
        0,
    )
    .await?;
    mini_send_event(
        channel,
        &crate::hid_protocol::Event::MouseButtons { mask: 0 },
        0,
    )
    .await
}

async fn mini_release(
    runtime: &dyn Runtime,
    channel: &Arc<dyn DataChannel>,
    responses: &mut ResponseCursor,
    session_id: [u8; 16],
    lease_epoch: u32,
    sequence: &mut u64,
) -> Result<()> {
    let request_sequence = *sequence;
    send_hid_event(
        channel,
        crate::hid_protocol::Envelope {
            message_type: crate::hid_protocol::LIVE_INPUT_DISARM,
            session_id,
            lease_epoch,
            sequence: request_sequence,
            event: crate::hid_protocol::Event::LiveInputDisarm,
        },
    )
    .await?;
    *sequence += 1;
    loop {
        match timeout(runtime, Duration::from_secs(5), channel.poll()).await? {
            Some(DataChannelEvent::OnMessage(message)) => {
                if message.data.first() == Some(&b'{') {
                    continue;
                }
                let (response, _) = responses.decode_any_epoch(&message.data)?;
                let response_sequence = responses.take_request_sequence();
                match response {
                    EndpointResponse::LeaseState { .. }
                        if response_sequence == Some(request_sequence) =>
                    {
                        return Ok(());
                    }
                    EndpointResponse::Error { code }
                        if response_sequence == Some(request_sequence) =>
                    {
                        bail!("Mini lease release rejected: {code}");
                    }
                    _ => {}
                }
            }
            Some(DataChannelEvent::OnClose) | None => {
                bail!("Mini RPC closed before cleanup completed")
            }
            Some(_) => {}
        }
    }
}

async fn process_mini_input_events<F: FnMut(SessionEvent)>(
    hidrpc: &Arc<dyn DataChannel>,
    motion_channel: &Arc<dyn DataChannel>,
    control: &SessionControl,
    epoch: u32,
    input: &mut MiniInput,
    on_event: &mut F,
) -> Result<()> {
    use crate::input_capture::{self, InputEvent};
    control.check_input_health()?;
    if !control.input_epoch_matches(epoch) {
        bail!("input capture generation changed");
    }
    if control.take_escape() {
        if control.input_is_remote() {
            mini_send_neutral(hidrpc).await?;
            if control.input_set_local()? {
                on_event(SessionEvent::InputLocalConfirmed);
            }
            if control.input_mode() == Some(SessionMode::Cli) {
                control.cancel();
            }
        } else if input_capture::capture_supported() {
            control.request_remote(0)?;
        }
    }
    let keepalive_interval = if control.input_keyboard_active() || control.input_buttons() != 0 {
        Duration::from_millis(50)
    } else {
        // Mini has no control-channel lease heartbeat. Keep idle sessions alive.
        HEARTBEAT_INTERVAL
    };
    if input
        .last_keyboard_keepalive
        .is_none_or(|sent| sent.elapsed() >= keepalive_interval)
    {
        hidrpc
            .send(BytesMut::from(crate::mini_hid::KEEPALIVE.as_slice()))
            .await?;
        input.last_keyboard_keepalive = Some(Instant::now());
    }
    if !control.input_is_remote() {
        while let Some(event) = control.next_input_event()? {
            if matches!(event, InputEvent::RemoteReady(0)) && control.confirm_remote(epoch) {
                on_event(SessionEvent::InputRemoteConfirmed);
            }
        }
        input.motion = (0, 0);
        input.last_motion_sent = None;
        return Ok(());
    }
    for _ in 0..8 {
        let Some(event) = control.next_input_event()? else {
            break;
        };
        match event {
            InputEvent::RemoteReady(_) => {}
            InputEvent::Motion { dx, dy } => {
                input.motion.0 += dx;
                input.motion.1 += dy;
            }
            event => {
                let buttons_before = control.input_buttons();
                if matches!(event, InputEvent::MouseButton { .. }) && input.motion != (0, 0) {
                    let (dx, dy) = take_motion_chunk(&mut input.motion);
                    mini_send_event(
                        motion_channel,
                        &crate::hid_protocol::Event::Motion { dx, dy },
                        buttons_before,
                    )
                    .await?;
                    input.note_motion_sent();
                }
                if let Some(event) = control.input_update(event) {
                    mini_send_event(hidrpc, &event, control.input_buttons()).await?;
                }
            }
        }
    }
    if input.motion != (0, 0)
        && input
            .last_motion_sent
            .is_none_or(|sent| sent.elapsed() >= control.input_motion_interval())
    {
        let (dx, dy) = take_motion_chunk(&mut input.motion);
        mini_send_event(
            motion_channel,
            &crate::hid_protocol::Event::Motion { dx, dy },
            control.input_buttons(),
        )
        .await?;
        input.note_motion_sent();
    }
    Ok(())
}

async fn send_hid_event(
    channel: &Arc<dyn DataChannel>,
    event: crate::hid_protocol::Envelope,
) -> Result<()> {
    let encoded = crate::hid_protocol::encode(&event);
    channel.send(BytesMut::from(encoded.as_slice())).await?;
    Ok(())
}

#[derive(Default)]
struct LiveInput {
    arm: Option<PendingInputArm>,
    pending: Option<(u64, crate::hid_protocol::Event)>,
    report: Option<(u64, Instant)>,
    disarm: Option<(u64, Instant)>,
    disarm_requested: bool,
    rearm_requested: bool,
    motion: (i32, i32),
    last_motion_sent: Option<Instant>,
}

impl LiveInput {
    fn arm_reply(&mut self, accepted: bool) -> bool {
        let Some(arm) = self.arm.as_mut() else {
            return false;
        };
        if arm.cancelled {
            self.arm = None;
        } else if accepted {
            arm.acknowledged = true;
        } else {
            self.arm = None;
            return true;
        }
        false
    }

    fn complete_handoff(&mut self, epoch: u32, attempt: u64) -> bool {
        if self.arm.as_ref().is_some_and(|arm| {
            arm.epoch == epoch
                && arm.sequence == attempt
                && arm.acknowledged
                && arm.handoff_requested
                && !arm.cancelled
        }) {
            self.arm = None;
            return true;
        }
        false
    }

    fn credit_reply(&mut self, request_sequence: Option<u64>) -> bool {
        for pending in [&mut self.disarm, &mut self.report] {
            if pending.is_some_and(|(sent, _)| Some(sent) == request_sequence) {
                *pending = None;
                return true;
            }
        }
        false
    }

    fn cancel_pending(&mut self) -> bool {
        self.rearm_requested = false;
        self.pending = None;
        self.motion = (0, 0);
        self.last_motion_sent = None;
        let Some(arm) = self.arm.as_mut() else {
            return false;
        };
        let needs_disarm = !arm.cancelled;
        arm.cancelled = true;
        if arm.acknowledged {
            self.arm = None;
        }
        needs_disarm
    }
}

struct PendingInputArm {
    epoch: u32,
    sequence: u64,
    deadline: Instant,
    acknowledged: bool,
    handoff_requested: bool,
    cancelled: bool,
}

impl PendingInputArm {
    fn matches_reply(
        &self,
        response: &EndpointResponse,
        epoch: u32,
        request_sequence: Option<u64>,
        client_fingerprint: [u8; 32],
    ) -> bool {
        if epoch != self.epoch || request_sequence != Some(self.sequence) {
            return false;
        }
        match response {
            EndpointResponse::LeaseState {
                owner: Some(owner),
                state: 1,
            } => *owner == client_fingerprint,
            EndpointResponse::Error { .. } => true,
            _ => false,
        }
    }
}

async fn send_live_event(
    channel: &Arc<dyn DataChannel>,
    session_id: [u8; 16],
    epoch: u32,
    sequence: &mut u64,
    message_type: u64,
    event: crate::hid_protocol::Event,
) -> Result<u64> {
    let current = *sequence;
    let next = current.checked_add(1).context("input sequence exhausted")?;
    send_hid_event(
        channel,
        crate::hid_protocol::Envelope {
            message_type,
            session_id,
            lease_epoch: epoch,
            sequence: current,
            event,
        },
    )
    .await?;
    *sequence = next;
    Ok(current)
}

async fn stop_live_input<F: FnMut(SessionEvent)>(
    control: &SessionControl,
    channel: &Arc<dyn DataChannel>,
    session_id: [u8; 16],
    epoch: u32,
    sequence: &mut u64,
    live: &mut LiveInput,
    on_event: &mut F,
) -> Result<()> {
    let was_remote = control.input_set_local()?;
    let had_arm = live.arm.is_some();
    let had_report = live.report.is_some();
    if was_remote {
        on_event(SessionEvent::InputLocalConfirmed);
    }
    if live.cancel_pending() || was_remote {
        // Input reports use another ordered channel. Do not let DISARM overtake
        // the last report; wait for its completion credit before sending it on
        // the control channel.
        live.disarm_requested = true;
        send_pending_disarm(
            channel, session_id, epoch, sequence, live, was_remote, had_arm, had_report,
        )
        .await?;
    }
    Ok(())
}

async fn send_pending_disarm(
    channel: &Arc<dyn DataChannel>,
    session_id: [u8; 16],
    epoch: u32,
    sequence: &mut u64,
    live: &mut LiveInput,
    was_remote: bool,
    had_arm: bool,
    had_report: bool,
) -> Result<()> {
    if !live.disarm_requested || live.report.is_some() || live.disarm.is_some() {
        return Ok(());
    }
    live.disarm_requested = false;
    let sent = send_live_event(
        channel,
        session_id,
        epoch,
        sequence,
        crate::hid_protocol::LIVE_INPUT_DISARM,
        crate::hid_protocol::Event::LiveInputDisarm,
    )
    .await?;
    live.disarm = Some((sent, Instant::now() + Duration::from_secs(5)));
    log::debug!(
        "live input disarm sent epoch={} sequence={} was_remote={} had_arm={} had_report={}",
        epoch,
        sent,
        was_remote,
        had_arm,
        had_report
    );
    Ok(())
}

async fn process_input_events<F: FnMut(SessionEvent)>(
    runtime: &dyn Runtime,
    control_channel: &Arc<dyn DataChannel>,
    input_channel: &Arc<dyn DataChannel>,
    motion_channel: &Arc<dyn DataChannel>,
    control: &SessionControl,
    session_id: [u8; 16],
    epoch: u32,
    control_sequence: &mut u64,
    input_sequence: &mut u64,
    motion_sequence: &mut u64,
    live: &mut LiveInput,
    can_arm: bool,
    protocol_supported: bool,
    on_event: &mut F,
) -> Result<()> {
    use crate::{
        hid_protocol as hid,
        input_capture::{self, InputEvent},
    };
    control.check_input_health()?;
    ensure!(
        control.input_epoch_matches(epoch),
        "input capture lease epoch changed"
    );
    if control.take_escape() {
        if live.arm.as_ref().is_some_and(|arm| !arm.cancelled)
            || control.input_is_remote()
            || live.disarm_requested
            || live.disarm.is_some()
        {
            stop_live_input(
                control,
                control_channel,
                session_id,
                epoch,
                control_sequence,
                live,
                on_event,
            )
            .await?;
            if control.input_mode() == Some(SessionMode::Cli) {
                control.cancel();
            }
            return Ok(());
        }
        if live.rearm_requested {
            // A second press while activation is waiting for disarm/lease
            // cleanup cancels that activation without issuing another disarm.
            live.rearm_requested = false;
            log::debug!("live input rearm cancelled by Escape");
            return Ok(());
        }
        if !input_capture::capture_supported() || !protocol_supported {
            on_event(SessionEvent::Notice(
                if !input_capture::capture_supported() {
                    "Remote input unavailable. Focused Windows capture is unavailable.".into()
                } else {
                    "Remote input unavailable. Endpoint lacks correlated live input support.".into()
                },
            ));
            return Ok(());
        }
        live.rearm_requested = true;
        log::debug!(
            "live input rearm requested arm_allowed={} disarm_pending={} arm_pending={}",
            can_arm,
            live.disarm.is_some(),
            live.arm.is_some()
        );
    }
    if live.rearm_requested && can_arm && live.disarm.is_none() && live.arm.is_none() {
        live.rearm_requested = false;
        let sent = send_live_event(
            control_channel,
            session_id,
            epoch,
            control_sequence,
            hid::LIVE_INPUT_ARM,
            hid::Event::LiveInputArm,
        )
        .await?;
        live.arm = Some(PendingInputArm {
            epoch,
            sequence: sent,
            deadline: Instant::now() + Duration::from_secs(5),
            acknowledged: false,
            handoff_requested: false,
            cancelled: false,
        });
        log::debug!(
            "live input arm sent epoch={} sequence={} waiting_for_disarm={}",
            epoch,
            sent,
            live.disarm.is_some()
        );
        return Ok(());
    }
    if let Some(arm) = live.arm.as_mut()
        && arm.acknowledged
        && !arm.cancelled
        && !arm.handoff_requested
    {
        control.request_remote(arm.sequence)?;
        arm.handoff_requested = true;
    }
    if live.report.is_some() {
        return Ok(());
    }

    // Keep motion fast and coalesced. State changes retain one USB completion
    // credit so button/key cleanup stays explicit.
    for _ in 0..4 {
        let (message_type, event) = if let Some((message_type, event)) = live.pending.take() {
            (message_type, event)
        } else if live.motion != (0, 0)
            && live
                .last_motion_sent
                .is_none_or(|sent| sent.elapsed() >= control.input_motion_interval())
        {
            let (dx, dy) = take_motion_chunk(&mut live.motion);
            (hid::MOTION, hid::Event::Motion { dx, dy })
        } else {
            let Some(input) = control.next_input_event()? else {
                break;
            };
            match input {
                InputEvent::RemoteReady(attempt) => {
                    if live.complete_handoff(epoch, attempt) && control.confirm_remote(epoch) {
                        log::debug!(
                            "live input capture handoff complete epoch={} attempt={}",
                            epoch,
                            attempt
                        );
                        on_event(SessionEvent::InputRemoteConfirmed);
                    }
                    continue;
                }
                InputEvent::Motion { dx, dy } if control.input_is_remote() => {
                    live.motion.0 += dx;
                    live.motion.1 += dy;
                    continue;
                }
                _ => {
                    let Some(event) = control.input_update(input) else {
                        continue;
                    };
                    let message_type = match &event {
                        hid::Event::KeyboardState { .. } => hid::KEYBOARD_STATE,
                        hid::Event::MouseButtons { .. } => hid::MOUSE_BUTTONS,
                        hid::Event::Wheel { .. } => hid::WHEEL,
                        hid::Event::Motion { .. } => hid::MOTION,
                        _ => unreachable!(),
                    };
                    (message_type, event)
                }
            }
        };
        let is_motion = matches!(&event, hid::Event::Motion { .. });
        // Control carries lease/heartbeat transitions, input carries ordered
        // key/button/wheel state, and motion carries only relative deltas.
        let use_motion_channel = is_motion;
        let channel = if use_motion_channel {
            motion_channel
        } else {
            input_channel
        };
        let channel_sequence = if use_motion_channel {
            &mut *motion_sequence
        } else {
            &mut *input_sequence
        };
        let motion_delta = match &event {
            hid::Event::Motion { dx, dy } => Some((*dx, *dy)),
            _ => None,
        };
        let retry_event = (!is_motion).then(|| event.clone());
        let sent = match timeout(
            runtime,
            INPUT_SEND_BUDGET,
            send_live_event(
                channel,
                session_id,
                epoch,
                channel_sequence,
                message_type,
                event,
            ),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                if let Some(event) = retry_event {
                    live.pending = Some((message_type, event));
                }
                if let Some((dx, dy)) = motion_delta {
                    live.motion.0 += i32::from(dx);
                    live.motion.1 += i32::from(dy);
                }
                log::debug!("live input send backpressured; prioritizing session heartbeat");
                break;
            }
        };
        if is_motion {
            live.last_motion_sent = Some(Instant::now());
        }
        if !is_motion {
            live.report = Some((sent, Instant::now() + Duration::from_secs(5)));
            break;
        }
    }
    Ok(())
}

fn take_motion_chunk(delta: &mut (i32, i32)) -> (i16, i16) {
    let dx = delta.0.clamp(-127, 127);
    let dy = delta.1.clamp(-127, 127);
    delta.0 -= dx;
    delta.1 -= dy;
    (dx as i16, dy as i16)
}

async fn drain_legacy_heartbeat_response(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    responses: &mut ResponseCursor,
    epoch: u32,
) -> Result<()> {
    match timeout(runtime, LEGACY_HEARTBEAT_RESPONSE_TIMEOUT, control.poll()).await {
        Ok(Some(DataChannelEvent::OnMessage(message))) => {
            ensure!(
                matches!(
                    responses.decode(&message.data, epoch)?,
                    EndpointResponse::LeaseState { .. }
                ),
                "unexpected legacy heartbeat response"
            );
        }
        Ok(Some(DataChannelEvent::OnClose)) | Ok(None) => {
            bail!("scripted control channel closed")
        }
        Ok(_) | Err(_) => {}
    }
    Ok(())
}

async fn send_heartbeat(
    runtime: &dyn Runtime,
    control: &Arc<dyn DataChannel>,
    responses: &mut ResponseCursor,
    event: crate::hid_protocol::Envelope,
    epoch: u32,
    client_fingerprint: [u8; 32],
    heartbeat_ack_supported: bool,
) -> Result<()> {
    send_hid_event(control, event).await?;
    if !heartbeat_ack_supported {
        drain_legacy_heartbeat_response(runtime, control, responses, epoch).await?;
        return Ok(());
    }
    require_response(
        receive_endpoint_response(runtime, control, responses, epoch).await?,
        EndpointResponse::LeaseState {
            owner: Some(client_fingerprint),
            state: 1,
        },
    )
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
    expected_sequence: Option<u64>,
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
        if metrics.scripted_events >= target
            && expected_sequence.is_none_or(|sequence| metrics.last_event_sequence == sequence)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "scripted telemetry barrier timed out: events={} target={} last_sequence={:?}",
                metrics.scripted_events,
                target,
                expected_sequence.map(|sequence| metrics.last_event_sequence != sequence),
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
    input: &Arc<dyn DataChannel>,
    motion: &Arc<dyn DataChannel>,
    session_id: [u8; 16],
    client_fingerprint: [u8; 32],
    endpoint_fingerprint: [u8; 32],
    mode: ScriptMode,
    heartbeat_ack_supported: bool,
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
        event(
            hid_protocol::LEASE_REQUEST,
            0,
            0,
            Event::LeaseRequest {
                heartbeat_ack_supported,
            },
        ),
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
            let input_sequence = INPUT_SEQUENCE_BASE + u64::from(repeat) * 3;
            send_heartbeat(
                runtime,
                control,
                &mut responses,
                event(
                    hid_protocol::HEARTBEAT,
                    epoch,
                    control_sequence,
                    Event::Heartbeat,
                ),
                epoch,
                client_fingerprint,
                heartbeat_ack_supported,
            )
            .await?;
            send_hid_event(
                input,
                event(
                    hid_protocol::KEYBOARD_STATE,
                    epoch,
                    input_sequence,
                    Event::KeyboardState {
                        modifiers: 2,
                        keys: vec![(7, 4)],
                    },
                ),
            )
            .await?;
            send_hid_event(
                input,
                event(
                    hid_protocol::MOUSE_BUTTONS,
                    epoch,
                    input_sequence + 1,
                    Event::MouseButtons { mask: 1 },
                ),
            )
            .await?;
            send_hid_event(
                input,
                event(
                    hid_protocol::WHEEL,
                    epoch,
                    input_sequence + 2,
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

    send_heartbeat(
        runtime,
        control,
        &mut responses,
        event(hid_protocol::HEARTBEAT, epoch, 1 + offset, Event::Heartbeat),
        epoch,
        client_fingerprint,
        heartbeat_ack_supported,
    )
    .await?;
    send_timed_hid_event(
        input,
        event(
            hid_protocol::KEYBOARD_STATE,
            epoch,
            INPUT_SEQUENCE_BASE,
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
        input,
        event(
            hid_protocol::MOUSE_BUTTONS,
            epoch,
            INPUT_SEQUENCE_BASE + 1,
            Event::MouseButtons { mask: 1 },
        ),
        hid_timing.as_ref(),
    )
    .await?;
    send_timed_hid_event(
        input,
        event(
            hid_protocol::WHEEL,
            epoch,
            INPUT_SEQUENCE_BASE + 2,
            Event::Wheel {
                vertical: 1,
                horizontal: -1,
            },
        ),
        hid_timing.as_ref(),
    )
    .await?;
    wait_for_scripted_telemetry(monitor, &telemetry_baseline, 4 + u32::from(hid_bench), None)
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
    wait_for_scripted_telemetry(monitor, &telemetry_baseline, 5, None)
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
    input: &'a Arc<dyn DataChannel>,
    responses: ResponseCursor,
    sequence: u64,
    input_sequence: u64,
    heartbeat_ack_supported: bool,
}

impl<'a> TakeoverClient<'a> {
    fn new<P>(
        label: &'static str,
        runtime: &'a dyn Runtime,
        peer: &'a SignalingPeer<P>,
        heartbeat_ack_supported: bool,
    ) -> Self {
        Self {
            label,
            runtime,
            control: &peer.control,
            input: &peer.input,
            responses: ResponseCursor::new(peer.session.session_id()),
            sequence: 0,
            input_sequence: INPUT_SEQUENCE_BASE,
            heartbeat_ack_supported,
        }
    }

    async fn send(
        &mut self,
        message_type: u64,
        epoch: u32,
        event: crate::hid_protocol::Event,
    ) -> Result<()> {
        let is_input = matches!(
            message_type,
            crate::hid_protocol::KEYBOARD_STATE
                | crate::hid_protocol::MOUSE_BUTTONS
                | crate::hid_protocol::WHEEL
        );
        let channel = if is_input { self.input } else { self.control };
        let sequence = if is_input {
            self.input_sequence
        } else {
            self.sequence
        };
        send_hid_event(
            channel,
            crate::hid_protocol::Envelope {
                message_type,
                session_id: self.responses.session_id,
                lease_epoch: epoch,
                sequence,
                event,
            },
        )
        .await?;
        if is_input {
            self.input_sequence += 1;
        } else {
            self.sequence += 1;
        }
        Ok(())
    }

    async fn heartbeat(&mut self, epoch: u32) -> Result<()> {
        let request_sequence = self.sequence;
        self.send(
            crate::hid_protocol::HEARTBEAT,
            epoch,
            crate::hid_protocol::Event::Heartbeat,
        )
        .await?;
        if !self.heartbeat_ack_supported {
            drain_legacy_heartbeat_response(self.runtime, self.control, &mut self.responses, epoch)
                .await?;
            return Ok(());
        }
        loop {
            let (response, response_request_sequence) = receive_endpoint_response_with_sequence(
                self.runtime,
                self.control,
                &mut self.responses,
                epoch,
            )
            .await?;
            match response {
                EndpointResponse::LeaseState { state: 1 | 2, .. }
                    if response_request_sequence == Some(request_sequence) =>
                {
                    return Ok(());
                }
                EndpointResponse::Error { code } => {
                    bail!("client {} heartbeat got endpoint error: {code}", self.label)
                }
                _ => {}
            }
        }
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

    async fn expect_with_heartbeat(
        &mut self,
        epoch: u32,
        expected: EndpointResponse,
        deadline: Duration,
    ) -> Result<()> {
        let started = Instant::now();
        let mut heartbeat = started + HEARTBEAT_INTERVAL;
        let mut heartbeat_request_sequences = Vec::new();
        let mut heartbeat_acknowledged = !self.heartbeat_ack_supported;
        let mut received = None;
        let mut received_sequence = None;
        let request_sequence = self.sequence;
        self.send(
            crate::hid_protocol::HEARTBEAT,
            epoch,
            crate::hid_protocol::Event::Heartbeat,
        )
        .await?;
        heartbeat_request_sequences.push(request_sequence);
        let received = loop {
            if Instant::now() >= heartbeat {
                let request_sequence = self.sequence;
                self.send(
                    crate::hid_protocol::HEARTBEAT,
                    epoch,
                    crate::hid_protocol::Event::Heartbeat,
                )
                .await?;
                heartbeat_request_sequences.push(request_sequence);
                heartbeat += HEARTBEAT_INTERVAL;
            }
            if started.elapsed() >= deadline {
                bail!(
                    "client {} timed out waiting for {expected:?} at epoch {epoch}",
                    self.label
                );
            }
            match timeout(
                self.runtime,
                Duration::from_millis(100),
                self.control.poll(),
            )
            .await
            {
                Ok(Some(DataChannelEvent::OnMessage(message))) => {
                    let response = self.responses.decode(&message.data, epoch)?;
                    let response_request_sequence = self.responses.take_request_sequence();
                    if self.heartbeat_ack_supported
                        && matches!(&response, EndpointResponse::LeaseState { .. })
                        && response_request_sequence
                            .is_some_and(|sequence| heartbeat_request_sequences.contains(&sequence))
                    {
                        heartbeat_acknowledged = true;
                        heartbeat_request_sequences
                            .retain(|sequence| Some(*sequence) != response_request_sequence);
                    }
                    if response == expected {
                        received_sequence = Some(self.responses.next_sequence - 1);
                        received = Some(response);
                    } else if !matches!(&response, EndpointResponse::LeaseState { .. }) {
                        bail!(
                            "client {} expected {expected:?}, got {response:?}",
                            self.label
                        );
                    }
                    if received.is_some() && heartbeat_acknowledged {
                        break received.take().expect("received response just checked");
                    }
                }
                Ok(Some(DataChannelEvent::OnClose)) | Ok(None) => {
                    bail!("scripted control channel closed")
                }
                Ok(_) | Err(_) => {}
            }
        };
        eprintln!(
            "M1.4 TAKEOVER client={} epoch={epoch} response_sequence={} response={received:?}",
            self.label,
            received_sequence.expect("received response sequence just recorded")
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
        response_window_ms: 30_000,
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
    a.send(
        hid::LEASE_REQUEST,
        0,
        Event::LeaseRequest {
            heartbeat_ack_supported: a.heartbeat_ack_supported,
        },
    )
    .await?;
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
        None,
    )?;
    let held = monitor.fresh_snapshot()?;
    ensure!(
        held.normalized_keys == 1 && held.normalized_buttons == 1,
        "A input did not set held keys and buttons"
    );
    eprintln!("M1.4 TAKEOVER A-held epoch={a_epoch} keys=1 buttons=1");

    a.heartbeat(a_epoch).await?;
    takeover_heartbeat_processed(
        a.runtime,
        monitor,
        a_epoch,
        a.sequence - 1,
        Instant::now() + Duration::from_millis(1500),
    )
    .await?;

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
    a.expect_with_heartbeat(a_epoch, notice(rejected), Duration::from_secs(5))
        .await?;
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

    a.heartbeat(a_epoch).await?;
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
    a.expect_with_heartbeat(a_epoch, notice(timed), Duration::from_secs(5))
        .await?;
    let pending_board_ms = monitor.fresh_snapshot()?.board_timestamp_ms;
    eprintln!("M1.4 TAKEOVER pending epoch={a_epoch} board_timestamp_ms={pending_board_ms}");
    for heartbeat in 1..=39 {
        let due = requested + Duration::from_millis(750 * heartbeat);
        a.runtime
            .sleep(due.saturating_duration_since(Instant::now()))
            .await;
        a.heartbeat(a_epoch).await?;
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
    let final_sample = requested + Duration::from_millis(29_500);
    a.runtime
        .sleep(final_sample.saturating_duration_since(Instant::now()))
        .await;
    let last_heartbeat = takeover_heartbeat_processed(
        a.runtime,
        monitor,
        a_epoch,
        a.sequence - 1,
        requested + Duration::from_millis(29_700),
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
        elapsed >= Duration::from_secs(30),
        "takeover granted before the 30-second window"
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
    b.heartbeat(b_epoch).await?;
    wait_for_scripted_telemetry(monitor, &heartbeat_baseline, 1, Some(b.sequence - 1))?;
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
    wait_for_scripted_count(monitor, &before_b, 2)?;
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
    heartbeat_ack_supported: bool,
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
                TakeoverClient::new("A", &*runtime, &a, heartbeat_ack_supported),
                TakeoverClient::new("B", &*runtime, &b, heartbeat_ack_supported),
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
    heartbeat_ack_supported: bool,
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
        let mut client = TakeoverClient::new("cleanup", &*runtime, &peer, heartbeat_ack_supported);
        client
            .send(
                crate::hid_protocol::LEASE_REQUEST,
                0,
                crate::hid_protocol::Event::LeaseRequest {
                    heartbeat_ack_supported: client.heartbeat_ack_supported,
                },
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
        None,
    )?;
    match path {
        CleanupPath::Disconnect => peer.close(&*runtime).await,
        CleanupPath::HeartbeatExpiry => {
            runtime.sleep(Duration::from_millis(10_500)).await;
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
    heartbeat_ack_supported: bool,
) -> Result<SocketAddr> {
    let advertised_mode =
        crate::discovery::compatibility_mode(service).map_err(anyhow::Error::msg)?;
    ensure!(
        advertised_mode
            == if heartbeat_ack_supported {
                crate::discovery::HeartbeatMode::Acknowledged
            } else {
                crate::discovery::HeartbeatMode::Legacy
            },
        "service compatibility mode changed before signaling test"
    );
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
                            heartbeat_ack_supported,
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
    fn pending_input_arm_leaves_heartbeat_and_takeover_to_the_session() {
        let fingerprint = [7; 32];
        let epoch = 4;
        let arm = PendingInputArm {
            epoch,
            deadline: Instant::now(),
            sequence: 41,
            acknowledged: false,
            handoff_requested: false,
            cancelled: false,
        };
        let state = EndpointResponse::LeaseState {
            owner: Some(fingerprint),
            state: 1,
        };
        let notice = EndpointResponse::TakeoverNotice {
            request_id: [1; 16],
            requester: [2; 32],
            response_window_ms: 30_000,
        };
        let stale = EndpointResponse::Error {
            code: "stale_epoch".into(),
        };
        let refused = EndpointResponse::Error {
            code: "invalid_capability".into(),
        };
        assert!(!arm.matches_reply(&state, epoch, Some(42), fingerprint));
        assert!(!arm.matches_reply(&notice, epoch, None, fingerprint));
        assert!(!arm.matches_reply(&stale, epoch, None, fingerprint));
        assert!(!arm.matches_reply(&state, epoch + 1, None, fingerprint));
        assert!(!arm.matches_reply(&state, epoch, None, fingerprint));
        assert!(!arm.matches_reply(&refused, epoch, None, fingerprint));
        assert!(arm.matches_reply(&state, epoch, Some(41), fingerprint));
        assert!(arm.matches_reply(&refused, epoch, Some(41), fingerprint));
        assert!(!arm.matches_reply(&state, epoch + 1, Some(41), fingerprint));
    }

    fn live_attempt(epoch: u32, sequence: u64) -> PendingInputArm {
        PendingInputArm {
            epoch,
            sequence,
            deadline: Instant::now(),
            acknowledged: false,
            handoff_requested: false,
            cancelled: false,
        }
    }

    #[test]
    fn pending_escape_cancels_success_and_rejection_without_new_arm() {
        for accepted in [false, true] {
            let mut live = LiveInput {
                arm: Some(live_attempt(3, 9)),
                ..Default::default()
            };
            assert!(live.cancel_pending());
            assert!(!live.cancel_pending());
            assert!(!live.arm_reply(accepted));
            assert!(live.arm.is_none());
            assert!(!live.rearm_requested);
            assert!(!live.complete_handoff(3, 9));
        }
        let mut live = LiveInput {
            arm: Some(live_attempt(3, 9)),
            ..Default::default()
        };
        assert!(!live.arm_reply(true));
        live.arm.as_mut().unwrap().handoff_requested = true;
        assert!(!live.complete_handoff(4, 9));
        assert!(!live.complete_handoff(3, 8));
        assert!(live.cancel_pending());
        assert!(!live.complete_handoff(3, 9));
    }

    #[test]
    fn cancellation_clears_drag_motion_and_pending_rearm_but_keeps_report_credit() {
        let mut live = LiveInput {
            report: Some((17, Instant::now())),
            motion: (610, -360),
            rearm_requested: true,
            ..Default::default()
        };
        assert!(!live.cancel_pending());
        assert!(live.report.is_some());
        assert_eq!(live.motion, (0, 0));
        assert!(!live.rearm_requested);
    }

    #[test]
    fn heartbeat_replies_cannot_mint_report_or_disarm_credit() {
        let mut live = LiveInput {
            report: Some((17, Instant::now())),
            ..Default::default()
        };
        assert!(!live.credit_reply(None));
        assert!(!live.credit_reply(Some(18)));
        assert!(live.report.is_some());
        assert!(live.credit_reply(Some(17)));
        assert!(live.report.is_none());
        live.disarm = Some((19, Instant::now()));
        assert!(!live.credit_reply(Some(17)));
        assert!(live.credit_reply(Some(19)));
        assert!(live.disarm.is_none());
    }

    #[test]
    fn motion_preserves_displacement_across_ordered_report_credits() {
        for samples in [125, 2000] {
            let mut drag = (samples * 5, -samples * 3);
            let mut endpoint = (0, 0);
            while drag != (0, 0) {
                let (dx, dy) = take_motion_chunk(&mut drag);
                assert!((-127..=127).contains(&dx) && (-127..=127).contains(&dy));
                endpoint.0 += i32::from(dx);
                endpoint.1 += i32::from(dy);
            }
            assert_eq!(endpoint, (samples * 5, -samples * 3));
        }
    }

    #[test]
    fn rollover_keeps_oldest_six_and_promotes_held_key() {
        use crate::hid_protocol::Event::KeyboardState;
        let mut keyboard = HeldKeyboard::default();
        for usage in (4..10).rev() {
            assert!(keyboard.update(7, usage, true).is_some());
        }
        assert!(keyboard.update(7, 10, true).is_none());
        assert!(keyboard.update(7, 10, false).is_none());
        assert!(keyboard.update(7, 10, true).is_none());
        assert!(keyboard.update(7, 4, true).is_none());
        assert!(keyboard.update(7, 0xe4, true).is_some());
        assert_eq!(
            keyboard.update(7, 7, false),
            Some(KeyboardState {
                modifiers: 16,
                keys: [4, 5, 6, 8, 9, 10]
                    .into_iter()
                    .map(|usage| (7, usage))
                    .collect(),
            })
        );
        for usage in [9, 8, 6, 5, 4, 10] {
            assert!(keyboard.update(7, usage, false).is_some());
        }
        assert_eq!(
            keyboard.update(7, 0xe4, false),
            Some(KeyboardState {
                modifiers: 0,
                keys: vec![]
            })
        );
    }

    #[test]
    #[ignore = "requires C fixture from firmware/test_live_input.py --fixture"]
    fn firmware_rule_rearm_and_capture_handoff() -> Result<()> {
        use crate::{
            hid_protocol::{self as hid, Event},
            input_capture::{InputEvent, TestCapture},
        };
        let fixture = std::fs::read(
            std::env::var_os("KVMCTL_LIVE_FIXTURE").context("set KVMCTL_LIVE_FIXTURE")?,
        )?;
        let mut frames = fixture.as_slice();
        let mut arm_reply = |attempt: u64| -> Result<(EndpointResponse, u32)> {
            let size = u16::from_le_bytes(frames[..2].try_into()?) as usize;
            let (frame, rest) = frames[2..].split_at(size);
            frames = rest;
            let mut expected_session = [0; 16];
            expected_session[0] = 1;
            let envelope = hid::decode(frame).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            assert_eq!(envelope.session_id, expected_session);
            assert_eq!(
                ResponseCursor::request_sequence(&envelope.event),
                Some(attempt)
            );
            Ok((
                ResponseCursor::response(envelope.event)?,
                envelope.lease_epoch,
            ))
        };
        let mut owner = [0; 32];
        owner[0] = 1;
        let mut live = LiveInput::default();
        let mut capture = TestCapture::new();
        let mut keyboard = HeldKeyboard::default();
        let mut destinations = Vec::new();
        for attempt in [2, 6] {
            assert!(!capture.key(0xa2, 0x1d, false, true));
            assert!(!capture.key(0xa0, 0x2a, false, true));
            assert!(capture.key(0x79, 0x44, false, true));
            assert!(capture.take_escape());
            assert!(capture.key(0x79, 0x44, false, false));
            live.arm = Some(live_attempt(1, attempt));
            let (response, epoch) = arm_reply(attempt)?;
            assert!(live.arm.as_ref().unwrap().matches_reply(
                &response,
                epoch,
                Some(attempt),
                owner
            ));
            assert!(!live.arm_reply(true));
            capture.request_remote(attempt);
            live.arm.as_mut().unwrap().handoff_requested = true;
            assert_eq!(capture.complete_handoff(), None);
            assert!(!capture.is_remote());
            assert!(!capture.key(0xa0, 0x2a, false, false));
            assert_eq!(capture.complete_handoff(), None);
            assert!(!capture.key(0xa2, 0x1d, false, false));
            assert!(live.complete_handoff(epoch, capture.complete_handoff().unwrap()));
            assert!(capture.is_remote());
            destinations.push(SessionEvent::InputRemoteConfirmed);
            if attempt == 2 {
                assert!(capture.key(0x41, 0x1e, false, true));
                let Some(InputEvent::Key { page, usage, down }) = capture.next_event() else {
                    panic!("missing held A")
                };
                assert_eq!(
                    keyboard.update(page, usage, down),
                    Some(Event::KeyboardState {
                        modifiers: 0,
                        keys: vec![(7, 4)]
                    })
                );
                assert!(capture.key(0xa2, 0x1d, false, true));
                assert!(capture.key(0xa0, 0x2a, false, true));
                assert!(capture.key(0x79, 0x44, false, true));
                assert!(capture.take_escape());
                capture.local();
                assert!(!capture.is_remote());
                destinations.push(SessionEvent::InputLocalConfirmed);
                assert!(capture.key(0x41, 0x1e, false, true));
                assert!(capture.key(0x41, 0x1e, false, false));
                assert!(capture.key(0x79, 0x44, false, false));
                assert!(capture.key(0xa0, 0x2a, false, false));
                assert!(capture.key(0xa2, 0x1d, false, false));
                live.disarm = Some((4, Instant::now()));
                assert_eq!(
                    arm_reply(4)?,
                    (
                        EndpointResponse::LeaseState {
                            owner: Some(owner),
                            state: 1
                        },
                        1
                    )
                );
                assert!(live.credit_reply(Some(4)));
                assert!(live.disarm.is_none());
            }
        }
        assert!(matches!(
            destinations.as_slice(),
            [
                SessionEvent::InputRemoteConfirmed,
                SessionEvent::InputLocalConfirmed,
                SessionEvent::InputRemoteConfirmed,
            ]
        ));
        assert!(frames.is_empty());
        Ok(())
    }

    #[test]
    fn failed_capture_is_terminal_at_the_reconnect_boundary() {
        let control = SessionControl::new();
        let (capture, events) = crate::input_capture::Capture::exited_for_test();
        *control.input.lock().unwrap() = Some(InputState {
            capture,
            events,
            motion_rate_hz: crate::settings::DEFAULT_MOTION_RATE_HZ,
            mode: SessionMode::Tray,
            remote: true,
            epoch: Some(3),
            keyboard: HeldKeyboard::default(),
            buttons: 1,
        });
        assert!(control.check_input_health().is_err());
        assert!(control.next_input_event().is_err());
        assert!(control.input_set_local().unwrap());
        assert!(!control.input_set_local().unwrap());
        control.input_fail_closed();
        assert!(control.check_input_health().is_err());
    }

    #[test]
    fn recovery_cancellation_does_not_wait_for_discovery() {
        let control = SessionControl::new();
        let worker_control = control.clone();
        let (release, blocked) = std::sync::mpsc::channel();
        let result = resolve_for_recovery(&control, move || {
            worker_control.cancel();
            let _ = blocked.recv_timeout(Duration::from_secs(5));
            Ok(None)
        });
        assert!(result.is_none());
        release.send(()).unwrap();
    }

    #[test]
    fn lease_expiry_reenters_recovery_without_masking_ownership_loss() {
        assert!(matches!(
            session_error_exit("lease_expired", 7),
            SessionExit::Lost(reason) if reason.contains("lease expired")
        ));
        assert!(matches!(
            session_error_exit("stale_epoch", 7),
            SessionExit::OwnershipLost(reason) if reason.contains("stale_epoch")
        ));
    }

    #[test]
    fn rtc_error_class_ignores_lower_log_levels() {
        assert_eq!(rtc_error_class(log::Level::Info, "dtls failed"), None);
        assert_eq!(
            rtc_error_class(log::Level::Debug, "certificate failed"),
            None
        );
        assert_eq!(rtc_error_class(log::Level::Warn, "dtls failed"), Some(3));
        assert_eq!(rtc_error_class(log::Level::Error, "other failure"), Some(4));
    }

    #[test]
    fn session_control_only_accepts_decision_for_pending_request() {
        let control = SessionControl::new();
        let request_id = [7; 16];
        control.start_takeover(request_id, 30_000);
        control.respond_to_takeover([8; 16], true);
        assert_eq!(control.pending_takeover(), Some(request_id));
        control.respond_to_takeover(request_id, false);
        assert_eq!(control.take_takeover_decision(request_id), Some(false));
        assert_eq!(control.pending_takeover(), None);
    }

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
                request_sequence: Some(42),
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
        assert_eq!(cursor.take_request_sequence(), Some(42));
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
                response_window_ms: 30_000,
            },
        };
        assert_eq!(
            ResponseCursor::new(session_id)
                .decode(&crate::hid_protocol::encode(&notice), 4)
                .unwrap(),
            EndpointResponse::TakeoverNotice {
                request_id: [1; 16],
                requester: [2; 32],
                response_window_ms: 30_000,
            }
        );

        let error = crate::hid_protocol::Envelope {
            message_type: crate::hid_protocol::ERROR,
            session_id,
            lease_epoch: 0,
            sequence: 0,
            event: crate::hid_protocol::Event::Error {
                request_sequence: None,
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
