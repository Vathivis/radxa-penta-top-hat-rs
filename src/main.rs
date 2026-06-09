use std::env;
use std::thread;
use std::time::Duration;

use radxa_penta_top_hat_rs::cli::Args;
use radxa_penta_top_hat_rs::config::Config;
use radxa_penta_top_hat_rs::env_file::PinMap;
use radxa_penta_top_hat_rs::fan::FanDecision;
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

    loop {
        let temp_c = read_cpu_temp_c(&args.cpu_temp_path).map_err(|err| err.to_string())?;
        let decision = FanDecision::cpu_only(temp_c, config.fan);

        if args.dry_run || args.once {
            println!(
                "cpu_temp_c={:.1} fan_level={:?} duty_percent={} active_low_duty={:.2}",
                decision.temp_c, decision.level, decision.duty_percent, decision.active_low_duty
            );
        }

        if args.once {
            return Ok(());
        }

        thread::sleep(Duration::from_secs(1));
    }
}

fn usage() -> String {
    let program = env::args()
        .next()
        .unwrap_or_else(|| "radxa-penta-top-hat-rs".to_string());
    radxa_penta_top_hat_rs::cli::usage(&program)
}
