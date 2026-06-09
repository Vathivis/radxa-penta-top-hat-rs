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

    Some(DriveTemperature {
        current_celsius,
        source: SmartTemperatureSource::JsonTemperatureCurrent,
    })
}

fn parse_json_ata_temperature_attribute(input: &str) -> Option<DriveTemperature> {
    let mut offset = 0;

    while let Some(name_pos) = find_json_key(input, "name", offset) {
        let Some(name) = json_string_after_key(input, "name", name_pos) else {
            offset = name_pos + 1;
            continue;
        };

        if is_temperature_attribute_name(&name) {
            let object_start = input[..name_pos].rfind('{')?;
            let object_end = matching_brace(input, object_start)?;
            let object = &input[object_start..=object_end];

            if let Some(raw) = json_object_after_key(object, "raw", 0)
                && let Some(current_celsius) = json_i32_after_key(raw, "value", 0)
            {
                return Some(DriveTemperature {
                    current_celsius,
                    source: SmartTemperatureSource::JsonAtaSmartAttribute,
                });
            }

            if let Some(raw) = json_object_after_key(object, "raw", 0)
                && let Some(raw_string) = json_string_after_key(raw, "string", 0)
                && let Some(current_celsius) = first_i32(&raw_string)
            {
                return Some(DriveTemperature {
                    current_celsius,
                    source: SmartTemperatureSource::JsonAtaSmartAttribute,
                });
            }
        }

        offset = name_pos + 1;
    }

    None
}

fn parse_text_temperature(output: &str) -> Option<DriveTemperature> {
    let mut best_attribute_temperature: Option<DriveTemperature> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if lower.starts_with("current drive temperature:")
            && let Some(current_celsius) = first_i32_after_colon(trimmed)
        {
            return Some(DriveTemperature {
                current_celsius,
                source: SmartTemperatureSource::TextCurrentDriveTemperature,
            });
        }

        if lower.starts_with("temperature:")
            && lower.contains("celsius")
            && let Some(current_celsius) = first_i32_after_colon(trimmed)
        {
            return Some(DriveTemperature {
                current_celsius,
                source: SmartTemperatureSource::TextNvmeTemperature,
            });
        }

        if is_temperature_attribute_line(&lower)
            && let Some(current_celsius) = smart_attribute_raw_value(trimmed)
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
    let key_pos = find_json_key(input, key, start)?;
    let colon = input[key_pos..].find(':')? + key_pos;
    let object_start = input[colon + 1..]
        .find(|c: char| !c.is_ascii_whitespace())
        .map(|idx| colon + 1 + idx)?;

    if input.as_bytes().get(object_start) != Some(&b'{') {
        return None;
    }

    let object_end = matching_brace(input, object_start)?;
    Some(&input[object_start..=object_end])
}

fn json_i32_after_key(input: &str, key: &str, start: usize) -> Option<i32> {
    let key_pos = find_json_key(input, key, start)?;
    let colon = input[key_pos..].find(':')? + key_pos;
    let value_start = input[colon + 1..]
        .find(|c: char| c == '-' || c.is_ascii_digit())
        .map(|idx| colon + 1 + idx)?;
    let rest = &input[value_start..];
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
    let key_pos = find_json_key(input, key, start)?;
    let colon = input[key_pos..].find(':')? + key_pos;
    let quote_start = input[colon + 1..].find('"')? + colon + 1;
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
}
