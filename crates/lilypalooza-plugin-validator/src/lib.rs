//! Isolated plugin validation helper.

use std::io;
use std::path::PathBuf;

/// Structured validation report for any supported plugin format.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum ValidationReport {
    /// CLAP validation report.
    Clap(lilypalooza_clap::ValidationReport),
    /// VST3 validation report.
    Vst3(lilypalooza_vst3::ValidationReport),
}

/// Runs validator CLI logic, writes CLI output, and returns a process exit code.
pub fn run_cli(args: Vec<String>) -> i32 {
    match run(args) {
        Ok(report) => match serde_json::to_writer(io::stdout(), &report) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("failed to write validation report: {error}");
                2
            }
        },
        Err(error) => {
            eprintln!("{error}");
            2
        }
    }
}

/// Runs validator CLI logic and returns a structured report.
pub fn run(args: Vec<String>) -> Result<ValidationReport, String> {
    let mut format = None;
    let mut path = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--format" => format = iter.next(),
            "--path" => path = iter.next().map(PathBuf::from),
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
    }

    let format = format.ok_or_else(usage)?;
    let path = path.ok_or_else(usage)?;
    match format.as_str() {
        lilypalooza_clap::FORMAT => {
            let result = lilypalooza_clap::probe(&path).map_err(|error| error.to_string());
            Ok(ValidationReport::Clap(lilypalooza_clap::ValidationReport {
                format,
                path,
                result,
            }))
        }
        lilypalooza_vst3::FORMAT => {
            let result = lilypalooza_vst3::probe(&path).map_err(|error| error.to_string());
            Ok(ValidationReport::Vst3(lilypalooza_vst3::ValidationReport {
                format,
                path,
                result,
            }))
        }
        _ => Err(format!("unsupported plugin format: {format}")),
    }
}

fn usage() -> String {
    "usage: lilypalooza-plugin-validator --format clap|vst3 --path <plugin>".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_format() {
        let error = run(vec![
            "--format".to_string(),
            "vst2".to_string(),
            "--path".to_string(),
            "/tmp/plugin".to_string(),
        ])
        .expect_err("unknown format should fail");

        assert!(error.contains("unsupported plugin format: vst2"));
    }
}
