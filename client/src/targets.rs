use std::fs;
use std::net::Ipv4Addr;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub endpoint_fingerprint: [u8; 32],
    pub tls_spki_hash: [u8; 32],
    pub client_fingerprint: [u8; 32],
    pub last_ipv4: Option<Ipv4Addr>,
}

impl Target {
    pub fn new(
        name: String,
        endpoint_fingerprint: [u8; 32],
        tls_spki_hash: [u8; 32],
        client_fingerprint: [u8; 32],
        last_ipv4: Option<Ipv4Addr>,
    ) -> Result<Self, String> {
        if name.is_empty() || name.chars().any(char::is_control) || name.contains('\t') {
            return Err("target name must be non-empty and contain no control characters".into());
        }
        Ok(Self {
            name,
            endpoint_fingerprint,
            tls_spki_hash,
            client_fingerprint,
            last_ipv4,
        })
    }
}

fn path() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join("kvmctl").join("targets.tsv"))
        .ok_or("cannot save targets: LOCALAPPDATA is unavailable".into())
}

fn parse_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(result)
}

fn hex(value: &[u8; 32]) -> String {
    crate::hex(value)
}

pub fn load() -> Result<Vec<Target>, String> {
    let path = path()?;
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read saved targets: {error}")),
    };
    contents
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            if fields.len() != 5 {
                return Err("saved target file contains an invalid record".into());
            }
            Target::new(
                fields[0].to_owned(),
                parse_hex(fields[1]).ok_or("invalid saved endpoint fingerprint")?,
                parse_hex(fields[2]).ok_or("invalid saved TLS pin")?,
                parse_hex(fields[3]).ok_or("invalid saved client fingerprint")?,
                if fields[4].is_empty() {
                    None
                } else {
                    Some(
                        fields[4]
                            .parse()
                            .map_err(|_| "invalid saved target IPv4 address")?,
                    )
                },
            )
        })
        .collect()
}

pub fn save(target: &Target) -> Result<(), String> {
    let path = path()?;
    let mut targets = load()?;
    if let Some(existing) = targets
        .iter_mut()
        .find(|existing| existing.name.eq_ignore_ascii_case(&target.name))
    {
        *existing = target.clone();
    } else {
        targets.push(target.clone());
    }
    targets.sort_by_key(|target| target.name.to_ascii_lowercase());
    write_targets(&path, &targets)
}

fn write_targets(path: &PathBuf, targets: &[Target]) -> Result<(), String> {
    let contents = targets
        .iter()
        .map(|target| {
            format!(
                "{}\t{}\t{}\t{}\t{}\n",
                target.name,
                hex(&target.endpoint_fingerprint),
                hex(&target.tls_spki_hash),
                hex(&target.client_fingerprint),
                target
                    .last_ipv4
                    .map_or(String::new(), |address| address.to_string())
            )
        })
        .collect::<String>();
    let parent = path.parent().ok_or("saved target path has no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create target directory: {error}"))?;
    let temporary = path.with_extension("tsv.tmp");
    fs::write(&temporary, contents)
        .map_err(|error| format!("cannot write saved targets: {error}"))?;
    let old = path.with_extension("tsv.old");
    if path.exists() {
        let _ = fs::remove_file(&old);
        fs::rename(path, &old).map_err(|error| format!("cannot stage saved targets: {error}"))?;
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        if old.exists() {
            let _ = fs::rename(&old, path);
        }
        format!("cannot replace saved targets: {error}")
    })?;
    let _ = fs::remove_file(old);
    Ok(())
}

pub fn find(name: &str) -> Result<Target, String> {
    load()?
        .into_iter()
        .find(|target| target.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("saved target `{name}` was not found"))
}

pub fn remove(name: &str) -> Result<(), String> {
    let mut targets = load()?;
    let before = targets.len();
    targets.retain(|target| !target.name.eq_ignore_ascii_case(name));
    if targets.len() == before {
        return Err(format!("saved target `{name}` was not found"));
    }
    write_targets(&path()?, &targets)
}

pub fn update_address(name: &str, address: Ipv4Addr) -> Result<(), String> {
    let mut target = find(name)?;
    target.last_ipv4 = Some(address);
    save(&target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_names_reject_file_delimiters() {
        assert!(Target::new("board\t1".into(), [0; 32], [0; 32], [0; 32], None).is_err());
        assert!(Target::new("board\n1".into(), [0; 32], [0; 32], [0; 32], None).is_err());
        assert!(Target::new("board".into(), [0; 32], [0; 32], [0; 32], None).is_ok());
    }
}
