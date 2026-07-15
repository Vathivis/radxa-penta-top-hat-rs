use std::fs;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::temp::read_cpu_temp_c;

pub const STATUS_PAGE_COUNT: usize = 3;

pub fn status_page(
    index: usize,
    cpu_temp_path: &Path,
    disks: &[String],
    fahrenheit: bool,
    fan_percent: u8,
) -> Vec<String> {
    match index % STATUS_PAGE_COUNT {
        0 => overview_page(cpu_temp_path, fahrenheit, fan_percent),
        1 => resource_page(),
        _ => disk_page(disks),
    }
}

fn overview_page(cpu_temp_path: &Path, fahrenheit: bool, fan_percent: u8) -> Vec<String> {
    vec![
        format_uptime(read_uptime_seconds()),
        format_temperature_and_fan(read_cpu_temp_c(cpu_temp_path).ok(), fahrenheit, fan_percent),
        format!("IP {}", primary_ipv4().unwrap_or_else(|| "--".to_string())),
    ]
}

fn resource_page() -> Vec<String> {
    let load = read_load_average()
        .map(|load| format!("LOAD {load:.2}"))
        .unwrap_or_else(|| "LOAD --".to_string());
    let memory = read_memory_mib()
        .map(|(used, total)| format!("MEM {used}/{total}M"))
        .unwrap_or_else(|| "MEM --".to_string());

    vec![load, memory]
}

fn disk_page(disks: &[String]) -> Vec<String> {
    let mut lines = Vec::with_capacity(4);
    lines.push(format_disk_line("ROOT", Path::new("/")));

    for disk in disks.iter().take(3) {
        let path = normalize_device_path(disk);
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(disk)
            .to_ascii_uppercase();
        lines.push(format_disk_line(&label, &path));
    }

    lines
}

fn format_disk_line(label: &str, path: &Path) -> String {
    match disk_usage_percent(path) {
        Some(percent) => format!("{label} {percent}%"),
        None => format!("{label} --"),
    }
}

fn normalize_device_path(value: &str) -> PathBuf {
    let path = Path::new(value.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new("/dev").join(path)
    }
}

fn disk_usage_percent(path: &Path) -> Option<u8> {
    let output = Command::new("df").arg("-P").arg(path).output().ok()?;
    parse_df_percent(&String::from_utf8_lossy(&output.stdout))
}

fn parse_df_percent(output: &str) -> Option<u8> {
    let line = output.lines().rfind(|line| !line.trim().is_empty())?;
    let fields: Vec<_> = line.split_whitespace().collect();
    let percent = fields
        .get(fields.len().checked_sub(2)?)?
        .strip_suffix('%')?;
    percent.parse().ok()
}

fn read_uptime_seconds() -> Option<u64> {
    let raw = fs::read_to_string("/proc/uptime").ok()?;
    let seconds = raw.split_whitespace().next()?.parse::<f64>().ok()?;
    if seconds.is_finite() && seconds >= 0.0 {
        Some(seconds as u64)
    } else {
        None
    }
}

fn format_uptime(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return "UP --".to_string();
    };
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;

    if days > 0 {
        format!("UP {days}D {hours:02}:{minutes:02}")
    } else {
        format!("UP {hours:02}:{minutes:02}")
    }
}

fn format_temperature_and_fan(celsius: Option<f64>, fahrenheit: bool, fan_percent: u8) -> String {
    let fan_percent = fan_percent.min(100);
    let Some(celsius) = celsius else {
        return format!("CPU -- FAN {fan_percent}%");
    };

    if fahrenheit {
        format!("CPU {:.1}F FAN {fan_percent}%", celsius * 1.8 + 32.0)
    } else {
        format!("CPU {celsius:.1}C FAN {fan_percent}%")
    }
}

fn read_load_average() -> Option<f64> {
    fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn read_memory_mib() -> Option<(u64, u64)> {
    parse_memory_mib(&fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_memory_mib(input: &str) -> Option<(u64, u64)> {
    let mut total_kib = None;
    let mut available_kib = None;

    for line in input.lines() {
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else {
            continue;
        };
        match key {
            "MemTotal:" => total_kib = fields.next()?.parse::<u64>().ok(),
            "MemAvailable:" => available_kib = fields.next()?.parse::<u64>().ok(),
            _ => {}
        }
    }

    let total_kib = total_kib?;
    let available_kib = available_kib?;
    Some((
        total_kib.saturating_sub(available_kib) / 1024,
        total_kib / 1024,
    ))
}

fn primary_ipv4() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_uptime_with_days() {
        assert_eq!(format_uptime(Some(183_720)), "UP 2D 03:02");
        assert_eq!(format_uptime(Some(3_720)), "UP 01:02");
    }

    #[test]
    fn formats_temperature_and_fan_in_configured_unit() {
        let celsius = format_temperature_and_fan(Some(50.0), false, 75);
        let fahrenheit = format_temperature_and_fan(Some(50.0), true, 100);

        assert_eq!(celsius, "CPU 50.0C FAN 75%");
        assert_eq!(fahrenheit, "CPU 122.0F FAN 100%");
        assert_eq!(fahrenheit.chars().count(), 19);
        assert_eq!(
            format_temperature_and_fan(None, false, 25),
            "CPU -- FAN 25%"
        );
    }

    #[test]
    fn clamps_displayed_fan_percent() {
        assert_eq!(
            format_temperature_and_fan(Some(50.0), false, 255),
            "CPU 50.0C FAN 100%"
        );
    }

    #[test]
    fn parses_memory_totals() {
        let input = "MemTotal:        8192000 kB\n\nMemFree: 100 kB\nMemAvailable:    3072000 kB\n";
        assert_eq!(parse_memory_mib(input), Some((5000, 8000)));
    }

    #[test]
    fn parses_posix_df_percentage() {
        let output = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sdb2 28799488 1000 2000 73% /\n";
        assert_eq!(parse_df_percent(output), Some(73));
    }

    #[test]
    fn normalizes_configured_disk_names() {
        assert_eq!(normalize_device_path("sdd1"), PathBuf::from("/dev/sdd1"));
        assert_eq!(
            normalize_device_path("/dev/sdc1"),
            PathBuf::from("/dev/sdc1")
        );
    }
}
