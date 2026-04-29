//! Isolated helper process for plugin validation.

use std::path::PathBuf;

fn main() {
    let result = run(std::env::args().skip(1).collect());
    match result {
        Ok(report) => {
            if let Err(error) = serde_json::to_writer(std::io::stdout(), &report) {
                eprintln!("failed to write validation report: {error}");
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

fn run(args: Vec<String>) -> Result<lilypalooza_clap::ValidationReport, String> {
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
            Ok(lilypalooza_clap::ValidationReport {
                format,
                path,
                result,
            })
        }
        _ => Err(format!("unsupported plugin format: {format}")),
    }
}

fn usage() -> String {
    "usage: lilypalooza-plugin-validator --format clap --path <plugin>".to_string()
}
