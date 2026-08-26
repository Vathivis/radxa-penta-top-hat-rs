use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SMARTCTL_BATCH_TIMEOUT: Duration = Duration::from_secs(5);
const SMARTCTL_WAIT_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DriveTemperature {
    pub current_celsius: i32,
    pub source: SmartTemperatureSource,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SmartTemperatureSource {
    JsonTemperatureCurrent,
    JsonAtaSmartAttribute,
    TextCurrentDriveTemperature,
    TextAttribute,
    TextNvmeTemperature,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SmartctlOutcome {
    Temperature(DriveTemperature),
    Standby,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriveTemperatureReading {
    pub device: String,
    pub temperature: DriveTemperature,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriveTemperatureFailure {
    pub device: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct DriveTemperaturePoll {
    pub readings: Vec<DriveTemperatureReading>,
    pub standby_devices: Vec<String>,
    pub failures: Vec<DriveTemperatureFailure>,
}

impl DriveTemperaturePoll {
    pub fn hottest(&self) -> Option<&DriveTemperatureReading> {
        self.readings
            .iter()
            .max_by_key(|reading| reading.temperature.current_celsius)
    }
}

#[derive(Debug, Clone, Copy)]
struct CachedDriveTemperature {
    temperature: DriveTemperature,
    observed_at: Instant,
}

#[derive(Debug, Default)]
pub struct DriveTemperatureState {
    cached: BTreeMap<String, CachedDriveTemperature>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct EffectiveDriveTemperatures {
    pub readings: Vec<DriveTemperatureReading>,
    pub stale_devices: Vec<String>,
    pub fail_safe_devices: Vec<String>,
}

impl EffectiveDriveTemperatures {
    pub fn hottest(&self) -> Option<&DriveTemperatureReading> {
        self.readings
            .iter()
            .max_by_key(|reading| reading.temperature.current_celsius)
    }

    pub fn requires_fail_safe(&self) -> bool {
        !self.fail_safe_devices.is_empty()
    }
}

impl DriveTemperatureState {
    pub fn update(
        &mut self,
        poll: &DriveTemperaturePoll,
        now: Instant,
        grace_period: Duration,
    ) -> EffectiveDriveTemperatures {
        let mut effective = EffectiveDriveTemperatures::default();

        for reading in &poll.readings {
            self.cached.insert(
                reading.device.clone(),
                CachedDriveTemperature {
                    temperature: reading.temperature,
                    observed_at: now,
                },
            );
            effective.readings.push(reading.clone());
        }

        for device in &poll.standby_devices {
            self.cached.remove(device);
        }

        for failure in &poll.failures {
            let cached = self.cached.get(&failure.device).copied();
            match cached {
                Some(cached)
                    if now.saturating_duration_since(cached.observed_at) <= grace_period =>
                {
                    effective.readings.push(DriveTemperatureReading {
                        device: failure.device.clone(),
                        temperature: cached.temperature,
                    });
                    effective.stale_devices.push(failure.device.clone());
                }
                _ => {
                    self.cached.remove(&failure.device);
                    effective.fail_safe_devices.push(failure.device.clone());
                }
            }
        }

        effective
    }
}

pub fn read_smart_temperature(device: &str) -> Result<SmartctlOutcome, SmartctlError> {
    read_smart_temperature_with_timeout(device, SMARTCTL_BATCH_TIMEOUT)
}

fn read_smart_temperature_with_timeout(
    device: &str,
    timeout: Duration,
) -> Result<SmartctlOutcome, SmartctlError> {
    let output = run_smartctl(device, timeout)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    classify_smartctl_output(&stdout, &stderr, output.status.code())
}

fn run_smartctl(device: &str, timeout: Duration) -> Result<std::process::Output, SmartctlError> {
    let mut child = Command::new("smartctl")
        .args(["-A", "-j", "-n", "standby,3,5", "-d", "ata", device])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(SmartctlError::Spawn)?;
    let deadline = Instant::now() + timeout;

    loop {
        if child.try_wait().map_err(SmartctlError::Wait)?.is_some() {
            return child.wait_with_output().map_err(SmartctlError::Wait);
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SmartctlError::Timeout {
                milliseconds: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            });
        }

        thread::sleep(SMARTCTL_WAIT_INTERVAL);
    }
}

fn classify_smartctl_output(
    stdout: &str,
    stderr: &str,
    status: Option<i32>,
) -> Result<SmartctlOutcome, SmartctlError> {
    if let Some(temperature) = parse_smart_temperature(stdout) {
        return Ok(SmartctlOutcome::Temperature(temperature));
    }

    if smartctl_reports_standby(stdout, stderr) {
        return Ok(SmartctlOutcome::Standby);
    }

    let detail = first_nonempty_line(stderr)
        .or_else(|| smartctl_message(stdout))
        .unwrap_or_else(|| "temperature field missing from SMART output".to_string());

    Err(SmartctlError::NoTemperature { status, detail })
}

pub fn poll_drive_temperatures(devices: &[String]) -> DriveTemperaturePoll {
    let deadline = Instant::now() + SMARTCTL_BATCH_TIMEOUT;

    poll_drive_temperatures_with(devices, |device| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(SmartctlError::BatchTimeout)
        } else {
            read_smart_temperature_with_timeout(device, remaining)
        }
    })
}

fn poll_drive_temperatures_with<F, E>(devices: &[String], mut read: F) -> DriveTemperaturePoll
where
    F: FnMut(&str) -> Result<SmartctlOutcome, E>,
    E: fmt::Display,
{
    let mut poll = DriveTemperaturePoll::default();

    for device in devices {
        match read(device) {
            Ok(SmartctlOutcome::Temperature(temperature)) => {
                poll.readings.push(DriveTemperatureReading {
                    device: device.clone(),
                    temperature,
                })
            }
            Ok(SmartctlOutcome::Standby) => poll.standby_devices.push(device.clone()),
            Err(err) => poll.failures.push(DriveTemperatureFailure {
                device: device.clone(),
                error: err.to_string(),
            }),
        }
    }

    poll
}

fn first_nonempty_line(input: &str) -> Option<String> {
    input
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

fn smartctl_message(input: &str) -> Option<String> {
    let messages = json_object_after_key(input, "smartctl", 0)?;
    json_string_after_key(messages, "string", 0)
}

fn smartctl_reports_standby(stdout: &str, stderr: &str) -> bool {
    if let Some(power_mode) = json_object_after_key(stdout, "power_mode", 0)
        && ["name", "string"]
            .iter()
            .filter_map(|key| json_string_after_key(power_mode, key, 0))
            .any(|value| value.eq_ignore_ascii_case("standby"))
    {
        return true;
    }

    json_messages_contain_standby(stdout)
        || stderr.lines().any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("device") && lower.contains("standby")
        })
}

fn json_messages_contain_standby(input: &str) -> bool {
    let mut offset = 0;

    while let Some(string_pos) = find_json_key(input, "string", offset) {
        if let Some(message) = json_string_after_key(input, "string", string_pos) {
            let lower = message.to_ascii_lowercase();
            if lower.contains("device") && lower.contains("standby") {
                return true;
            }
        }
        offset = string_pos + 1;
    }

    false
}

#[derive(Debug)]
pub enum SmartctlError {
    Spawn(io::Error),
    Wait(io::Error),
    Timeout { milliseconds: u64 },
    BatchTimeout,
    NoTemperature { status: Option<i32>, detail: String },
}

impl fmt::Display for SmartctlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(err) => write!(f, "failed to run smartctl: {err}"),
            Self::Wait(err) => write!(f, "failed while waiting for smartctl: {err}"),
            Self::Timeout { milliseconds } if milliseconds % 1000 == 0 => write!(
                f,
                "smartctl timed out after {} seconds",
                milliseconds / 1000
            ),
            Self::Timeout { milliseconds } => {
                write!(f, "smartctl timed out after {milliseconds} milliseconds")
            }
            Self::BatchTimeout => write!(f, "SMART batch deadline reached before device poll"),
            Self::NoTemperature { status, detail } => match status {
                Some(status) => write!(
                    f,
                    "smartctl returned no temperature (exit status {status}): {detail}"
                ),
                None => write!(f, "smartctl returned no temperature: {detail}"),
            },
        }
    }
}

impl std::error::Error for SmartctlError {}

pub fn parse_smart_temperature(output: &str) -> Option<DriveTemperature> {
    let trimmed = output.trim_start();

    if trimmed.starts_with('{') {
        parse_json_temperature_current(trimmed)
            .or_else(|| parse_json_ata_temperature_attribute(trimmed))
    } else {
        parse_text_temperature(output)
    }
}

fn parse_json_temperature_current(input: &str) -> Option<DriveTemperature> {
    let object = json_object_after_key(input, "temperature", 0)?;
    let current_celsius = json_i32_after_key(object, "current", 0)?;

    if !is_plausible_temperature(current_celsius) {
        return None;
    }

    Some(DriveTemperature {
        current_celsius,
        source: SmartTemperatureSource::JsonTemperatureCurrent,
    })
}

fn parse_json_ata_temperature_attribute(input: &str) -> Option<DriveTemperature> {
    let mut offset = 0;
    let mut hottest_celsius = None;

    while let Some(name_pos) = find_json_key(input, "name", offset) {
        let Some(name) = json_string_after_key(input, "name", name_pos) else {
            offset = name_pos + 1;
            continue;
        };

        if is_temperature_attribute_name(&name) {
            let object = input[..name_pos].rfind('{').and_then(|object_start| {
                matching_brace(input, object_start)
                    .map(|object_end| &input[object_start..=object_end])
            });

            if let Some(raw) = object.and_then(|object| json_object_after_key(object, "raw", 0)) {
                let current_celsius = json_string_after_key(raw, "string", 0)
                    .and_then(|raw_string| first_i32(&raw_string))
                    .filter(|temp| is_plausible_temperature(*temp))
                    .or_else(|| {
                        json_i32_after_key(raw, "value", 0)
                            .filter(|temp| is_plausible_temperature(*temp))
                    });

                if let Some(current_celsius) = current_celsius
                    && hottest_celsius
                        .map(|hottest| current_celsius > hottest)
                        .unwrap_or(true)
                {
                    hottest_celsius = Some(current_celsius);
                }
            }
        }

        offset = name_pos + 1;
    }

    hottest_celsius.map(|current_celsius| DriveTemperature {
        current_celsius,
        source: SmartTemperatureSource::JsonAtaSmartAttribute,
    })
}

fn parse_text_temperature(output: &str) -> Option<DriveTemperature> {
    let mut best_attribute_temperature: Option<DriveTemperature> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if lower.starts_with("current drive temperature:")
            && let Some(current_celsius) = first_i32_after_colon(trimmed)
            && is_plausible_temperature(current_celsius)
        {
            return Some(DriveTemperature {
                current_celsius,
                source: SmartTemperatureSource::TextCurrentDriveTemperature,
            });
        }

        if lower.starts_with("temperature:")
            && lower.contains("celsius")
            && let Some(current_celsius) = first_i32_after_colon(trimmed)
            && is_plausible_temperature(current_celsius)
        {
            return Some(DriveTemperature {
                current_celsius,
                source: SmartTemperatureSource::TextNvmeTemperature,
            });
        }

        if is_temperature_attribute_line(&lower)
            && let Some(current_celsius) = smart_attribute_raw_value(trimmed)
            && is_plausible_temperature(current_celsius)
        {
            let candidate = DriveTemperature {
                current_celsius,
                source: SmartTemperatureSource::TextAttribute,
            };

            if best_attribute_temperature
                .map(|best| candidate.current_celsius > best.current_celsius)
                .unwrap_or(true)
            {
                best_attribute_temperature = Some(candidate);
            }
        }
    }

    best_attribute_temperature
}

fn is_temperature_attribute_name(name: &str) -> bool {
    matches!(
        name,
        "Temperature_Celsius"
            | "Temperature_Internal"
            | "Airflow_Temperature_Cel"
            | "Drive_Temperature"
    )
}

fn is_temperature_attribute_line(lower: &str) -> bool {
    lower.contains("temperature_celsius")
        || lower.contains("temperature_internal")
        || lower.contains("airflow_temperature_cel")
        || lower.contains("drive_temperature")
}

fn is_plausible_temperature(temp_c: i32) -> bool {
    (-40..=125).contains(&temp_c)
}

fn smart_attribute_raw_value(line: &str) -> Option<i32> {
    let raw_value = line.split_whitespace().nth(9)?;
    first_i32(raw_value)
}

fn first_i32_after_colon(line: &str) -> Option<i32> {
    let (_, value) = line.split_once(':')?;
    first_i32(value)
}

fn first_i32(input: &str) -> Option<i32> {
    let start = input.find(|c: char| c == '-' || c.is_ascii_digit())?;
    let rest = &input[start..];
    let end = rest
        .char_indices()
        .find_map(|(idx, c)| {
            if idx > 0 && !c.is_ascii_digit() {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(rest.len());

    rest[..end].parse().ok()
}

fn find_json_key(input: &str, key: &str, start: usize) -> Option<usize> {
    let needle = format!("\"{key}\"");
    input.get(start..)?.find(&needle).map(|idx| start + idx)
}

fn json_object_after_key<'a>(input: &'a str, key: &str, start: usize) -> Option<&'a str> {
    let object_start = json_value_start(input, key, start)?;

    if input.as_bytes().get(object_start) != Some(&b'{') {
        return None;
    }

    let object_end = matching_brace(input, object_start)?;
    Some(&input[object_start..=object_end])
}

fn json_i32_after_key(input: &str, key: &str, start: usize) -> Option<i32> {
    let value_start = json_value_start(input, key, start)?;
    let rest = &input[value_start..];
    let first = rest.chars().next()?;
    if first != '-' && !first.is_ascii_digit() {
        return None;
    }
    let end = rest
        .char_indices()
        .find_map(|(idx, c)| {
            if idx > 0 && !c.is_ascii_digit() {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(rest.len());

    rest[..end].parse().ok()
}

fn json_string_after_key(input: &str, key: &str, start: usize) -> Option<String> {
    let quote_start = json_value_start(input, key, start)?;
    if input.as_bytes().get(quote_start) != Some(&b'"') {
        return None;
    }
    let string_start = quote_start + 1;
    let mut escaped = false;

    for (relative_idx, c) in input[string_start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        if c == '\\' {
            escaped = true;
            continue;
        }

        if c == '"' {
            let end = string_start + relative_idx;
            return Some(input[string_start..end].to_string());
        }
    }

    None
}

fn json_value_start(input: &str, key: &str, start: usize) -> Option<usize> {
    let key_pos = find_json_key(input, key, start)?;
    let colon = input[key_pos..].find(':')? + key_pos;
    input[colon + 1..]
        .find(|c: char| !c.is_ascii_whitespace())
        .map(|idx| colon + 1 + idx)
}

fn matching_brace(input: &str, open_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (relative_idx, c) in input[open_idx..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open_idx + relative_idx);
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temperature_reading(device: &str, current_celsius: i32) -> DriveTemperatureReading {
        DriveTemperatureReading {
            device: device.to_string(),
            temperature: DriveTemperature {
                current_celsius,
                source: SmartTemperatureSource::JsonTemperatureCurrent,
            },
        }
    }

    #[test]
    fn parses_smartctl_json_temperature_current() {
        let output = r#"{
            "temperature": {
                "current": 37,
                "power_cycle_min": 18,
                "power_cycle_max": 44
            }
        }"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 37,
                source: SmartTemperatureSource::JsonTemperatureCurrent,
            })
        );
    }

    #[test]
    fn parses_smartctl_json_ata_attribute_fallback() {
        let output = r#"{
            "ata_smart_attributes": {
                "table": [
                    {
                        "id": 194,
                        "name": "Temperature_Celsius",
                        "raw": {
                            "value": 32,
                            "string": "32 (Min/Max 20/49)"
                        }
                    }
                ]
            }
        }"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 32,
                source: SmartTemperatureSource::JsonAtaSmartAttribute,
            })
        );
    }

    #[test]
    fn parses_smartctl_json_ata_attribute_raw_string_fallback() {
        let output = r#"{
            "ata_smart_attributes": {
                "table": [
                    {
                        "id": 190,
                        "name": "Airflow_Temperature_Cel",
                        "raw": {
                            "string": "34"
                        }
                    }
                ]
            }
        }"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 34,
                source: SmartTemperatureSource::JsonAtaSmartAttribute,
            })
        );
    }

    #[test]
    fn prefers_seagate_raw_string_over_packed_raw_integer() {
        let output = r#"{
            "ata_smart_attributes": {
                "table": [
                    {
                        "id": 194,
                        "name": "Temperature_Celsius",
                        "raw": {
                            "value": 81604378668,
                            "string": "44 (0 19 0 0 0)"
                        }
                    }
                ]
            }
        }"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 44,
                source: SmartTemperatureSource::JsonAtaSmartAttribute,
            })
        );
    }

    #[test]
    fn null_json_current_does_not_consume_a_later_number() {
        let output = r#"{
            "temperature": {
                "current": null,
                "power_cycle_min": 18
            },
            "ata_smart_attributes": {
                "table": [
                    {
                        "name": "Temperature_Celsius",
                        "raw": { "string": "42" }
                    }
                ]
            }
        }"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 42,
                source: SmartTemperatureSource::JsonAtaSmartAttribute,
            })
        );
    }

    #[test]
    fn uses_highest_temperature_when_json_attributes_disagree() {
        let output = r#"{
            "ata_smart_attributes": {
                "table": [
                    {
                        "name": "Airflow_Temperature_Cel",
                        "raw": { "string": "41" }
                    },
                    {
                        "name": "Temperature_Celsius",
                        "raw": { "string": "44" }
                    }
                ]
            }
        }"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 44,
                source: SmartTemperatureSource::JsonAtaSmartAttribute,
            })
        );
    }

    #[test]
    fn parses_ata_smart_text_attribute() {
        let output = r#"
ID# ATTRIBUTE_NAME          FLAG     VALUE WORST THRESH TYPE      UPDATED  WHEN_FAILED RAW_VALUE
194 Temperature_Celsius     0x0022   065   054   000    Old_age   Always       -       35 (Min/Max 20/49)
"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 35,
                source: SmartTemperatureSource::TextAttribute,
            })
        );
    }

    #[test]
    fn ignores_non_temperature_attributes_from_real_seagate_output() {
        let output = r#"
  7 Seek_Error_Rate         0x000f   083   060   045    Pre-fail  Always       -       194264277
190 Airflow_Temperature_Cel 0x0022   056   054   040    Old_age   Always       -       44 (Min/Max 36/46)
194 Temperature_Celsius     0x0022   044   046   000    Old_age   Always       -       44 (0 20 0 0 0)
"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 44,
                source: SmartTemperatureSource::TextAttribute,
            })
        );
    }

    #[test]
    fn uses_highest_temperature_when_text_attributes_disagree() {
        let output = r#"
190 Airflow_Temperature_Cel 0x0022   059   056   040    Old_age   Always       -       41 (Min/Max 34/44)
194 Temperature_Celsius     0x0022   044   046   000    Old_age   Always       -       44 (0 20 0 0 0)
"#;

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 44,
                source: SmartTemperatureSource::TextAttribute,
            })
        );
    }

    #[test]
    fn parses_current_drive_temperature_text() {
        let output = "Current Drive Temperature:     31 C\n";

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 31,
                source: SmartTemperatureSource::TextCurrentDriveTemperature,
            })
        );
    }

    #[test]
    fn parses_nvme_temperature_text() {
        let output = "Temperature:                        33 Celsius\n";

        assert_eq!(
            parse_smart_temperature(output),
            Some(DriveTemperature {
                current_celsius: 33,
                source: SmartTemperatureSource::TextNvmeTemperature,
            })
        );
    }

    #[test]
    fn returns_none_when_temperature_is_absent() {
        let output = r#"{
            "smartctl": {
                "messages": [
                    {
                        "string": "Smartctl open device failed",
                        "severity": "error"
                    }
                ]
            }
        }"#;

        assert_eq!(parse_smart_temperature(output), None);
    }

    #[test]
    fn accepts_temperature_even_when_smartctl_has_health_status_bits() {
        let output = include_str!("../tests/fixtures/smartctl-seagate-ata.json");

        assert_eq!(
            classify_smartctl_output(output, "", Some(8)).unwrap(),
            SmartctlOutcome::Temperature(DriveTemperature {
                current_celsius: 46,
                source: SmartTemperatureSource::JsonTemperatureCurrent,
            })
        );
    }

    #[test]
    fn classifies_standby_without_treating_it_as_a_failure() {
        let output = include_str!("../tests/fixtures/smartctl-standby.json");

        assert_eq!(
            classify_smartctl_output(output, "", Some(3)).unwrap(),
            SmartctlOutcome::Standby
        );
    }

    #[test]
    fn standby_command_argument_alone_does_not_mean_drive_is_asleep() {
        let output = r#"{
            "smartctl": {
                "argv": ["smartctl", "-n", "standby,3,5"],
                "exit_status": 2
            }
        }"#;

        assert!(classify_smartctl_output(output, "", Some(2)).is_err());
    }

    #[test]
    fn selects_hottest_successful_drive_and_keeps_failures() {
        let devices = vec![
            "/dev/sdc".to_string(),
            "/dev/sdd".to_string(),
            "/dev/sde".to_string(),
        ];
        let poll = poll_drive_temperatures_with(&devices, |device| match device {
            "/dev/sdc" => Ok(SmartctlOutcome::Temperature(DriveTemperature {
                current_celsius: 41,
                source: SmartTemperatureSource::JsonTemperatureCurrent,
            })),
            "/dev/sdd" => Err("drive is in standby"),
            "/dev/sde" => Ok(SmartctlOutcome::Temperature(DriveTemperature {
                current_celsius: 47,
                source: SmartTemperatureSource::JsonTemperatureCurrent,
            })),
            _ => unreachable!(),
        });

        assert_eq!(poll.readings.len(), 2);
        assert_eq!(poll.failures.len(), 1);
        assert_eq!(poll.failures[0].device, "/dev/sdd");
        assert_eq!(poll.hottest().unwrap().device, "/dev/sde");
        assert_eq!(poll.hottest().unwrap().temperature.current_celsius, 47);
    }

    #[test]
    fn all_failed_drive_reads_produce_no_hottest_temperature() {
        let devices = vec!["/dev/sdc".to_string(), "/dev/sdd".to_string()];
        let poll =
            poll_drive_temperatures_with(&devices, |_| Err::<SmartctlOutcome, _>("unavailable"));

        assert!(poll.readings.is_empty());
        assert_eq!(poll.failures.len(), 2);
        assert!(poll.hottest().is_none());
    }

    #[test]
    fn standby_drives_are_excluded_without_becoming_failures() {
        let devices = vec!["/dev/sdc".to_string(), "/dev/sdd".to_string()];
        let poll =
            poll_drive_temperatures_with(&devices, |_| Ok::<_, &str>(SmartctlOutcome::Standby));

        assert!(poll.readings.is_empty());
        assert!(poll.failures.is_empty());
        assert_eq!(poll.standby_devices, devices);
        assert!(poll.hottest().is_none());
    }

    #[test]
    fn transient_failure_keeps_recent_temperature_until_grace_expires() {
        let started = Instant::now();
        let grace = Duration::from_secs(60);
        let mut state = DriveTemperatureState::default();
        let fresh = DriveTemperaturePoll {
            readings: vec![temperature_reading("/dev/sdc", 47)],
            ..DriveTemperaturePoll::default()
        };
        let failure = DriveTemperaturePoll {
            failures: vec![DriveTemperatureFailure {
                device: "/dev/sdc".to_string(),
                error: "temporary SMART error".to_string(),
            }],
            ..DriveTemperaturePoll::default()
        };

        state.update(&fresh, started, grace);
        let stale = state.update(&failure, started + Duration::from_secs(30), grace);

        assert_eq!(
            stale
                .hottest()
                .map(|reading| reading.temperature.current_celsius),
            Some(47)
        );
        assert_eq!(stale.stale_devices, vec!["/dev/sdc"]);
        assert!(!stale.requires_fail_safe());

        let expired = state.update(&failure, started + Duration::from_secs(61), grace);
        assert!(expired.readings.is_empty());
        assert_eq!(expired.fail_safe_devices, vec!["/dev/sdc"]);
        assert!(expired.requires_fail_safe());
    }

    #[test]
    fn standby_clears_cached_temperature_without_requesting_fail_safe() {
        let started = Instant::now();
        let grace = Duration::from_secs(60);
        let mut state = DriveTemperatureState::default();
        let fresh = DriveTemperaturePoll {
            readings: vec![temperature_reading("/dev/sdc", 47)],
            ..DriveTemperaturePoll::default()
        };
        let standby = DriveTemperaturePoll {
            standby_devices: vec!["/dev/sdc".to_string()],
            ..DriveTemperaturePoll::default()
        };

        state.update(&fresh, started, grace);
        let effective = state.update(&standby, started + Duration::from_secs(30), grace);

        assert!(effective.readings.is_empty());
        assert!(!effective.requires_fail_safe());

        let failure = DriveTemperaturePoll {
            failures: vec![DriveTemperatureFailure {
                device: "/dev/sdc".to_string(),
                error: "unavailable".to_string(),
            }],
            ..DriveTemperaturePoll::default()
        };
        let unavailable = state.update(&failure, started + Duration::from_secs(31), grace);
        assert!(unavailable.requires_fail_safe());
    }

    #[test]
    fn uncached_partial_failure_requires_fail_safe_despite_other_readings() {
        let mut state = DriveTemperatureState::default();
        let poll = DriveTemperaturePoll {
            readings: vec![temperature_reading("/dev/sdc", 40)],
            failures: vec![DriveTemperatureFailure {
                device: "/dev/sdd".to_string(),
                error: "unavailable".to_string(),
            }],
            ..DriveTemperaturePoll::default()
        };

        let effective = state.update(&poll, Instant::now(), Duration::from_secs(60));

        assert_eq!(
            effective
                .hottest()
                .map(|reading| reading.temperature.current_celsius),
            Some(40)
        );
        assert_eq!(effective.fail_safe_devices, vec!["/dev/sdd"]);
        assert!(effective.requires_fail_safe());
    }

    #[test]
    fn successful_read_recovers_from_fail_safe() {
        let started = Instant::now();
        let grace = Duration::from_secs(60);
        let mut state = DriveTemperatureState::default();
        let failure = DriveTemperaturePoll {
            failures: vec![DriveTemperatureFailure {
                device: "/dev/sdc".to_string(),
                error: "unavailable".to_string(),
            }],
            ..DriveTemperaturePoll::default()
        };
        assert!(state.update(&failure, started, grace).requires_fail_safe());

        let recovered = DriveTemperaturePoll {
            readings: vec![temperature_reading("/dev/sdc", 42)],
            ..DriveTemperaturePoll::default()
        };
        let effective = state.update(&recovered, started + Duration::from_secs(30), grace);

        assert!(!effective.requires_fail_safe());
        assert!(effective.stale_devices.is_empty());
        assert_eq!(
            effective
                .hottest()
                .map(|reading| reading.temperature.current_celsius),
            Some(42)
        );
    }
}
