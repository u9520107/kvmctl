use crate::credentials::ClientCredential;
use crate::protocol::{self, Challenge, Transcript};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, Error as TlsError, SignatureScheme,
};
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::parse_x509_certificate;

const MAX_WSS_MESSAGE: usize = 16 * 1024;
const MAX_HTTP_HEADERS: usize = 4096;
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const CHALLENGE_DOMAIN: &[u8] = b"kvmctl/challenge/v1";
const TRANSCRIPT_DOMAIN: &[u8] = b"kvmctl/transcript/v1";

fn read_control_payload(stream: &mut impl Read, length: usize) -> Result<Vec<u8>, String> {
    if length > 125 {
        return Err("oversized WebSocket control frame".into());
    }
    let mut payload = vec![0u8; length];
    stream
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    Ok(payload)
}

#[derive(Debug)]
struct PinnedVerifier {
    expected_host: String,
    expected_spki_hash: [u8; 32],
}

fn validation_error(message: impl Into<String>) -> TlsError {
    TlsError::General(message.into())
}

fn validate_pinned_certificate(
    der: &[u8],
    expected_host: &str,
    expected_spki_hash: &[u8; 32],
    now: u64,
) -> Result<(), String> {
    let (remaining, certificate) =
        parse_x509_certificate(der).map_err(|_| "TLS certificate is not valid DER".to_owned())?;
    if !remaining.is_empty() {
        return Err("TLS certificate has trailing DER data".into());
    }
    if certificate.tbs_certificate.subject != certificate.tbs_certificate.issuer {
        return Err("TLS certificate is not self-issued".into());
    }
    let san = certificate
        .subject_alternative_name()
        .map_err(|_| "TLS certificate has malformed SAN".to_owned())?
        .ok_or("TLS certificate is missing SAN")?;
    if !san.value.general_names.iter().any(
        |name| matches!(name, GeneralName::DNSName(dns) if dns.eq_ignore_ascii_case(expected_host)),
    ) {
        return Err("TLS certificate SAN does not match the USB-pinned endpoint name".into());
    }
    let not_before = certificate.validity().not_before.timestamp();
    let not_after = certificate.validity().not_after.timestamp();
    let now = i64::try_from(now).map_err(|_| "invalid TLS verification time")?;
    if now < not_before || now > not_after {
        return Err("TLS certificate is outside its validity period".into());
    }
    if crate::sha256(certificate.tbs_certificate.subject_pki.raw)? != *expected_spki_hash {
        return Err("TLS certificate SPKI does not match the USB pin".into());
    }
    certificate
        .verify_signature(None)
        .map_err(|_| "TLS self-signature is invalid".to_owned())?;
    Ok(())
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        if !intermediates.is_empty()
            || !server_name
                .to_str()
                .eq_ignore_ascii_case(&self.expected_host)
        {
            return Err(validation_error("unexpected TLS chain or server name"));
        }
        validate_pinned_certificate(
            end_entity.as_ref(),
            &self.expected_host,
            &self.expected_spki_hash,
            now.as_secs(),
        )
        .map_err(validation_error)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let provider = rustls::crypto::ring::default_provider();
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let provider = rustls::crypto::ring::default_provider();
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        let provider = rustls::crypto::ring::default_provider();
        provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn head(&mut self, expected_major: u8) -> Result<u64, String> {
        let first = *self.data.get(self.pos).ok_or("truncated WSS CBOR")?;
        self.pos += 1;
        if first >> 5 != expected_major {
            return Err("unexpected WSS CBOR type".into());
        }
        let additional = first & 31;
        let count = match additional {
            value @ 0..=23 => value as u64,
            24 => self.read_number(1)?,
            25 => self.read_number(2)?,
            26 => self.read_number(4)?,
            27 => self.read_number(8)?,
            _ => return Err("indefinite or reserved WSS CBOR item".into()),
        };
        if (additional == 24 && count < 24)
            || (additional == 25 && count <= 0xff)
            || (additional == 26 && count <= 0xffff)
            || (additional == 27 && count <= 0xffff_ffff)
        {
            return Err("non-canonical WSS CBOR integer".into());
        }
        Ok(count)
    }

    fn read_number(&mut self, count: usize) -> Result<u64, String> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or("WSS CBOR length overflow")?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or("truncated WSS CBOR number")?;
        self.pos = end;
        Ok(bytes
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
    }

    fn uint(&mut self) -> Result<u64, String> {
        self.head(0)
    }

    fn expect_uint(&mut self, expected: u64) -> Result<(), String> {
        if self.uint()? == expected {
            Ok(())
        } else {
            Err("unexpected WSS CBOR map key/value".into())
        }
    }

    fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], String> {
        let length = usize::try_from(self.head(2)?).map_err(|_| "WSS CBOR byte length overflow")?;
        if length > maximum {
            return Err("oversized WSS CBOR byte string".into());
        }
        let end = self
            .pos
            .checked_add(length)
            .ok_or("WSS CBOR length overflow")?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or("truncated WSS CBOR byte string")?;
        self.pos = end;
        Ok(bytes)
    }

    fn text(&mut self, maximum: usize) -> Result<&'a str, String> {
        let length = usize::try_from(self.head(3)?).map_err(|_| "WSS CBOR text length overflow")?;
        if length > maximum {
            return Err("oversized WSS CBOR text".into());
        }
        let end = self
            .pos
            .checked_add(length)
            .ok_or("WSS CBOR length overflow")?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or("truncated WSS CBOR text")?;
        self.pos = end;
        std::str::from_utf8(bytes).map_err(|_| "WSS CBOR text is not UTF-8".into())
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let bytes = self.bytes(N)?;
        bytes
            .try_into()
            .map_err(|_| "invalid WSS CBOR byte length".into())
    }

    fn done(&self) -> bool {
        self.pos == self.data.len()
    }
}

fn head(out: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => out.push(prefix | value as u8),
        24..=0xff => out.extend_from_slice(&[prefix | 24, value as u8]),
        0x100..=0xffff => out.extend_from_slice(&[prefix | 25, (value >> 8) as u8, value as u8]),
        0x1_0000..=0xffff_ffff => out.extend_from_slice(&[
            prefix | 26,
            (value >> 24) as u8,
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ]),
        _ => out.extend_from_slice(&[
            prefix | 27,
            (value >> 56) as u8,
            (value >> 48) as u8,
            (value >> 40) as u8,
            (value >> 32) as u8,
            (value >> 24) as u8,
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ]),
    }
}

fn put_uint(out: &mut Vec<u8>, value: u64) {
    head(out, 0, value);
}
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    head(out, 2, bytes.len() as u64);
    out.extend_from_slice(bytes);
}
fn put_text(out: &mut Vec<u8>, text: &str) {
    head(out, 3, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

struct Envelope<'a> {
    message_type: u64,
    session_id: [u8; 16],
    sequence: u64,
    payload: &'a [u8],
}

fn parse_envelope(data: &[u8]) -> Result<Envelope<'_>, String> {
    let mut decoder = Decoder::new(data);
    if decoder.head(5)? != 7 {
        return Err("invalid WSS envelope field count".into());
    }
    decoder.expect_uint(0)?;
    decoder.expect_uint(1)?;
    decoder.expect_uint(1)?;
    decoder.expect_uint(0)?;
    decoder.expect_uint(2)?;
    let message_type = decoder.uint()?;
    decoder.expect_uint(3)?;
    let session_id = decoder.fixed::<16>()?;
    decoder.expect_uint(4)?;
    decoder.expect_uint(0)?;
    decoder.expect_uint(5)?;
    let sequence = decoder.uint()?;
    decoder.expect_uint(6)?;
    Ok(Envelope {
        message_type,
        session_id,
        sequence,
        payload: &data[decoder.pos..],
    })
}

fn encode_envelope(
    message_type: u64,
    session_id: &[u8; 16],
    sequence: u64,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 64);
    head(&mut out, 5, 7);
    put_uint(&mut out, 0);
    put_uint(&mut out, 1);
    put_uint(&mut out, 1);
    put_uint(&mut out, 0);
    put_uint(&mut out, 2);
    put_uint(&mut out, message_type);
    put_uint(&mut out, 3);
    put_bytes(&mut out, session_id);
    put_uint(&mut out, 4);
    put_uint(&mut out, 0);
    put_uint(&mut out, 5);
    put_uint(&mut out, sequence);
    put_uint(&mut out, 6);
    out.extend_from_slice(payload);
    out
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let mut bytes = [0u8; N];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            N as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(format!("BCryptGenRandom failed: 0x{:08x}", status as u32));
    }
    Ok(bytes)
}

fn websocket_accept(key: &str) -> String {
    let mut digest = Sha1::new();
    digest.update(key.as_bytes());
    digest.update(WEBSOCKET_GUID.as_bytes());
    BASE64.encode(digest.finalize())
}

pub struct OfferExchange {
    pub answer: String,
    pub client_transcript_proof: Vec<u8>,
}

fn verify_endpoint_transcript_proof(
    endpoint_spki: &[u8],
    transcript: &Transcript<'_>,
    proof: Option<&[u8]>,
) -> Result<(), String> {
    let proof = proof
        .filter(|proof| !proof.is_empty())
        .ok_or("missing endpoint transcript proof")?;
    ClientCredential::verify(
        endpoint_spki,
        TRANSCRIPT_DOMAIN,
        &protocol::encode_transcript(transcript)?,
        proof,
    )
    .map_err(|error| format!("invalid endpoint transcript proof: {error}"))
}

pub struct Session {
    stream: rustls::StreamOwned<ClientConnection, TcpStream>,
    session_id: [u8; 16],
    transmit_sequence: u64,
    receive_sequence: u64,
    credential_fingerprint: [u8; 32],
    endpoint_fingerprint: [u8; 32],
    endpoint_spki: Vec<u8>,
    client_nonce: [u8; 32],
    device_nonce: [u8; 32],
    tls_spki_hash: [u8; 32],
}

impl Session {
    pub fn session_id(&self) -> [u8; 16] {
        self.session_id
    }

    pub fn connect(
        address: SocketAddr,
        certificate_hostname: &str,
        endpoint_fingerprint: [u8; 32],
        tls_spki_hash: [u8; 32],
        credential: &ClientCredential,
    ) -> Result<Self, String> {
        Self::connect_inner(
            address,
            certificate_hostname,
            endpoint_fingerprint,
            tls_spki_hash,
            credential,
            false,
        )
    }

    pub fn expect_invalid_client_proof(
        address: SocketAddr,
        certificate_hostname: &str,
        endpoint_fingerprint: [u8; 32],
        tls_spki_hash: [u8; 32],
        credential: &ClientCredential,
    ) -> Result<(), String> {
        match Self::connect_inner(
            address,
            certificate_hostname,
            endpoint_fingerprint,
            tls_spki_hash,
            credential,
            true,
        ) {
            Err(error) if error == "endpoint rejected signaling: unauthorized" => Ok(()),
            Err(error) => Err(format!(
                "invalid client challenge proof was rejected unexpectedly: {error}"
            )),
            Ok(mut session) => {
                session.close();
                Err("endpoint accepted an invalid client challenge proof".into())
            }
        }
    }

    fn connect_inner(
        address: SocketAddr,
        certificate_hostname: &str,
        endpoint_fingerprint: [u8; 32],
        tls_spki_hash: [u8; 32],
        credential: &ClientCredential,
        invalid_client_proof: bool,
    ) -> Result<Self, String> {
        let expected_host = crate::discovery::endpoint_hostname(&endpoint_fingerprint);
        if !certificate_hostname
            .trim_end_matches('.')
            .eq_ignore_ascii_case(&expected_host)
        {
            return Err("discovered host does not match the USB endpoint identity".into());
        }
        let verifier = Arc::new(PinnedVerifier {
            expected_host: expected_host.clone(),
            expected_spki_hash: tls_spki_hash,
        });
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS12])
            .map_err(|error| format!("TLS provider rejected required TLS 1.2: {error}"))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let server_name = ServerName::try_from(expected_host.clone())
            .map_err(|_| "invalid endpoint TLS host name")?;
        let socket = TcpStream::connect_timeout(&address, Duration::from_secs(5))
            .map_err(|error| format!("cannot connect to discovered WSS endpoint: {error}"))?;
        socket
            .set_nodelay(true)
            .map_err(|error| error.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        let connection = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|error| format!("cannot create pinned TLS connection: {error}"))?;
        let mut stream = rustls::StreamOwned::new(connection, socket);
        let websocket_key = BASE64.encode(random::<16>()?);
        let request = format!(
            "GET /v1/ws HTTP/1.1\r\nHost: {expected_host}:443\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {websocket_key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: kvmctl.v1\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| format!("WSS Upgrade failed: {error}"))?;
        let response = read_http_headers(&mut stream)?;
        validate_upgrade(&response, &websocket_key)?;

        let session_id = crate::random_id()?;
        let client_nonce = random::<32>()?;
        let mut session = Self {
            stream,
            session_id,
            transmit_sequence: 0,
            receive_sequence: 0,
            credential_fingerprint: credential.fingerprint,
            endpoint_fingerprint,
            endpoint_spki: Vec::new(),
            client_nonce,
            device_nonce: [0; 32],
            tls_spki_hash,
        };
        let mut hello = Vec::new();
        head(&mut hello, 5, 2);
        put_uint(&mut hello, 0);
        put_bytes(&mut hello, &credential.fingerprint);
        put_uint(&mut hello, 1);
        put_bytes(&mut hello, &client_nonce);
        session.send_envelope(0x01, &hello)?;
        let envelope = session
            .receive_envelope(0x02)
            .map_err(|error| with_receive_stage("endpoint challenge receive failed", error))?;
        let mut payload = Decoder::new(&envelope.payload);
        if payload.head(5)? != 6 {
            return Err("invalid signed challenge field count".into());
        }
        payload.expect_uint(0)?;
        let endpoint_fp = payload.fixed::<32>()?;
        payload.expect_uint(1)?;
        session.device_nonce = payload.fixed::<32>()?;
        payload.expect_uint(2)?;
        if payload.head(4)? != 3
            || payload.text(32)? != "control"
            || payload.text(32)? != "input"
            || payload.text(32)? != "motion"
        {
            return Err("endpoint capability list is invalid".into());
        }
        payload.expect_uint(3)?;
        let peer_tls_hash = payload.fixed::<32>()?;
        payload.expect_uint(4)?;
        let device_signature = payload.bytes(72)?.to_vec();
        payload.expect_uint(5)?;
        session.endpoint_spki = payload.bytes(128)?.to_vec();
        if !payload.done() {
            return Err("trailing signed challenge data".into());
        }
        if endpoint_fp != endpoint_fingerprint || peer_tls_hash != tls_spki_hash {
            return Err("WSS challenge identity does not match USB status".into());
        }
        if session.endpoint_spki.len() != 91
            || crate::sha256(&session.endpoint_spki)? != endpoint_fingerprint
        {
            return Err(
                "signed challenge endpoint key does not match the USB identity fingerprint".into(),
            );
        }
        let challenge = Challenge {
            session_id: &session.session_id,
            client_fingerprint: &session.credential_fingerprint,
            endpoint_fingerprint: &session.endpoint_fingerprint,
            client_nonce: &session.client_nonce,
            device_nonce: &session.device_nonce,
            tls_spki_hash: &session.tls_spki_hash,
        };
        let encoded = protocol::encode_challenge(&challenge)?;
        ClientCredential::verify(
            &session.endpoint_spki,
            CHALLENGE_DOMAIN,
            &encoded,
            &device_signature,
        )?;
        let mut proof = protocol::sign_client_proof(credential, &challenge)?;
        if invalid_client_proof {
            *proof.last_mut().ok_or("empty client challenge proof")? ^= 1;
        }
        session.send_signature(0x03, &proof)?;
        if invalid_client_proof {
            let rejection = session.receive_any_envelope().map_err(|error| {
                with_receive_stage("client-proof rejection receive failed", error)
            })?;
            if rejection.message_type == 0x0a {
                let error = parse_error(&rejection.payload);
                if error == "endpoint rejected signaling: unauthorized" {
                    return Err(error);
                }
            }
            return Err("endpoint did not reject the invalid client challenge proof".into());
        }
        Ok(session)
    }

    pub fn send_offer(
        &mut self,
        credential: &ClientCredential,
        offer: &str,
    ) -> Result<OfferExchange, String> {
        if offer.is_empty()
            || offer.len() > 8192
            || !offer.is_ascii()
            || !offer.starts_with("v=0\r\n")
        {
            return Err("WebRTC offer is malformed or exceeds the WSS signaling limit".into());
        }
        let client_dtls_fingerprint =
            crate::transport::parse_sha256_fingerprint(offer).map_err(|error| error.to_string())?;
        let mut payload = Vec::with_capacity(offer.len() + 8);
        head(&mut payload, 5, 1);
        put_uint(&mut payload, 0);
        put_text(&mut payload, offer);
        self.send_envelope(0x04, &payload)?;
        let answer_envelope = self
            .receive_envelope(0x05)
            .map_err(|error| with_receive_stage("SDP answer receive failed", error))?;
        let mut answer_payload = Decoder::new(&answer_envelope.payload);
        if answer_payload.head(5)? != 1 {
            return Err("invalid SDP answer field count".into());
        }
        answer_payload.expect_uint(0)?;
        let answer = answer_payload.text(8192)?.to_owned();
        if !answer_payload.done() || !answer.starts_with("v=0\r\n") || !answer.is_ascii() {
            return Err("SDP answer is malformed".into());
        }
        let endpoint_dtls_fingerprint = crate::transport::parse_sha256_fingerprint(&answer)
            .map_err(|error| error.to_string())?;
        let signed_transcript = self.receive_envelope(0x08).map_err(|error| {
            with_receive_stage("endpoint transcript proof receive failed", error)
        })?;
        let mut proof_payload = Decoder::new(&signed_transcript.payload);
        if proof_payload.head(5)? != 1 {
            return Err("invalid endpoint transcript field count".into());
        }
        proof_payload.expect_uint(0)?;
        let endpoint_proof = proof_payload.bytes(72)?.to_vec();
        if !proof_payload.done() {
            return Err("trailing endpoint transcript proof".into());
        }
        let transcript = Transcript {
            session_id: &self.session_id,
            client_fingerprint: &self.credential_fingerprint,
            endpoint_fingerprint: &self.endpoint_fingerprint,
            client_nonce: &self.client_nonce,
            device_nonce: &self.device_nonce,
            tls_spki_hash: &self.tls_spki_hash,
            offer: offer.as_bytes(),
            answer: answer.as_bytes(),
            client_dtls_fingerprint: &client_dtls_fingerprint,
            endpoint_dtls_fingerprint: &endpoint_dtls_fingerprint,
        };
        verify_endpoint_transcript_proof(&self.endpoint_spki, &transcript, Some(&endpoint_proof))?;
        let client_proof = protocol::sign_transcript(credential, &transcript)?;
        Ok(OfferExchange {
            answer,
            client_transcript_proof: client_proof,
        })
    }

    pub fn send_transcript_proof(&mut self, proof: &[u8]) -> Result<(), String> {
        self.send_transcript_proof_frame(proof)?;
        self.receive_ready()
    }

    pub fn send_transcript_proof_frame(&mut self, proof: &[u8]) -> Result<(), String> {
        if !(8..=72).contains(&proof.len()) {
            return Err("invalid transcript proof length".into());
        }
        self.send_signature(0x08, proof)
    }

    fn receive_ready(&mut self) -> Result<(), String> {
        let envelope = self
            .receive_envelope(0x09)
            .map_err(|error| with_receive_stage("ready response receive failed", error))?;
        let mut payload = Decoder::new(&envelope.payload);
        if payload.head(5)? != 0 || !payload.done() {
            return Err("invalid ready response".into());
        }
        Ok(())
    }

    pub fn close(&mut self) {
        let _ = self.send_ws_frame(0x8, &[]);
        let _ = self.stream.flush();
    }

    pub fn probe_oversized_reassembly(&mut self) -> Result<(), String> {
        let first = vec![0u8; MAX_WSS_MESSAGE];
        self.send_ws_fragment(0x2, false, &first)?;
        self.send_ws_fragment(0x0, true, &[0])?;
        let rejection = self.receive_rejection()?;
        if rejection == "endpoint rejected signaling: resource_exhausted" {
            Ok(())
        } else {
            Err(format!(
                "oversized fragmented WSS message returned unexpected rejection: {rejection}"
            ))
        }
    }

    fn send_signature(&mut self, message_type: u64, signature: &[u8]) -> Result<(), String> {
        let mut payload = Vec::new();
        head(&mut payload, 5, 1);
        put_uint(&mut payload, 0);
        put_bytes(&mut payload, signature);
        self.send_envelope(message_type, &payload)
    }

    fn send_envelope(&mut self, message_type: u64, payload: &[u8]) -> Result<(), String> {
        let sequence = self.transmit_sequence;
        let bytes = encode_envelope(message_type, &self.session_id, sequence, payload);
        self.send_ws_frame(0x2, &bytes)?;
        self.transmit_sequence += 1;
        Ok(())
    }

    fn receive_any_envelope(&mut self) -> Result<OwnedEnvelope, String> {
        let bytes = self.read_ws_message()?;
        let envelope = parse_envelope(&bytes)?;
        if envelope.session_id != self.session_id || envelope.sequence != self.receive_sequence {
            return Err("WSS session ID or sequence was replayed".into());
        }
        self.receive_sequence += 1;
        Ok(OwnedEnvelope {
            message_type: envelope.message_type,
            payload: envelope.payload.to_vec(),
        })
    }

    fn send_ws_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        if payload.len() > MAX_WSS_MESSAGE {
            return Err("WSS message exceeds 16 KiB".into());
        }
        self.send_ws_fragment(opcode, true, payload)
    }

    fn send_ws_fragment(
        &mut self,
        opcode: u8,
        final_fragment: bool,
        payload: &[u8],
    ) -> Result<(), String> {
        if payload.len() > MAX_WSS_MESSAGE {
            return Err("WSS frame exceeds 16 KiB".into());
        }
        let mask = random::<4>()?;
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push((if final_fragment { 0x80 } else { 0 }) | opcode);
        match payload.len() {
            len @ 0..=125 => frame.push(0x80 | len as u8),
            len @ 126..=65535 => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(len as u16).to_be_bytes());
            }
            len => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(len as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index & 3]),
        );
        self.stream
            .write_all(&frame)
            .map_err(|error| format!("WSS write failed: {error}"))?;
        self.stream
            .flush()
            .map_err(|error| format!("WSS flush failed: {error}"))
    }

    fn read_ws_message(&mut self) -> Result<Vec<u8>, String> {
        let mut message = Vec::with_capacity(2048);
        let mut fragmented = false;
        loop {
            let mut header = [0u8; 2];
            self.stream
                .read_exact(&mut header)
                .map_err(|error| format!("WSS read failed: {error}"))?;
            let fin = header[0] & 0x80 != 0;
            if header[0] & 0x70 != 0 {
                return Err("WSS frame used unsupported extensions".into());
            }
            let opcode = header[0] & 0x0f;
            let masked = header[1] & 0x80 != 0;
            if masked {
                return Err("server sent a masked WebSocket frame".into());
            }
            let marker = header[1] & 0x7f;
            let length = match marker {
                126 => {
                    let mut bytes = [0u8; 2];
                    self.stream
                        .read_exact(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    u16::from_be_bytes(bytes) as usize
                }
                127 => {
                    let mut bytes = [0u8; 8];
                    self.stream
                        .read_exact(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    let length = u64::from_be_bytes(bytes);
                    if length >> 63 != 0 {
                        return Err("invalid WebSocket 64-bit length".into());
                    }
                    usize::try_from(length).map_err(|_| "WebSocket frame length overflow")?
                }
                value => value as usize,
            };
            if length > MAX_WSS_MESSAGE || message.len() > MAX_WSS_MESSAGE - length {
                return Err("WSS reassembled message exceeds 16 KiB".into());
            }
            if opcode >= 8 && (!fin || length > 125) {
                return Err("invalid WebSocket control frame".into());
            }
            match opcode {
                0 if !fragmented => return Err("unexpected WebSocket continuation frame".into()),
                1 => return Err("text signaling frames are not accepted".into()),
                2 if !message.is_empty() || fragmented => {
                    return Err("nested WebSocket data frame".into());
                }
                2 => fragmented = !fin,
                0 => fragmented = !fin,
                8 => return Err("WSS peer closed the signaling session".into()),
                9 => {
                    let payload = read_control_payload(&mut self.stream, length)?;
                    self.send_ws_frame(0xA, &payload)?;
                    continue;
                }
                10 => {
                    let _ = read_control_payload(&mut self.stream, length)?;
                    continue;
                }
                _ => return Err("unsupported WebSocket frame opcode".into()),
            }
            let start = message.len();
            message.resize(start + length, 0);
            self.stream
                .read_exact(&mut message[start..])
                .map_err(|error| format!("WSS frame read failed: {error}"))?;
            if fin {
                return Ok(message);
            }
        }
    }
}

struct OwnedEnvelope {
    message_type: u64,
    payload: Vec<u8>,
}

fn parse_error(payload: &[u8]) -> String {
    let parsed = (|| {
        let mut decoder = Decoder::new(payload);
        if decoder.head(5)? != 1 {
            return Err("invalid signaling error".to_owned());
        }
        decoder.expect_uint(0)?;
        let code = decoder.text(64)?;
        if !decoder.done() {
            return Err("trailing signaling error".into());
        }
        Ok(code.to_owned())
    })();
    format!(
        "endpoint rejected signaling: {}",
        parsed.unwrap_or_else(|error| error)
    )
}

fn with_receive_stage(stage: &str, error: String) -> String {
    if error.starts_with("endpoint rejected signaling:") {
        error
    } else {
        format!("{stage}: {error}")
    }
}

fn read_http_headers(stream: &mut impl Read) -> Result<String, String> {
    let mut response = Vec::with_capacity(512);
    while response.len() < MAX_HTTP_HEADERS {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .map_err(|error| format!("WSS Upgrade response failed: {error}"))?;
        response.push(byte[0]);
        if response.ends_with(b"\r\n\r\n") {
            return String::from_utf8(response)
                .map_err(|_| "WSS Upgrade response is not ASCII".into());
        }
    }
    Err("WSS Upgrade headers exceed 4 KiB".into())
}

fn validate_upgrade(response: &str, websocket_key: &str) -> Result<(), String> {
    let mut lines = response.split("\r\n");
    let status = lines.next().ok_or("missing WSS Upgrade status")?;
    if !(status.starts_with("HTTP/1.1 101 ") || status == "HTTP/1.1 101") {
        return Err(format!("WSS Upgrade was rejected: {status}"));
    }
    let mut headers = std::collections::BTreeMap::<String, Vec<String>>::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or("malformed WSS Upgrade header")?;
        headers
            .entry(name.trim().to_ascii_lowercase())
            .or_default()
            .push(value.trim().to_owned());
    }
    let one = |name: &str| -> Result<&str, String> {
        let values = headers
            .get(name)
            .ok_or_else(|| format!("missing WSS Upgrade header {name}"))?;
        if values.len() != 1 {
            return Err(format!("duplicate WSS Upgrade header {name}"));
        }
        Ok(values[0].as_str())
    };
    if !one("upgrade")?.eq_ignore_ascii_case("websocket")
        || !one("connection")?
            .split(',')
            .any(|value| value.trim().eq_ignore_ascii_case("upgrade"))
        || one("sec-websocket-protocol")? != "kvmctl.v1"
        || one("sec-websocket-accept")? != websocket_accept(websocket_key)
        || headers.contains_key("sec-websocket-extensions")
    {
        return Err("WSS Upgrade response failed protocol validation".into());
    }
    Ok(())
}

fn parse_error_response(envelope: OwnedEnvelope) -> Result<String, String> {
    if envelope.message_type != 0x0a {
        return Err("invalid transcript proof was not rejected".into());
    }
    Ok(parse_error(&envelope.payload))
}

impl Session {
    fn receive_envelope(&mut self, expected_type: u64) -> Result<OwnedEnvelope, String> {
        let envelope = self.receive_any_envelope()?;
        if envelope.message_type == 0x0a {
            return Err(parse_error(&envelope.payload));
        }
        if envelope.message_type != expected_type {
            return Err(format!(
                "unexpected WSS message type {}",
                envelope.message_type
            ));
        }
        Ok(envelope)
    }

    pub fn receive_rejection(&mut self) -> Result<String, String> {
        parse_error_response(self.receive_any_envelope()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_transcript_requires_a_valid_proof_for_this_session() {
        let endpoint = ClientCredential::generate().unwrap();
        let transcript = Transcript {
            session_id: &[1; 16],
            client_fingerprint: &[2; 32],
            endpoint_fingerprint: &endpoint.fingerprint,
            client_nonce: &[3; 32],
            device_nonce: &[4; 32],
            tls_spki_hash: &[5; 32],
            offer: b"offer",
            answer: b"answer",
            client_dtls_fingerprint: &[6; 32],
            endpoint_dtls_fingerprint: &[7; 32],
        };
        let valid = protocol::sign_transcript(&endpoint, &transcript).unwrap();
        verify_endpoint_transcript_proof(&endpoint.spki, &transcript, Some(&valid)).unwrap();
        assert_eq!(
            verify_endpoint_transcript_proof(&endpoint.spki, &transcript, None).unwrap_err(),
            "missing endpoint transcript proof"
        );
        assert_eq!(
            verify_endpoint_transcript_proof(&endpoint.spki, &transcript, Some(&[])).unwrap_err(),
            "missing endpoint transcript proof"
        );

        let mut tampered = valid;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(
            verify_endpoint_transcript_proof(&endpoint.spki, &transcript, Some(&tampered))
                .unwrap_err()
                .starts_with("invalid endpoint transcript proof:")
        );

        let previous_session = Transcript {
            session_id: &[9; 16],
            ..transcript
        };
        let replayed = protocol::sign_transcript(&endpoint, &previous_session).unwrap();
        verify_endpoint_transcript_proof(&endpoint.spki, &previous_session, Some(&replayed))
            .unwrap();
        assert!(
            verify_endpoint_transcript_proof(&endpoint.spki, &transcript, Some(&replayed))
                .unwrap_err()
                .starts_with("invalid endpoint transcript proof:")
        );
    }

    #[test]
    fn control_payload_is_consumed_before_the_next_frame() {
        let mut stream = std::io::Cursor::new(b"pong\x82\x01\xa0".as_slice());
        assert_eq!(read_control_payload(&mut stream, 4).unwrap(), b"pong");
        let mut following_frame = [0u8; 3];
        stream.read_exact(&mut following_frame).unwrap();
        assert_eq!(following_frame, [0x82, 0x01, 0xa0]);
        assert!(read_control_payload(&mut stream, 126).is_err());
    }

    fn certificate(host: &str, not_after_year: i32) -> (Vec<u8>, [u8; 32]) {
        let mut params = rcgen::CertificateParams::new(vec![host.to_owned()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2026, 1, 1);
        params.not_after = rcgen::date_time_ymd(not_after_year, 12, 31);
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let der = cert.der().to_vec();
        let (_, parsed) = parse_x509_certificate(&der).unwrap();
        let spki = crate::sha256(parsed.tbs_certificate.subject_pki.raw).unwrap();
        (der, spki)
    }

    #[test]
    fn websocket_accept_matches_rfc_example() {
        assert_eq!(
            websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn signaling_envelope_checks_canonical_fields_and_payload() {
        let session = [0x33; 16];
        let payload = [0xa0];
        let encoded = encode_envelope(9, &session, 7, &payload);
        let envelope = parse_envelope(&encoded).unwrap();
        assert_eq!(envelope.message_type, 9);
        assert_eq!(envelope.session_id, session);
        assert_eq!(envelope.sequence, 7);
        assert_eq!(envelope.payload, payload);
        let mut malformed = encoded;
        malformed[1] = 0x18;
        assert!(parse_envelope(&malformed).is_err());
    }

    #[test]
    fn pinned_certificate_requires_pin_san_validity_and_self_signature() {
        let host = "kvmctl-0011223344556677.local";
        let (der, spki) = certificate(host, 2036);
        validate_pinned_certificate(&der, host, &spki, 1_800_000_000).unwrap();
        assert!(validate_pinned_certificate(&der, host, &[0; 32], 1_800_000_000).is_err());
        assert!(validate_pinned_certificate(&der, "other.local", &spki, 1_800_000_000).is_err());
        assert!(validate_pinned_certificate(&der, host, &spki, 2_200_000_000).is_err());
        let mut tampered = der;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(validate_pinned_certificate(&tampered, host, &spki, 1_800_000_000).is_err());
    }
}
