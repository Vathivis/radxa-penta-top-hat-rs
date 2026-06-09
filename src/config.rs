use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub fan: FanConfig,
    pub key: KeyConfig,
    pub time: TimeConfig,
    pub oled: OledConfig,
    pub disks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanConfig {
    pub lv0: f64,
    pub lv1: f64,
    pub lv2: f64,
    pub lv3: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyConfig {
    pub click: String,
    pub twice: String,
    pub press: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeConfig {
    pub twice: f64,
    pub press: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OledConfig {
    pub rotate: bool,
    pub f_temp: bool,
    pub auto_slide: bool,
    pub auto_slide_time: f64,
    pub sleep: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fan: FanConfig::default(),
            key: KeyConfig::default(),
            time: TimeConfig::default(),
            oled: OledConfig::default(),
            disks: Vec::new(),
        }
    }
}

impl Default for FanConfig {
    fn default() -> Self {
        Self {
            lv0: 35.0,
            lv1: 40.0,
            lv2: 45.0,
            lv3: 50.0,
        }
    }
}

impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            click: "slider".to_string(),
            twice: "switch".to_string(),
            press: "none".to_string(),
        }
    }
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self {
            twice: 0.7,
            press: 1.8,
        }
    }
}

impl Default for OledConfig {
    fn default() -> Self {
        Self {
            rotate: false,
            f_temp: false,
            auto_slide: true,
            auto_slide_time: 10.0,
            sleep: 0.0,
        }
    }
}

impl Config {
    pub fn from_file_or_defaults(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(contents) => Self::parse(&contents),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(ConfigError::Io(err)),
        }
    }

    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let ini = parse_ini(input)?;
        let mut config = Self::default();

        if let Some(section) = ini.get("fan") {
            config.fan.lv0 = parse_f64(section, "lv0", config.fan.lv0)?;
            config.fan.lv1 = parse_f64(section, "lv1", config.fan.lv1)?;
            config.fan.lv2 = parse_f64(section, "lv2", config.fan.lv2)?;
            config.fan.lv3 = parse_f64(section, "lv3", config.fan.lv3)?;
        }

        if let Some(section) = ini.get("key") {
            config.key.click = get_string(section, "click", &config.key.click);
            config.key.twice = get_string(section, "twice", &config.key.twice);
            config.key.press = get_string(section, "press", &config.key.press);
        }

        if let Some(section) = ini.get("time") {
            config.time.twice = parse_f64(section, "twice", config.time.twice)?;
            config.time.press = parse_f64(section, "press", config.time.press)?;
        }

        if let Some(section) = ini.get("oled") {
            config.oled.rotate = parse_bool(section, "rotate", config.oled.rotate)?;
            config.oled.f_temp = parse_bool(section, "f-temp", config.oled.f_temp)?;
            config.oled.auto_slide = parse_bool(section, "auto_slide", config.oled.auto_slide)?;
            config.oled.auto_slide_time =
                parse_f64(section, "auto_slide_time", config.oled.auto_slide_time)?;
            config.oled.sleep = parse_f64(section, "sleep", config.oled.sleep)?;
        }

        if let Some(section) = ini.get("disk") {
            if let Some(extra) = section.get("extra") {
                config.disks = split_csv(extra);
            }
        }

        Ok(config)
    }
}

type Section = BTreeMap<String, String>;
type Ini = BTreeMap<String, Section>;

fn parse_ini(input: &str) -> Result<Ini, ConfigError> {
    let mut ini = Ini::new();
    let mut current_section = String::new();

    for (idx, raw_line) in input.lines().enumerate() {
        let line_number = idx + 1;
        let line = strip_comment(raw_line).trim();

        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') {
            let Some(section) = line.strip_suffix(']') else {
                return Err(ConfigError::Parse {
                    line: line_number,
                    message: "unterminated section header".to_string(),
                });
            };
            current_section = section[1..].trim().to_ascii_lowercase();
            if current_section.is_empty() {
                return Err(ConfigError::Parse {
                    line: line_number,
                    message: "empty section header".to_string(),
                });
            }
            ini.entry(current_section.clone()).or_default();
            continue;
        }

        if current_section.is_empty() {
            return Err(ConfigError::Parse {
                line: line_number,
                message: "key/value appears before any section".to_string(),
            });
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(ConfigError::Parse {
                line: line_number,
                message: "expected key = value".to_string(),
            });
        };

        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            return Err(ConfigError::Parse {
                line: line_number,
                message: "empty key".to_string(),
            });
        }

        ini.entry(current_section.clone())
            .or_default()
            .insert(key, value.trim().to_string());
    }

    Ok(ini)
}

fn strip_comment(line: &str) -> &str {
    let hash = line.find('#');
    let semicolon = line.find(';');
    let end = match (hash, semicolon) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => line.len(),
    };
    &line[..end]
}

fn parse_f64(section: &Section, key: &str, default: f64) -> Result<f64, ConfigError> {
    let Some(value) = section.get(key) else {
        return Ok(default);
    };
    value.parse::<f64>().map_err(|_| ConfigError::Value {
        key: key.to_string(),
        value: value.clone(),
        expected: "number",
    })
}

fn parse_bool(section: &Section, key: &str, default: bool) -> Result<bool, ConfigError> {
    let Some(value) = section.get(key) else {
        return Ok(default);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(ConfigError::Value {
            key: key.to_string(),
            value: value.clone(),
            expected: "boolean",
        }),
    }
}

fn get_string(section: &Section, key: &str, default: &str) -> String {
    section
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    Parse {
        line: usize,
        message: String,
    },
    Value {
        key: String,
        value: String,
        expected: &'static str,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "config I/O error: {err}"),
            Self::Parse { line, message } => write!(f, "config parse error on line {line}: {message}"),
            Self::Value {
                key,
                value,
                expected,
            } => write!(f, "invalid config value for {key}: {value:?}, expected {expected}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_python_fallbacks() {
        let config = Config::default();
        assert_eq!(
            config.fan,
            FanConfig {
                lv0: 35.0,
                lv1: 40.0,
                lv2: 45.0,
                lv3: 50.0,
            }
        );
        assert_eq!(config.key.click, "slider");
        assert_eq!(config.key.twice, "switch");
        assert_eq!(config.key.press, "none");
        assert_eq!(config.time.twice, 0.7);
        assert_eq!(config.time.press, 1.8);
        assert!(!config.oled.rotate);
        assert!(!config.oled.f_temp);
        assert!(config.oled.auto_slide);
        assert_eq!(config.oled.auto_slide_time, 10.0);
        assert_eq!(config.oled.sleep, 0.0);
    }

    #[test]
    fn parses_upstream_style_config() {
        let config = Config::parse(
            r#"
            [fan]
            lv0 = 42
            lv1 = 51
            lv2 = 58
            lv3 = 64

            [key]
            click = slider
            twice = switch
            press = none

            [time]
            twice = 0.7
            press = 1.8

            [oled]
            rotate = false
            f-temp = false
            auto_slide = false
            auto_slide_time = 10
            sleep = 60

            [disk]
            extra = sdc1, sdd1
            "#,
        )
        .expect("config should parse");

        assert_eq!(config.fan.lv0, 42.0);
        assert_eq!(config.fan.lv3, 64.0);
        assert!(!config.oled.auto_slide);
        assert_eq!(config.oled.sleep, 60.0);
        assert_eq!(config.disks, vec!["sdc1".to_string(), "sdd1".to_string()]);
    }
}
