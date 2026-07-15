use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub fan: FanConfig,
    pub fan_curve: FanCurveConfig,
    pub fan_drives: FanDrivesConfig,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanCurveTail {
    Hold,
    Extrapolate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanCurveConfig {
    pub enabled: bool,
    pub duty0: u8,
    pub duty1: u8,
    pub duty2: u8,
    pub duty3: u8,
    pub tail: FanCurveTail,
    pub max_duty: u8,
    pub hysteresis: u8,
    pub ramp_down: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FanDrivesConfig {
    pub enabled: bool,
    pub devices: Vec<String>,
    pub thresholds: FanConfig,
    pub poll_seconds: u64,
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

impl Default for FanCurveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            duty0: 25,
            duty1: 50,
            duty2: 75,
            duty3: 100,
            tail: FanCurveTail::Extrapolate,
            max_duty: 100,
            hysteresis: 1,
            ramp_down: 0,
        }
    }
}

impl Default for FanDrivesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            devices: Vec::new(),
            thresholds: FanConfig {
                lv0: 45.0,
                lv1: 50.0,
                lv2: 55.0,
                lv3: 60.0,
            },
            poll_seconds: 30,
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

        if let Some(section) = ini.get("fan_curve") {
            validate_section_keys(
                "fan_curve",
                section,
                &[
                    "enabled",
                    "duty0",
                    "duty1",
                    "duty2",
                    "duty3",
                    "tail",
                    "max_duty",
                    "hysteresis",
                    "ramp_down",
                ],
            )?;
            config.fan_curve.enabled = parse_bool(section, "enabled", config.fan_curve.enabled)?;
            config.fan_curve.duty0 = parse_percent(section, "duty0", config.fan_curve.duty0)?;
            config.fan_curve.duty1 = parse_percent(section, "duty1", config.fan_curve.duty1)?;
            config.fan_curve.duty2 = parse_percent(section, "duty2", config.fan_curve.duty2)?;
            config.fan_curve.duty3 = parse_percent(section, "duty3", config.fan_curve.duty3)?;
            config.fan_curve.tail = parse_fan_curve_tail(section, "tail", config.fan_curve.tail)?;
            config.fan_curve.max_duty =
                parse_percent(section, "max_duty", config.fan_curve.max_duty)?;
            config.fan_curve.hysteresis =
                parse_percent(section, "hysteresis", config.fan_curve.hysteresis)?;
            config.fan_curve.ramp_down =
                parse_percent(section, "ramp_down", config.fan_curve.ramp_down)?;
        }

        if let Some(section) = ini.get("fan_drives") {
            config.fan_drives.enabled = parse_bool(section, "enabled", config.fan_drives.enabled)?;
            if let Some(devices) = section.get("devices") {
                config.fan_drives.devices = split_csv(devices);
            }
            config.fan_drives.thresholds.lv0 =
                parse_f64(section, "lv0", config.fan_drives.thresholds.lv0)?;
            config.fan_drives.thresholds.lv1 =
                parse_f64(section, "lv1", config.fan_drives.thresholds.lv1)?;
            config.fan_drives.thresholds.lv2 =
                parse_f64(section, "lv2", config.fan_drives.thresholds.lv2)?;
            config.fan_drives.thresholds.lv3 =
                parse_f64(section, "lv3", config.fan_drives.thresholds.lv3)?;
            config.fan_drives.poll_seconds =
                parse_positive_u64(section, "poll_seconds", config.fan_drives.poll_seconds)?;
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

        if let Some(section) = ini.get("disk")
            && let Some(extra) = section.get("extra")
        {
            config.disks = split_csv(extra);
        }

        validate_fan_thresholds("fan", config.fan)?;
        validate_fan_thresholds("fan_drives", config.fan_drives.thresholds)?;
        validate_fan_curve(config.fan_curve)?;

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

fn parse_positive_u64(section: &Section, key: &str, default: u64) -> Result<u64, ConfigError> {
    let Some(value) = section.get(key) else {
        return Ok(default);
    };

    match value.parse::<u64>() {
        Ok(value) if value > 0 => Ok(value),
        _ => Err(ConfigError::Value {
            key: key.to_string(),
            value: value.clone(),
            expected: "positive integer",
        }),
    }
}

fn parse_percent(section: &Section, key: &str, default: u8) -> Result<u8, ConfigError> {
    let Some(value) = section.get(key) else {
        return Ok(default);
    };

    match value.parse::<u8>() {
        Ok(value) if value <= 100 => Ok(value),
        _ => Err(ConfigError::Value {
            key: key.to_string(),
            value: value.clone(),
            expected: "integer in 0..=100",
        }),
    }
}

fn parse_fan_curve_tail(
    section: &Section,
    key: &str,
    default: FanCurveTail,
) -> Result<FanCurveTail, ConfigError> {
    let Some(value) = section.get(key) else {
        return Ok(default);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "hold" => Ok(FanCurveTail::Hold),
        "extrapolate" => Ok(FanCurveTail::Extrapolate),
        _ => Err(ConfigError::Value {
            key: key.to_string(),
            value: value.clone(),
            expected: "hold or extrapolate",
        }),
    }
}

fn validate_fan_thresholds(section: &'static str, config: FanConfig) -> Result<(), ConfigError> {
    let values = [config.lv0, config.lv1, config.lv2, config.lv3];
    let finite = values.iter().all(|value| value.is_finite());
    let ascending = values.windows(2).all(|pair| pair[0] < pair[1]);

    if finite && ascending {
        Ok(())
    } else {
        Err(ConfigError::Thresholds { section })
    }
}

fn validate_fan_curve(config: FanCurveConfig) -> Result<(), ConfigError> {
    let duties = [config.duty0, config.duty1, config.duty2, config.duty3];

    if duties.windows(2).all(|pair| pair[0] <= pair[1]) {
        Ok(())
    } else {
        Err(ConfigError::FanCurveDuties)
    }
}

fn validate_section_keys(
    section_name: &'static str,
    section: &Section,
    allowed: &[&str],
) -> Result<(), ConfigError> {
    if let Some(key) = section.keys().find(|key| !allowed.contains(&key.as_str())) {
        Err(ConfigError::UnknownKey {
            section: section_name,
            key: key.clone(),
        })
    } else {
        Ok(())
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
    Thresholds {
        section: &'static str,
    },
    FanCurveDuties,
    UnknownKey {
        section: &'static str,
        key: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "config I/O error: {err}"),
            Self::Parse { line, message } => {
                write!(f, "config parse error on line {line}: {message}")
            }
            Self::Value {
                key,
                value,
                expected,
            } => write!(
                f,
                "invalid config value for {key}: {value:?}, expected {expected}"
            ),
            Self::Thresholds { section } => write!(
                f,
                "invalid [{section}] thresholds: lv0 through lv3 must be finite and strictly ascending"
            ),
            Self::FanCurveDuties => write!(
                f,
                "invalid [fan_curve] duties: duty0 through duty3 must be nondecreasing"
            ),
            Self::UnknownKey { section, key } => {
                write!(f, "unknown key {key:?} in [{section}]")
            }
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
        assert_eq!(
            config.fan_curve,
            FanCurveConfig {
                enabled: false,
                duty0: 25,
                duty1: 50,
                duty2: 75,
                duty3: 100,
                tail: FanCurveTail::Extrapolate,
                max_duty: 100,
                hysteresis: 1,
                ramp_down: 0,
            }
        );
        assert!(!config.fan_drives.enabled);
        assert!(config.fan_drives.devices.is_empty());
        assert_eq!(
            config.fan_drives.thresholds,
            FanConfig {
                lv0: 45.0,
                lv1: 50.0,
                lv2: 55.0,
                lv3: 60.0,
            }
        );
        assert_eq!(config.fan_drives.poll_seconds, 30);
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

    #[test]
    fn parses_explicit_drive_fan_config() {
        let config = Config::parse(
            r#"
            [fan_drives]
            enabled = true
            devices = /dev/sdc, /dev/sdd, /dev/sde, /dev/sdf
            lv0 = 45
            lv1 = 50
            lv2 = 55
            lv3 = 60
            poll_seconds = 45
            "#,
        )
        .expect("drive fan config should parse");

        assert!(config.fan_drives.enabled);
        assert_eq!(
            config.fan_drives.devices,
            vec!["/dev/sdc", "/dev/sdd", "/dev/sde", "/dev/sdf"]
        );
        assert_eq!(config.fan_drives.thresholds.lv0, 45.0);
        assert_eq!(config.fan_drives.thresholds.lv3, 60.0);
        assert_eq!(config.fan_drives.poll_seconds, 45);
    }

    #[test]
    fn parses_explicit_fan_curve_config() {
        let config = Config::parse(
            r#"
            [fan_curve]
            enabled = true
            duty0 = 0
            duty1 = 25
            duty2 = 50
            duty3 = 75
            tail = ExTrApOlAtE
            max_duty = 80
            hysteresis = 2
            ramp_down = 5
            "#,
        )
        .expect("fan curve config should parse");

        assert_eq!(
            config.fan_curve,
            FanCurveConfig {
                enabled: true,
                duty0: 0,
                duty1: 25,
                duty2: 50,
                duty3: 75,
                tail: FanCurveTail::Extrapolate,
                max_duty: 80,
                hysteresis: 2,
                ramp_down: 5,
            }
        );
    }

    #[test]
    fn allows_fan_curve_points_above_hard_max() {
        let config = Config::parse(
            r#"
            [fan_curve]
            duty0 = 25
            duty1 = 50
            duty2 = 75
            duty3 = 100
            max_duty = 80
            "#,
        )
        .expect("hard max should clamp the computed curve, not constrain its points");

        assert_eq!(config.fan_curve.duty3, 100);
        assert_eq!(config.fan_curve.max_duty, 80);
    }

    #[test]
    fn rejects_fan_curve_percent_out_of_range() {
        let err = Config::parse(
            r#"
            [fan_curve]
            max_duty = 101
            "#,
        )
        .expect_err("out-of-range percentage should fail");

        assert!(err.to_string().contains("max_duty"));
        assert!(err.to_string().contains("0..=100"));
    }

    #[test]
    fn rejects_non_integer_fan_curve_percent() {
        let err = Config::parse(
            r#"
            [fan_curve]
            duty1 = 12.5
            "#,
        )
        .expect_err("fractional percentage should fail");

        assert!(err.to_string().contains("duty1"));
        assert!(err.to_string().contains("integer"));
    }

    #[test]
    fn rejects_invalid_fan_curve_stability_percentages() {
        for (key, value) in [
            ("hysteresis", "101"),
            ("hysteresis", "1.5"),
            ("ramp_down", "101"),
            ("ramp_down", "1.5"),
        ] {
            let input = format!("[fan_curve]\n{key} = {value}\n");
            let err = Config::parse(&input).expect_err("invalid percentage should fail");

            assert!(err.to_string().contains(key));
            assert!(err.to_string().contains("integer in 0..=100"));
        }
    }

    #[test]
    fn rejects_decreasing_fan_curve_duties() {
        let err = Config::parse(
            r#"
            [fan_curve]
            duty0 = 0
            duty1 = 50
            duty2 = 25
            duty3 = 75
            "#,
        )
        .expect_err("decreasing duties should fail");

        assert!(err.to_string().contains("[fan_curve] duties"));
        assert!(err.to_string().contains("nondecreasing"));
    }

    #[test]
    fn rejects_unknown_fan_curve_tail() {
        let err = Config::parse(
            r#"
            [fan_curve]
            tail = accelerate
            "#,
        )
        .expect_err("unknown tail mode should fail");

        assert!(err.to_string().contains("tail"));
        assert!(err.to_string().contains("hold or extrapolate"));
    }

    #[test]
    fn rejects_unknown_fan_curve_key() {
        let err = Config::parse(
            r#"
            [fan_curve]
            enabled = true
            max-duty = 80
            "#,
        )
        .expect_err("a misspelled hard maximum must not silently use the default");

        assert!(err.to_string().contains("max-duty"));
        assert!(err.to_string().contains("[fan_curve]"));
    }

    #[test]
    fn rejects_zero_drive_poll_interval() {
        let err = Config::parse(
            r#"
            [fan_drives]
            poll_seconds = 0
            "#,
        )
        .expect_err("zero poll interval should fail");

        assert!(err.to_string().contains("poll_seconds"));
        assert!(err.to_string().contains("positive integer"));
    }

    #[test]
    fn rejects_non_finite_cpu_threshold() {
        let err = Config::parse(
            r#"
            [fan]
            lv0 = NaN
            "#,
        )
        .expect_err("NaN threshold should fail");

        assert!(err.to_string().contains("[fan] thresholds"));
    }

    #[test]
    fn rejects_unordered_drive_thresholds() {
        let err = Config::parse(
            r#"
            [fan_drives]
            lv0 = 45
            lv1 = 55
            lv2 = 50
            lv3 = 60
            "#,
        )
        .expect_err("unordered thresholds should fail");

        assert!(err.to_string().contains("[fan_drives] thresholds"));
    }
}
