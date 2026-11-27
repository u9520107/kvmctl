use std::fs;
use std::path::PathBuf;

pub const MOD_ALT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_SHIFT: u8 = 4;
pub const MOD_WIN: u8 = 8;
pub const DEFAULT_MOTION_RATE_HZ: u32 = 125;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapeBinding {
    pub modifiers: u8,
    pub virtual_key: u32,
}

impl EscapeBinding {
    pub fn new(modifiers: u8, virtual_key: u32) -> Result<Self, String> {
        if modifiers == 0 || virtual_key == 0 || virtual_key > 0xff {
            return Err("escape binding needs modifiers and one virtual key".into());
        }
        Ok(Self {
            modifiers,
            virtual_key,
        })
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        let mut modifiers = 0;
        let mut virtual_key = None;
        for part in value.split('+') {
            let part = part.trim().to_ascii_uppercase();
            if part.is_empty() {
                return Err("escape binding contains an empty key".into());
            }
            let modifier = match part.as_str() {
                "ALT" => MOD_ALT,
                "CTRL" | "CONTROL" => MOD_CTRL,
                "SHIFT" => MOD_SHIFT,
                "WIN" | "META" => MOD_WIN,
                _ => 0,
            };
            if modifier != 0 {
                modifiers |= modifier;
                continue;
            }
            if virtual_key.is_some() {
                return Err("escape binding accepts only one non-modifier key".into());
            }
            virtual_key = Some(match part.as_str() {
                "ESC" | "ESCAPE" => 0x1b,
                "COPILOT" => {
                    modifiers |= MOD_WIN | MOD_SHIFT;
                    0x86 // VK_F23
                }
                value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => {
                    value.as_bytes()[0] as u32
                }
                value
                    if value.strip_prefix('F').is_some_and(|number| {
                        number
                            .parse::<u32>()
                            .is_ok_and(|number| (1..=24).contains(&number))
                    }) =>
                {
                    let number = value[1..].parse::<u32>().unwrap();
                    0x70 + number - 1
                }
                _ => return Err(format!("unsupported escape key `{part}`")),
            });
        }
        Self::new(
            modifiers,
            virtual_key.ok_or("escape binding needs one non-modifier key")?,
        )
    }

    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        for (mask, name) in [
            (MOD_CTRL, "CTRL"),
            (MOD_ALT, "ALT"),
            (MOD_SHIFT, "SHIFT"),
            (MOD_WIN, "WIN"),
        ] {
            if self.modifiers & mask != 0 {
                parts.push(name.to_owned());
            }
        }
        parts.push(match self.virtual_key {
            0x1b => "ESC".into(),
            value if (0x70..=0x87).contains(&value) => format!("F{}", value - 0x70 + 1),
            value
                if (b'A' as u32..=b'Z' as u32).contains(&value)
                    || (b'0' as u32..=b'9' as u32).contains(&value) =>
            {
                char::from_u32(value).unwrap().to_string()
            }
            value => format!("VK_{value:02X}"),
        });
        parts.join("+")
    }

    pub fn contains_key(&self, virtual_key: u32) -> bool {
        self.virtual_key == virtual_key
            || (virtual_key == 0xa0 || virtual_key == 0xa1) && self.modifiers & MOD_SHIFT != 0
            || (virtual_key == 0xa2 || virtual_key == 0xa3) && self.modifiers & MOD_CTRL != 0
            || (virtual_key == 0xa4 || virtual_key == 0xa5) && self.modifiers & MOD_ALT != 0
            || (virtual_key == 0x5b || virtual_key == 0x5c) && self.modifiers & MOD_WIN != 0
    }
}

fn path() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join("kvmctl").join("settings.tsv"))
        .ok_or("cannot save settings: LOCALAPPDATA is unavailable".into())
}

pub fn default_escape_bindings() -> Vec<EscapeBinding> {
    vec![EscapeBinding::parse("CTRL+SHIFT+F10").unwrap()]
}

fn parse_motion_rate(value: &str) -> Result<u32, String> {
    let rate = value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("motion rate must be an integer, got `{value}`"))?;
    if !(1..=2000).contains(&rate) {
        return Err("motion rate must be between 1 and 2000 Hz".into());
    }
    Ok(rate)
}

pub fn default_motion_rate_hz() -> u32 {
    DEFAULT_MOTION_RATE_HZ
}

pub fn load_escape_bindings() -> Result<Vec<EscapeBinding>, String> {
    let path = path()?;
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(default_escape_bindings());
        }
        Err(error) => return Err(format!("cannot read settings: {error}")),
    };
    let mut bindings = Vec::new();
    for line in contents.lines().filter(|line| !line.is_empty()) {
        if let Some(value) = line.strip_prefix("escape\t") {
            bindings.push(EscapeBinding::parse(value)?);
        } else if line.strip_prefix("input_route\t").is_none()
            && line.strip_prefix("motion_rate_hz\t").is_none()
        {
            return Err("settings contains an invalid record".into());
        }
    }
    if bindings.is_empty() {
        return Err("settings contain no escape bindings".into());
    }
    Ok(bindings)
}

pub fn load_motion_rate_hz() -> Result<u32, String> {
    let path = path()?;
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(default_motion_rate_hz());
        }
        Err(error) => return Err(format!("cannot read settings: {error}")),
    };
    let mut rate = default_motion_rate_hz();
    for line in contents.lines().filter(|line| !line.is_empty()) {
        if let Some(value) = line.strip_prefix("motion_rate_hz\t") {
            rate = parse_motion_rate(value)?;
        } else if line.strip_prefix("escape\t").is_none()
            && line.strip_prefix("input_route\t").is_none()
        {
            return Err("settings contains an invalid record".into());
        }
    }
    Ok(rate)
}

pub fn save_escape_bindings(bindings: &[EscapeBinding]) -> Result<(), String> {
    if bindings.is_empty() {
        return Err("at least one escape binding is required".into());
    }
    let path = path()?;
    let parent = path.parent().ok_or("settings path has no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create settings directory: {error}"))?;
    let temporary = path.with_extension("tsv.tmp");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut contents = bindings
        .iter()
        .map(|binding| format!("escape\t{}\n", binding.display()))
        .collect::<String>();
    if let Some(rate) = existing
        .lines()
        .find_map(|line| line.strip_prefix("motion_rate_hz\t"))
    {
        contents.push_str(&format!("motion_rate_hz\t{rate}\n"));
    }
    fs::write(&temporary, contents).map_err(|error| format!("cannot write settings: {error}"))?;
    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("cannot replace settings: {error}")
    })
}

pub fn save_motion_rate_hz(rate: u32) -> Result<(), String> {
    let rate = parse_motion_rate(&rate.to_string())?;
    let path = path()?;
    let parent = path
        .parent()
        .ok_or("settings path has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create settings directory: {error}"))?;
    let temporary = path.with_extension("tsv.tmp");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut contents = String::new();
    let mut wrote_rate = false;
    let mut wrote_escape = false;
    for line in existing.lines().filter(|line| !line.is_empty()) {
        if line.strip_prefix("escape\t").is_some() {
            contents.push_str(line);
            contents.push('\n');
            wrote_escape = true;
        } else if line.strip_prefix("input_route\t").is_some() {
            // Legacy route records are accepted for migration but are no longer saved.
        } else if line.strip_prefix("motion_rate_hz\t").is_some() {
            if !wrote_rate {
                contents.push_str(&format!("motion_rate_hz\t{rate}\n"));
                wrote_rate = true;
            }
        } else {
            return Err("settings contains an invalid record".into());
        }
    }
    if !wrote_escape {
        for binding in default_escape_bindings() {
            contents.insert_str(0, &format!("escape\t{}\n", binding.display()));
        }
    }
    if !wrote_rate {
        contents.push_str(&format!("motion_rate_hz\t{rate}\n"));
    }
    fs::write(&temporary, contents).map_err(|error| format!("cannot write settings: {error}"))?;
    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("cannot replace settings: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_usable_and_round_trips() {
        let binding = &default_escape_bindings()[0];
        assert!(binding.modifiers != 0);
        assert_eq!(EscapeBinding::parse(&binding.display()).unwrap(), *binding);
        assert!(binding.contains_key(0xa0));
        assert!(binding.contains_key(0x79));
    }

    #[test]
    fn parser_accepts_multiple_binding_shapes() {
        assert_eq!(
            EscapeBinding::parse("ctrl+alt+esc").unwrap().virtual_key,
            0x1b
        );
        let copilot = EscapeBinding::parse("copilot").unwrap();
        assert_eq!(copilot, EscapeBinding::parse("WIN+SHIFT+F23").unwrap());
        assert_eq!(copilot.display(), "SHIFT+WIN+F23");
        assert_eq!(
            EscapeBinding::parse("CTRL+COPILOT").unwrap(),
            EscapeBinding::parse("CTRL+WIN+SHIFT+F23").unwrap()
        );
        assert!(EscapeBinding::parse("F10").is_err());
        assert!(EscapeBinding::parse("CTRL+F25").is_err());
        assert!(EscapeBinding::parse("CTRL+A+B").is_err());
        assert!(EscapeBinding::parse("CTRL+ESC+F10").is_err());
        assert!(EscapeBinding::parse("COPILOT+CTRL+A").is_err());
        assert!(EscapeBinding::parse("A+CTRL").is_ok());
    }

    #[test]
    fn motion_rate_has_safe_default_and_bounds() {
        assert_eq!(default_motion_rate_hz(), 125);
        assert_eq!(parse_motion_rate("250").unwrap(), 250);
        assert!(parse_motion_rate("0").is_err());
        assert!(parse_motion_rate("2001").is_err());
    }
}
