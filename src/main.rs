use std::env;
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
use radxa_penta_top_hat_rs::oled::OledRuntime;
use radxa_penta_top_hat_rs::pwm::{Duty, FanOutput, FanPwmOutput};
use radxa_penta_top_hat_rs::shutdown;
use radxa_penta_top_hat_rs::smart::{DriveTemperatureFailure, poll_drive_temperatures};
use radxa_penta_top_hat_rs::temp::read_cpu_temp_c;

#[derive(Debug, Clone, Eq, PartialEq)]
struct DriveLogState {
    level: Option<FanLevel>,
    duty_percent: Option<u8>,
    standby_devices: Vec<String>,
    failures: Vec<DriveTemperatureFailure>,
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
    let fan_enabled = Arc::new(AtomicBool::new(true));
    let initial_fan_percent = FanDecision::cpu_only_with_curve(
        read_cpu_temp_c(&args.cpu_temp_path).map_err(|err| err.to_string())?,
        config.fan,
        config.fan_curve,
    )
    .duty_percent;
    let fan_percent = Arc::new(AtomicU8::new(initial_fan_percent));
    let slide_requested = Arc::new(AtomicBool::new(false));
    let oled_runtime = if args.dry_run || args.once {
        None
    } else {
        match OledRuntime::start(
            &pin_map,
            config.oled,
            args.cpu_temp_path.clone(),
            config.disks.clone(),
            Arc::clone(&slide_requested),
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
            Arc::clone(&slide_requested),
        )
        .map_err(|err| err.to_string())?
    };
    let drive_polling_enabled = config.fan_drives.enabled && !config.fan_drives.devices.is_empty();
    let drive_poll_interval = Duration::from_secs(config.fan_drives.poll_seconds);
    let mut last_drive_poll = None;
    let mut hottest_drive_temp_c = None;
    let mut last_drive_log_state: Option<DriveLogState> = None;
    let mut duty_stabilizer = DutyStabilizer::default();

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
        let mut temp_c = read_cpu_temp_c(&args.cpu_temp_path).map_err(|err| err.to_string())?;
        let mut decision = FanDecision::from_temperatures_with_curve(
            temp_c,
            config.fan,
            hottest_drive_temp_c,
            config.fan_drives.thresholds,
            config.fan_curve,
        );
        let mut commanded_duty_percent = commanded_duty(
            &decision,
            &fan_enabled,
            config.fan_curve,
            &mut duty_stabilizer,
            Instant::now(),
        );
        fan_percent.store(commanded_duty_percent, Ordering::SeqCst);

        // Establish a CPU-safe duty before the first, potentially slower, SMART batch.
        if let Some(output) = output.as_mut()
            && apply_changed_duty(output, commanded_duty_percent, &mut last_duty_percent)?
        {
            log_fan_change(&decision, commanded_duty_percent, &fan_enabled);
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
            let log_state = DriveLogState {
                level: drive_level,
                duty_percent: drive_duty,
                standby_devices: poll.standby_devices.clone(),
                failures: poll.failures.clone(),
            };

            if last_drive_log_state.as_ref() != Some(&log_state) {
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
                                "{}={}C",
                                reading.device, reading.temperature.current_celsius
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    eprintln!(
                        "fan-drives: readings={} standby={} failed={} hottest_device={} hottest_temp_c={} drive_level={:?} drive_duty_percent={:?}",
                        readings,
                        poll.standby_devices.len(),
                        poll.failures.len(),
                        hottest.device,
                        hottest.temperature.current_celsius,
                        drive_level,
                        drive_duty
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

                last_drive_log_state = Some(log_state);
            }
        }

        if polled_drives {
            temp_c = read_cpu_temp_c(&args.cpu_temp_path).map_err(|err| err.to_string())?;
        }

        decision = FanDecision::from_temperatures_with_curve(
            temp_c,
            config.fan,
            hottest_drive_temp_c,
            config.fan_drives.thresholds,
            config.fan_curve,
        );
        commanded_duty_percent = commanded_duty(
            &decision,
            &fan_enabled,
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

        if let Some(output) = output.as_mut()
            && apply_changed_duty(output, commanded_duty_percent, &mut last_duty_percent)?
        {
            log_fan_change(&decision, commanded_duty_percent, &fan_enabled);
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
    fan_enabled: &AtomicBool,
    curve: FanCurveConfig,
    stabilizer: &mut DutyStabilizer,
    now: Instant,
) -> u8 {
    if fan_enabled.load(Ordering::SeqCst) {
        stabilizer.update(decision.duty_percent, curve, now)
    } else {
        stabilizer.force(0, curve, now)
    }
}

fn log_fan_change(decision: &FanDecision, commanded_duty_percent: u8, fan_enabled: &AtomicBool) {
    eprintln!(
        "fan: cpu_temp_c={:.1} cpu_level={:?} cpu_duty_percent={} hottest_drive_temp_c={:?} drive_level={:?} drive_duty_percent={:?} fan_level={:?} duty_percent={} enabled={}",
        decision.cpu_temp_c,
        decision.cpu_level,
        decision.cpu_duty_percent,
        decision.hottest_drive_temp_c,
        decision.drive_level,
        decision.drive_duty_percent,
        decision.level,
        commanded_duty_percent,
        fan_enabled.load(Ordering::SeqCst)
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
        let fan_enabled = AtomicBool::new(false);
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
            &fan_enabled,
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
}
