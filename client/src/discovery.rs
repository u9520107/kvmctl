use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_REQUEST_VERSION1, DNS_RECORDW, DNS_SERVICE_BROWSE_REQUEST,
    DNS_SERVICE_BROWSE_REQUEST_0, DNS_SERVICE_CANCEL, DNS_SERVICE_INSTANCE,
    DNS_SERVICE_RESOLVE_REQUEST, DNS_TYPE_PTR, DnsServiceBrowse, DnsServiceBrowseCancel,
    DnsServiceFreeInstance, DnsServiceResolve, DnsServiceResolveCancel,
};

const REQUEST_PENDING: i32 = 9506;
const BROWSE_WINDOW: Duration = Duration::from_secs(5);
const RESOLVE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct Service {
    pub instance: String,
    pub hostname: String,
    pub port: u16,
    pub ipv4: Option<Ipv4Addr>,
    pub properties: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeartbeatMode {
    Legacy,
    Acknowledged,
}

const SUPPORTED_PROTOCOL_VERSION: u64 = 1;

pub fn compatibility_mode(service: &Service) -> Result<HeartbeatMode, String> {
    let protocol_version = service
        .properties
        .get("v")
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                format!(
                    "Firmware {} advertises an invalid transport protocol version `{value}`. Upgrade firmware before connecting.",
                    firmware_label(service)
                )
            })
        })
        .transpose()?;
    if let Some(version) = protocol_version
        && version != SUPPORTED_PROTOCOL_VERSION
    {
        return Err(format!(
            "Firmware {} requires transport protocol v{version}; this client supports v{SUPPORTED_PROTOCOL_VERSION}. Upgrade firmware before connecting.",
            firmware_label(service)
        ));
    }
    Ok(
        if service
            .properties
            .get("hb")
            .is_some_and(|value| value == "1")
        {
            HeartbeatMode::Acknowledged
        } else {
            HeartbeatMode::Legacy
        },
    )
}

fn firmware_label(service: &Service) -> String {
    service
        .properties
        .get("fw")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| "unknown firmware".to_owned())
}

enum BrowseEvent {
    Instance(String),
    Error(u32),
}

#[derive(Clone)]
enum QuerySender {
    Browse(Sender<BrowseEvent>),
    Resolve(Sender<Result<Service, String>>),
}

static QUERY_SENDERS: Mutex<BTreeMap<usize, QuerySender>> = Mutex::new(BTreeMap::new());
static NEXT_QUERY: AtomicUsize = AtomicUsize::new(1);

struct QueryRegistration(usize);

impl QueryRegistration {
    fn new(sender: QuerySender) -> Result<Self, String> {
        let id = NEXT_QUERY
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "DNS-SD query identifiers exhausted")?;
        QUERY_SENDERS.lock().unwrap().insert(id, sender);
        Ok(Self(id))
    }

    fn context(&self) -> *mut c_void {
        // Windows only echoes this token. No callback dereferences an address
        // owned by the caller, including callbacks after cancellation/timeout.
        self.0 as *mut c_void
    }
}

impl Drop for QueryRegistration {
    fn drop(&mut self) {
        QUERY_SENDERS.lock().unwrap().remove(&self.0);
    }
}

fn query_sender(context: *const c_void) -> Option<QuerySender> {
    QUERY_SENDERS.lock().ok()?.get(&(context as usize)).cloned()
}

fn wide_string(value: *const u16) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut length = 0usize;
    while length < 512 && unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, length) })
}

unsafe extern "system" fn browse_callback(
    status: u32,
    context: *const c_void,
    records: *const DNS_RECORDW,
) {
    let Some(QuerySender::Browse(sender)) = query_sender(context) else {
        if !records.is_null() {
            unsafe { DnsRecordListFree(records.cast_mut(), 1) };
        }
        return;
    };
    if status != 0 {
        let _ = sender.send(BrowseEvent::Error(status));
    }
    let mut current = records.cast_mut();
    while !current.is_null() {
        let record = unsafe { &*current };
        if record.wType == DNS_TYPE_PTR {
            let instance = unsafe { record.Data.PTR.pNameHost };
            let name = wide_string(instance);
            if !name.is_empty() {
                let _ = sender.send(BrowseEvent::Instance(name));
            }
        }
        current = record.pNext;
    }
    if !records.is_null() {
        unsafe { DnsRecordListFree(records.cast_mut(), 1) };
    }
}

unsafe extern "system" fn resolve_callback(
    status: u32,
    context: *const c_void,
    instance: *const DNS_SERVICE_INSTANCE,
) {
    let Some(QuerySender::Resolve(sender)) = query_sender(context) else {
        if !instance.is_null() {
            unsafe { DnsServiceFreeInstance(instance) };
        }
        return;
    };
    if status != 0 || instance.is_null() {
        let _ = sender.send(Err(format!("DNS-SD resolve failed (status {status})")));
        if !instance.is_null() {
            unsafe { DnsServiceFreeInstance(instance) };
        }
        return;
    }

    let raw = unsafe { &*instance };
    let hostname = wide_string(raw.pszHostName);
    let instance_name = wide_string(raw.pszInstanceName);
    let property_count = (raw.dwPropertyCount as usize).min(32);
    let mut properties = BTreeMap::new();
    if property_count != 0 && !raw.keys.is_null() && !raw.values.is_null() {
        let keys = unsafe { std::slice::from_raw_parts(raw.keys, property_count) };
        let values = unsafe { std::slice::from_raw_parts(raw.values, property_count) };
        for (key, value) in keys.iter().zip(values) {
            let key = wide_string(*key);
            let value = wide_string(*value);
            if !key.is_empty() && key.len() <= 64 && value.len() <= 256 {
                properties.insert(key, value);
            }
        }
    }
    let ipv4 = if raw.ip4Address.is_null() {
        None
    } else {
        Some(ipv4_from_dns_api_value(unsafe { *raw.ip4Address }))
    };
    let _ = sender.send(Ok(Service {
        instance: instance_name,
        hostname,
        port: raw.wPort,
        ipv4,
        properties,
    }));
    unsafe { DnsServiceFreeInstance(instance) };
}

fn browse_instances() -> Result<Vec<String>, String> {
    let query: Vec<u16> = "_kvmctl._tcp.local\0".encode_utf16().collect();
    let (sender, receiver) = mpsc::channel();
    let registration = QueryRegistration::new(QuerySender::Browse(sender))?;
    let mut cancel = DNS_SERVICE_CANCEL::default();
    let request = DNS_SERVICE_BROWSE_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1,
        InterfaceIndex: 0,
        QueryName: query.as_ptr().cast_mut(),
        Anonymous: DNS_SERVICE_BROWSE_REQUEST_0 {
            pBrowseCallback: Some(browse_callback),
        },
        pQueryContext: registration.context(),
    };
    let status = unsafe { DnsServiceBrowse(&request, &mut cancel) };
    if status != REQUEST_PENDING {
        return Err(format!("Windows DNS-SD browse failed (status {status})"));
    }
    let deadline = Instant::now() + BROWSE_WINDOW;
    let mut instances = BTreeSet::new();
    let mut callback_error = None;
    while Instant::now() < deadline {
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(BrowseEvent::Instance(name)) => {
                if name.len() <= 255 {
                    instances.insert(name);
                }
            }
            Ok(BrowseEvent::Error(status)) => callback_error = Some(status),
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let cancel_status = unsafe { DnsServiceBrowseCancel(&cancel) };
    if cancel_status != 0 {
        return Err(format!(
            "Windows DNS-SD browse cancellation failed (status {cancel_status})"
        ));
    }
    if instances.is_empty()
        && let Some(status) = callback_error
    {
        return Err(format!("Windows DNS-SD callback failed (status {status})"));
    }
    Ok(instances.into_iter().collect())
}

fn ipv4_from_dns_api_value(value: u32) -> Ipv4Addr {
    Ipv4Addr::from(value.to_ne_bytes())
}

fn resolve(instance_name: &str) -> Result<Service, String> {
    let query: Vec<u16> = format!("{instance_name}\0").encode_utf16().collect();
    let (sender, receiver): (
        Sender<Result<Service, String>>,
        Receiver<Result<Service, String>>,
    ) = mpsc::channel();
    let registration = QueryRegistration::new(QuerySender::Resolve(sender))?;
    let mut cancel = DNS_SERVICE_CANCEL::default();
    let request = DNS_SERVICE_RESOLVE_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1,
        InterfaceIndex: 0,
        QueryName: query.as_ptr().cast_mut(),
        pResolveCompletionCallback: Some(resolve_callback),
        pQueryContext: registration.context(),
    };
    let status = unsafe { DnsServiceResolve(&request, &mut cancel) };
    if status != REQUEST_PENDING {
        return Err(format!("Windows DNS-SD resolve failed (status {status})"));
    }
    let result = receiver.recv_timeout(RESOLVE_WINDOW);
    if result.is_err() {
        let cancel_status = unsafe { DnsServiceResolveCancel(&cancel) };
        if cancel_status != 0 {
            return Err(format!(
                "Windows DNS-SD resolve cancellation failed (status {cancel_status})"
            ));
        }
    }
    result.map_err(|_| "DNS-SD service details did not arrive within 2 seconds".to_owned())?
}

pub fn browse() -> Result<Vec<Service>, String> {
    let instances = browse_instances()?;
    let mut services = Vec::new();
    for instance in instances.into_iter().take(16) {
        match resolve(&instance) {
            Ok(service) => services.push(service),
            Err(error) => eprintln!("Discovery diagnostic: {instance}: {error}"),
        }
    }
    Ok(services)
}

pub(crate) fn discover_target(fingerprint: &[u8; 32]) -> Result<Option<Service>, String> {
    let mut matches = browse()
        .map_err(|error| {
            format!("DNS-SD browse failed: {error}; pass an IPv4 address to use manual-IP fallback")
        })?
        .into_iter()
        .filter(|service| matches_endpoint(service, fingerprint));
    let service = matches.next();
    if matches.next().is_some() {
        return Err("multiple DNS-SD services claim this saved target identity".into());
    }
    Ok(service)
}

pub fn socket_address(service: &Service) -> Result<SocketAddr, String> {
    if !(1..=65535).contains(&service.port) {
        return Err("discovered service has invalid port".into());
    }
    if let Some(address) = service.ipv4 {
        return Ok(SocketAddr::V4(SocketAddrV4::new(address, service.port)));
    }
    Ok((service.hostname.as_str(), service.port)
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve discovered host: {error}"))?
        .next()
        .ok_or("discovered host did not resolve")?)
}

pub fn endpoint_hostname(fingerprint: &[u8; 32]) -> String {
    let suffix = fingerprint[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("kvmctl-{suffix}.local")
}

pub fn matches_endpoint(service: &Service, fingerprint: &[u8; 32]) -> bool {
    let advertised = service.properties.get("fp");
    let expected = fingerprint
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    service.port == 443
        && service
            .hostname
            .trim_end_matches('.')
            .eq_ignore_ascii_case(&endpoint_hostname(fingerprint))
        && advertised.is_some_and(|value| value.eq_ignore_ascii_case(&expected))
        && service
            .properties
            .get("path")
            .is_some_and(|value| value == "/v1/ws")
}

#[link(name = "dnsapi")]
unsafe extern "system" {
    fn DnsRecordListFree(records: *mut DNS_RECORDW, free_type: i32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callbacks_after_query_timeout_do_not_dereference_dropped_context() {
        let (sender, receiver) = mpsc::channel();
        let query = QueryRegistration::new(QuerySender::Browse(sender)).unwrap();
        let context = query.context();
        drop(query);
        unsafe { browse_callback(1223, context, std::ptr::null()) };
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));

        let (sender, receiver) = mpsc::channel();
        let query = QueryRegistration::new(QuerySender::Resolve(sender)).unwrap();
        let context = query.context();
        drop(query);
        unsafe { resolve_callback(1223, context, std::ptr::null()) };
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn in_flight_callback_retains_sender_after_query_unregisters() {
        let (sender, receiver) = mpsc::channel();
        let query = QueryRegistration::new(QuerySender::Browse(sender)).unwrap();
        let Some(QuerySender::Browse(in_flight)) = query_sender(query.context()) else {
            panic!("registered query missing");
        };
        drop(query);
        in_flight.send(BrowseEvent::Error(1223)).unwrap();
        assert!(matches!(receiver.recv().unwrap(), BrowseEvent::Error(1223)));
    }

    #[test]
    fn discovery_txt_is_only_accepted_when_it_matches_usb_identity() {
        let fingerprint = [0x12; 32];
        let service = Service {
            instance: "KVMCTL._kvmctl._tcp.local".into(),
            hostname: endpoint_hostname(&fingerprint),
            port: 443,
            ipv4: Some(Ipv4Addr::new(192, 168, 50, 42)),
            properties: BTreeMap::from([
                (
                    "fp".into(),
                    fingerprint
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                ),
                ("path".into(), "/v1/ws".into()),
                ("v".into(), "1".into()),
            ]),
        };
        assert!(matches_endpoint(&service, &fingerprint));
        assert_eq!(compatibility_mode(&service), Ok(HeartbeatMode::Legacy));
        let mut capable = service.clone();
        capable.properties.insert("hb".into(), "1".into());
        assert_eq!(
            compatibility_mode(&capable),
            Ok(HeartbeatMode::Acknowledged)
        );
        let mut incompatible = service.clone();
        incompatible.properties.insert("v".into(), "2".into());
        incompatible.properties.insert("fw".into(), "2.0.0".into());
        assert!(matches_endpoint(&incompatible, &fingerprint));
        let error = compatibility_mode(&incompatible).unwrap_err();
        assert!(error.contains("2.0.0"));
        assert!(error.contains("Upgrade firmware"));
        let mut forged = service.clone();
        forged.properties.insert("fp".into(), "00".repeat(32));
        assert!(!matches_endpoint(&forged, &fingerprint));
        let mut wrong_host = service.clone();
        wrong_host.hostname = "attacker.local".into();
        assert!(!matches_endpoint(&wrong_host, &fingerprint));
        let mut wrong_port = service;
        wrong_port.port = 80;
        assert!(!matches_endpoint(&wrong_port, &fingerprint));
    }

    #[test]
    fn dns_service_ipv4_uses_api_byte_order() {
        assert_eq!(
            ipv4_from_dns_api_value(u32::from_ne_bytes([192, 168, 50, 42])),
            Ipv4Addr::new(192, 168, 50, 42)
        );
    }
}
