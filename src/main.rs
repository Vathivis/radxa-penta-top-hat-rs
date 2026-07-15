use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use radxa_penta_top_hat_rs::button::ButtonRuntime;
use radxa_penta_top_hat_rs::cli::Args;
use radxa_penta_top_hat_rs::config::Config;
use radxa_penta_top_hat_rs::env_file::PinMap;
use radxa_penta_top_hat_rs::fan::FanDecision;
use radxa_penta_top_hat_rs::oled::OledRuntime;
use radxa_penta_top_hat_rs::pwm::{Duty, FanOutput, FanPwmOutput};
use radxa_penta_top_hat_rs::shutdown;
use radxa_penta_top_hat_rs::temp::read_cpu_temp_c;

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

    while !shutdown::requested() {
        let temp_c = read_cpu_temp_c(&args.cpu_temp_path).map_err(|err| err.to_string())?;
        let decision = FanDecision::cpu_only(temp_c, config.fan);
        let commanded_duty_percent = if fan_enabled.load(Ordering::SeqCst) {
            decision.duty_percent
        } else {
            0
        };

        if args.dry_run || args.once {
            println!(
                "cpu_temp_c={:.1} fan_level={:?} duty_percent={} active_low_duty={:.2}",
                decision.temp_c, decision.level, decision.duty_percent, decision.active_low_duty
            );
        }

        if let Some(output) = output.as_mut()
            && apply_changed_duty(output, commanded_duty_percent, &mut last_duty_percent)?
        {
            eprintln!(
                "fan: cpu_temp_c={:.1} fan_level={:?} duty_percent={} enabled={}",
                decision.temp_c,
                decision.level,
                commanded_duty_percent,
                fan_enabled.load(Ordering::SeqCst)
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
        output
            .set_logical_duty(Duty::from_percent(0))
            .map_err(|err| err.to_string())?;
        eprintln!("shutdown requested: fan duty set to 0 percent");
    }

    Ok(())
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

    fn decision(duty_percent: u8) -> FanDecision {
        FanDecision {
            temp_c: 0.0,
            level: radxa_penta_top_hat_rs::fan::FanLevel::Off,
            duty_percent,
            active_low_duty: 1.0,
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
        let computed = decision(75);
        let commanded = if fan_enabled.load(Ordering::SeqCst) {
            computed.duty_percent
        } else {
            0
        };

        assert_eq!(commanded, 0);
    }
}
