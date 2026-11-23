use crate::{Secret, sha256};
use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;

const PRIVATE_MAGIC: u32 = 0x3253_4345;
const PUBLIC_MAGIC: u32 = 0x3153_4345;
const KEY_BLOB_BYTES: usize = 104;
const SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];
const ECDSA_P256: [u16; 11] = [69, 67, 68, 83, 65, 95, 80, 50, 53, 54, 0];
const PRIVATE_BLOB: [u16; 15] = [69, 67, 67, 80, 82, 73, 86, 65, 84, 69, 66, 76, 79, 66, 0];
const PUBLIC_BLOB: [u16; 14] = [69, 67, 67, 80, 85, 66, 76, 73, 67, 66, 76, 79, 66, 0];

#[repr(C)]
struct DataBlob {
    size: u32,
    data: *mut u8,
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptOpenAlgorithmProvider(
        algorithm: *mut *mut c_void,
        id: *const u16,
        implementation: *const u16,
        flags: u32,
    ) -> i32;
    fn BCryptCloseAlgorithmProvider(algorithm: *mut c_void, flags: u32) -> i32;
    fn BCryptGenerateKeyPair(
        algorithm: *mut c_void,
        key: *mut *mut c_void,
        length: u32,
        flags: u32,
    ) -> i32;
    fn BCryptFinalizeKeyPair(key: *mut c_void, flags: u32) -> i32;
    fn BCryptExportKey(
        key: *mut c_void,
        export_key: *mut c_void,
        blob_type: *const u16,
        output: *mut u8,
        output_size: u32,
        result_size: *mut u32,
        flags: u32,
    ) -> i32;
    fn BCryptImportKeyPair(
        algorithm: *mut c_void,
        import_key: *mut c_void,
        blob_type: *const u16,
        key: *mut *mut c_void,
        input: *mut u8,
        input_size: u32,
        flags: u32,
    ) -> i32;
    fn BCryptSignHash(
        key: *mut c_void,
        padding: *mut c_void,
        hash: *mut u8,
        hash_size: u32,
        output: *mut u8,
        output_size: u32,
        result_size: *mut u32,
        flags: u32,
    ) -> i32;
    fn BCryptVerifySignature(
        key: *mut c_void,
        padding: *mut c_void,
        hash: *mut u8,
        hash_size: u32,
        signature: *mut u8,
        signature_size: u32,
        flags: u32,
    ) -> i32;
    fn BCryptDestroyKey(key: *mut c_void) -> i32;
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        input: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

pub struct ClientCredential {
    private_blob: Secret,
    pub spki: [u8; 91],
    pub fingerprint: [u8; 32],
}

impl ClientCredential {
    pub fn load_existing(takeover_peer: bool) -> Result<Self, String> {
        let path = credential_path(takeover_peer)?;
        Self::load_from(&path)
    }

    fn load_from(path: &std::path::Path) -> Result<Self, String> {
        let protected =
            std::fs::read(path).map_err(|_| "no DPAPI-protected client credential exists")?;
        let private_blob = Secret::new(unprotect(&protected)?);
        Self::from_private_blob(private_blob)
    }

    pub fn load_or_create(takeover_peer: bool) -> Result<Self, String> {
        let path = credential_path(takeover_peer)?;
        if path.exists() {
            return Self::load_from(&path);
        }
        let credential = Self::generate()?;
        Self::store_to(&path, &credential)?;
        Ok(credential)
    }

    fn store_to(path: &std::path::Path, credential: &Self) -> Result<(), String> {
        let protected = Secret::new(protect(credential.private_blob.as_slice())?);
        let parent = path.parent().ok_or("invalid client key path")?;
        std::fs::create_dir_all(parent).map_err(|_| "could not create client key directory")?;
        let nonce = crate::random_id()?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let temporary = path.with_extension(format!("dpapi.{nonce}.tmp"));
        let mut temporary_created = false;
        let result = (|| {
            {
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                    .map_err(|_| "could not save protected client key")?;
                temporary_created = true;
                output
                    .write_all(protected.as_slice())
                    .map_err(|_| "could not save protected client key")?;
                output
                    .sync_all()
                    .map_err(|_| "could not flush protected client key")?;
            }
            std::fs::rename(&temporary, path).map_err(|_| "could not finalize protected client key")
        })();
        if result.is_err() && temporary_created {
            std::fs::remove_file(&temporary)
                .map_err(|_| "could not remove failed protected client key temporary file")?;
        }
        result.map_err(str::to_owned)
    }

    pub fn sign(&self, domain: &[u8], message: &[u8]) -> Result<Vec<u8>, String> {
        let digest = sha256(&[domain, message].concat())?;
        let (algorithm, key) = import_key(&self.private_blob.0, &PRIVATE_BLOB)?;
        let mut raw = [0u8; 64];
        let mut written = 0;
        let result = unsafe {
            BCryptSignHash(
                key,
                std::ptr::null_mut(),
                digest.as_ptr() as *mut u8,
                digest.len() as u32,
                raw.as_mut_ptr(),
                raw.len() as u32,
                &mut written,
                0,
            )
        };
        unsafe {
            BCryptDestroyKey(key);
            BCryptCloseAlgorithmProvider(algorithm, 0);
        }
        if result < 0 || written != raw.len() as u32 {
            return Err("client signature failed".into());
        }
        p1363_to_der(&raw)
    }

    pub fn verify(spki: &[u8], domain: &[u8], message: &[u8], der: &[u8]) -> Result<(), String> {
        if spki.len() != 91 || !spki.starts_with(&SPKI_PREFIX) || spki[26] != 0x04 {
            return Err("invalid P-256 public key".into());
        }
        let mut raw = der_to_p1363(der)?;
        let digest = sha256(&[domain, message].concat())?;
        let mut public = Vec::with_capacity(72);
        public.extend_from_slice(&PUBLIC_MAGIC.to_le_bytes());
        public.extend_from_slice(&32u32.to_le_bytes());
        public.extend_from_slice(&spki[27..]);
        let (algorithm, key) = import_key(&public, &PUBLIC_BLOB)?;
        let result = unsafe {
            BCryptVerifySignature(
                key,
                std::ptr::null_mut(),
                digest.as_ptr() as *mut u8,
                digest.len() as u32,
                raw.as_mut_ptr(),
                raw.len() as u32,
                0,
            )
        };
        unsafe {
            BCryptDestroyKey(key);
            BCryptCloseAlgorithmProvider(algorithm, 0);
        }
        if result < 0 {
            return Err("signature verification failed".into());
        }
        Ok(())
    }

    pub fn generate() -> Result<Self, String> {
        let mut algorithm = std::ptr::null_mut();
        let result = unsafe {
            BCryptOpenAlgorithmProvider(&mut algorithm, ECDSA_P256.as_ptr(), std::ptr::null(), 0)
        };
        if result < 0 {
            return Err("cannot open P-256 provider".into());
        }
        let mut key = std::ptr::null_mut();
        let generated = unsafe { BCryptGenerateKeyPair(algorithm, &mut key, 256, 0) };
        let finalized = if generated >= 0 {
            unsafe { BCryptFinalizeKeyPair(key, 0) }
        } else {
            generated
        };
        if finalized < 0 {
            if !key.is_null() {
                unsafe {
                    BCryptDestroyKey(key);
                }
            }
            unsafe {
                BCryptCloseAlgorithmProvider(algorithm, 0);
            }
            return Err("cannot generate client key".into());
        }
        let mut private = Secret::new(vec![0; KEY_BLOB_BYTES]);
        let mut public = [0u8; 72];
        let mut private_len = 0;
        let mut public_len = 0;
        let private_result = unsafe {
            BCryptExportKey(
                key,
                std::ptr::null_mut(),
                PRIVATE_BLOB.as_ptr(),
                private.0.as_mut_ptr(),
                private.0.len() as u32,
                &mut private_len,
                0,
            )
        };
        let public_result = unsafe {
            BCryptExportKey(
                key,
                std::ptr::null_mut(),
                PUBLIC_BLOB.as_ptr(),
                public.as_mut_ptr(),
                public.len() as u32,
                &mut public_len,
                0,
            )
        };
        unsafe {
            BCryptDestroyKey(key);
            BCryptCloseAlgorithmProvider(algorithm, 0);
        }
        if private_result < 0
            || public_result < 0
            || private_len != KEY_BLOB_BYTES as u32
            || public_len != 72
        {
            return Err("cannot export client key".into());
        }
        Self::from_public_and_private(private, &public[8..])
    }

    fn from_private_blob(private_blob: Secret) -> Result<Self, String> {
        if private_blob.0.len() != KEY_BLOB_BYTES
            || u32::from_le_bytes(private_blob.0[0..4].try_into().unwrap()) != PRIVATE_MAGIC
            || u32::from_le_bytes(private_blob.0[4..8].try_into().unwrap()) != 32
        {
            return Err("protected client key has invalid format".into());
        }
        let xy = private_blob.0[8..72].to_vec();
        Self::from_public_and_private(private_blob, &xy)
    }

    fn from_public_and_private(private_blob: Secret, xy: &[u8]) -> Result<Self, String> {
        if xy.len() != 64 {
            return Err("client public key has invalid length".into());
        }
        let mut spki = [0u8; 91];
        spki[..26].copy_from_slice(&SPKI_PREFIX);
        spki[26] = 0x04;
        spki[27..].copy_from_slice(xy);
        let fingerprint = sha256(&spki)?;
        Ok(Self {
            private_blob,
            spki,
            fingerprint,
        })
    }
}

fn import_key(blob: &[u8], kind: &[u16]) -> Result<(*mut c_void, *mut c_void), String> {
    let mut algorithm = std::ptr::null_mut();
    let result = unsafe {
        BCryptOpenAlgorithmProvider(&mut algorithm, ECDSA_P256.as_ptr(), std::ptr::null(), 0)
    };
    if result < 0 {
        return Err("cannot open P-256 provider".into());
    }
    let mut key = std::ptr::null_mut();
    let result = unsafe {
        BCryptImportKeyPair(
            algorithm,
            std::ptr::null_mut(),
            kind.as_ptr(),
            &mut key,
            blob.as_ptr() as *mut u8,
            blob.len() as u32,
            0,
        )
    };
    if result < 0 {
        unsafe {
            BCryptCloseAlgorithmProvider(algorithm, 0);
        }
        return Err("cannot import client key".into());
    }
    Ok((algorithm, key))
}

fn credential_path(takeover_peer: bool) -> Result<PathBuf, String> {
    let root =
        std::env::var_os("LOCALAPPDATA").ok_or("Windows user profile path is unavailable")?;
    let filename = if takeover_peer {
        "takeover-client-key.dpapi"
    } else {
        "client-key.dpapi"
    };
    Ok(PathBuf::from(root).join("kvmctl").join(filename))
}

fn protect(input: &[u8]) -> Result<Vec<u8>, String> {
    protect_or_unprotect(input, true)
}

fn unprotect(input: &[u8]) -> Result<Vec<u8>, String> {
    protect_or_unprotect(input, false)
}

fn protect_or_unprotect(input: &[u8], protect_data: bool) -> Result<Vec<u8>, String> {
    let source = DataBlob {
        size: input.len() as u32,
        data: input.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        size: 0,
        data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if protect_data {
            CryptProtectData(
                &source,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
                &mut output,
            )
        }
    };
    if ok == 0 || output.data.is_null() {
        if !output.data.is_null() {
            unsafe {
                LocalFree(output.data as *mut c_void);
            }
        }
        return Err("Windows DPAPI operation failed".into());
    }
    let result = unsafe { std::slice::from_raw_parts(output.data, output.size as usize).to_vec() };
    unsafe {
        LocalFree(output.data as *mut c_void);
    }
    Ok(result)
}

fn p1363_to_der(raw: &[u8; 64]) -> Result<Vec<u8>, String> {
    fn integer(value: &[u8]) -> Vec<u8> {
        let first = value
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(value.len() - 1);
        let value = &value[first..];
        let pad = usize::from(value[0] & 0x80 != 0);
        let mut out = Vec::with_capacity(value.len() + pad + 2);
        out.push(0x02);
        out.push((value.len() + pad) as u8);
        if pad != 0 {
            out.push(0);
        }
        out.extend_from_slice(value);
        out
    }
    let mut body = integer(&raw[..32]);
    body.extend(integer(&raw[32..]));
    let mut der = vec![0x30, body.len() as u8];
    der.extend(body);
    Ok(der)
}

fn der_to_p1363(der: &[u8]) -> Result<Vec<u8>, String> {
    if der.len() < 8 || der.len() > 72 || der[0] != 0x30 || der[1] as usize != der.len() - 2 {
        return Err("malformed ECDSA signature".into());
    }
    let mut at = 2;
    let mut raw = Vec::with_capacity(64);
    for _ in 0..2 {
        if der.get(at) != Some(&0x02) {
            return Err("malformed ECDSA signature".into());
        }
        let len = *der.get(at + 1).ok_or("malformed ECDSA signature")? as usize;
        at += 2;
        let value = der.get(at..at + len).ok_or("malformed ECDSA signature")?;
        if value.is_empty()
            || value[0] & 0x80 != 0
            || (value.len() > 1 && value[0] == 0 && value[1] & 0x80 == 0)
        {
            return Err("malformed ECDSA signature".into());
        }
        let value = if value[0] == 0 { &value[1..] } else { value };
        if value.len() > 32 {
            return Err("malformed ECDSA signature".into());
        }
        raw.extend(std::iter::repeat_n(0, 32 - value.len()));
        raw.extend_from_slice(value);
        at += len;
    }
    if at != der.len() {
        return Err("malformed ECDSA signature".into());
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = crate::random_id()
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        std::env::temp_dir().join(format!("kvmctl-{label}-{nonce}"))
    }

    #[test]
    fn independent_clients_have_distinct_keys_and_signatures() {
        let first = ClientCredential::generate().unwrap();
        let second = ClientCredential::generate().unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
        assert_ne!(first.spki, second.spki);
        let domain = b"kvmctl/independent-clients/v1";
        let message = b"each client keeps its own signing key";
        let first_signature = first.sign(domain, message).unwrap();
        let second_signature = second.sign(domain, message).unwrap();
        assert_ne!(first_signature, second_signature);
        ClientCredential::verify(&first.spki, domain, message, &first_signature).unwrap();
        ClientCredential::verify(&second.spki, domain, message, &second_signature).unwrap();
        assert!(ClientCredential::verify(&first.spki, domain, message, &second_signature).is_err());
        assert!(ClientCredential::verify(&second.spki, domain, message, &first_signature).is_err());
    }

    #[test]
    fn failed_protected_key_write_leaves_no_credential_or_temporary_file() {
        let directory = temporary_directory("credential-write-failure");
        std::fs::create_dir_all(&directory).unwrap();
        // The destination name is valid, but its temporary filename exceeds
        // Windows's 255-character component limit and cannot be created.
        let path = directory.join("k".repeat(240));
        let credential = ClientCredential::generate().unwrap();
        assert_eq!(
            ClientCredential::store_to(&path, &credential).unwrap_err(),
            "could not save protected client key"
        );
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_protected_key_rename_removes_its_temporary_file() {
        let directory = temporary_directory("credential-rename-failure");
        let path = directory.join("client-key.dpapi");
        std::fs::create_dir_all(&path).unwrap();
        let credential = ClientCredential::generate().unwrap();
        assert_eq!(
            ClientCredential::store_to(&path, &credential).unwrap_err(),
            "could not finalize protected client key"
        );
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ecdsa_der_round_trip_and_rejects_noncanonical_values() {
        let mut raw = [0u8; 64];
        raw[0] = 0x80;
        raw[31] = 1;
        raw[32] = 1;
        raw[63] = 0x80;
        let der = p1363_to_der(&raw).unwrap();
        assert_eq!(der_to_p1363(&der).unwrap(), raw);
        assert!(der_to_p1363(&[0x30, 0x06, 0x02, 0x01, 0x80, 0x02, 0x01, 0x01]).is_err());
    }

    #[test]
    fn cng_signatures_and_dpapi_round_trip() {
        let credential = ClientCredential::generate().unwrap();
        let message = b"client transcript proof";
        let signature = credential.sign(b"kvmctl/transcript/v1", message).unwrap();
        ClientCredential::verify(
            &credential.spki,
            b"kvmctl/transcript/v1",
            message,
            &signature,
        )
        .unwrap();
        assert!(
            ClientCredential::verify(
                &credential.spki,
                b"kvmctl/transcript/v1",
                b"wrong",
                &signature
            )
            .is_err()
        );

        let protected = protect(credential.private_blob.as_slice()).unwrap();
        assert_ne!(protected, credential.private_blob.as_slice());
        assert_eq!(
            unprotect(&protected).unwrap(),
            credential.private_blob.as_slice()
        );
    }

    #[test]
    fn dpapi_credential_persists_and_reloads_same_identity() {
        let unique = format!(
            "kvmctl-credential-test-{}-{}",
            std::process::id(),
            crate::random_id()
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let directory = std::env::temp_dir().join(unique);
        let path = directory.join("client-key.dpapi");
        let first = ClientCredential::generate().unwrap();
        ClientCredential::store_to(&path, &first).unwrap();
        let protected_file = std::fs::read(&path).unwrap();
        assert_ne!(protected_file, first.private_blob.as_slice());
        let second = ClientCredential::load_from(&path).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.spki, second.spki);
        let message = b"persistence proves the same private key survived disk reload";
        let signature = second.sign(b"kvmctl/persistence/v1", message).unwrap();
        ClientCredential::verify(&first.spki, b"kvmctl/persistence/v1", message, &signature)
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
