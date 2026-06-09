use std::fs;
use std::io;
use std::path::Path;

pub fn read_cpu_temp_c(path: &Path) -> Result<f64, TempError> {
    let raw = fs::read_to_string(path).map_err(TempError::Io)?;
    parse_millidegrees_c(&raw)
}

pub fn parse_millidegrees_c(raw: &str) -> Result<f64, TempError> {
    let trimmed = raw.trim();
    let millidegrees = trimmed.parse::<i64>().map_err(|_| TempError::Parse {
        raw: trimmed.to_string(),
    })?;
    Ok(millidegrees as f64 / 1000.0)
}

#[derive(Debug)]
pub enum TempError {
    Io(io::Error),
    Parse { raw: String },
}

impl std::fmt::Display for TempError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "temperature I/O error: {err}"),
            Self::Parse { raw } => write!(f, "invalid millidegree temperature: {raw:?}"),
        }
    }
}

impl std::error::Error for TempError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_temp_millidegrees() {
        assert_eq!(parse_millidegrees_c("55123\n").unwrap(), 55.123);
    }
}
