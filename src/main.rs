use std::env;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use radxa_penta_top_hat_rs::button::ButtonRuntime;
use radxa_penta_top_hat_rs::cli::Args;
use radxa_penta_top_hat_rs::config::{Config, FanCurveConfig};
use radxa_penta_top_hat_rs::env_file::PinMap;
use radxa_penta_top_hat_rs::fan::{
    DutyStabilizer, FanDecision, FanLevel, duty_for_temperature, level_for_temperature,
};
use radxa_penta_top_hat_rs::oled::{OledRuntime, OledSignal};
use radxa_penta_top_hat_rs::pwm::{Duty, FanOutput, FanPwmOutput};
use radxa_penta_top_hat_rs::shutdown;
use radxa_penta_top_hat_rs::smart::{DriveTemperatureFailure, poll_drive_temperatures};
use radxa_penta_top_hat_rs::temp::read_cpu_temp_c;

#[derive(Debug, Clone, Eq, PartialEq)]
struct DriveLogSnapshot {
    level: Option<FanLevel>,
    duty_percent: Option<u8>,
    standby_devices: Vec<String>,
    failures: Vec<DriveFailureLogKey>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DriveFailureLogKey {
    device: String,
    error: String,
}

const FAN_LOG_IMMEDIATE_DUTY_CHANGE: u8 = 10;
const FAN_LOG_MIN_INTERVAL: Duration = Duration::from_secs(10);
const FAN_LOG_MAX_QUIET: Duration = Duration::from_secs(60);
const DRIVE_LOG_IMMEDIATE_DUTY_CHANGE: u8 = 10;
const DRIVE_LOG_MAX_QUIET: Duration = Duration::from_secs(600);

#[derive(Debug, Default)]
struct DriveLogState {
    last: Option<DriveLogSnapshot>,
    last_logged_at: Option<Instant>,
}

impl DriveLogState {
    fn should_log(&mut self, snapshot: DriveLogSnapshot, now: Instant) -> bool {
        let should_log = match self.last.as_ref() {
            None => true,
            Some(last) => {
                let availability_changed = last.level.is_some() != snapshot.level.is_some();
                let status_changed = last.standby_devices != snapshot.standby_devices
                    || last.failures != snapshot.failures;
                let level_changed = last.level != snapshot.level;
                let duty_change = match (last.duty_percent, snapshot.duty_percent) {
                    (Some(last), Some(current)) => last.abs_diff(current),
                    _ => 0,
                };
                let quiet_period_elapsed = self.last_logged_at.is_none_or(|last_logged_at| {
                    now.duration_since(last_logged_at) >= DRIVE_LOG_MAX_QUIET
                });

                availability_changed
                    || status_changed
                    || level_changed
                    || duty_change >= DRIVE_LOG_IMMEDIATE_DUTY_CHANGE
                    || (duty_change > 0 && quiet_period_elapsed)
            }
        };

        if should_log {
            self.last = Some(snapshot);
            self.last_logged_at = Some(now);
        }

        should_log
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct FanLogSnapshot {
    cpu_level: FanLevel,
    drive_level: Option<FanLevel>,
    level: FanLevel,
    duty_percent: u8,
    enabled: bool,
    at_maximum: bool,
}

#[derive(Debug, Default)]
struct FanLogState {
    last: Option<FanLogSnapshot>,
    last_logged_at: Option<Instant>,
}

impl FanLogState {
    fn should_log(
        &mut self,
        decision: &FanDecision,
        duty_percent: u8,
        enabled: bool,
        maximum_duty: u8,
        now: Instant,
    ) -> bool {
        let snapshot = FanLogSnapshot {
            cpu_level: decision.cpu_level,
            drive_level: decision.drive_level,
            level: decision.level,
            duty_percent,
            enabled,
            at_maximum: duty_percent >= maximum_duty,
        };
        let should_log = match self.last {
            None => true,
            Some(last) => {
                let level_changed = last.cpu_level != snapshot.cpu_level
                    || last.drive_level != snapshot.drive_level
                    || last.level != snapshot.level;
                let immediate_boundary_changed = last.enabled != snapshot.enabled
                    || (last.duty_percent == 0) != (snapshot.duty_percent == 0);
                let entered_maximum = !last.at_maximum && snapshot.at_maximum;
                let left_maximum = last.at_maximum && !snapshot.at_maximum;
                let duty_change = last.duty_percent.abs_diff(snapshot.duty_percent);
                let minimum_interval_elapsed = self.last_logged_at.is_none_or(|last_logged_at| {
                    now.duration_since(last_logged_at) >= FAN_LOG_MIN_INTERVAL
                });
                let quiet_period_elapsed = self.last_logged_at.is_none_or(|last_logged_at| {
                    now.duration_since(last_logged_at) >= FAN_LOG_MAX_QUIET
                });

                immediate_boundary_changed
                    || entered_maximum
                    || (duty_change >= FAN_LOG_IMMEDIATE_DUTY_CHANGE && minimum_interval_elapsed)
                    || ((level_changed || left_maximum || duty_change > 0) && quiet_period_elapsed)
            }
        };

        if should_log {
            self.last = Some(snapshot);
            self.last_logged_at = Some(now);
        }

        should_log
    }
}

struct FanLogLine<'a> {
    decision: &'a FanDecision,
    commanded_duty_percent: u8,
    enabled: bool,
}

impl fmt::Display for FanLogLine<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.decision.cpu_temp_c.is_finite() {
            write!(
                output,
                "fan: cpu={:.1}C/{}/{}%",
                self.decision.cpu_temp_c,
                fan_level_label(self.decision.cpu_level),
                self.decision.cpu_duty_percent
            )?;
        } else {
            write!(
                output,
                "fan: cpu=err/{}/{}%",
                fan_level_label(self.decision.cpu_level),
                self.decision.cpu_duty_percent
            )?;
        }

        match (
            self.decision.hottest_drive_temp_c,
            self.decision.drive_level,
            self.decision.drive_duty_percent,
        ) {
            (Some(temp_c), Some(level), Some(duty_percent)) => write!(
                output,
                " drv={temp_c:.1}C/{}/{}%",
                fan_level_label(level),
                duty_percent
            )?,
            _ => write!(output, " drv=-")?,
        }

        write!(
            output,
            " target={}/{}% out={}% {}",
            fan_level_label(self.decision.level),
            self.decision.duty_percent,
            self.commanded_duty_percent,
            if self.enabled { "on" } else { "off" }
        )
    }
}

fn fan_level_label(level: FanLevel) -> &'static str {
    match level {
        FanLevel::Off => "off",
        FanLevel::Lv0 => "L0",
        FanLevel::Lv1 => "L1",
        FanLevel::Lv2 => "L2",
        FanLevel::Lv3 => "L3",
    }
}

fn short_device_name(device: &str) -> &str {
    device.strip_prefix("/dev/").unwrap_or(device)
}

fn drive_failure_log_key(failure: &DriveTemperatureFailure) -> DriveFailureLogKey {
    let error = if failure.error.starts_with("smartctl timed out after ") {
        "smartctl timeout".to_string()
    } else {
        failure.error.clone()
    };

    DriveFailureLogKey {
        device: failure.device.clone(),
        error,
    }
}

#[derive(Debug, Default)]
struct CpuTempLogState {
    last_error: Option<String>,
}

impl CpuTempLogState {
    fn read_for_fan(&mut self, path: &Path) -> f64 {
        self.value_or_fail_safe(read_cpu_temp_c(path).map_err(|err| err.to_string()))
    }

    fn value_or_fail_safe(&mut self, result: Result<f64, String>) -> f64 {
        match result {
            Ok(temp_c) => {
                if self.last_error.take().is_some() {
                    eprintln!("cpu-temperature: reading recovered");
                }
                temp_c
            }
            Err(error) => {
                if self.last_error.as_deref() != Some(&error) {
                    eprintln!(
                        "cpu-temperature: read failed; commanding fail-safe maximum duty: {error}"
                    );
                    self.last_error = Some(error);
                }
                f64::NAN
            }
        }
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse().map_err(|err| format!("{err}\n\n{}", usage()))?;

    if args.help {
        print!("{}", usage());
        return Ok(());
    }

    let config = Config::from_file_or_defaults(&args.config_path).map_err(|err| err.to_string())?;
    let pin_map = PinMap::from_file_or_empty(&args.env_file).map_err(|err| err.to_string())?;

    if args.dry_run {
        eprintln!(
            "dry-run: config={}, env-file={}, hardware-pwm={}",
            args.config_path.display(),
            args.env_file.display(),
            pin_map.hardware_pwm
        );
    }

    if let Some(percent) = args.test_fan_duty {
        validate_test_fan_duty(percent, config.fan_curve)?;
        return run_fan_test(&args, &pin_map, percent);
    }

    let mut output = if args.dry_run || args.once {
        None
    } else {
        shutdown::install_signal_handlers().map_err(|err| err.to_string())?;
        Some(FanOutput::open_from_pin_map(&pin_map).map_err(|err| err.to_string())?)
    };
    let mut last_duty_percent = None;
    let mut fan_log_state = FanLogState::default();
    let mut cpu_temp_log_state = CpuTempLogState::default();
    let mut duty_stabilizer = DutyStabilizer::default();
    let fan_enabled = Arc::new(AtomicBool::new(true));
    let initial_decision = FanDecision::cpu_only_with_curve(
        cpu_temp_log_state.read_for_fan(&args.cpu_temp_path),
        config.fan,
        config.fan_curve,
    );
    let initial_fan_percent = duty_stabilizer.force(
        initial_decision.duty_percent,
        config.fan_curve,
        Instant::now(),
    );
    let fan_percent = Arc::new(AtomicU8::new(initial_fan_percent));
    let oled_signal = Arc::new(OledSignal::default());
    let maximum_duty = if config.fan_curve.enabled {
        config.fan_curve.max_duty
    } else {
        100
    };

    // Establish CPU-safe output before optional OLED and button initialization.
    if let Some(output) = output.as_mut()
        && apply_changed_duty(output, initial_fan_percent, &mut last_duty_percent)?
    {
        log_fan_change(
            &initial_decision,
            initial_fan_percent,
            true,
            maximum_duty,
            &mut fan_log_state,
            Instant::now(),
        );
    }

    let oled_runtime = if args.dry_run || args.once {
        None
    } else {
        match OledRuntime::start(
            &pin_map,
            config.oled,
            args.cpu_temp_path.clone(),
            config.disks.clone(),
            Arc::clone(&oled_signal),
            Arc::clone(&fan_percent),
        ) {
            Ok(runtime) => {
                if runtime.is_some() {
                    eprintln!("oled: initialized");
                } else {
                    eprintln!("oled: disabled because SDA/SCL are not configured");
                }
                runtime
            }
            Err(err) => {
                eprintln!("oled: initialization failed; continuing without display: {err}");
                None
            }
        }
    };
    let button_runtime = if args.dry_run || args.once {
        None
    } else {
        ButtonRuntime::start(
            &pin_map,
            config.key.clone(),
            config.time,
            Arc::clone(&fan_enabled),
            Arc::clone(&oled_signal),
        )
        .map_err(|err| err.to_string())?
    };
    let drive_polling_enabled = config.fan_drives.enabled && !config.fan_drives.devices.is_empty();
    let drive_poll_interval = Duration::from_secs(config.fan_drives.poll_seconds);
    let mut last_drive_poll = None;
    let mut hottest_drive_temp_c = None;
    let mut drive_log_state = DriveLogState::default();

    if config.fan_curve.enabled {
        eprintln!(
            "fan-curve: enabled duties={}/{}/{}/{} tail={:?} max_duty={} hysteresis={} ramp_down={}/s",
            config.fan_curve.duty0,
            config.fan_curve.duty1,
            config.fan_curve.duty2,
            config.fan_curve.duty3,
            config.fan_curve.tail,
            config.fan_curve.max_duty,
            config.fan_curve.hysteresis,
            config.fan_curve.ramp_down
        );
    }

    if config.fan_drives.enabled {
        if config.fan_drives.devices.is_empty() {
            eprintln!(
                "fan-drives: enabled but no devices are configured; using CPU temperature only"
            );
        } else {
            eprintln!(
                "fan-drives: enabled devices={} poll_seconds={}",
                config.fan_drives.devices.join(","),
                config.fan_drives.poll_seconds
            );
        }
    }

    while !shutdown::requested() {
        let mut temp_c = cpu_temp_log_state.read_for_fan(&args.cpu_temp_path);
        let mut decision = FanDecision::from_temperatures_with_curve(
            temp_c,
            config.fan,
            hottest_drive_temp_c,
            config.fan_drives.thresholds,
            config.fan_curve,
        );
        let mut enabled = fan_enabled.load(Ordering::SeqCst);
        let mut commanded_duty_percent = commanded_duty(
            &decision,
            enabled,
            config.fan_curve,
            &mut duty_stabilizer,
            Instant::now(),
        );
        fan_percent.store(commanded_duty_percent, Ordering::SeqCst);

        // Establish a CPU-safe duty before the first, potentially slower, SMART batch.
        if let Some(output) = output.as_mut() {
            apply_changed_duty(output, commanded_duty_percent, &mut last_duty_percent)?;
            log_fan_change(
                &decision,
                commanded_duty_percent,
                enabled,
                maximum_duty,
                &mut fan_log_state,
                Instant::now(),
            );
        }

        let mut polled_drives = false;
        if drive_polling_enabled
            && last_drive_poll
                .map(|last: Instant| last.elapsed() >= drive_poll_interval)
                .unwrap_or(true)
        {
            let poll = poll_drive_temperatures(&config.fan_drives.devices);
            last_drive_poll = Some(Instant::now());
            polled_drives = true;

            hottest_drive_temp_c = poll
                .hottest()
                .map(|reading| f64::from(reading.temperature.current_celsius));
            let drive_level = hottest_drive_temp_c
                .map(|temp_c| level_for_temperature(temp_c, config.fan_drives.thresholds));
            let drive_duty = hottest_drive_temp_c.map(|temp_c| {
                duty_for_temperature(temp_c, config.fan_drives.thresholds, config.fan_curve)
            });
            let log_snapshot = DriveLogSnapshot {
                level: drive_level,
                duty_percent: drive_duty,
                standby_devices: poll.standby_devices.clone(),
                failures: poll.failures.iter().map(drive_failure_log_key).collect(),
            };

            if drive_log_state.should_log(log_snapshot, Instant::now()) {
                for failure in &poll.failures {
                    eprintln!(
                        "fan-drives: device={} read failed: {}",
                        failure.device, failure.error
                    );
                }

                if let Some(hottest) = poll.hottest() {
                    let readings = poll
                        .readings
                        .iter()
                        .map(|reading| {
                            format!(
                                "{}:{}",
                                short_device_name(&reading.device),
                                reading.temperature.current_celsius
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    eprintln!(
                        "fan-drives: tempC={} hot={}/{}C/{}/{}% standby={} errors={}",
                        readings,
                        short_device_name(&hottest.device),
                        hottest.temperature.current_celsius,
                        fan_level_label(drive_level.unwrap_or(FanLevel::Off)),
                        drive_duty.unwrap_or(0),
                        poll.standby_devices.len(),
                        poll.failures.len()
                    );
                } else if poll.failures.is_empty() {
                    eprintln!(
                        "fan-drives: all configured drives are in standby; using CPU temperature only"
                    );
                } else {
                    eprintln!(
                        "fan-drives: no configured drive temperatures available; using CPU temperature only"
                    );
                }
            }
        }

        if polled_drives {
            temp_c = cpu_temp_log_state.read_for_fan(&args.cpu_temp_path);
        }

        decision = FanDecision::from_temperatures_with_curve(
            temp_c,
            config.fan,
            hottest_drive_temp_c,
            config.fan_drives.thresholds,
            config.fan_curve,
        );
        enabled = fan_enabled.load(Ordering::SeqCst);
        commanded_duty_percent = commanded_duty(
            &decision,
            enabled,
            config.fan_curve,
            &mut duty_stabilizer,
            Instant::now(),
        );
        fan_percent.store(commanded_duty_percent, Ordering::SeqCst);

        if args.dry_run || args.once {
            println!(
                "cpu_temp_c={:.1} cpu_level={:?} cpu_duty_percent={} hottest_drive_temp_c={:?} drive_level={:?} drive_duty_percent={:?} fan_level={:?} target_duty_percent={} duty_percent={} active_low_duty={:.2}",
                decision.cpu_temp_c,
                decision.cpu_level,
                decision.cpu_duty_percent,
                decision.hottest_drive_temp_c,
                decision.drive_level,
                decision.drive_duty_percent,
                decision.level,
                decision.duty_percent,
                commanded_duty_percent,
                1.0 - f64::from(commanded_duty_percent) / 100.0
            );
        }

        if let Some(output) = output.as_mut() {
            apply_changed_duty(output, commanded_duty_percent, &mut last_duty_percent)?;
            log_fan_change(
                &decision,
                commanded_duty_percent,
                enabled,
                maximum_duty,
                &mut fan_log_state,
                Instant::now(),
            );
        }

        if args.once {
            return Ok(());
        }

        if let Some(err) = output.as_ref().and_then(FanOutput::last_error) {
            return Err(format!("software PWM error: {err}"));
        }

        if let Some(err) = button_runtime.as_ref().and_then(ButtonRuntime::last_error) {
            return Err(format!("button handler error: {err}"));
        }

        if let Some(err) = oled_runtime.as_ref().and_then(OledRuntime::take_error) {
            eprintln!("oled: runtime error; display disabled: {err}");
        }

        thread::sleep(Duration::from_secs(1));
    }

    if let Some(output) = output.as_mut() {
        fan_percent.store(0, Ordering::SeqCst);
        output
            .set_logical_duty(Duty::from_percent(0))
            .map_err(|err| err.to_string())?;
        eprintln!("shutdown requested: fan duty set to 0 percent");
    }

    Ok(())
}

fn commanded_duty(
    decision: &FanDecision,
    enabled: bool,
    curve: FanCurveConfig,
    stabilizer: &mut DutyStabilizer,
    now: Instant,
) -> u8 {
    if enabled {
        stabilizer.update(decision.duty_percent, curve, now)
    } else {
        stabilizer.force(0, curve, now)
    }
}

fn log_fan_change(
    decision: &FanDecision,
    commanded_duty_percent: u8,
    enabled: bool,
    maximum_duty: u8,
    log_state: &mut FanLogState,
    now: Instant,
) {
    if !log_state.should_log(decision, commanded_duty_percent, enabled, maximum_duty, now) {
        return;
    }

    eprintln!(
        "{}",
        FanLogLine {
            decision,
            commanded_duty_percent,
            enabled,
        }
    );
}

fn validate_test_fan_duty(percent: u8, curve: FanCurveConfig) -> Result<(), String> {
    if curve.enabled && percent > curve.max_duty {
        Err(format!(
            "test fan duty {percent}% exceeds configured fan-curve max_duty {}%",
            curve.max_duty
        ))
    } else {
        Ok(())
    }
}

fn apply_changed_duty<O>(
    output: &mut O,
    duty_percent: u8,
    last_duty_percent: &mut Option<u8>,
) -> Result<bool, String>
where
    O: FanPwmOutput,
{
    if *last_duty_percent == Some(duty_percent) {
        return Ok(false);
    }

    output
        .set_logical_duty(Duty::from_percent(duty_percent))
        .map_err(|err| err.to_string())?;
    *last_duty_percent = Some(duty_percent);

    Ok(true)
}

fn run_fan_test(args: &Args, pin_map: &PinMap, percent: u8) -> Result<(), String> {
    if args.dry_run {
        println!(
            "dry-run: would set fan duty to {} percent for {} seconds",
            percent, args.test_fan_seconds
        );
        return Ok(());
    }

    let mut output = FanOutput::open_from_pin_map(pin_map).map_err(|err| err.to_string())?;
    output
        .set_logical_duty(Duty::from_percent(percent))
        .map_err(|err| err.to_string())?;
    thread::sleep(Duration::from_secs(args.test_fan_seconds));
    output
        .set_logical_duty(Duty::from_percent(0))
        .map_err(|err| err.to_string())?;

    Ok(())
}

fn usage() -> String {
    let program = env::args()
        .next()
        .unwrap_or_else(|| "radxa-penta-top-hat-rs".to_string());
    radxa_penta_top_hat_rs::cli::usage(&program)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[derive(Default)]
    struct RecordingOutput {
        duties: Vec<u8>,
    }

    impl FanPwmOutput for RecordingOutput {
        fn set_logical_duty(&mut self, duty: Duty) -> io::Result<()> {
            self.duties.push((duty.fraction() * 100.0).round() as u8);
            Ok(())
        }
    }

    fn fan_decision(temp_c: f64) -> FanDecision {
        FanDecision::cpu_only(
            temp_c,
            radxa_penta_top_hat_rs::config::FanConfig {
                lv0: 40.0,
                lv1: 50.0,
                lv2: 60.0,
                lv3: 70.0,
            },
        )
    }

    #[test]
    fn applies_first_duty() {
        let mut output = RecordingOutput::default();
        let mut last_duty = None;

        assert!(apply_changed_duty(&mut output, 25, &mut last_duty).unwrap());

        assert_eq!(output.duties, vec![25]);
        assert_eq!(last_duty, Some(25));
    }

    #[test]
    fn skips_unchanged_duty() {
        let mut output = RecordingOutput::default();
        let mut last_duty = Some(50);

        assert!(!apply_changed_duty(&mut output, 50, &mut last_duty).unwrap());

        assert!(output.duties.is_empty());
        assert_eq!(last_duty, Some(50));
    }

    #[test]
    fn fan_disabled_commands_zero_duty() {
        let computed = FanDecision::cpu_only(
            55.0,
            radxa_penta_top_hat_rs::config::FanConfig {
                lv0: 40.0,
                lv1: 45.0,
                lv2: 50.0,
                lv3: 60.0,
            },
        );
        let mut stabilizer = DutyStabilizer::default();
        let commanded = commanded_duty(
            &computed,
            false,
            FanCurveConfig::default(),
            &mut stabilizer,
            Instant::now(),
        );

        assert_eq!(commanded, 0);
    }

    #[test]
    fn fan_test_respects_enabled_curve_maximum() {
        let curve = FanCurveConfig {
            enabled: true,
            max_duty: 80,
            ..FanCurveConfig::default()
        };

        assert!(validate_test_fan_duty(80, curve).is_ok());
        assert!(validate_test_fan_duty(81, curve).is_err());
        assert!(validate_test_fan_duty(100, FanCurveConfig::default()).is_ok());
    }

    #[test]
    fn fan_logging_suppresses_small_routine_changes() {
        let now = Instant::now();
        let decision = fan_decision(55.0);
        let mut state = FanLogState::default();

        assert!(state.should_log(&decision, 50, true, 100, now));
        assert!(!state.should_log(&decision, 51, true, 100, now + Duration::from_secs(1)));
        assert!(!state.should_log(&decision, 60, true, 100, now + Duration::from_secs(2)));
        assert!(state.should_log(&decision, 60, true, 100, now + FAN_LOG_MIN_INTERVAL));
    }

    #[test]
    fn fan_logging_reports_boundaries_and_periodic_drift() {
        let now = Instant::now();
        let decision = fan_decision(55.0);
        let mut state = FanLogState::default();

        assert!(state.should_log(&decision, 50, true, 100, now));
        assert!(state.should_log(&decision, 50, false, 100, now + Duration::from_secs(1)));
        assert!(!state.should_log(&decision, 51, false, 100, now + Duration::from_secs(2)));
        assert!(state.should_log(
            &decision,
            51,
            false,
            100,
            now + FAN_LOG_MAX_QUIET + Duration::from_secs(1)
        ));
    }

    #[test]
    fn fan_logging_reports_zero_and_maximum_boundaries_immediately() {
        let now = Instant::now();
        let decision = fan_decision(55.0);
        let mut state = FanLogState::default();

        assert!(state.should_log(&decision, 50, true, 100, now));
        assert!(state.should_log(&decision, 0, true, 100, now + Duration::from_secs(1)));
        assert!(state.should_log(&decision, 1, true, 100, now + Duration::from_secs(2)));
        assert!(state.should_log(&decision, 100, true, 100, now + Duration::from_secs(3)));
        assert!(!state.should_log(&decision, 99, true, 100, now + Duration::from_secs(4)));
    }

    #[test]
    fn fan_logging_suppresses_level_jitter_until_summary_interval() {
        let now = Instant::now();
        let lower = fan_decision(49.9);
        let upper = fan_decision(50.1);
        let mut state = FanLogState::default();

        assert!(state.should_log(&lower, 49, true, 100, now));
        assert!(!state.should_log(&upper, 50, true, 100, now + Duration::from_secs(1)));
        assert!(!state.should_log(&lower, 49, true, 100, now + Duration::from_secs(2)));
        assert!(state.should_log(&upper, 50, true, 100, now + FAN_LOG_MAX_QUIET));
    }

    #[test]
    fn compact_fan_log_preserves_control_details() {
        let thresholds = radxa_penta_top_hat_rs::config::FanConfig {
            lv0: 40.0,
            lv1: 50.0,
            lv2: 60.0,
            lv3: 70.0,
        };
        let decision = FanDecision::from_temperatures(55.0, thresholds, Some(45.0), thresholds);
        let line = FanLogLine {
            decision: &decision,
            commanded_duty_percent: 52,
            enabled: true,
        }
        .to_string();

        assert_eq!(
            line,
            "fan: cpu=55.0C/L1/50% drv=45.0C/L0/25% target=L1/50% out=52% on"
        );
        assert!(line.len() < 80);
    }

    #[test]
    fn compact_fan_log_formats_missing_drive_and_fail_safe() {
        let normal = fan_decision(55.0);
        let normal_line = FanLogLine {
            decision: &normal,
            commanded_duty_percent: 0,
            enabled: false,
        }
        .to_string();
        assert_eq!(
            normal_line,
            "fan: cpu=55.0C/L1/50% drv=- target=L1/50% out=0% off"
        );

        let fail_safe = fan_decision(f64::NAN);
        let fail_safe_line = FanLogLine {
            decision: &fail_safe,
            commanded_duty_percent: 100,
            enabled: true,
        }
        .to_string();
        assert_eq!(
            fail_safe_line,
            "fan: cpu=err/L3/100% drv=- target=L3/100% out=100% on"
        );
    }

    #[test]
    fn drive_logging_limits_small_duty_drift() {
        fn snapshot(duty_percent: u8) -> DriveLogSnapshot {
            DriveLogSnapshot {
                level: Some(FanLevel::Lv0),
                duty_percent: Some(duty_percent),
                standby_devices: Vec::new(),
                failures: Vec::new(),
            }
        }

        let now = Instant::now();
        let mut state = DriveLogState::default();

        assert!(state.should_log(snapshot(25), now));
        assert!(!state.should_log(snapshot(26), now + Duration::from_secs(30)));
        assert!(state.should_log(snapshot(26), now + DRIVE_LOG_MAX_QUIET));
        assert!(state.should_log(
            snapshot(40),
            now + DRIVE_LOG_MAX_QUIET + Duration::from_secs(30)
        ));
    }

    #[test]
    fn drive_logging_normalizes_timeout_duration() {
        let first = DriveTemperatureFailure {
            device: "/dev/sdc".to_string(),
            error: "smartctl timed out after 4998 milliseconds".to_string(),
        };
        let second = DriveTemperatureFailure {
            device: "/dev/sdc".to_string(),
            error: "smartctl timed out after 4971 milliseconds".to_string(),
        };

        assert_eq!(
            drive_failure_log_key(&first),
            drive_failure_log_key(&second)
        );
    }

    #[test]
    fn cpu_temperature_failure_uses_non_finite_fail_safe_and_recovers() {
        let mut state = CpuTempLogState::default();

        assert!(
            state
                .value_or_fail_safe(Err("sensor unavailable".to_string()))
                .is_nan()
        );
        assert_eq!(state.last_error.as_deref(), Some("sensor unavailable"));
        assert_eq!(state.value_or_fail_safe(Ok(52.5)), 52.5);
        assert!(state.last_error.is_none());
    }
}
