#![allow(missing_docs)]

use std::env;
use std::path::{Path, PathBuf};

use editor_host::{
    EditorFrame, EditorFrameAction, EditorHostOptions, EditorHostState, InstalledHost,
    WindowSnapshot, host_layout,
};
use iced::widget::{container, text};
use iced::{Color, Element, Length, Size, Subscription, Task, window};
use lilypalooza_audio::{
    AudioEngine, AudioEngineOptions, BUILTIN_SOUNDFONT_ID, EditorDescriptor, EditorParent,
    EditorSession, MixerState, SlotAddress, SlotState, SoundfontResource, TrackId,
};
use lilypalooza_builtins::soundfont_synth;

const SOUNDFONT_ID: &str = "debug";

fn main() -> iced::Result {
    let options = match Options::from_args(env::args_os().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            return Ok(());
        }
    };

    if let Err(error) = editor_host::prepare_process() {
        eprintln!("failed to prepare editor host process: {error}");
        return Ok(());
    }

    iced::application(
        move || SoundfontEditorApp::new(options.clone()),
        update,
        view,
    )
    .subscription(subscription)
    .title(title)
    .window(window_settings())
    .run()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    path: PathBuf,
    bank: u16,
    program: u8,
}

impl Options {
    fn from_args<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = std::ffi::OsString>,
    {
        let mut path = None;
        let mut bank = 0;
        let mut program = 0;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.to_string_lossy().as_ref() {
                "--soundfont" => path = args.next().map(PathBuf::from),
                "--bank" => bank = parse_next(&mut args, "--bank")?,
                "--program" => program = parse_next(&mut args, "--program")?,
                value if value.starts_with('-') => return Err(format!("unknown option `{value}`")),
                _ => path = Some(PathBuf::from(arg)),
            }
        }

        Ok(Self {
            path: path.ok_or("missing <soundfont.sf2>")?,
            bank,
            program,
        })
    }
}

fn parse_next<T, I>(args: &mut I, option: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    I: Iterator<Item = std::ffi::OsString>,
{
    args.next()
        .ok_or_else(|| format!("missing value for {option}"))?
        .to_string_lossy()
        .parse()
        .map_err(|_| format!("invalid value for {option}"))
}

struct SoundfontRuntime {
    _engine: AudioEngine,
    descriptor: EditorDescriptor,
    session: Option<Box<dyn EditorSession>>,
    title: String,
}

impl SoundfontRuntime {
    fn new(options: &Options) -> Result<Self, String> {
        lilypalooza_builtins::register_all();

        let mut engine = AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
            .map_err(|error| error.to_string())?;
        let resource = SoundfontResource {
            id: SOUNDFONT_ID.to_string(),
            name: soundfont_name(&options.path),
            path: options.path.clone(),
        };

        {
            let mut mixer = engine.mixer();
            mixer
                .set_soundfont(resource)
                .map_err(|error| error.to_string())?;
            mixer
                .set_track_instrument(
                    TrackId(0),
                    SlotState::built_in(
                        BUILTIN_SOUNDFONT_ID,
                        soundfont_synth::state(SOUNDFONT_ID, options.bank, options.program),
                    ),
                )
                .map_err(|error| error.to_string())?;
        }

        let controller = engine
            .controller(SlotAddress {
                strip_index: 1,
                slot_index: 0,
            })
            .map_err(|error| error.to_string())?
            .ok_or("SF-01 controller is unavailable")?;
        let descriptor = controller
            .descriptor()
            .editor
            .ok_or("SF-01 builtin has no editor")?;
        let session = controller
            .create_editor_session()
            .map_err(|error| error.to_string())?
            .ok_or("SF-01 editor session is unavailable")?;

        Ok(Self {
            _engine: engine,
            descriptor,
            session: Some(session),
            title: format!("SF-01 - {}", soundfont_name(&options.path)),
        })
    }
}

struct SoundfontEditorApp {
    runtime: Result<SoundfontRuntime, String>,
    host_window_id: Option<window::Id>,
    host: Option<InstalledHost>,
    attached: bool,
    status: String,
}

impl SoundfontEditorApp {
    fn new(options: Options) -> Self {
        Self {
            runtime: SoundfontRuntime::new(&options),
            host_window_id: None,
            host: None,
            attached: false,
            status: "opening editor".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    CloseRequested,
    Frame,
    HostReady {
        window_id: window::Id,
        host: Result<WindowSnapshot, String>,
    },
}

fn update(app: &mut SoundfontEditorApp, message: Message) -> Task<Message> {
    match message {
        Message::WindowOpened(window_id) => {
            app.host_window_id = Some(window_id);
            window::run(window_id, move |window| {
                let host = window
                    .window_handle()
                    .map_err(|error| error.to_string())
                    .and_then(|handle| {
                        WindowSnapshot::capture(
                            handle.as_raw(),
                            window.display_handle().ok().map(|display| display.as_raw()),
                        )
                        .map_err(|error| error.to_string())
                    });
                Message::HostReady { window_id, host }
            })
        }
        Message::WindowClosed(window_id) => {
            if Some(window_id) == app.host_window_id {
                if let Ok(runtime) = &mut app.runtime
                    && let Some(mut session) = runtime.session.take()
                {
                    let _ = session.detach();
                }
                return iced::exit();
            }
            Task::none()
        }
        Message::CloseRequested => {
            if let Some(host) = app.host.as_mut() {
                let _ = host.set_visible(false);
            }
            if let Ok(runtime) = &mut app.runtime
                && let Some(session) = runtime.session.as_mut()
            {
                let _ = session.set_visible(false);
            }
            Task::none()
        }
        Message::Frame => {
            if app
                .host
                .as_ref()
                .is_some_and(InstalledHost::close_requested)
            {
                return update(app, Message::CloseRequested);
            }
            Task::none()
        }
        Message::HostReady { window_id, host } => {
            if Some(window_id) != app.host_window_id || app.attached {
                return Task::none();
            }
            let Ok(runtime) = &mut app.runtime else {
                return Task::none();
            };
            let Some(session) = runtime.session.as_mut() else {
                return Task::none();
            };
            let host = match host {
                Ok(host) => host,
                Err(error) => {
                    app.status = error;
                    return Task::none();
                }
            };
            let installed = match editor_host::install_editor_host(
                &host,
                &EditorHostOptions::new(runtime.title.clone())
                    .with_resizable(runtime.descriptor.resizable),
                DebugEditorFrame::default(),
            ) {
                Ok(host) => host,
                Err(error) => {
                    app.status = error.to_string();
                    return Task::none();
                }
            };
            let parent = match snapshot_into_editor_parent(installed.content()) {
                Ok(parent) => parent,
                Err(error) => {
                    app.status = error;
                    return Task::none();
                }
            };
            match session.attach(parent) {
                Ok(()) => {
                    app.host = Some(installed);
                    app.attached = true;
                    app.status = "editor attached".to_string();
                }
                Err(error) => {
                    app.status = error.to_string();
                }
            }
            Task::none()
        }
    }
}

fn view(app: &SoundfontEditorApp) -> Element<'_, Message> {
    let status = match &app.runtime {
        Ok(_) => app.status.as_str(),
        Err(error) => error.as_str(),
    };
    container(text(status))
        .width(Length::Fill)
        .height(Length::Fill)
        .center(Length::Fill)
        .into()
}

fn subscription(app: &SoundfontEditorApp) -> Subscription<Message> {
    let mut subscriptions = vec![
        window::open_events().map(Message::WindowOpened),
        window::close_events().map(Message::WindowClosed),
    ];
    if app.host.is_some() {
        subscriptions
            .push(iced::time::every(std::time::Duration::from_millis(33)).map(|_| Message::Frame));
    }
    Subscription::batch(subscriptions)
}

fn title(app: &SoundfontEditorApp) -> String {
    app.runtime
        .as_ref()
        .map(|runtime| runtime.title.clone())
        .unwrap_or_else(|_| "SF-01".to_string())
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p lilypalooza-builtins --example open_soundfont_synth -- <soundfont.sf2> [--bank N] [--program N]"
    );
}

fn window_settings() -> window::Settings {
    window::Settings {
        size: Size::new(820.0, 456.0),
        min_size: Some(Size::new(820.0, 456.0)),
        resizable: false,
        decorations: false,
        exit_on_close_request: false,
        ..window::Settings::default()
    }
}

const DEBUG_EDITOR_FRAME_THICKNESS: f64 = 2.0;
const DEBUG_EDITOR_FRAME_BORDER_WIDTH: f32 = 0.5;
const DEBUG_EDITOR_FRAME_BORDER_INSET: f32 = DEBUG_EDITOR_FRAME_BORDER_WIDTH / 2.0;

#[derive(Debug, Clone)]
struct DebugEditorFrame {
    titlebar_height: f64,
    frame_thickness: f64,
    style: DebugEditorFrameStyle,
}

impl Default for DebugEditorFrame {
    fn default() -> Self {
        Self {
            titlebar_height: 30.0,
            frame_thickness: DEBUG_EDITOR_FRAME_THICKNESS,
            style: DebugEditorFrameStyle::from_theme(&iced::Theme::Dark),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DebugEditorFrameStyle {
    frame_color: editor_host::egui::Color32,
    titlebar_color: editor_host::egui::Color32,
    border_color: editor_host::egui::Color32,
    title_color: editor_host::egui::Color32,
    close_background: editor_host::egui::Color32,
    close_background_hovered: editor_host::egui::Color32,
    close_icon: editor_host::egui::Color32,
    close_icon_hovered: editor_host::egui::Color32,
}

impl DebugEditorFrameStyle {
    fn from_theme(theme: &iced::Theme) -> Self {
        let palette = theme.extended_palette();
        let titlebar = mix_iced_color(palette.background.weak.color, Color::WHITE, 0.04);
        let border = mix_iced_color(
            palette.background.weak.color,
            palette.background.strong.color,
            0.40,
        );
        let close_icon = mix_iced_color(
            palette.background.strong.text,
            palette.background.weak.color,
            0.12,
        );

        Self {
            frame_color: egui_color(palette.background.base.color),
            titlebar_color: egui_color(titlebar),
            border_color: egui_color(border),
            title_color: egui_color(palette.background.base.text),
            close_background: editor_host::egui::Color32::TRANSPARENT,
            close_background_hovered: egui_color(palette.background.base.color),
            close_icon: egui_color(close_icon),
            close_icon_hovered: egui_color(palette.background.base.text),
        }
    }
}

impl EditorFrame for DebugEditorFrame {
    fn layout(&self, content_size: editor_host::Size) -> editor_host::EditorFrameLayout {
        host_layout(
            content_size.width,
            content_size.height,
            self.titlebar_height,
            self.frame_thickness,
        )
    }

    fn render(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        state: &EditorHostState,
    ) -> EditorFrameAction {
        let layout = host_layout(
            state.content_size.width,
            state.content_size.height,
            self.titlebar_height,
            self.frame_thickness,
        );
        let rect = editor_host::egui::Rect::from_min_size(
            ui.max_rect().left_top(),
            editor_host::egui::vec2(layout.outer_width as f32, layout.outer_height as f32),
        );
        ui.painter().rect_filled(rect, 0.0, self.style.frame_color);
        ui.painter().rect_stroke(
            rect.shrink(DEBUG_EDITOR_FRAME_BORDER_INSET),
            0.0,
            editor_host::egui::Stroke::new(
                DEBUG_EDITOR_FRAME_BORDER_WIDTH,
                self.style.border_color,
            ),
            editor_host::egui::StrokeKind::Inside,
        );
        let titlebar = editor_host::egui::Rect::from_min_size(
            rect.left_top()
                + editor_host::egui::vec2(self.frame_thickness as f32, self.frame_thickness as f32),
            editor_host::egui::vec2(
                rect.width() - (self.frame_thickness * 2.0) as f32,
                self.titlebar_height as f32,
            ),
        );
        ui.painter()
            .rect_filled(titlebar, 0.0, self.style.titlebar_color);
        let drag_rect = editor_host::egui::Rect::from_min_max(
            titlebar.left_top() + editor_host::egui::vec2(32.0, 0.0),
            titlebar.right_bottom(),
        );
        let drag = ui.allocate_rect(drag_rect, editor_host::egui::Sense::drag());

        let close_rect = editor_host::egui::Rect::from_center_size(
            titlebar.left_center() + editor_host::egui::vec2(15.0, 0.0),
            editor_host::egui::vec2(20.0, 20.0),
        );
        let close = ui.allocate_rect(close_rect, editor_host::egui::Sense::click());
        let close_background = if close.hovered() {
            self.style.close_background_hovered
        } else {
            self.style.close_background
        };
        let close_icon = if close.hovered() {
            self.style.close_icon_hovered
        } else {
            self.style.close_icon
        };
        ui.painter().rect_filled(close_rect, 4.0, close_background);
        let icon_rect = close_rect.shrink(6.0);
        let stroke = editor_host::egui::Stroke::new(1.5, close_icon);
        ui.painter()
            .line_segment([icon_rect.left_top(), icon_rect.right_bottom()], stroke);
        ui.painter()
            .line_segment([icon_rect.right_top(), icon_rect.left_bottom()], stroke);
        ui.painter().text(
            titlebar.left_center() + editor_host::egui::vec2(36.0, 0.0),
            editor_host::egui::Align2::LEFT_CENTER,
            &state.title,
            editor_host::egui::FontId::proportional(12.0),
            self.style.title_color,
        );

        if close.clicked() {
            EditorFrameAction::Close
        } else if drag.drag_started_by(editor_host::egui::PointerButton::Primary) {
            EditorFrameAction::DragWindow
        } else {
            EditorFrameAction::None
        }
    }
}

fn egui_color(color: Color) -> editor_host::egui::Color32 {
    editor_host::egui::Color32::from_rgba_unmultiplied(
        color_channel_u8(color.r),
        color_channel_u8(color.g),
        color_channel_u8(color.b),
        color_channel_u8(color.a),
    )
}

fn color_channel_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn mix_iced_color(a: Color, b: Color, amount: f32) -> Color {
    let t = amount.clamp(0.0, 1.0);

    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

fn snapshot_into_editor_parent(snapshot: WindowSnapshot) -> Result<EditorParent, String> {
    let window = snapshot
        .raw_window_handle()
        .map_err(|error| error.to_string())?;
    let display = snapshot
        .raw_display_handle()
        .map_err(|error| error.to_string())?;
    Ok(EditorParent { window, display })
}

fn soundfont_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "SoundFont".to_string())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::Options;

    #[test]
    fn parses_positional_soundfont_with_defaults() {
        let options =
            Options::from_args([OsString::from("piano.sf2")]).expect("options should parse");

        assert_eq!(options.path, PathBuf::from("piano.sf2"));
        assert_eq!(options.bank, 0);
        assert_eq!(options.program, 0);
    }

    #[test]
    fn parses_program_selection() {
        let options = Options::from_args([
            OsString::from("--soundfont"),
            OsString::from("piano.sf2"),
            OsString::from("--bank"),
            OsString::from("128"),
            OsString::from("--program"),
            OsString::from("42"),
        ])
        .expect("options should parse");

        assert_eq!(options.path, PathBuf::from("piano.sf2"));
        assert_eq!(options.bank, 128);
        assert_eq!(options.program, 42);
    }

    #[test]
    fn window_size_starts_with_editor_content_size() {
        let settings = super::window_settings();

        assert_eq!(settings.size, iced::Size::new(820.0, 456.0));
        assert_eq!(settings.min_size, Some(iced::Size::new(820.0, 456.0)));
        assert!(!settings.decorations);
    }
}
