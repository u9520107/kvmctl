use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const CHIP: &str = "esp32s3";
const BAUD: &str = "115200";
const PARTITION_TABLE_OFFSET: u32 = 0x8000;
const PARTITION_TABLE_SIZE: u32 = 0x1000;
const PARTITION_TABLE_MAX_DATA: usize = 0x0c00;
const PARTITION_ENTRY_SIZE: usize = 32;
const PARTITION_MAGIC: u16 = 0x50aa;
const PARTITION_MD5_MAGIC: u16 = 0xebeb;
const PARTITION_MD5_PREFIX: [u8; 16] = [
    0xeb, 0xeb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];
const ESP_APP_DESC_OFFSET: u64 = 32;
const ESP_APP_DESC_MAGIC: [u8; 4] = [0x32, 0x54, 0xcd, 0xab];

#[derive(Clone, Copy, Debug)]
struct Partition {
    kind: u8,
    subtype: u8,
    offset: u32,
    size: u32,
}

struct Esptool {
    python: PathBuf,
    launcher_args: Vec<OsString>,
}

impl Esptool {
    fn discover() -> Result<Self, String> {
        let candidates = python_candidates();
        if candidates.is_empty() {
            return Err("Python/esptool not found; set KVMCTL_ESPTOOL_PYTHON to the ESP-IDF Python executable".into());
        }
        for (python, launcher_args) in candidates {
            let mut command = Command::new(&python);
            command
                .args(&launcher_args)
                .args(["-m", "esptool", "version"]);
            if command.output().is_ok_and(|output| output.status.success()) {
                return Ok(Self {
                    python,
                    launcher_args,
                });
            }
        }
        Err("could not run esptool; set KVMCTL_ESPTOOL_PYTHON to an ESP-IDF Python environment with esptool installed".into())
    }

    fn invoke<I, S>(&self, port: &str, after: &str, args: I) -> Result<(), String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.python);
        command
            .args(&self.launcher_args)
            .args([
                "-m", "esptool", "--chip", CHIP, "--port", port, "--baud", BAUD,
            ])
            .args(["--before", "default-reset", "--after", after])
            .args(args);
        let status = command
            .status()
            .map_err(|error| format!("cannot start esptool: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("esptool failed with {status}"))
        }
    }

    fn inspect_image(&self, image: &Path) -> Result<(), String> {
        let mut command = Command::new(&self.python);
        command.args(&self.launcher_args).args([
            OsStr::new("-m"),
            OsStr::new("esptool"),
            OsStr::new("--chip"),
            OsStr::new(CHIP),
            OsStr::new("image-info"),
        ]);
        command.arg(image);
        let status = command
            .status()
            .map_err(|error| format!("cannot start esptool: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("image is not a valid ESP32-S3 firmware image".into())
        }
    }
}

pub fn run(port: &str, image: &Path, backup: &Path) -> Result<(), String> {
    validate_paths(image, backup)?;
    let image_size = fs::metadata(image)
        .map_err(|error| format!("cannot read firmware image: {error}"))?
        .len();
    if image_size == 0 {
        return Err("firmware image is empty".into());
    }

    let esptool = Esptool::discover()?;
    esptool.inspect_image(image)?;
    let mut file = fs::File::open(image).map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(ESP_APP_DESC_OFFSET))
        .map_err(|error| error.to_string())?;
    let mut app_desc_magic = [0; 4];
    file.read_exact(&mut app_desc_magic)
        .map_err(|error| format!("cannot read ESP application metadata: {error}"))?;
    if app_desc_magic != ESP_APP_DESC_MAGIC {
        return Err("image is not an ESP-IDF application image".into());
    }

    let table_path = temporary_path_in(&env::temp_dir(), "partition-table.bin");
    reserve_temp(&table_path)?;
    let table_result = esptool.invoke(
        port,
        "no-reset",
        [
            OsString::from("read-flash"),
            OsString::from(format!("{PARTITION_TABLE_OFFSET:#x}")),
            OsString::from(format!("{PARTITION_TABLE_SIZE:#x}")),
            table_path.as_os_str().to_owned(),
        ],
    );
    if let Err(error) = table_result {
        let _ = fs::remove_file(&table_path);
        return Err(error);
    }
    let table =
        fs::read(&table_path).map_err(|error| format!("cannot read live partition table: {error}"));
    let _ = fs::remove_file(&table_path);
    let factory = parse_factory_partition(&table?)?;
    if image_size > u64::from(factory.size) {
        return Err(format!(
            "firmware image ({image_size} bytes) does not fit factory app partition ({} bytes)",
            factory.size
        ));
    }

    println!("Detected ESP32-S3 on {port}.");
    println!(
        "Factory app partition: offset {:#x}, size {} bytes.",
        factory.offset, factory.size
    );
    println!("Firmware image: {} ({image_size} bytes).", image.display());
    println!("Verified backup will be saved to: {}", backup.display());
    print!("Back up current app, flash this image, and verify it (yes/no)? ");
    use std::io::Write;
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    if !answer.trim().eq_ignore_ascii_case("yes") {
        return Err("flash cancelled; board flash was not changed".into());
    }

    let backup_parent = backup
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp_backup = temporary_path_in(
        backup_parent,
        backup
            .file_name()
            .unwrap_or_else(|| OsStr::new("app-backup.bin")),
    );
    reserve_temp(&temp_backup)?;
    let backup_result = (|| {
        esptool.invoke(
            port,
            "no-reset",
            [
                OsString::from("read-flash"),
                OsString::from(format!("{:#x}", factory.offset)),
                OsString::from(format!("{:#x}", factory.size)),
                temp_backup.as_os_str().to_owned(),
            ],
        )?;
        let length = fs::metadata(&temp_backup)
            .map_err(|error| format!("cannot read app backup: {error}"))?
            .len();
        if length != u64::from(factory.size) {
            return Err(format!(
                "app backup is incomplete ({length} of {} bytes); no flash write performed",
                factory.size
            ));
        }
        esptool.invoke(
            port,
            "no-reset",
            [
                OsString::from("verify-flash"),
                OsString::from(format!("{:#x}", factory.offset)),
                temp_backup.as_os_str().to_owned(),
            ],
        )?;
        fs::rename(&temp_backup, backup)
            .map_err(|error| format!("cannot publish verified backup: {error}"))?;
        Ok::<(), String>(())
    })();
    if let Err(error) = backup_result {
        let _ = fs::remove_file(&temp_backup);
        return Err(error);
    }
    println!("Current app backed up and verified.");

    let flash_result = esptool
        .invoke(
            port,
            "no-reset",
            [
                OsString::from("write-flash"),
                OsString::from("--flash-mode"),
                OsString::from("keep"),
                OsString::from("--flash-freq"),
                OsString::from("keep"),
                OsString::from("--flash-size"),
                OsString::from("keep"),
                OsString::from(format!("{:#x}", factory.offset)),
                image.as_os_str().to_owned(),
            ],
        )
        .and_then(|()| {
            esptool.invoke(
                port,
                "hard-reset",
                [
                    OsString::from("verify-flash"),
                    OsString::from(format!("{:#x}", factory.offset)),
                    image.as_os_str().to_owned(),
                ],
            )
        });

    if let Err(flash_error) = flash_result {
        eprintln!("Firmware write or verification failed; restoring the verified backup.");
        let restore_result = esptool
            .invoke(
                port,
                "no-reset",
                [
                    OsString::from("write-flash"),
                    OsString::from("--flash-mode"),
                    OsString::from("keep"),
                    OsString::from("--flash-freq"),
                    OsString::from("keep"),
                    OsString::from("--flash-size"),
                    OsString::from("keep"),
                    OsString::from(format!("{:#x}", factory.offset)),
                    backup.as_os_str().to_owned(),
                ],
            )
            .and_then(|()| {
                esptool.invoke(
                    port,
                    "hard-reset",
                    [
                        OsString::from("verify-flash"),
                        OsString::from(format!("{:#x}", factory.offset)),
                        backup.as_os_str().to_owned(),
                    ],
                )
            });
        return Err(match restore_result {
            Ok(()) => format!(
                "{flash_error}; original app restored and verified from {}",
                backup.display()
            ),
            Err(restore_error) => format!(
                "{flash_error}; automatic restore failed ({restore_error}); verified backup remains at {}",
                backup.display()
            ),
        });
    }

    println!("Firmware flashed and verified; board reset.");
    Ok(())
}

fn validate_paths(image: &Path, backup: &Path) -> Result<(), String> {
    if !image.is_file() {
        return Err(format!(
            "firmware image does not exist: {}",
            image.display()
        ));
    }
    if backup.exists() {
        return Err(format!(
            "backup path already exists; choose a new path: {}",
            backup.display()
        ));
    }
    let parent = backup
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(format!(
            "backup directory does not exist: {}",
            parent.display()
        ));
    }
    let image_path = fs::canonicalize(image).map_err(|error| error.to_string())?;
    let backup_name = backup
        .file_name()
        .ok_or_else(|| "backup path must name a file".to_string())?;
    let backup_path = fs::canonicalize(parent)
        .map_err(|error| error.to_string())?
        .join(backup_name);
    if image_path
        .to_string_lossy()
        .eq_ignore_ascii_case(&backup_path.to_string_lossy())
    {
        return Err("firmware image and backup path must be different".into());
    }
    Ok(())
}

fn parse_factory_partition(bytes: &[u8]) -> Result<Partition, String> {
    if bytes.len() < PARTITION_TABLE_MAX_DATA {
        return Err("live partition table read was incomplete".into());
    }

    let records = bytes[..PARTITION_TABLE_MAX_DATA].chunks_exact(PARTITION_ENTRY_SIZE);
    let terminator = records
        .clone()
        .position(|record| {
            matches!(
                u16::from_le_bytes([record[0], record[1]]),
                PARTITION_MD5_MAGIC | 0xffff
            )
        })
        .ok_or_else(|| "live partition table has no terminator".to_string())?;
    let md5_offset = terminator * PARTITION_ENTRY_SIZE;
    let md5_record = &bytes[md5_offset..md5_offset + PARTITION_ENTRY_SIZE];
    if u16::from_le_bytes([md5_record[0], md5_record[1]]) != PARTITION_MD5_MAGIC {
        return Err("live partition table has no MD5 checksum".into());
    }
    if md5_record[..16] != PARTITION_MD5_PREFIX
        || super::md5(&bytes[..md5_offset])?.as_slice() != &md5_record[16..32]
    {
        return Err("live partition table MD5 checksum is invalid".into());
    }

    let mut entries = Vec::new();
    for record in bytes[..md5_offset].chunks_exact(PARTITION_ENTRY_SIZE) {
        let magic = u16::from_le_bytes([record[0], record[1]]);
        if magic != PARTITION_MAGIC {
            return Err("live partition table is malformed; no flash write performed".into());
        }
        let offset = u32::from_le_bytes(record[4..8].try_into().unwrap());
        let size = u32::from_le_bytes(record[8..12].try_into().unwrap());
        let end = offset
            .checked_add(size)
            .ok_or_else(|| "live partition table has an overflowing partition".to_string())?;
        if size == 0 || offset % 0x1000 != 0 || size % 0x1000 != 0 || end <= offset {
            return Err("live partition table has invalid partition bounds".into());
        }
        entries.push(Partition {
            kind: record[2],
            subtype: record[3],
            offset,
            size,
        });
    }
    if entries.is_empty() {
        return Err("live partition table contains no valid entries".into());
    }
    for (index, left) in entries.iter().enumerate() {
        let left_end = left.offset + left.size;
        for right in &entries[index + 1..] {
            let right_end = right.offset + right.size;
            if left.offset < right_end && right.offset < left_end {
                return Err("live partition table contains overlapping partitions".into());
            }
        }
    }
    let factories: Vec<_> = entries
        .iter()
        .copied()
        .filter(|entry| entry.kind == 0 && entry.subtype == 0)
        .collect();
    if factories.len() != 1 {
        return Err("live partition table must contain exactly one factory app partition".into());
    }
    let factory = factories[0];
    let partition_table_end = PARTITION_TABLE_OFFSET + PARTITION_TABLE_SIZE;
    if factory.offset < partition_table_end {
        return Err(
            "factory app overlaps the ESP-IDF partition table; no flash write performed".into(),
        );
    }
    Ok(factory)
}

fn python_candidates() -> Vec<(PathBuf, Vec<OsString>)> {
    let mut candidates = Vec::new();
    if let Some(python) = env::var_os("KVMCTL_ESPTOOL_PYTHON") {
        candidates.push((PathBuf::from(python), Vec::new()));
        return candidates;
    }
    if let Some(python_env) = env::var_os("IDF_PYTHON_ENV_PATH") {
        candidates.push((
            PathBuf::from(python_env).join("Scripts/python.exe"),
            Vec::new(),
        ));
    }
    if let Some(tools) = env::var_os("IDF_TOOLS_PATH") {
        add_espressif_python_candidates(&mut candidates, &PathBuf::from(tools).join("python"));
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        add_espressif_python_candidates(
            &mut candidates,
            &PathBuf::from(profile).join(".espressif/tools/python"),
        );
    }
    add_espressif_python_candidates(&mut candidates, Path::new(r"C:\Espressif\tools\python"));
    candidates.push((PathBuf::from("python"), Vec::new()));
    candidates.push((PathBuf::from("py"), vec![OsString::from("-3")]));
    candidates
}

fn add_espressif_python_candidates(candidates: &mut Vec<(PathBuf, Vec<OsString>)>, root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort_by(|a, b| b.cmp(a));
    for path in paths {
        candidates.push((path.join("venv/Scripts/python.exe"), Vec::new()));
    }
}

fn temporary_path_in(directory: &Path, file_name: impl AsRef<OsStr>) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut name = OsString::from("kvmctl-");
    name.push(std::process::id().to_string());
    name.push("-");
    name.push(nonce.to_string());
    name.push("-");
    name.push(file_name);
    directory.join(name)
}

fn reserve_temp(path: &Path) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create temporary file: {error}"))?;
    drop(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_factory_partition_and_rejects_overlap() {
        let mut table = vec![0xff; PARTITION_TABLE_MAX_DATA];
        write_partition(&mut table, 0, 0, 0, 0x10000, 0x177000);
        write_partition(&mut table, 1, 1, 2, 0x9000, 0x5000);
        write_md5(&mut table, 2);
        let partition = parse_factory_partition(&table).unwrap();
        assert_eq!(partition.offset, 0x10000);
        assert_eq!(partition.size, 0x177000);

        write_partition(&mut table, 1, 0, 1, 0x100000, 0x1000);
        write_md5(&mut table, 2);
        assert!(
            parse_factory_partition(&table)
                .unwrap_err()
                .contains("overlapping")
        );
    }

    fn write_partition(
        table: &mut [u8],
        index: usize,
        kind: u8,
        subtype: u8,
        offset: u32,
        size: u32,
    ) {
        let start = index * PARTITION_ENTRY_SIZE;
        let record = &mut table[start..start + PARTITION_ENTRY_SIZE];
        record[0..2].copy_from_slice(&PARTITION_MAGIC.to_le_bytes());
        record[2] = kind;
        record[3] = subtype;
        record[4..8].copy_from_slice(&offset.to_le_bytes());
        record[8..12].copy_from_slice(&size.to_le_bytes());
    }

    fn write_md5(table: &mut [u8], index: usize) {
        let offset = index * PARTITION_ENTRY_SIZE;
        let digest = super::super::md5(&table[..offset]).unwrap();
        let record = &mut table[offset..offset + PARTITION_ENTRY_SIZE];
        record[..16].copy_from_slice(&PARTITION_MD5_PREFIX);
        record[16..].copy_from_slice(&digest);
    }
}
