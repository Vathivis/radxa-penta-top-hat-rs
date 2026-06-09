use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

const DEFAULT_CONFIG_PATH: &str = "/etc/rockpi-penta.conf";
const DEFAULT_ENV_FILE: &str = "/etc/rockpi-penta.env";
const DEFAULT_CPU_TEMP_PATH: &str = "/sys/class/thermal/thermal_zone0/temp";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Args {
    pub config_path: PathBuf,
    pub env_file: PathBuf,
    pub cpu_temp_path: PathBuf,
    pub dry_run: bool,
    pub once: bool,
    pub help: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            config_path: PathBuf::from(DEFAULT_CONFIG_PATH),
            env_file: PathBuf::from(DEFAULT_ENV_FILE),
            cpu_temp_path: PathBuf::from(DEFAULT_CPU_TEMP_PATH),
            dry_run: false,
            once: false,
            help: false,
        }
    }
}

impl Args {
    pub fn parse() -> Result<Self, String> {
        Self::parse_from(env::args_os())
    }

    pub fn parse_from<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut parsed = Self::default();
        let mut iter = args.into_iter().map(Into::into);

        let _program = iter.next();
        while let Some(arg) = iter.next() {
            let arg = arg
                .into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_string())?;

            if arg == "--help" || arg == "-h" {
                parsed.help = true;
            } else if arg == "--dry-run" {
                parsed.dry_run = true;
            } else if arg == "--once" {
                parsed.once = true;
            } else if let Some(value) = arg.strip_prefix("--config=") {
                parsed.config_path = PathBuf::from(value);
            } else if arg == "--config" {
                parsed.config_path = next_path(&mut iter, "--config")?;
            } else if let Some(value) = arg.strip_prefix("--env-file=") {
                parsed.env_file = PathBuf::from(value);
            } else if arg == "--env-file" {
                parsed.env_file = next_path(&mut iter, "--env-file")?;
            } else if let Some(value) = arg.strip_prefix("--cpu-temp-path=") {
                parsed.cpu_temp_path = PathBuf::from(value);
            } else if arg == "--cpu-temp-path" {
                parsed.cpu_temp_path = next_path(&mut iter, "--cpu-temp-path")?;
            } else {
                return Err(format!("unknown argument: {arg}"));
            }
        }

        Ok(parsed)
    }
}

fn next_path<I>(iter: &mut I, flag: &str) -> Result<PathBuf, String>
where
    I: Iterator<Item = OsString>,
{
    let value = iter
        .next()
        .ok_or_else(|| format!("{flag} requires a value"))?
        .into_string()
        .map_err(|_| format!("{flag} value must be valid UTF-8"))?;

    if value.is_empty() {
        return Err(format!("{flag} value must not be empty"));
    }

    Ok(PathBuf::from(value))
}

pub fn usage(program: &str) -> String {
    format!(
        "\
Usage: {program} [OPTIONS]

Options:
      --config <PATH>         Config file path [default: {DEFAULT_CONFIG_PATH}]
      --env-file <PATH>       Board pin env file path [default: {DEFAULT_ENV_FILE}]
      --cpu-temp-path <PATH>  CPU temp path [default: {DEFAULT_CPU_TEMP_PATH}]
      --dry-run               Print fan decisions without hardware output
      --once                  Take one sample and exit
  -h, --help                  Show this help
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults() {
        let args = Args::parse_from(["daemon"]).expect("default args should parse");
        assert_eq!(args.config_path, PathBuf::from(DEFAULT_CONFIG_PATH));
        assert_eq!(args.env_file, PathBuf::from(DEFAULT_ENV_FILE));
        assert_eq!(args.cpu_temp_path, PathBuf::from(DEFAULT_CPU_TEMP_PATH));
        assert!(!args.dry_run);
        assert!(!args.once);
    }

    #[test]
    fn parses_flags_and_paths() {
        let args = Args::parse_from([
            "daemon",
            "--config",
            "/tmp/config.ini",
            "--env-file=/tmp/pins.env",
            "--cpu-temp-path",
            "/tmp/temp",
            "--dry-run",
            "--once",
        ])
        .expect("explicit args should parse");

        assert_eq!(args.config_path, PathBuf::from("/tmp/config.ini"));
        assert_eq!(args.env_file, PathBuf::from("/tmp/pins.env"));
        assert_eq!(args.cpu_temp_path, PathBuf::from("/tmp/temp"));
        assert!(args.dry_run);
        assert!(args.once);
    }
}
