use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::env_file::PinMap;
use crate::gpio_cdev::GpioLine;

const DUTY_SCALE: u32 = 1_000_000;
const DEFAULT_PWM_ROOT: &str = "/sys/class/pwm";
const DEFAULT_HARDWARE_PWM_CHANNEL: u32 = 0;
const DEFAULT_HARDWARE_PWM_PERIOD_NS: u64 = 40_000;
const DEFAULT_SOFTWARE_PWM_PERIOD: Duration = Duration::from_millis(25);
const DEFAULT_HARDWARE_PWM_ACTIVE_LOW: bool = true;
const DEFAULT_SOFTWARE_PWM_ACTIVE_LOW: bool = false;
const DEFAULT_CONSUMER: &str = "radxa-penta-top-hat-rs";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Duty {
    fraction: f64,
}

impl Duty {
    pub fn from_fraction(fraction: f64) -> Self {
        let fraction = if fraction.is_finite() { fraction } else { 0.0 };

        Self {
            fraction: fraction.clamp(0.0, 1.0),
        }
    }

    pub fn from_percent(percent: u8) -> Self {
        Self::from_fraction(f64::from(percent) / 100.0)
    }

    pub fn fraction(self) -> f64 {
        self.fraction
    }

    fn parts_per_million(self) -> u32 {
        (self.fraction * f64::from(DUTY_SCALE)).round() as u32
    }
}

pub trait FanPwmOutput {
    fn set_logical_duty(&mut self, duty: Duty) -> io::Result<()>;
}

#[derive(Debug)]
pub enum FanOutput {
    Sysfs(SysfsPwm),
    Software(SoftwarePwm),
}

impl FanOutput {
    pub fn open_from_pin_map(pin_map: &PinMap) -> io::Result<Self> {
        if pin_map.hardware_pwm {
            let pwmchip = pin_map
                .pwmchip
                .as_deref()
                .ok_or_else(|| missing_pin_map_value("PWMCHIP"))?;
            let pwm = SysfsPwm::open(
                pwmchip,
                DEFAULT_HARDWARE_PWM_CHANNEL,
                DEFAULT_HARDWARE_PWM_PERIOD_NS,
                DEFAULT_HARDWARE_PWM_ACTIVE_LOW,
            )?;
            Ok(Self::Sysfs(pwm))
        } else {
            let fan_chip = pin_map
                .fan_chip
                .as_deref()
                .ok_or_else(|| missing_pin_map_value("FAN_CHIP"))?;
            let fan_line = pin_map
                .fan_line
                .ok_or_else(|| missing_pin_map_value("FAN_LINE"))?;
            let pin = GpioLine::request_output(fan_chip, fan_line, false, DEFAULT_CONSUMER)?;
            let pwm = SoftwarePwm::start(
                pin,
                DEFAULT_SOFTWARE_PWM_PERIOD,
                DEFAULT_SOFTWARE_PWM_ACTIVE_LOW,
            );
            Ok(Self::Software(pwm))
        }
    }

    pub fn last_error(&self) -> Option<String> {
        match self {
            Self::Sysfs(_) => None,
            Self::Software(pwm) => pwm.last_error(),
        }
    }
}

impl FanPwmOutput for FanOutput {
    fn set_logical_duty(&mut self, duty: Duty) -> io::Result<()> {
        match self {
            Self::Sysfs(pwm) => pwm.set_logical_duty(duty),
            Self::Software(pwm) => pwm.set_logical_duty(duty),
        }
    }
}

fn missing_pin_map_value(name: &'static str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("missing required pin-map value {name}"),
    )
}

#[derive(Debug)]
pub struct SysfsPwm {
    pwm_path: PathBuf,
    period_ns: u64,
    active_low: bool,
}

impl SysfsPwm {
    pub fn open(chip: &str, channel: u32, period_ns: u64, active_low: bool) -> io::Result<Self> {
        Self::open_at(DEFAULT_PWM_ROOT, chip, channel, period_ns, active_low)
    }

    pub fn open_at(
        pwm_root: impl AsRef<Path>,
        chip: &str,
        channel: u32,
        period_ns: u64,
        active_low: bool,
    ) -> io::Result<Self> {
        let chip_path = pwm_root.as_ref().join(normalize_pwmchip(chip));
        let pwm_path = chip_path.join(format!("pwm{channel}"));

        if !pwm_path.exists() {
            fs::write(chip_path.join("export"), channel.to_string())?;
            wait_for_path(&pwm_path, Duration::from_millis(500))?;
        }

        write_optional(pwm_path.join("enable"), "0")?;
        fs::write(pwm_path.join("period"), period_ns.to_string())?;
        fs::write(pwm_path.join("duty_cycle"), "0")?;
        fs::write(pwm_path.join("enable"), "1")?;

        Ok(Self {
            pwm_path,
            period_ns,
            active_low,
        })
    }

    fn write_wire_duty(&self, wire_duty: Duty) -> io::Result<()> {
        let duty_cycle = duty_cycle_ns(self.period_ns, wire_duty);
        fs::write(self.pwm_path.join("duty_cycle"), duty_cycle.to_string())
    }
}

impl FanPwmOutput for SysfsPwm {
    fn set_logical_duty(&mut self, duty: Duty) -> io::Result<()> {
        self.write_wire_duty(to_wire_duty(duty, self.active_low))
    }
}

impl Drop for SysfsPwm {
    fn drop(&mut self) {
        let _ = fs::write(self.pwm_path.join("enable"), "0");
    }
}

pub trait DigitalOutput: Send {
    fn set_active(&mut self, active: bool) -> io::Result<()>;
}

#[derive(Debug)]
pub struct SoftwarePwm {
    duty: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl SoftwarePwm {
    pub fn start<P>(pin: P, period: Duration, active_low: bool) -> Self
    where
        P: DigitalOutput + 'static,
    {
        let duty = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let last_error = Arc::new(Mutex::new(None));

        let thread = {
            let duty = Arc::clone(&duty);
            let stop = Arc::clone(&stop);
            let last_error = Arc::clone(&last_error);

            thread::spawn(move || run_software_pwm(pin, period, active_low, duty, stop, last_error))
        };

        Self {
            duty,
            stop,
            last_error,
            thread: Some(thread),
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|error| error.clone())
    }
}

impl FanPwmOutput for SoftwarePwm {
    fn set_logical_duty(&mut self, duty: Duty) -> io::Result<()> {
        self.duty
            .store(duty.parts_per_million().min(DUTY_SCALE), Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for SoftwarePwm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_software_pwm<P>(
    mut pin: P,
    period: Duration,
    active_low: bool,
    duty: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
) where
    P: DigitalOutput,
{
    while !stop.load(Ordering::Relaxed) {
        let logical_duty = duty.load(Ordering::Relaxed).min(DUTY_SCALE);
        let wire_duty = if active_low {
            DUTY_SCALE - logical_duty
        } else {
            logical_duty
        };

        if let Err(err) = drive_pwm_cycle(&mut pin, period, wire_duty, &stop) {
            if let Ok(mut last_error) = last_error.lock() {
                *last_error = Some(err.to_string());
            }
            break;
        }
    }

    let _ = pin.set_active(false);
}

fn drive_pwm_cycle<P>(
    pin: &mut P,
    period: Duration,
    wire_duty: u32,
    stop: &AtomicBool,
) -> io::Result<()>
where
    P: DigitalOutput,
{
    if wire_duty == 0 {
        pin.set_active(false)?;
        sleep_until_stop(period, stop);
        return Ok(());
    }

    if wire_duty >= DUTY_SCALE {
        pin.set_active(true)?;
        sleep_until_stop(period, stop);
        return Ok(());
    }

    let active = duration_from_ppm(period, wire_duty);
    let inactive = period.saturating_sub(active);

    pin.set_active(true)?;
    sleep_until_stop(active, stop);
    pin.set_active(false)?;
    sleep_until_stop(inactive, stop);

    Ok(())
}

fn sleep_until_stop(duration: Duration, stop: &AtomicBool) {
    let slice = Duration::from_millis(5);
    let mut remaining = duration;

    while !stop.load(Ordering::Relaxed) && !remaining.is_zero() {
        let sleep_for = remaining.min(slice);
        thread::sleep(sleep_for);
        remaining = remaining.saturating_sub(sleep_for);
    }
}

fn normalize_pwmchip(chip: &str) -> String {
    if chip.starts_with("pwmchip") {
        chip.to_string()
    } else {
        format!("pwmchip{chip}")
    }
}

fn wait_for_path(path: &Path, timeout: Duration) -> io::Result<()> {
    let interval = Duration::from_millis(10);
    let mut waited = Duration::ZERO;

    while waited < timeout {
        if path.exists() {
            return Ok(());
        }

        thread::sleep(interval);
        waited += interval;
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("timed out waiting for {}", path.display()),
    ))
}

fn write_optional(path: impl AsRef<Path>, contents: &str) -> io::Result<()> {
    match fs::write(path, contents) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn to_wire_duty(duty: Duty, active_low: bool) -> Duty {
    if active_low {
        Duty::from_fraction(1.0 - duty.fraction())
    } else {
        duty
    }
}

fn duty_cycle_ns(period_ns: u64, duty: Duty) -> u64 {
    let duty_cycle = u128::from(period_ns) * u128::from(duty.parts_per_million());
    (duty_cycle / u128::from(DUTY_SCALE)) as u64
}

fn duration_from_ppm(period: Duration, duty_ppm: u32) -> Duration {
    let nanos = period.as_nanos() * u128::from(duty_ppm.min(DUTY_SCALE));
    let nanos = nanos / u128::from(DUTY_SCALE);
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_duty_fraction() {
        assert_eq!(Duty::from_fraction(-1.0).fraction(), 0.0);
        assert_eq!(Duty::from_fraction(0.42).fraction(), 0.42);
        assert_eq!(Duty::from_fraction(2.0).fraction(), 1.0);
        assert_eq!(Duty::from_fraction(f64::NAN).fraction(), 0.0);
    }

    #[test]
    fn converts_logical_to_wire_duty() {
        assert_eq!(
            to_wire_duty(Duty::from_fraction(0.25), false).fraction(),
            0.25
        );
        assert_eq!(
            to_wire_duty(Duty::from_fraction(0.25), true).fraction(),
            0.75
        );
    }

    #[test]
    fn calculates_sysfs_duty_cycle_nanoseconds() {
        assert_eq!(duty_cycle_ns(40_000, Duty::from_fraction(0.0)), 0);
        assert_eq!(duty_cycle_ns(40_000, Duty::from_fraction(0.25)), 10_000);
        assert_eq!(duty_cycle_ns(40_000, Duty::from_fraction(1.0)), 40_000);
    }

    #[test]
    fn calculates_software_pwm_durations() {
        let period = Duration::from_millis(25);

        assert_eq!(duration_from_ppm(period, 0), Duration::ZERO);
        assert_eq!(
            duration_from_ppm(period, 500_000),
            Duration::from_micros(12_500)
        );
        assert_eq!(duration_from_ppm(period, 1_000_000), period);
    }

    #[test]
    fn normalizes_pwmchip_names() {
        assert_eq!(normalize_pwmchip("14"), "pwmchip14");
        assert_eq!(normalize_pwmchip("pwmchip1"), "pwmchip1");
    }
}
