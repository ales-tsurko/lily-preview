# Lilypalooza

**Lilypalooza** is a desktop LilyPond IDE with DAW-style playback, editing,
score preview, plugin hosting, and project persistence.

![Screenshot 1](assets/screenshots/screenshot_1.png)
![Screenshot 2](assets/screenshots/screenshot_2.png)
![Screenshot 3](assets/screenshots/screenshot_3.png)
![Screenshot 4](assets/screenshots/screenshot_4.png)

## Features

- multi-tab LilyPond editor with tree-sitter highlighting
- watched LilyPond sources with automatic recompilation on save/external change
- score preview with point-and-click navigation back to source
- MIDI playback with transport, piano roll and feature-full DAW-grade mixer
- DAW-style processor slots for built-in, CLAP, and VST3 instruments/effects
- built-in SoundFont sampler
- plugin editor windows with preset controls and native embedded plugin views
- non-blocking CLAP/VST3 scanning with isolated validation and persistent cache
- mixer buses, output routing, sends, pre/post fader sends, feedback prevention,
  effect bypass, effect reordering, and plugin delay compensation
- dockable multi-pane workspace with editor, score, piano roll, mixer, logs, and
  project state persistence

## Run

Use `make run` for development. It builds the plugin validator helper first and
then starts the app.

```bash
make run
```

Pass app arguments after `--`:

```bash
make run -- --soundfont assets/soundfonts/FluidR3_GM.sf2 --score path/to/score.ly
```

Useful direct environment overrides:

```bash
LILYPALOOZA_SOUNDFONT=assets/soundfonts/FluidR3_GM.sf2 cargo run
LILYPALOOZA_SCORE=path/to/score.ly cargo run
```

Trace plugin/editor hosting:

```bash
RUST_LOG=lilypalooza_vst3=trace,editor_host=trace,lilypalooza::editor_windows=trace make run
```

## Settings

Settings are stored in the platform config directory:

```text
lilypalooza/settings.toml
```

The file is generated with documented defaults. Important sections include:

- `clap_search_paths` and `vst3_search_paths`
- editor view and theme settings
- shortcut overrides
- playback SoundFonts and audio output device

Default plugin search paths are created for the current platform. On macOS this
includes:

```text
/Library/Audio/Plug-Ins/CLAP
~/Library/Audio/Plug-Ins/CLAP
/Library/Audio/Plug-Ins/VST3
~/Library/Audio/Plug-Ins/VST3
```

Plugin scanning runs in the background. The UI and log/status line report scan
progress while the app is not blocked and the picker may still be filling.

## Projects

When a score belongs to a saved project, project state is stored under:

```text
.lilypalooza/project.ron
```

Project/global state preserves editor tabs, mixer state, plugin choices,
routing, and user processor presets.

## Workspace Crates

- `lilypalooza-audio`: engine, mixer, routing, processor interfaces, PDC
- `lilypalooza-builtins`: built-in processors such as SF-01 and Gain
- `lilypalooza-clap`: CLAP discovery, runtime, state, audio, and editor adapter
- `lilypalooza-vst3`: VST3 discovery, runtime, audio, and editor adapter
- `lilypalooza-plugin-scan`: reusable background scanner and cache
- `lilypalooza-plugin-validator`: subprocess validator for plugin candidates
- `editor-host`: cross-format embedded plugin editor window/frame host
- `lilypalooza-egui-baseview`: egui/baseview frame rendering support
- `crates/vendor/*`: vendored UI/editor/parser dependencies

## Development Commands

```bash
make fmt
make clippy
make test-all
```

Build only the validator helper:

```bash
make validator
```

## Built-in Debugging

Open the SF-01 editor directly:

```bash
cargo run -p lilypalooza-builtins --example open_soundfont_synth
```

Other useful built-in examples:

```bash
cargo run -p lilypalooza-builtins --example play_soundfont_midi
cargo run -p lilypalooza-builtins --example profile_engine
```

## Status

The app is under active development. macOS is the primary tested platform right
now. CLAP and VST3 hosting are implemented; Windows and X11 paths are kept in
the architecture but mostly placeholders yet. Wayland plugin editor hosting is
not a current target.
