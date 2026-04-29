//! Lilypalooza desktop application entry point.
//!
//! This binary wires the UI modules and starts the Iced runtime.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

mod app;
mod browser_file_watcher;
mod control_icon;
mod editor_file_watcher;
mod error_prompt;
mod fonts;
mod icons;
mod lilypond;
mod logger;
mod midi;
mod plugin_scan;
mod score_watcher;
mod settings;
mod shortcuts;
mod state;
mod status_bar;
mod track_colors;
mod track_names;
mod ui_style;

fn main() -> iced::Result {
    lilypalooza_builtins::register_all();
    let startup = startup_options();
    app::run(startup.soundfont, startup.score, startup.audio_enabled)
}

struct StartupOptions {
    audio_enabled: bool,
    soundfont: Option<PathBuf>,
    score: Option<PathBuf>,
}

fn startup_options() -> StartupOptions {
    startup_options_from_iter(env::args_os().skip(1))
}

fn startup_options_from_iter<I>(arguments: I) -> StartupOptions
where
    I: IntoIterator<Item = OsString>,
{
    const SOUND_FONT_ENV: &str = "LILYPALOOZA_SOUNDFONT";
    const SOUND_FONT_FLAG: &str = "--soundfont";
    const NO_AUDIO_FLAG: &str = "--no-audio";
    const SCORE_ENV: &str = "LILYPALOOZA_SCORE";
    const SCORE_FLAG: &str = "--score";
    const SCORE_ALIAS_FLAG: &str = "--file";

    let mut args = arguments.into_iter().peekable();
    let mut audio_enabled = true;
    let mut cli_soundfont: Option<PathBuf> = None;
    let mut cli_score: Option<PathBuf> = None;

    while let Some(argument) = args.next() {
        if argument == NO_AUDIO_FLAG {
            audio_enabled = false;
            continue;
        }

        if argument == SOUND_FONT_FLAG {
            let Some(value) = args.next() else {
                eprintln!("Ignoring {SOUND_FONT_FLAG}: no path was provided");
                continue;
            };
            cli_soundfont = Some(PathBuf::from(value));
            continue;
        }

        if argument == SCORE_FLAG || argument == SCORE_ALIAS_FLAG {
            let Some(value) = args.next() else {
                eprintln!("Ignoring {argument:?}: no path was provided");
                continue;
            };
            cli_score = Some(PathBuf::from(value));
            continue;
        }

        let Some(argument_str) = argument.to_str() else {
            continue;
        };

        if let Some(value) = argument_str.strip_prefix("--soundfont=") {
            if value.is_empty() {
                eprintln!("Ignoring --soundfont=: empty path");
                continue;
            }
            cli_soundfont = Some(PathBuf::from(value));
            continue;
        }

        let score_value = argument_str
            .strip_prefix("--score=")
            .or_else(|| argument_str.strip_prefix("--file="));
        let Some(value) = score_value else {
            continue;
        };

        if value.is_empty() {
            eprintln!("Ignoring score startup flag: empty path");
            continue;
        }

        cli_score = Some(PathBuf::from(value));
    }

    let soundfont = cli_soundfont.or_else(|| {
        env::var_os(SOUND_FONT_ENV)
            .filter(|value| !is_empty_os_string(value))
            .map(PathBuf::from)
    });
    let score = cli_score.or_else(|| {
        env::var_os(SCORE_ENV)
            .filter(|value| !is_empty_os_string(value))
            .map(PathBuf::from)
    });

    StartupOptions {
        audio_enabled,
        soundfont,
        score,
    }
}

fn is_empty_os_string(value: &OsString) -> bool {
    value.to_str().is_none_or(str::is_empty)
}

#[cfg(test)]
mod tests {
    use super::startup_options_from_iter;
    use std::ffi::OsString;

    #[test]
    fn parses_no_audio_flag() {
        let startup = startup_options_from_iter([OsString::from("--no-audio")]);
        assert!(!startup.audio_enabled);
    }
}
