use crate::credentials::ClientCredential;

#[cfg(test)]
const ENDPOINT_CHALLENGE_DOMAIN: &[u8] = b"kvmctl/challenge/v1";
const CLIENT_PROOF_DOMAIN: &[u8] = b"kvmctl/client-proof/v1";
const TRANSCRIPT_DOMAIN: &[u8] = b"kvmctl/transcript/v1";
const CAPABILITIES: [&str; 3] = ["control", "input", "motion"];

pub struct Challenge<'a> {
    pub session_id: &'a [u8; 16],
    pub client_fingerprint: &'a [u8; 32],
    pub endpoint_fingerprint: &'a [u8; 32],
    pub client_nonce: &'a [u8; 32],
    pub device_nonce: &'a [u8; 32],
    pub tls_spki_hash: &'a [u8; 32],
}

pub struct Transcript<'a> {
    pub session_id: &'a [u8; 16],
    pub client_fingerprint: &'a [u8; 32],
    pub endpoint_fingerprint: &'a [u8; 32],
    pub client_nonce: &'a [u8; 32],
    pub device_nonce: &'a [u8; 32],
    pub tls_spki_hash: &'a [u8; 32],
    pub offer: &'a [u8],
    pub answer: &'a [u8],
    pub client_dtls_fingerprint: &'a [u8; 32],
    pub endpoint_dtls_fingerprint: &'a [u8; 32],
}

pub fn sign_client_proof(
    credential: &ClientCredential,
    challenge: &Challenge<'_>,
) -> Result<Vec<u8>, String> {
    credential.sign(CLIENT_PROOF_DOMAIN, &encode_challenge(challenge)?)
}

pub fn sign_transcript(
    credential: &ClientCredential,
    transcript: &Transcript<'_>,
) -> Result<Vec<u8>, String> {
    credential.sign(TRANSCRIPT_DOMAIN, &encode_transcript(transcript)?)
}

pub fn encode_challenge(challenge: &Challenge<'_>) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(256);
    map(&mut out, 9);
    uint(&mut out, 0);
    uint(&mut out, 1);
    uint(&mut out, 1);
    uint(&mut out, 0);
    uint(&mut out, 2);
    bytes(&mut out, challenge.session_id);
    uint(&mut out, 3);
    bytes(&mut out, challenge.client_fingerprint);
    uint(&mut out, 4);
    bytes(&mut out, challenge.endpoint_fingerprint);
    uint(&mut out, 5);
    bytes(&mut out, challenge.client_nonce);
    uint(&mut out, 6);
    bytes(&mut out, challenge.device_nonce);
    uint(&mut out, 7);
    capabilities(&mut out)?;
    uint(&mut out, 8);
    bytes(&mut out, challenge.tls_spki_hash);
    Ok(out)
}

pub fn encode_transcript(transcript: &Transcript<'_>) -> Result<Vec<u8>, String> {
    let capabilities = &CAPABILITIES;
    let mut out = Vec::with_capacity(512);
    map(&mut out, 13);
    uint(&mut out, 0);
    uint(&mut out, 1);
    uint(&mut out, 1);
    uint(&mut out, 0);
    uint(&mut out, 2);
    bytes(&mut out, transcript.session_id);
    uint(&mut out, 3);
    bytes(&mut out, transcript.client_fingerprint);
    uint(&mut out, 4);
    bytes(&mut out, transcript.endpoint_fingerprint);
    uint(&mut out, 5);
    bytes(&mut out, transcript.client_nonce);
    uint(&mut out, 6);
    bytes(&mut out, transcript.device_nonce);
    uint(&mut out, 7);
    capability_values(&mut out, capabilities)?;
    uint(&mut out, 8);
    bytes(&mut out, transcript.tls_spki_hash);
    uint(&mut out, 9);
    bytes(&mut out, &crate::sha256(transcript.offer)?);
    uint(&mut out, 10);
    bytes(&mut out, &crate::sha256(transcript.answer)?);
    uint(&mut out, 11);
    bytes(&mut out, transcript.client_dtls_fingerprint);
    uint(&mut out, 12);
    bytes(&mut out, transcript.endpoint_dtls_fingerprint);
    Ok(out)
}

fn capabilities(out: &mut Vec<u8>) -> Result<(), String> {
    capability_values(out, &CAPABILITIES)
}

fn capability_values(out: &mut Vec<u8>, values: &[&str]) -> Result<(), String> {
    if values.is_empty() || values.len() > 8 || values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("capabilities must be sorted, unique, and bounded".into());
    }
    head(out, 4, values.len() as u64);
    for value in values {
        if value.is_empty()
            || value.len() > 32
            || !value.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            return Err("invalid capability identifier".into());
        }
        head(out, 3, value.len() as u64);
        out.extend_from_slice(value.as_bytes());
    }
    Ok(())
}

fn map(out: &mut Vec<u8>, len: u64) {
    head(out, 5, len);
}
fn uint(out: &mut Vec<u8>, value: u64) {
    head(out, 0, value);
}
fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    head(out, 2, value.len() as u64);
    out.extend_from_slice(value);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_hex(name: &str) -> String {
        include_str!("../../firmware/main/protocol_vectors.h")
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split('"').nth(1))
            .expect("shared firmware vector")
            .to_owned()
    }

    fn shared_string(name: &str) -> String {
        include_str!("../../firmware/main/protocol_vectors.h")
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split('"').nth(1))
            .expect("shared firmware string vector")
            .to_owned()
    }

    fn sample() -> (Challenge<'static>, Transcript<'static>) {
        static SESSION: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        static CLIENT_FP: [u8; 32] = [0x11; 32];
        static ENDPOINT_FP: [u8; 32] = [0x22; 32];
        static CLIENT_NONCE: [u8; 32] = [0x33; 32];
        static DEVICE_NONCE: [u8; 32] = [0x44; 32];
        static TLS_HASH: [u8; 32] = [0x55; 32];
        static OFFER_FP: [u8; 32] = [0; 32];
        static ANSWER_FP: [u8; 32] = [0; 32];
        let challenge = Challenge {
            session_id: &SESSION,
            client_fingerprint: &CLIENT_FP,
            endpoint_fingerprint: &ENDPOINT_FP,
            client_nonce: &CLIENT_NONCE,
            device_nonce: &DEVICE_NONCE,
            tls_spki_hash: &TLS_HASH,
        };
        let transcript = Transcript {
            session_id: &SESSION,
            client_fingerprint: &CLIENT_FP,
            endpoint_fingerprint: &ENDPOINT_FP,
            client_nonce: &CLIENT_NONCE,
            device_nonce: &DEVICE_NONCE,
            tls_spki_hash: &TLS_HASH,
            offer: b"offer",
            answer: b"answer",
            client_dtls_fingerprint: &OFFER_FP,
            endpoint_dtls_fingerprint: &ANSWER_FP,
        };
        (challenge, transcript)
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn challenge_and_transcript_match_shared_c_vectors() {
        let (challenge, transcript) = sample();
        assert_eq!(
            hex(&encode_challenge(&challenge).unwrap()),
            shared_hex("#define KVMCTL_M13_CHALLENGE_HEX ")
        );
        assert_eq!(
            hex(&encode_transcript(&transcript).unwrap()),
            shared_hex("#define KVMCTL_M13_TRANSCRIPT_HEX ")
        );
    }

    #[test]
    fn endpoint_challenge_signature_cannot_be_reflected_as_client_proof() {
        let endpoint_domain = shared_string("#define KVMCTL_M13_CHALLENGE_DOMAIN ");
        let client_domain = shared_string("#define KVMCTL_M13_CLIENT_PROOF_DOMAIN ");
        assert_eq!(ENDPOINT_CHALLENGE_DOMAIN, endpoint_domain.as_bytes());
        assert_eq!(CLIENT_PROOF_DOMAIN, client_domain.as_bytes());
        assert_ne!(ENDPOINT_CHALLENGE_DOMAIN, CLIENT_PROOF_DOMAIN);
        let endpoint = ClientCredential::generate().unwrap();
        let (sample, _) = sample();
        let challenge = Challenge {
            client_fingerprint: &endpoint.fingerprint,
            ..sample
        };
        let encoded = encode_challenge(&challenge).unwrap();
        let endpoint_signature = endpoint.sign(ENDPOINT_CHALLENGE_DOMAIN, &encoded).unwrap();
        assert!(
            ClientCredential::verify(
                &endpoint.spki,
                CLIENT_PROOF_DOMAIN,
                &encoded,
                &endpoint_signature,
            )
            .is_err()
        );
        let client_proof = sign_client_proof(&endpoint, &challenge).unwrap();
        ClientCredential::verify(&endpoint.spki, CLIENT_PROOF_DOMAIN, &encoded, &client_proof)
            .unwrap();
    }

    #[test]
    fn transcript_signatures_bind_offer_and_answer() {
        let (_, transcript) = sample();
        let credential = ClientCredential::generate().unwrap();
        let signature = sign_transcript(&credential, &transcript).unwrap();
        ClientCredential::verify(
            &credential.spki,
            TRANSCRIPT_DOMAIN,
            &encode_transcript(&transcript).unwrap(),
            &signature,
        )
        .unwrap();
        assert!(
            ClientCredential::verify(
                &credential.spki,
                TRANSCRIPT_DOMAIN,
                &encode_transcript(&Transcript {
                    offer: b"changed",
                    ..transcript
                })
                .unwrap(),
                &signature
            )
            .is_err()
        );
    }
}
