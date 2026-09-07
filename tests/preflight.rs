use std::io::Write;
use std::process::{Command, Output, Stdio};

fn preflight(pins: &str) -> Output {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("radxa-preflight-{}-{id}.env", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(pins.as_bytes()).unwrap();
    drop(file);
    let result = Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/packaging/debian/validate-config"
        ))
        .arg(&path)
        .stdin(Stdio::null())
        .output();
    std::fs::remove_file(path).unwrap();
    result.unwrap()
}

#[test]
fn unavailable_oled_does_not_block_fan_preflight() {
    // /dev/null stands in for an available character device; no GPIO is opened.
    let result = preflight(
        "SDA=SDA\nSCL=SCL\nOLED_I2C_DEVICE=/dev/null/missing\nFAN_CHIP=/dev/null\nFAN_LINE=27\nHARDWARE_PWM=0\n",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn incomplete_optional_oled_map_is_left_to_the_daemon() {
    let result = preflight("SDA=SDA\nFAN_CHIP=/dev/null\nFAN_LINE=27\nHARDWARE_PWM=0\n");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn missing_fan_device_still_blocks_startup() {
    let result = preflight("FAN_CHIP=/dev/null/missing\nFAN_LINE=27\nHARDWARE_PWM=0\n");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("fan GPIO device is unavailable"));
}
