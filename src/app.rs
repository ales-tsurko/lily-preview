use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_host::WindowSnapshot;
use iced::event;
use iced::keyboard;
use iced::widget::{Id, pane_grid, svg};
use iced::{Color, Point, Rectangle, Size, Subscription, Task, mouse, window};
use iced_core::{Bytes, image};
use lilypalooza_audio::MixerState;
use lilypalooza_audio::{AudioEngine, AudioEngineOptions};
use tempfile::TempDir;

use crate::browser_file_watcher::BrowserFileWatcher;
use crate::editor_file_watcher::EditorFileWatcher;
use crate::error_prompt::{ErrorPrompt, PromptSelectedButton};
use crate::lilypond;
use crate::logger::Logger;
use crate::score_watcher::ScoreWatcher;
use crate::settings::{
    self, DockAxis, DockNodeSettings, FoldedPaneRestoreSettings, FoldedPaneSettings,
};
use crate::state::{self, GlobalState, ProjectState};
use crate::ui_style;

use messages::{
    EditorMessage, FileMessage, KeyPress, LoggerMessage, Message, MixerMessage, PaneMessage,
    PianoRollMessage, PromptMessage, ViewerMessage,
};
use piano_roll::PianoRollState;
use processor_editor_windows::EditorWindowManager;
use score_cursor::{ScoreCursorMaps, ScoreCursorPlacement};
use update::update;
use view::view;

mod controls;
mod dock_view;
mod editor;
mod messages;
mod meters;
mod mixer;
mod piano_roll;
mod processor_editor_windows;
pub(crate) mod processor_presets;
mod score_cursor;
mod score_view;
mod transport_bar;
mod update;
mod view;

#[cfg(test)]
static ICED_SNAPSHOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MIN_WINDOW_WIDTH: f32 = 960.0;
const MIN_WINDOW_HEIGHT: f32 = 640.0;
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const EDITOR_TICK_INTERVAL: Duration = Duration::from_millis(500);
const PLUGIN_SCAN_POLL_INTERVAL: Duration = Duration::from_millis(120);
const SPINNER_POLL_INTERVAL: Duration = Duration::from_millis(120);
const EDITOR_TABBAR_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(16);
pub(super) const SCORE_SCROLLABLE_ID: &str = "score-scrollable";
pub(super) const EDITOR_TABBAR_SCROLL_ID: &str = "editor-tabbar-scroll";
pub(super) const EDITOR_FILE_BROWSER_SCROLL_ID: &str = "editor-file-browser-scroll";
pub(super) const EDITOR_FILE_BROWSER_HEIGHT: f32 = 176.0;
pub(super) const EDITOR_FILE_BROWSER_COLUMN_WIDTH: f32 = 220.0;
pub(super) const EDITOR_FILE_BROWSER_ENTRY_HEIGHT: f32 = 26.0;
pub(super) const SHORTCUTS_SCROLLABLE_ID: &str = "shortcuts-scrollable";
pub(super) const TRACK_RENAME_INPUT_ID: &str = "track-rename-input";
const EDITOR_FRAME_THICKNESS: f64 = 2.0;
const EDITOR_FRAME_BORDER_WIDTH: f32 = 0.5;
const EDITOR_FRAME_COMPACT_CHROME_HEIGHT: f64 = 34.0;
const EDITOR_FRAME_EXPANDED_CHROME_HEIGHT: f64 = 160.0;
const EDITOR_FRAME_TITLE_ROW_HEIGHT: f32 = 32.0;
const EDITOR_FRAME_PRESET_ROW_HEIGHT: f32 = 24.0;
const EDITOR_FRAME_PRESET_BROWSER_HEIGHT: f32 = 116.0;
const EDITOR_FRAME_ICON_SIZE: f32 = 13.0;
const EDITOR_FRAME_RESIZE_GRIP_SIZE: f32 = 18.0;
const EDITOR_FRAME_MIN_CONTENT_WIDTH: f64 = 160.0;
const EDITOR_FRAME_MIN_CONTENT_HEIGHT: f64 = 120.0;
const EGUI_ICON_CHEVRON_LEFT: &[u8] = include_bytes!("../assets/icons/chevron-left.svg");
const EGUI_ICON_CHEVRON_RIGHT: &[u8] = include_bytes!("../assets/icons/chevron-right.svg");
const EGUI_ICON_CHEVRON_DOWN: &[u8] = include_bytes!("../assets/icons/chevron-down.svg");
const EGUI_ICON_CHEVRON_UP: &[u8] = include_bytes!("../assets/icons/chevron-up.svg");
const EGUI_ICON_PENCIL: &[u8] = include_bytes!("../assets/icons/pencil.svg");
const EGUI_ICON_SAVE: &[u8] = include_bytes!("../assets/icons/save.svg");
const EGUI_ICON_TRASH: &[u8] = include_bytes!("../assets/icons/trash-2.svg");

#[derive(Clone)]
pub(super) struct AppEditorFrame {
    titlebar_height: f64,
    frame_thickness: f64,
    border_width: f32,
    renaming_preset_id: Option<String>,
    renaming_preset_value: String,
    rename_focus_requested: bool,
    delete_confirmation_preset_id: Option<String>,
    icon_textures: HashMap<AppEditorFrameIcon, editor_host::egui::TextureHandle>,
    style: AppEditorFrameStyle,
}

impl std::fmt::Debug for AppEditorFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppEditorFrame")
            .field("titlebar_height", &self.titlebar_height)
            .field("frame_thickness", &self.frame_thickness)
            .field("border_width", &self.border_width)
            .field("renaming_preset_id", &self.renaming_preset_id)
            .field("renaming_preset_value", &self.renaming_preset_value)
            .field("rename_focus_requested", &self.rename_focus_requested)
            .field(
                "delete_confirmation_preset_id",
                &self.delete_confirmation_preset_id,
            )
            .field("style", &self.style)
            .finish()
    }
}

impl Default for AppEditorFrame {
    fn default() -> Self {
        Self::from_theme(&iced::Theme::Dark)
    }
}

impl AppEditorFrame {
    pub(super) fn from_theme(theme: &iced::Theme) -> Self {
        Self {
            titlebar_height: EDITOR_FRAME_COMPACT_CHROME_HEIGHT,
            frame_thickness: EDITOR_FRAME_THICKNESS,
            border_width: EDITOR_FRAME_BORDER_WIDTH,
            renaming_preset_id: None,
            renaming_preset_value: String::new(),
            rename_focus_requested: false,
            delete_confirmation_preset_id: None,
            icon_textures: HashMap::new(),
            style: AppEditorFrameStyle::from_theme(theme),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct AppEditorFramePresetLayout {
    title_row: editor_host::egui::Rect,
    title_text: editor_host::egui::Rect,
    close_button: editor_host::egui::Rect,
    preset_row: editor_host::egui::Rect,
    previous: editor_host::egui::Rect,
    name: editor_host::egui::Rect,
    next: editor_host::egui::Rect,
    browser: editor_host::egui::Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppEditorFrameStyle {
    frame_color: editor_host::egui::Color32,
    titlebar_color: editor_host::egui::Color32,
    border_color: editor_host::egui::Color32,
    title_color: editor_host::egui::Color32,
    close_background: editor_host::egui::Color32,
    close_background_hovered: editor_host::egui::Color32,
    close_icon: editor_host::egui::Color32,
    close_icon_hovered: editor_host::egui::Color32,
    control_background: editor_host::egui::Color32,
    control_background_hovered: editor_host::egui::Color32,
    control_background_active: editor_host::egui::Color32,
    muted_text: editor_host::egui::Color32,
}

impl AppEditorFrameStyle {
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
        let control = mix_iced_color(
            palette.background.base.color,
            palette.background.weak.color,
            0.50,
        );
        let control_hovered = mix_iced_color(control, palette.primary.weak.color, 0.20);
        let control_active = mix_iced_color(control, palette.primary.weak.color, 0.32);

        Self {
            frame_color: egui_color(palette.background.base.color),
            titlebar_color: egui_color(titlebar),
            border_color: egui_color(border),
            title_color: egui_color(palette.background.base.text),
            close_background: editor_host::egui::Color32::TRANSPARENT,
            close_background_hovered: egui_color(palette.background.base.color),
            close_icon: egui_color(close_icon),
            close_icon_hovered: egui_color(palette.background.base.text),
            control_background: egui_color(control),
            control_background_hovered: egui_color(control_hovered),
            control_background_active: egui_color(control_active),
            muted_text: egui_color(palette.background.weak.text),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AppEditorFrameIcon {
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    Pencil,
    Save,
    Trash,
}

impl AppEditorFrameIcon {
    fn svg_bytes(self) -> &'static [u8] {
        match self {
            Self::ChevronLeft => EGUI_ICON_CHEVRON_LEFT,
            Self::ChevronRight => EGUI_ICON_CHEVRON_RIGHT,
            Self::ChevronDown => EGUI_ICON_CHEVRON_DOWN,
            Self::ChevronUp => EGUI_ICON_CHEVRON_UP,
            Self::Pencil => EGUI_ICON_PENCIL,
            Self::Save => EGUI_ICON_SAVE,
            Self::Trash => EGUI_ICON_TRASH,
        }
    }
}

impl editor_host::EditorFrame for AppEditorFrame {
    fn layout(&self, content_size: editor_host::Size) -> editor_host::EditorFrameLayout {
        editor_host::host_layout(
            content_size.width,
            content_size.height,
            self.titlebar_height,
            self.frame_thickness,
        )
    }

    fn render(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        state: &editor_host::EditorHostState,
    ) -> editor_host::EditorFrameAction {
        let rect = self.frame_rect(ui.max_rect(), state);
        ui.painter().rect_filled(rect, 0.0, self.style.frame_color);
        ui.painter().rect_stroke(
            rect.shrink(self.border_width / 2.0),
            0.0,
            editor_host::egui::Stroke::new(self.border_width, self.style.border_color),
            editor_host::egui::StrokeKind::Inside,
        );

        let titlebar = editor_host::egui::Rect::from_min_size(
            rect.left_top()
                + editor_host::egui::vec2(self.frame_thickness as f32, self.frame_thickness as f32),
            editor_host::egui::vec2(
                rect.width() - (self.frame_thickness * 2.0) as f32,
                Self::chrome_height(state) as f32,
            ),
        );
        ui.painter()
            .rect_filled(titlebar, 0.0, self.style.titlebar_color);

        let preset_layout = self.preset_layout(titlebar);
        let drag_rect = editor_host::egui::Rect::from_min_max(
            editor_host::egui::pos2(
                preset_layout.preset_row.right() + 8.0,
                preset_layout.title_row.top(),
            ),
            editor_host::egui::pos2(
                preset_layout.close_button.left() - 8.0,
                preset_layout.title_row.bottom(),
            ),
        );
        let drag = ui.allocate_rect(drag_rect, editor_host::egui::Sense::drag());

        let close_rect = preset_layout.close_button;
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
        let title = ellipsize_for_width(
            &state.title,
            preset_layout.title_text.width(),
            ui_style::FONT_SIZE_UI_SM as f32,
        );
        ui.painter().text(
            preset_layout.title_text.right_center(),
            editor_host::egui::Align2::RIGHT_CENTER,
            title,
            editor_host::egui::FontId::proportional(ui_style::FONT_SIZE_UI_SM as f32),
            self.style.title_color,
        );

        let preset_command = self.render_preset_strip(ui, state, titlebar);
        let resize_command = self.render_resize_grip(ui, rect, state);

        if close.clicked() {
            editor_host::EditorFrameAction::Close
        } else if drag.drag_started_by(editor_host::egui::PointerButton::Primary) {
            editor_host::EditorFrameAction::DragWindow
        } else if let Some(command) = resize_command {
            editor_host::EditorFrameAction::Command(command)
        } else if let Some(command) = preset_command {
            editor_host::EditorFrameAction::Command(command)
        } else {
            editor_host::EditorFrameAction::None
        }
    }
}

impl AppEditorFrame {
    fn render_resize_grip(
        &self,
        ui: &mut editor_host::egui::Ui,
        rect: editor_host::egui::Rect,
        state: &editor_host::EditorHostState,
    ) -> Option<editor_host::EditorFrameCommand> {
        if !state.resizable {
            return None;
        }

        let grip_rect = self.resize_grip_rect(rect);
        let response = ui.allocate_rect(grip_rect, editor_host::egui::Sense::drag());
        let tint = if response.hovered() || response.dragged() {
            self.style.title_color
        } else {
            self.style.muted_text
        };
        let painter = ui.painter();
        let stroke = editor_host::egui::Stroke::new(1.0, tint);
        for offset in [5.0, 9.0, 13.0] {
            painter.line_segment(
                [
                    grip_rect.right_bottom() - editor_host::egui::vec2(offset, 2.0),
                    grip_rect.right_bottom() - editor_host::egui::vec2(2.0, offset),
                ],
                stroke,
            );
        }

        response.dragged().then(|| {
            let delta = ui.input(|input| input.pointer.delta());
            self.resize_command_for_drag(state.content_size, delta)
        })
    }

    fn resize_grip_rect(&self, rect: editor_host::egui::Rect) -> editor_host::egui::Rect {
        editor_host::egui::Rect::from_min_max(
            rect.right_bottom()
                - editor_host::egui::vec2(
                    EDITOR_FRAME_RESIZE_GRIP_SIZE,
                    EDITOR_FRAME_RESIZE_GRIP_SIZE,
                ),
            rect.right_bottom(),
        )
    }

    fn resize_command_for_drag(
        &self,
        content_size: editor_host::Size,
        delta: editor_host::egui::Vec2,
    ) -> editor_host::EditorFrameCommand {
        let rel_x = f64::from(delta.x) / content_size.width.max(1.0);
        let rel_y = f64::from(delta.y) / content_size.height.max(1.0);
        let rel = if rel_x.abs() >= rel_y.abs() {
            rel_x
        } else {
            rel_y
        };
        let scale = (1.0 + rel).max(
            (EDITOR_FRAME_MIN_CONTENT_WIDTH / content_size.width.max(1.0))
                .max(EDITOR_FRAME_MIN_CONTENT_HEIGHT / content_size.height.max(1.0)),
        );
        editor_host::EditorFrameCommand::ResizeContent {
            width: (content_size.width * scale).round().max(1.0) as u32,
            height: (content_size.height * scale).round().max(1.0) as u32,
        }
    }

    fn frame_rect(
        &self,
        available: editor_host::egui::Rect,
        state: &editor_host::EditorHostState,
    ) -> editor_host::egui::Rect {
        let layout = editor_host::host_layout(
            state.content_size.width,
            state.content_size.height,
            Self::chrome_height(state),
            self.frame_thickness,
        );
        editor_host::egui::Rect::from_min_size(
            available.left_top(),
            editor_host::egui::vec2(layout.outer_width as f32, layout.outer_height as f32),
        )
    }

    fn preset_menu_icon(expanded: bool) -> AppEditorFrameIcon {
        if expanded {
            AppEditorFrameIcon::ChevronDown
        } else {
            AppEditorFrameIcon::ChevronUp
        }
    }

    fn begin_preset_rename(&mut self, id: &str, name: &str) {
        self.renaming_preset_id = Some(id.to_string());
        self.renaming_preset_value = name.to_string();
        self.rename_focus_requested = true;
        self.delete_confirmation_preset_id = None;
    }

    fn cancel_preset_rename(&mut self) {
        self.renaming_preset_id = None;
        self.renaming_preset_value.clear();
        self.rename_focus_requested = false;
    }

    fn request_preset_delete(&mut self, id: &str) -> Option<editor_host::EditorFrameCommand> {
        self.delete_confirmation_preset_id = Some(id.to_string());
        self.cancel_preset_rename();
        None
    }

    fn cancel_preset_delete(&mut self) {
        self.delete_confirmation_preset_id = None;
    }

    fn delete_confirmation_label(name: &str, width: f32) -> String {
        let fixed_width = ui_style::FONT_SIZE_UI_SM as f32 * 10.0 * 0.56;
        let name = ellipsize_for_width(
            name,
            (width - fixed_width).max(ui_style::FONT_SIZE_UI_SM as f32 * 3.0),
            ui_style::FONT_SIZE_UI_SM as f32,
        );
        format!("Remove \"{name}\"?")
    }

    fn confirm_preset_delete(&mut self, id: &str) -> Option<editor_host::EditorFrameCommand> {
        if self.delete_confirmation_preset_id.as_deref() != Some(id) {
            return None;
        }
        self.delete_confirmation_preset_id = None;
        Some(editor_host::EditorFrameCommand::DeletePreset(
            id.to_string(),
        ))
    }

    fn chrome_height(state: &editor_host::EditorHostState) -> f64 {
        state
            .preset
            .as_ref()
            .map_or(EDITOR_FRAME_COMPACT_CHROME_HEIGHT, |preset| {
                if preset.expanded {
                    EDITOR_FRAME_EXPANDED_CHROME_HEIGHT
                } else {
                    EDITOR_FRAME_COMPACT_CHROME_HEIGHT
                }
            })
    }

    fn preset_layout(&self, titlebar: editor_host::egui::Rect) -> AppEditorFramePresetLayout {
        let left_inset = 8.0;
        let title_gap = 10.0;
        let title_width = 180.0;
        let button_width = 25.0;
        let title_row = editor_host::egui::Rect::from_min_size(
            titlebar.left_top(),
            editor_host::egui::vec2(titlebar.width(), EDITOR_FRAME_TITLE_ROW_HEIGHT),
        );
        let close_button = editor_host::egui::Rect::from_center_size(
            title_row.right_center() - editor_host::egui::vec2(18.0, 0.0),
            editor_host::egui::vec2(20.0, 20.0),
        );
        let preset_max_width = (close_button.left() - title_gap - 72.0 - left_inset).max(80.0);
        let preset_width = (titlebar.width() - 176.0)
            .clamp(188.0, 360.0)
            .min(preset_max_width);
        let preset_row = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(titlebar.left() + left_inset, titlebar.top() + 4.0),
            editor_host::egui::vec2(preset_width, EDITOR_FRAME_PRESET_ROW_HEIGHT),
        );
        let title_text = editor_host::egui::Rect::from_min_max(
            editor_host::egui::pos2(
                (close_button.left() - 8.0 - title_width).max(preset_row.right() + title_gap),
                title_row.top(),
            ),
            editor_host::egui::pos2(close_button.left() - 8.0, title_row.bottom()),
        );
        let previous = editor_host::egui::Rect::from_min_size(
            preset_row.left_top(),
            editor_host::egui::vec2(button_width, preset_row.height()),
        );
        let next = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(preset_row.right() - button_width, preset_row.top()),
            editor_host::egui::vec2(button_width, preset_row.height()),
        );
        let name = editor_host::egui::Rect::from_min_max(
            editor_host::egui::pos2(previous.right(), preset_row.top()),
            editor_host::egui::pos2(next.left(), preset_row.bottom()),
        );
        let browser = editor_host::egui::Rect::from_min_size(
            titlebar.left_top() + editor_host::egui::vec2(8.0, EDITOR_FRAME_TITLE_ROW_HEIGHT + 6.0),
            editor_host::egui::vec2(titlebar.width() - 16.0, EDITOR_FRAME_PRESET_BROWSER_HEIGHT),
        );
        AppEditorFramePresetLayout {
            title_row,
            title_text,
            close_button,
            preset_row,
            previous,
            name,
            next,
            browser,
        }
    }

    fn render_preset_strip(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        state: &editor_host::EditorHostState,
        titlebar: editor_host::egui::Rect,
    ) -> Option<editor_host::EditorFrameCommand> {
        let preset = state.preset.as_ref()?;
        let layout = self.preset_layout(titlebar);

        ui.painter().rect_filled(
            layout.preset_row,
            6.0,
            if preset.expanded {
                self.style.control_background_active
            } else {
                self.style.control_background
            },
        );
        ui.painter().rect_stroke(
            layout.preset_row,
            6.0,
            editor_host::egui::Stroke::new(self.border_width, self.style.border_color),
            editor_host::egui::StrokeKind::Inside,
        );
        ui.painter().line_segment(
            [layout.previous.right_top(), layout.previous.right_bottom()],
            editor_host::egui::Stroke::new(self.border_width, self.style.border_color),
        );
        ui.painter().line_segment(
            [layout.next.left_top(), layout.next.left_bottom()],
            editor_host::egui::Stroke::new(self.border_width, self.style.border_color),
        );

        let previous =
            self.preset_segment_button(ui, layout.previous, AppEditorFrameIcon::ChevronLeft);
        let next = self.preset_segment_button(ui, layout.next, AppEditorFrameIcon::ChevronRight);
        let name = ui.allocate_rect(layout.name, editor_host::egui::Sense::click());
        if name.hovered() {
            ui.painter().rect_filled(
                layout.name.shrink2(editor_host::egui::vec2(0.0, 1.0)),
                0.0,
                self.style.control_background_hovered,
            );
        }
        ui.painter().text(
            layout.name.left_center() + editor_host::egui::vec2(10.0, 0.0),
            editor_host::egui::Align2::LEFT_CENTER,
            ellipsize_for_width(
                &preset.current_name,
                layout.name.width() - 34.0,
                ui_style::FONT_SIZE_UI_SM as f32,
            ),
            editor_host::egui::FontId::proportional(ui_style::FONT_SIZE_UI_SM as f32),
            self.style.title_color,
        );
        self.paint_icon(
            ui,
            editor_host::egui::Rect::from_center_size(
                layout.name.right_center() - editor_host::egui::vec2(12.0, 0.0),
                editor_host::egui::vec2(EDITOR_FRAME_ICON_SIZE, EDITOR_FRAME_ICON_SIZE),
            ),
            Self::preset_menu_icon(preset.expanded),
            self.style.muted_text,
        );

        if name.clicked() {
            return Some(editor_host::EditorFrameCommand::TogglePresetBrowser);
        }
        if previous.clicked() {
            return Some(editor_host::EditorFrameCommand::PreviousPreset);
        }
        if next.clicked() {
            return Some(editor_host::EditorFrameCommand::NextPreset);
        }

        if preset.expanded {
            return self.render_preset_browser(ui, preset, layout.browser);
        }
        None
    }

    fn preset_segment_button(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        rect: editor_host::egui::Rect,
        icon: AppEditorFrameIcon,
    ) -> editor_host::egui::Response {
        let response = ui.allocate_rect(rect, editor_host::egui::Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect.shrink(1.0), 5.0, self.style.control_background_hovered);
        }
        let icon_rect = editor_host::egui::Rect::from_center_size(
            rect.center(),
            editor_host::egui::vec2(EDITOR_FRAME_ICON_SIZE, EDITOR_FRAME_ICON_SIZE),
        );
        self.paint_icon(ui, icon_rect, icon, self.style.close_icon);
        response
    }

    fn preset_icon_button(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        rect: editor_host::egui::Rect,
        icon: AppEditorFrameIcon,
    ) -> editor_host::egui::Response {
        let response = ui.allocate_rect(rect, editor_host::egui::Sense::click());
        let fill = if response.hovered() {
            self.style.control_background_hovered
        } else {
            self.style.control_background
        };
        ui.painter().rect_filled(rect, 5.0, fill);
        ui.painter().rect_stroke(
            rect,
            5.0,
            editor_host::egui::Stroke::new(self.border_width, self.style.border_color),
            editor_host::egui::StrokeKind::Inside,
        );
        let icon_rect = editor_host::egui::Rect::from_center_size(
            rect.center(),
            editor_host::egui::vec2(EDITOR_FRAME_ICON_SIZE, EDITOR_FRAME_ICON_SIZE),
        );
        self.paint_icon(ui, icon_rect, icon, self.style.close_icon);
        response
    }

    fn paint_icon(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        rect: editor_host::egui::Rect,
        icon: AppEditorFrameIcon,
        tint: editor_host::egui::Color32,
    ) {
        if let Some(texture) = self.icon_texture(ui.ctx(), icon) {
            ui.put(
                rect,
                editor_host::egui::Image::from_texture((texture.id(), rect.size())).tint(tint),
            );
        }
    }

    fn icon_texture(
        &mut self,
        ctx: &editor_host::egui::Context,
        icon: AppEditorFrameIcon,
    ) -> Option<&editor_host::egui::TextureHandle> {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.icon_textures.entry(icon) {
            let image = render_lucide_icon(icon)?;
            let texture = ctx.load_texture(
                format!("editor-frame-icon-{icon:?}"),
                image,
                editor_host::egui::TextureOptions::LINEAR,
            );
            entry.insert(texture);
        }
        self.icon_textures.get(&icon)
    }

    fn render_preset_browser(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        preset: &editor_host::EditorPresetState,
        rect: editor_host::egui::Rect,
    ) -> Option<editor_host::EditorFrameCommand> {
        let row_height = 24.0;
        ui.painter().rect_filled(rect, 7.0, self.style.frame_color);
        ui.painter().rect_stroke(
            rect,
            7.0,
            editor_host::egui::Stroke::new(1.0, self.style.border_color),
            editor_host::egui::StrokeKind::Inside,
        );
        let save_rect = Self::preset_browser_save_rect(rect);
        let save = self.preset_icon_button(ui, save_rect, AppEditorFrameIcon::Save);
        if save.clicked() {
            return Some(editor_host::EditorFrameCommand::SavePreset);
        }
        let list_rect = Self::preset_browser_list_rect(rect);
        if preset.items.is_empty() {
            ui.painter().text(
                list_rect.center(),
                editor_host::egui::Align2::CENTER_CENTER,
                "No user presets",
                editor_host::egui::FontId::proportional(ui_style::FONT_SIZE_UI_XS as f32),
                self.style.close_icon,
            );
            return None;
        }

        let mut command = None;
        ui.scope_builder(
            editor_host::egui::UiBuilder::new()
                .max_rect(list_rect)
                .layout(editor_host::egui::Layout::top_down(
                    editor_host::egui::Align::Min,
                )),
            |ui| {
                editor_host::egui::ScrollArea::vertical()
                    .id_salt("editor-frame-preset-browser")
                    .auto_shrink([false, false])
                    .max_height(list_rect.height())
                    .show(ui, |ui| {
                        ui.set_width(list_rect.width());
                        for item in &preset.items {
                            if command.is_some() {
                                break;
                            }
                            command = self.render_preset_browser_item(
                                ui,
                                preset,
                                item,
                                row_height,
                                list_rect.width(),
                            );
                        }
                        ui.add_space(6.0);
                    });
            },
        );
        command
    }

    fn preset_browser_save_rect(rect: editor_host::egui::Rect) -> editor_host::egui::Rect {
        editor_host::egui::Rect::from_min_size(
            rect.left_top() + editor_host::egui::vec2(6.0, 5.0),
            editor_host::egui::vec2(28.0, 22.0),
        )
    }

    fn preset_browser_list_rect(rect: editor_host::egui::Rect) -> editor_host::egui::Rect {
        editor_host::egui::Rect::from_min_max(
            rect.left_top() + editor_host::egui::vec2(6.0, 34.0),
            rect.right_bottom() - editor_host::egui::vec2(6.0, 10.0),
        )
    }

    fn render_preset_browser_item(
        &mut self,
        ui: &mut editor_host::egui::Ui,
        preset: &editor_host::EditorPresetState,
        item: &editor_host::EditorPresetItem,
        row_height: f32,
        row_width: f32,
    ) -> Option<editor_host::EditorFrameCommand> {
        let is_renaming = self.renaming_preset_id.as_deref() == Some(item.id.as_str());
        let is_confirming_delete =
            self.delete_confirmation_preset_id.as_deref() == Some(item.id.as_str());
        let sense = if is_renaming || is_confirming_delete {
            editor_host::egui::Sense::hover()
        } else {
            editor_host::egui::Sense::click()
        };
        let (item_rect, response) =
            ui.allocate_exact_size(editor_host::egui::vec2(row_width, row_height), sense);
        if response.hovered()
            || preset.selected_id.as_deref() == Some(item.id.as_str())
            || is_renaming
            || is_confirming_delete
        {
            ui.painter()
                .rect_filled(item_rect.shrink(2.0), 5.0, self.style.titlebar_color);
        }

        if is_confirming_delete {
            ui.painter().text(
                item_rect.left_center() + editor_host::egui::vec2(8.0, 0.0),
                editor_host::egui::Align2::LEFT_CENTER,
                Self::delete_confirmation_label(&item.name, item_rect.width() - 126.0),
                editor_host::egui::FontId::proportional(ui_style::FONT_SIZE_UI_SM as f32),
                self.style.title_color,
            );
            let remove_rect = editor_host::egui::Rect::from_min_size(
                editor_host::egui::pos2(item_rect.right() - 112.0, item_rect.top() + 2.0),
                editor_host::egui::vec2(62.0, item_rect.height() - 4.0),
            );
            let cancel_rect = editor_host::egui::Rect::from_min_size(
                editor_host::egui::pos2(item_rect.right() - 46.0, item_rect.top() + 2.0),
                editor_host::egui::vec2(42.0, item_rect.height() - 4.0),
            );
            if self.preset_text_button(ui, remove_rect, "Remove").clicked() {
                return self.confirm_preset_delete(&item.id);
            }
            if self.preset_text_button(ui, cancel_rect, "Cancel").clicked() {
                self.cancel_preset_delete();
            }
            return None;
        }

        let rename_rect = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(item_rect.right() - 56.0, item_rect.top() + 2.0),
            editor_host::egui::vec2(24.0, item_rect.height() - 4.0),
        );
        let delete_rect = rename_rect.translate(editor_host::egui::vec2(28.0, 0.0));
        let content_rect = editor_host::egui::Rect::from_min_max(
            item_rect.left_top(),
            editor_host::egui::pos2(rename_rect.left() - 4.0, item_rect.bottom()),
        );
        if is_renaming {
            let rename_rect = editor_host::egui::Rect::from_min_size(
                editor_host::egui::pos2(item_rect.right() - 56.0, item_rect.top() + 2.0),
                editor_host::egui::vec2(24.0, item_rect.height() - 4.0),
            );
            let edit_rect = editor_host::egui::Rect::from_min_max(
                item_rect.left_top() + editor_host::egui::vec2(6.0, 3.0),
                rename_rect.left_bottom() - editor_host::egui::vec2(6.0, 3.0),
            );
            let edit = ui.put(
                edit_rect,
                editor_host::egui::TextEdit::singleline(&mut self.renaming_preset_value)
                    .id_salt(("preset-rename", item.id.as_str()))
                    .desired_width(edit_rect.width())
                    .font(editor_host::egui::FontId::proportional(
                        ui_style::FONT_SIZE_UI_SM as f32,
                    )),
            );
            let focus_requested = self.rename_focus_requested;
            if focus_requested {
                edit.request_focus();
                self.rename_focus_requested = false;
            }
            let enter = ui.input(|input| input.key_pressed(editor_host::egui::Key::Enter));
            let escape = ui.input(|input| input.key_pressed(editor_host::egui::Key::Escape));
            if enter {
                let name = self.renaming_preset_value.trim().to_string();
                self.cancel_preset_rename();
                if !name.is_empty() {
                    return Some(editor_host::EditorFrameCommand::RenamePreset {
                        id: item.id.clone(),
                        name,
                    });
                }
            } else if escape || (!focus_requested && edit.lost_focus()) {
                self.cancel_preset_rename();
            }
        } else {
            ui.painter().text(
                item_rect.left_center() + editor_host::egui::vec2(8.0, 0.0),
                editor_host::egui::Align2::LEFT_CENTER,
                ellipsize_for_width(
                    &item.name,
                    content_rect.width() - 12.0,
                    ui_style::FONT_SIZE_UI_SM as f32,
                ),
                editor_host::egui::FontId::proportional(ui_style::FONT_SIZE_UI_SM as f32),
                self.style.title_color,
            );
        }
        let rename = self.preset_icon_button(ui, rename_rect, AppEditorFrameIcon::Pencil);
        if rename.clicked() {
            self.begin_preset_rename(&item.id, &item.name);
        }
        let delete = self.preset_icon_button(ui, delete_rect, AppEditorFrameIcon::Trash);
        if delete.clicked() {
            return self.request_preset_delete(&item.id);
        }
        if response.clicked() {
            return Some(editor_host::EditorFrameCommand::LoadPreset(item.id.clone()));
        }
        None
    }

    fn preset_text_button(
        &self,
        ui: &mut editor_host::egui::Ui,
        rect: editor_host::egui::Rect,
        label: &str,
    ) -> editor_host::egui::Response {
        let response = ui.allocate_rect(rect, editor_host::egui::Sense::click());
        let fill = if response.hovered() {
            self.style.control_background_hovered
        } else {
            self.style.control_background
        };
        ui.painter().rect_filled(rect, 5.0, fill);
        ui.painter().rect_stroke(
            rect,
            5.0,
            editor_host::egui::Stroke::new(self.border_width, self.style.border_color),
            editor_host::egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            editor_host::egui::Align2::CENTER_CENTER,
            label,
            editor_host::egui::FontId::proportional(ui_style::FONT_SIZE_UI_XS as f32),
            self.style.title_color,
        );
        response
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

fn render_lucide_icon(icon: AppEditorFrameIcon) -> Option<editor_host::egui::ColorImage> {
    let svg = std::str::from_utf8(icon.svg_bytes())
        .ok()?
        .replace("currentColor", "white");
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &options).ok()?;
    let size = tree.size().to_int_size().to_size();
    let dimension = 64;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(dimension, dimension)?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        dimension as f32 / size.width(),
        dimension as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(editor_host::egui::ColorImage::from_rgba_unmultiplied(
        [dimension as usize, dimension as usize],
        pixmap.data(),
    ))
}

fn ellipsize_for_width(text: &str, width: f32, font_size: f32) -> String {
    let max_chars = (width / (font_size * 0.56)).floor().max(1.0) as usize;
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let byte_index = text
        .char_indices()
        .nth(max_chars - 3)
        .map_or(text.len(), |(index, _)| index);
    format!("{}...", &text[..byte_index])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenameTarget {
    Track(usize),
    Bus(u16),
}
pub(super) const KEYBOARD_SCROLL_STEP: f32 = 84.0;
pub(super) const SHORTCUTS_ACTION_ROW_HEIGHT: f32 = 48.0;
const MIN_SVG_ZOOM: f32 = 0.4;
const MAX_SVG_ZOOM: f32 = 3.0;
const SVG_ZOOM_STEP: f32 = 0.1;
const MIN_SVG_PAGE_BRIGHTNESS: u8 = 0;
const MAX_SVG_PAGE_BRIGHTNESS: u8 = 100;
const SVG_PAGE_BRIGHTNESS_STEP: u8 = 10;
const SCORE_ZOOM_PREVIEW_INTERVAL: Duration = Duration::from_millis(16);
const SCORE_ZOOM_PREVIEW_SETTLE_DELAY: Duration = Duration::from_millis(120);
const ACTIVE_PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(33);
const PASSIVE_PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(120);
const EDITOR_HOST_POLL_INTERVAL: Duration = Duration::from_millis(33);
pub(super) const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn audio_engine_options(playback: &settings::PlaybackSettings) -> AudioEngineOptions {
    AudioEngineOptions {
        device: playback.device.clone(),
        chase_notes_on_seek: playback.chase_notes_on_seek,
        sample_rate: playback.sample_rate,
        block_size: playback.block_size,
        ..AudioEngineOptions::default()
    }
}

fn plugin_scan_cache_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "lilypalooza")
        .map(|project_dirs| project_dirs.config_dir().join("plugin-cache.ron"))
        .unwrap_or_else(|| PathBuf::from("plugin-cache.ron"))
}

fn plugin_validator_path() -> PathBuf {
    plugin_validator_path_for_exe(
        &std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lilypalooza")),
    )
}

fn plugin_validator_path_for_exe(exe: &Path) -> PathBuf {
    let validator_name = format!(
        "lilypalooza-plugin-validator{}",
        std::env::consts::EXE_SUFFIX
    );

    if is_macos_app_bundle_exe(exe)
        && let Some(contents_dir) = exe
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name == "Contents"))
    {
        return contents_dir.join("MacOS").join(validator_name);
    }

    exe.parent()
        .map(|parent| parent.join(&validator_name))
        .unwrap_or_else(|| PathBuf::from(validator_name))
}

fn is_macos_app_bundle_exe(exe: &Path) -> bool {
    exe.ancestors()
        .any(|path| path.extension().is_some_and(|extension| extension == "app"))
}

fn ensure_plugin_validator_available(validator: &Path) -> Result<(), String> {
    if validator.exists() {
        Ok(())
    } else {
        Err(format!(
            "Plugin validator helper not found at {}",
            validator.display()
        ))
    }
}

pub(super) fn editor_file_browser_column_scroll_id(index: usize) -> Id {
    Id::new(Box::leak(
        format!("editor-file-browser-column-{index}").into_boxed_str(),
    ))
}

pub(super) type WorkspacePaneKind = crate::settings::WorkspacePane;

type DockGroupId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorHeaderMenuSection {
    File,
    Edit,
    Appearance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorFileMenuSection {
    OpenRecent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectMenuSection {
    Project,
    View,
}

struct Lilypalooza {
    theme: iced::Theme,
    main_window_id: window::Id,
    main_window_snapshot: Option<WindowSnapshot>,
    window_width: f32,
    window_height: f32,
    lilypond_status: LilypondStatus,
    current_score: Option<SelectedScore>,
    error_prompt: Option<ErrorPrompt>,
    prompt_ok_action: Option<PromptOkAction>,
    prompt_selected_button: PromptSelectedButton,
    logger: Logger,
    score_watcher: Option<ScoreWatcher>,
    editor_file_watcher: Option<EditorFileWatcher>,
    browser_file_watcher: Option<BrowserFileWatcher>,
    browser_history_dir: Option<TempDir>,
    browser_history_next_stash_id: u64,
    browser_undo_stack: Vec<BrowserHistoryEntry>,
    browser_redo_stack: Vec<BrowserHistoryEntry>,
    mixer_undo_stack: Vec<MixerState>,
    mixer_redo_stack: Vec<MixerState>,
    pending_mixer_undo_snapshot: Option<MixerState>,
    pending_mixer_message_after_editor_close: Option<(window::Id, MixerMessage)>,
    pending_mixer_message_after_editor_detach: Option<MixerMessage>,
    build_dir: Option<TempDir>,
    compile_requested: bool,
    compile_outputs_loading: bool,
    compile_generation: u64,
    spinner_step: usize,
    compile_session: Option<lilypond::CompileSession>,
    playback: Option<AudioEngine>,
    soundfont_status: SoundfontStatus,
    playback_settings: settings::PlaybackSettings,
    plugin_search_paths: Vec<settings::PluginSearchPath>,
    plugin_scan: lilypalooza_plugin_scan::PluginScanState,
    plugin_scan_cache: lilypalooza_plugin_scan::PluginScanCache,
    saved_project_state: Option<ProjectState>,
    project_mixer_state: MixerState,
    processor_presets: processor_presets::ProcessorPresetLibrary,
    expanded_processor_preset_browser: Option<processor_editor_windows::EditorTarget>,
    workspace_panes: pane_grid::State<DockGroupId>,
    dock_layout: Option<DockNode>,
    dock_groups: HashMap<DockGroupId, DockGroup>,
    next_dock_group_id: DockGroupId,
    folded_panes: Vec<FoldedPaneState>,
    focused_workspace_pane: Option<WorkspacePaneKind>,
    hovered_workspace_pane: Option<WorkspacePaneKind>,
    pressed_workspace_pane: Option<WorkspacePaneKind>,
    workspace_drag_origin: Option<Point>,
    dragged_workspace_pane: Option<WorkspacePaneKind>,
    dock_drop_target: Option<DockDropTarget>,
    open_header_overflow_menu: Option<DockGroupId>,
    open_editor_menu_section: Option<EditorHeaderMenuSection>,
    open_editor_file_menu_section: Option<EditorFileMenuSection>,
    hovered_editor_file_menu_section: Option<EditorFileMenuSection>,
    open_project_menu: bool,
    open_project_menu_section: Option<ProjectMenuSection>,
    open_project_recent: bool,
    open_shortcuts_dialog: bool,
    processor_editor_windows: EditorWindowManager,
    shortcuts_search_query: String,
    shortcuts_search_input_id: Id,
    shortcuts_selected_action: Option<settings::ShortcutActionId>,
    hovered_tooltip_key: Option<String>,
    open_tooltip_key: Option<String>,
    pressed_editor_tab: Option<u64>,
    hovered_editor_tab: Option<u64>,
    dragged_editor_tab: Option<u64>,
    editor_tab_drag_origin: Option<Point>,
    editor_tab_drop_after: bool,
    editor_file_browser_focused: bool,
    editor_file_browser_scroll_x: f32,
    editor_file_browser_viewport_width: f32,
    editor_file_browser_column_scroll_y: HashMap<usize, f32>,
    editor_file_browser_column_viewport_height: HashMap<usize, f32>,
    editor_file_browser_cursor: Option<Point>,
    browser_clipboard: Option<BrowserClipboard>,
    browser_inline_edit: Option<BrowserInlineEdit>,
    browser_inline_edit_value: String,
    browser_inline_edit_input_id: Id,
    browser_pressed_entry: Option<BrowserPressedEntry>,
    browser_drag_state: Option<BrowserDragState>,
    browser_drop_target: Option<BrowserDropTarget>,
    editor_tabbar_scroll_x: f32,
    editor_tabbar_viewport_width: f32,
    mixer_instrument_scroll_x: f32,
    mixer_instrument_viewport_width: f32,
    mixer_bus_scroll_x: f32,
    mixer_bus_viewport_width: f32,
    open_mixer_effect_rack_tracks: Vec<usize>,
    effect_rack_scroll_y: HashMap<usize, f32>,
    effect_rack_viewport_height: HashMap<usize, f32>,
    editor_tabbar_autoscroll_direction: i8,
    editor_tabbar_drag_pointer_x: Option<f32>,
    pending_reveal_editor_tab: Option<u64>,
    renaming_editor_tab: Option<u64>,
    editor_tab_rename_value: String,
    editor_tab_rename_input_id: Id,
    renaming_target: Option<RenameTarget>,
    renaming_origin: Option<WorkspacePaneKind>,
    track_color_picker_target: Option<(usize, WorkspacePaneKind)>,
    track_rename_was_focused: bool,
    track_rename_value: String,
    track_rename_color_picker_open: bool,
    track_rename_color_value: Color,
    selected_track_index: Option<usize>,
    open_processor_browser_target: Option<processor_editor_windows::EditorTarget>,
    hovered_processor_slot: Option<(
        processor_editor_windows::EditorTarget,
        mixer::ProcessorSlotSegment,
    )>,
    effect_rack_hovered_effect: Option<(usize, usize)>,
    effect_drag_source: Option<(usize, usize)>,
    effect_drag_target: Option<(usize, usize)>,
    effect_rack_autoscroll_direction: i8,
    effect_rack_drag_pointer_y: Option<f32>,
    open_instrument_browser_track: Option<usize>,
    instrument_browser_search: String,
    processor_browser_expanded_sections: Vec<mixer::ProcessorBrowserSectionKey>,
    instrument_browser_search_input_id: Id,
    track_name_overrides: Vec<Option<String>>,
    track_color_overrides: Vec<Option<Color>>,
    metronome: crate::state::MetronomeState,
    metronome_menu_open: bool,
    editor_recent_files: Vec<PathBuf>,
    recent_projects: Vec<PathBuf>,
    editor_recent_files_limit: usize,
    editor: editor::EditorState,
    editor_font_metrics_refresh_pending: bool,
    rendered_score: Option<RenderedScore>,
    score_cursor_maps: Option<ScoreCursorMaps>,
    score_cursor_overlay: Option<ScoreCursorPlacement>,
    piano_roll: PianoRollState,
    svg_zoom: f32,
    svg_page_brightness: u8,
    svg_scroll_x: f32,
    svg_scroll_y: f32,
    score_viewport_cursor: Option<iced::Point>,
    score_zoom_last_interaction: Option<Instant>,
    score_zoom_persist_pending: bool,
    score_zoom_preview: Option<ScoreZoomPreview>,
    score_zoom_preview_pending: Option<ScoreZoomPreviewRequest>,
    piano_roll_viewport_cursor: Option<iced::Point>,
    transport_seek_preview: Option<f32>,
    keyboard_modifiers: keyboard::Modifiers,
    primary_mouse_pressed: bool,
    shortcut_settings: settings::ShortcutSettings,
    project_root: Option<PathBuf>,
    project_name: Option<String>,
    pending_editor_action: Option<PendingEditorAction>,
    pending_editor_save_as_tab: Option<u64>,
    pending_editor_rename_tab: Option<u64>,
    default_global_state: GlobalState,
}

struct SelectedScore {
    path: PathBuf,
    file_name: String,
}

struct RenderedScore {
    pages: Vec<RenderedPage>,
    current_page: usize,
}

#[derive(Debug, Clone)]
struct LoadedCompileOutputs {
    score_path: PathBuf,
    rendered_pages: Vec<LoadedRenderedPage>,
    midi_files: Vec<crate::midi::MidiRollFile>,
    score_cursor_maps: Option<ScoreCursorMaps>,
    point_and_click_disabled: bool,
    score_has_repeats: bool,
}

struct RenderedPage {
    handle: svg::Handle,
    svg_bytes: Bytes,
    display_size: SvgSize,
    coord_size: SvgSize,
    note_anchors: Vec<score_cursor::SvgNoteAnchor>,
    system_bands: Vec<score_cursor::SystemBand>,
}

#[derive(Debug, Clone)]
struct LoadedRenderedPage {
    svg_bytes: Vec<u8>,
    display_size: SvgSize,
    coord_size: SvgSize,
    note_anchors: Vec<score_cursor::SvgNoteAnchor>,
    system_bands: Vec<score_cursor::SystemBand>,
}

#[derive(Debug, Clone, Copy)]
struct SvgSize {
    width: f32,
    height: f32,
}

#[derive(Clone)]
struct ScoreZoomPreview {
    page_index: usize,
    tier: ScoreZoomPreviewTier,
    handle: image::Handle,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScoreZoomPreviewRequest {
    page_index: usize,
    zoom: f32,
    tier: ScoreZoomPreviewTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScoreZoomPreviewTier {
    Fallback,
    Primary,
}

impl RenderedScore {
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn current_page_number(&self) -> usize {
        self.current_page.saturating_add(1)
    }

    fn current_page(&self) -> Option<&RenderedPage> {
        self.pages.get(self.current_page)
    }
}

#[derive(Debug, Clone)]
struct DockGroup {
    tabs: Vec<WorkspacePaneKind>,
    active: WorkspacePaneKind,
}

#[derive(Debug, Clone, PartialEq)]
struct FoldedPaneState {
    pane: WorkspacePaneKind,
    restore: FoldedPaneRestore,
}

#[derive(Debug, Clone, PartialEq)]
enum FoldedPaneRestore {
    Tab {
        anchor: WorkspacePaneKind,
    },
    Standalone,
    Split {
        anchor: WorkspacePaneKind,
        axis: pane_grid::Axis,
        ratio: f32,
        insert_first: bool,
        sibling_panes: Vec<WorkspacePaneKind>,
    },
}

#[derive(Debug, Clone)]
enum DockNode {
    Group(DockGroupId),
    Split {
        axis: pane_grid::Axis,
        ratio: f32,
        first: Box<DockNode>,
        second: Box<DockNode>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DockDropRegion {
    Top,
    Right,
    Bottom,
    Left,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DockDropTarget {
    pub(super) group_id: DockGroupId,
    pub(super) region: DockDropRegion,
}

enum LilypondStatus {
    Checking,
    Ready {
        detected: lilypond::Version,
        min_required: lilypond::Version,
    },
    Unavailable,
}

enum SoundfontStatus {
    NotSelected,
    Ready(PathBuf),
    Error,
}

#[derive(Debug, Clone)]
enum PromptOkAction {
    ExitApp,
    ClearLogs,
    ReloadEditorTab(u64),
    DeleteBrowserPath(PathBuf),
    RemoveBus(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserInlineEditKind {
    Rename,
    NewFile,
    NewDirectory,
}

#[derive(Debug, Clone)]
enum BrowserHistoryEntry {
    Create {
        path: PathBuf,
        stash_path: Option<PathBuf>,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    Delete {
        path: PathBuf,
        stash_path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserClipboardKind {
    Cut,
    Copy,
}

#[derive(Debug, Clone)]
struct BrowserClipboard {
    path: PathBuf,
    kind: BrowserClipboardKind,
}

#[derive(Debug, Clone)]
struct BrowserInlineEdit {
    column_index: usize,
    parent_dir: PathBuf,
    target_path: Option<PathBuf>,
    kind: BrowserInlineEditKind,
}

#[derive(Debug, Clone)]
struct BrowserPressedEntry {
    path: PathBuf,
    is_dir: bool,
    origin: Point,
}

#[derive(Debug, Clone)]
struct BrowserDragState {
    source_path: PathBuf,
    source_is_dir: bool,
}

#[derive(Debug, Clone)]
struct BrowserDropTarget {
    column_index: usize,
    path: PathBuf,
    target_dir: PathBuf,
}

#[derive(Debug, Clone)]
enum PendingEditorAction {
    ResolveDirtyTabs {
        dirty_tab_ids: Vec<u64>,
        continuation: EditorContinuation,
    },
    ResolveDirtyProject {
        continuation: EditorContinuation,
    },
}

#[derive(Debug, Clone)]
enum EditorContinuation {
    CloseTab(u64),
    LoadProject(PathBuf),
    OpenScore(PathBuf),
    ExitApp,
}

pub fn run(
    startup_soundfont: Option<PathBuf>,
    startup_score: Option<PathBuf>,
    audio_enabled: bool,
) -> iced::Result {
    iced::daemon(
        move || {
            new(
                startup_soundfont.clone(),
                startup_score.clone(),
                audio_enabled,
            )
        },
        update,
        window_view,
    )
    .default_font(crate::fonts::UI)
    .font(crate::fonts::MANROPE_REGULAR_BYTES)
    .font(crate::fonts::MANROPE_MEDIUM_BYTES)
    .font(crate::fonts::MANROPE_BOLD_BYTES)
    .font(crate::fonts::JETBRAINS_MONO_BYTES)
    .font(crate::fonts::JETBRAINS_MONO_BOLD_BYTES)
    .font(crate::fonts::JETBRAINS_MONO_ITALIC_BYTES)
    .font(crate::fonts::JETBRAINS_MONO_BOLD_ITALIC_BYTES)
    .font(crate::fonts::JETBRAINS_MONO_MEDIUM_BYTES)
    .font(crate::fonts::JETBRAINS_MONO_MEDIUM_ITALIC_BYTES)
    .theme(|state: &Lilypalooza, _window| state.theme.clone())
    .title(window_title)
    .subscription(subscription)
    .run()
}

fn main_window_settings() -> window::Settings {
    window::Settings {
        min_size: Some(Size::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)),
        exit_on_close_request: false,
        ..window::Settings::default()
    }
}

fn window_view<'a>(app: &'a Lilypalooza, window_id: window::Id) -> iced::Element<'a, Message> {
    if window_id == app.main_window_id {
        return view(app);
    }

    iced::widget::container(iced::widget::text(""))
        .width(iced::Fill)
        .height(iced::Fill)
        .into()
}

fn window_title(app: &Lilypalooza, window_id: window::Id) -> String {
    if window_id == app.main_window_id {
        return "Lilypalooza".to_string();
    }

    app.processor_editor_windows
        .window_title(window_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "Processor Editor".to_string())
}

fn new(
    startup_soundfont: Option<PathBuf>,
    startup_score: Option<PathBuf>,
    audio_enabled: bool,
) -> (Lilypalooza, Task<Message>) {
    let default_settings = settings::AppSettings::default();
    let default_global_state = GlobalState::default();
    let (stored_settings, settings_error) = match settings::load() {
        Ok(settings) => (settings, None),
        Err(error) => (default_settings.clone(), Some(error)),
    };
    let (stored_state, state_error) = match state::load_global() {
        Ok(state) => (state, None),
        Err(error) => (default_global_state.clone(), Some(error)),
    };

    new_with_loaded_state(
        startup_soundfont,
        startup_score,
        audio_enabled,
        stored_settings,
        settings_error,
        stored_state,
        state_error,
    )
}

fn new_with_loaded_state(
    startup_soundfont: Option<PathBuf>,
    startup_score: Option<PathBuf>,
    audio_enabled: bool,
    stored_settings: settings::AppSettings,
    settings_error: Option<String>,
    mut stored_state: GlobalState,
    state_error: Option<String>,
) -> (Lilypalooza, Task<Message>) {
    let editor_host_error = editor_host::prepare_process().err();
    let (main_window_id, open_main_window) = window::open(main_window_settings());
    let default_global_state = GlobalState::default();
    let browser_history_dir = tempfile::Builder::new()
        .prefix("lilypalooza-browser-history")
        .tempdir()
        .ok();
    let startup_soundfonts = match startup_soundfont {
        Some(path) => vec![path],
        None => stored_settings.playback.soundfonts.clone(),
    };
    migrate_workspace_layout(
        &mut stored_state.workspace_layout.root,
        &stored_state.workspace_layout.folded_panes,
    );

    let (dock_layout, dock_groups, next_dock_group_id, workspace_panes) =
        build_dock_runtime(stored_state.workspace_layout.root.as_ref());
    let mut folded_panes: Vec<_> = stored_state
        .workspace_layout
        .folded_panes
        .iter()
        .cloned()
        .map(folded_pane_from_settings)
        .collect();
    if folded_panes.is_empty() && !stored_state.workspace_layout.piano_visible {
        folded_panes.push(FoldedPaneState {
            pane: WorkspacePaneKind::PianoRoll,
            restore: FoldedPaneRestore::Tab {
                anchor: WorkspacePaneKind::Score,
            },
        });
    }
    let mixer_visible = dock_groups
        .values()
        .any(|group| group.tabs.contains(&WorkspacePaneKind::Mixer));
    if !mixer_visible
        && !folded_panes
            .iter()
            .any(|folded| folded.pane == WorkspacePaneKind::Mixer)
    {
        folded_panes.push(FoldedPaneState {
            pane: WorkspacePaneKind::Mixer,
            restore: FoldedPaneRestore::Standalone,
        });
    }
    let piano_roll_visible = !folded_panes
        .iter()
        .any(|folded| folded.pane == WorkspacePaneKind::PianoRoll);

    let mut piano_roll = PianoRollState::new(default_global_state.piano_roll_view);
    piano_roll.visible = piano_roll_visible;
    piano_roll.apply_view_settings(
        stored_state.piano_roll_view.zoom_x,
        stored_state.piano_roll_view.beat_subdivision,
    );

    let initial_focused_workspace_pane = dock_layout
        .as_ref()
        .and_then(|layout| first_active_workspace_pane(layout, &dock_groups))
        .or_else(|| dock_groups.values().next().map(|group| group.active));

    let (playback, playback_init_error) = if audio_enabled {
        match AudioEngine::start_cpal(
            MixerState::new(),
            audio_engine_options(&stored_settings.playback),
        ) {
            Ok(engine) => (Some(engine), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let project_mixer_state = MixerState::new();
    let shortcuts_search_input_id = Id::unique();
    let browser_inline_edit_input_id = Id::unique();
    let editor_tab_rename_input_id = Id::unique();
    let instrument_browser_search_input_id = Id::unique();
    let editor = editor::EditorState::new(
        iced::Theme::Dark,
        stored_settings.editor_view,
        stored_settings.editor_theme,
    );
    let logger = Logger::new();
    let processor_editor_windows = EditorWindowManager::default();
    let track_rename_color_value = crate::track_colors::default_track_color(0);
    let editor_recent_files = stored_state.editor_recent_files.clone();
    let recent_projects = stored_state.recent_projects.clone();
    let editor_recent_files_limit = stored_settings.editor_recent_files_limit.max(1);
    let shortcut_settings = stored_settings.shortcuts.clone();
    let svg_zoom = stored_state
        .score_view
        .zoom
        .clamp(MIN_SVG_ZOOM, MAX_SVG_ZOOM);
    let svg_page_brightness = stored_state
        .score_view
        .page_brightness
        .clamp(MIN_SVG_PAGE_BRIGHTNESS, MAX_SVG_PAGE_BRIGHTNESS);

    let mut app = Lilypalooza {
        theme: iced::Theme::Dark,
        main_window_id,
        main_window_snapshot: None,
        window_width: MIN_WINDOW_WIDTH,
        window_height: MIN_WINDOW_HEIGHT,
        lilypond_status: LilypondStatus::Checking,
        current_score: None,
        error_prompt: None,
        prompt_ok_action: None,
        prompt_selected_button: PromptSelectedButton::Ok,
        logger,
        score_watcher: None,
        editor_file_watcher: None,
        browser_file_watcher: None,
        browser_history_dir,
        browser_history_next_stash_id: 1,
        browser_undo_stack: Vec::new(),
        browser_redo_stack: Vec::new(),
        mixer_undo_stack: Vec::new(),
        mixer_redo_stack: Vec::new(),
        pending_mixer_undo_snapshot: None,
        pending_mixer_message_after_editor_close: None,
        pending_mixer_message_after_editor_detach: None,
        build_dir: None,
        compile_requested: false,
        compile_outputs_loading: false,
        compile_generation: 0,
        spinner_step: 0,
        compile_session: None,
        playback,
        soundfont_status: SoundfontStatus::NotSelected,
        playback_settings: stored_settings.playback.clone(),
        plugin_search_paths: stored_settings.plugin_search_paths(),
        plugin_scan: lilypalooza_plugin_scan::PluginScanState::default(),
        plugin_scan_cache: lilypalooza_plugin_scan::PluginScanCache::load_from(
            &plugin_scan_cache_path(),
        ),
        saved_project_state: None,
        project_mixer_state,
        processor_presets: stored_state.processor_presets.clone(),
        expanded_processor_preset_browser: None,
        workspace_panes,
        dock_layout,
        dock_groups,
        next_dock_group_id,
        folded_panes,
        focused_workspace_pane: initial_focused_workspace_pane,
        hovered_workspace_pane: None,
        pressed_workspace_pane: None,
        workspace_drag_origin: None,
        dragged_workspace_pane: None,
        dock_drop_target: None,
        open_header_overflow_menu: None,
        open_editor_menu_section: None,
        open_editor_file_menu_section: None,
        hovered_editor_file_menu_section: None,
        open_project_menu: false,
        open_project_menu_section: None,
        open_project_recent: false,
        open_shortcuts_dialog: false,
        processor_editor_windows,
        shortcuts_search_query: String::new(),
        shortcuts_search_input_id,
        shortcuts_selected_action: None,
        hovered_tooltip_key: None,
        open_tooltip_key: None,
        pressed_editor_tab: None,
        hovered_editor_tab: None,
        dragged_editor_tab: None,
        editor_tab_drag_origin: None,
        editor_tab_drop_after: false,
        editor_file_browser_focused: false,
        editor_file_browser_scroll_x: 0.0,
        editor_file_browser_viewport_width: 0.0,
        editor_file_browser_column_scroll_y: HashMap::new(),
        editor_file_browser_column_viewport_height: HashMap::new(),
        editor_file_browser_cursor: None,
        browser_clipboard: None,
        browser_inline_edit: None,
        browser_inline_edit_value: String::new(),
        browser_inline_edit_input_id,
        browser_pressed_entry: None,
        browser_drag_state: None,
        browser_drop_target: None,
        editor_tabbar_scroll_x: 0.0,
        editor_tabbar_viewport_width: 0.0,
        mixer_instrument_scroll_x: 0.0,
        mixer_instrument_viewport_width: 0.0,
        mixer_bus_scroll_x: 0.0,
        mixer_bus_viewport_width: 0.0,
        open_mixer_effect_rack_tracks: Vec::new(),
        effect_rack_scroll_y: HashMap::new(),
        effect_rack_viewport_height: HashMap::new(),
        editor_tabbar_autoscroll_direction: 0,
        editor_tabbar_drag_pointer_x: None,
        pending_reveal_editor_tab: None,
        renaming_editor_tab: None,
        editor_tab_rename_value: String::new(),
        editor_tab_rename_input_id,
        renaming_target: None,
        renaming_origin: None,
        track_color_picker_target: None,
        track_rename_was_focused: false,
        track_rename_value: String::new(),
        track_rename_color_picker_open: false,
        track_rename_color_value,
        selected_track_index: None,
        open_processor_browser_target: None,
        hovered_processor_slot: None,
        effect_rack_hovered_effect: None,
        effect_drag_source: None,
        effect_drag_target: None,
        effect_rack_autoscroll_direction: 0,
        effect_rack_drag_pointer_y: None,
        open_instrument_browser_track: None,
        instrument_browser_search: String::new(),
        processor_browser_expanded_sections: Vec::new(),
        instrument_browser_search_input_id,
        track_name_overrides: Vec::new(),
        track_color_overrides: Vec::new(),
        metronome: crate::state::MetronomeState::default(),
        metronome_menu_open: false,
        editor_recent_files,
        recent_projects,
        editor_recent_files_limit,
        editor,
        editor_font_metrics_refresh_pending: true,
        rendered_score: None,
        score_cursor_maps: None,
        score_cursor_overlay: None,
        piano_roll,
        svg_zoom,
        svg_page_brightness,
        svg_scroll_x: 0.0,
        svg_scroll_y: 0.0,
        score_viewport_cursor: None,
        score_zoom_last_interaction: None,
        score_zoom_persist_pending: false,
        score_zoom_preview: None,
        score_zoom_preview_pending: None,
        piano_roll_viewport_cursor: None,
        transport_seek_preview: None,
        keyboard_modifiers: keyboard::Modifiers::default(),
        primary_mouse_pressed: false,
        shortcut_settings,
        project_root: None,
        project_name: None,
        pending_editor_action: None,
        pending_editor_save_as_tab: None,
        pending_editor_rename_tab: None,
        default_global_state,
    };

    app.logger.push("Checking LilyPond availability");
    if !audio_enabled {
        app.logger.push("Audio engine disabled by --no-audio");
    }
    for path in &startup_soundfonts {
        app.logger
            .push(format!("Startup soundfont requested: {}", path.display()));
    }
    if let Some(path) = startup_score.as_ref() {
        app.logger
            .push(format!("Startup score requested: {}", path.display()));
    }
    if let Some(error) = settings_error {
        app.logger.push(format!("Settings load failed: {error}"));
    }
    if let Some(error) = state_error {
        app.logger.push(format!("State load failed: {error}"));
    }
    if let Some(error) = playback_init_error {
        app.logger
            .push(format!("Playback engine startup failed: {error}"));
    }
    if let Some(error) = editor_host_error {
        app.logger
            .push(format!("Editor host setup failed: {error}"));
    }
    #[cfg(not(test))]
    app.start_plugin_scan();

    app.restore_editor_session(
        &stored_state.editor_tabs,
        stored_state.active_editor_tab.as_deref(),
        stored_state.has_clean_untitled_editor_tab,
    );

    let mut startup_tasks = vec![
        open_main_window.map(|_| Message::Noop),
        Task::perform(
            async { lilypond::check_lilypond().map_err(|error| error.to_string()) },
            Message::StartupChecked,
        ),
    ];
    startup_tasks.push(Task::perform(
        cleanup_stale_browser_history_dirs(
            app.browser_history_dir
                .as_ref()
                .map(|dir| dir.path().to_path_buf()),
        ),
        Message::BrowserHistoryCleanupFinished,
    ));

    if audio_enabled && !startup_soundfonts.is_empty() {
        app.initialize_playback_soundfonts(startup_soundfonts);
    }
    app.saved_project_state = Some(app.current_project_state());
    if let Some(path) = startup_score.or(stored_state.main_score.clone()) {
        startup_tasks.push(Task::done(Message::File(FileMessage::Picked(Some(path)))));
    }

    (app, Task::batch(startup_tasks))
}

#[cfg(test)]
fn new_with_default_test_state() -> (Lilypalooza, Task<Message>) {
    new_with_loaded_state(
        None,
        None,
        false,
        settings::AppSettings::default(),
        None,
        GlobalState::default(),
        None,
    )
}

fn subscription(app: &Lilypalooza) -> Subscription<Message> {
    let mut subscriptions = vec![
        window::open_events().map(Message::WindowOpened),
        window::close_events().map(Message::WindowClosed),
        window::resize_events().map(|(window_id, size)| Message::WindowResized { window_id, size }),
        window::close_requests().map(Message::WindowCloseRequested),
        event::listen_with(runtime_event_to_message),
    ];

    if app.watch_poll_active() {
        subscriptions.push(iced::time::every(WATCH_POLL_INTERVAL).map(|_| Message::Tick));
    }

    if app.editor_tick_active() {
        subscriptions.push(iced::time::every(EDITOR_TICK_INTERVAL).map(|_| Message::Tick));
    }

    if app.spinner_active() {
        subscriptions.push(iced::time::every(SPINNER_POLL_INTERVAL).map(|_| Message::Tick));
    }

    if app.plugin_scan.is_active() {
        subscriptions.push(iced::time::every(PLUGIN_SCAN_POLL_INTERVAL).map(|_| Message::Tick));
    }

    if app.dragged_editor_tab.is_some() {
        subscriptions
            .push(iced::time::every(EDITOR_TABBAR_AUTOSCROLL_INTERVAL).map(|_| Message::Tick));
    }

    if app.effect_drag_source.is_some() && app.effect_rack_autoscroll_direction != 0 {
        subscriptions
            .push(iced::time::every(EDITOR_TABBAR_AUTOSCROLL_INTERVAL).map(|_| Message::Tick));
    }

    if app.score_zoom_preview_active() || app.score_zoom_persist_pending {
        subscriptions.push(iced::time::every(SCORE_ZOOM_PREVIEW_INTERVAL).map(|_| Message::Tick));
    }

    if let Some(interval) = app.playback_poll_interval() {
        subscriptions.push(iced::time::every(interval).map(Message::Frame));
    }

    if app.processor_editor_windows.has_installed_hosts() {
        subscriptions.push(iced::time::every(EDITOR_HOST_POLL_INTERVAL).map(Message::Frame));
    }
    if app.pending_mixer_message_after_editor_detach.is_some() {
        subscriptions.push(iced::time::every(EDITOR_HOST_POLL_INTERVAL).map(Message::Frame));
    }

    Subscription::batch(subscriptions)
}

async fn cleanup_stale_browser_history_dirs(current_dir: Option<PathBuf>) -> Result<(), String> {
    let temp_dir = std::env::temp_dir();
    let entries = fs::read_dir(&temp_dir)
        .map_err(|error| format!("Failed to read temp dir {}: {error}", temp_dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "Failed to read temp dir entry in {}: {error}",
                temp_dir.display()
            )
        })?;
        let path = entry.path();
        if current_dir.as_ref() == Some(&path) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !file_name.starts_with("lilypalooza-browser-history") {
            continue;
        }
        if let Ok(file_type) = entry.file_type()
            && file_type.is_dir()
        {
            fs::remove_dir_all(&path).map_err(|error| {
                format!(
                    "Failed to remove stale browser history directory {}: {error}",
                    path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn runtime_event_to_message(
    event: iced::Event,
    status: event::Status,
    window_id: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(window::Event::Focused) => Some(Message::WindowFocused(window_id)),
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
            Some(Message::ModifiersChanged(modifiers))
        }
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            ..
        }) => Some(Message::KeyPressed(KeyPress {
            status,
            key,
            physical_key,
            modifiers,
        })),
        iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
            Some(Message::PrimaryMousePressed(true))
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(Message::PrimaryMousePressed(false))
        }
        _ => None,
    }
}

impl Lilypalooza {
    fn watch_poll_active(&self) -> bool {
        self.compile_session.is_some()
            || self.score_watcher.is_some()
            || self.browser_file_watcher.is_some()
            || self.editor_file_watcher.is_some()
    }

    fn editor_tick_active(&self) -> bool {
        self.editor.has_document() && self.group_for_pane(WorkspacePaneKind::Editor).is_some()
    }

    fn score_pane_visible(&self) -> bool {
        self.group_for_pane(WorkspacePaneKind::Score).is_some()
    }

    fn piano_roll_pane_visible(&self) -> bool {
        self.group_for_pane(WorkspacePaneKind::PianoRoll).is_some()
    }

    fn mixer_pane_visible(&self) -> bool {
        self.group_for_pane(WorkspacePaneKind::Mixer).is_some()
    }

    fn playback_poll_interval(&self) -> Option<Duration> {
        if !(self.playback.is_some() && self.piano_roll.playback_is_playing()) {
            return None;
        }

        if self.score_pane_visible() || self.piano_roll_pane_visible() || self.mixer_pane_visible()
        {
            Some(ACTIVE_PLAYBACK_POLL_INTERVAL)
        } else {
            Some(PASSIVE_PLAYBACK_POLL_INTERVAL)
        }
    }

    pub(super) fn zoom_modifier_active(&self) -> bool {
        self.keyboard_modifiers.command() || self.keyboard_modifiers.control()
    }

    pub(super) fn shortcut_modifier_active(&self) -> bool {
        self.keyboard_modifiers.command() || self.keyboard_modifiers.control()
    }

    pub(super) fn workspace_group(&self, group_id: DockGroupId) -> Option<&DockGroup> {
        self.dock_groups.get(&group_id)
    }

    pub(super) fn group_for_pane(&self, pane: WorkspacePaneKind) -> Option<DockGroupId> {
        self.dock_groups
            .iter()
            .find_map(|(group_id, group)| group.tabs.contains(&pane).then_some(*group_id))
    }

    pub(super) fn focused_workspace_pane(&self) -> Option<WorkspacePaneKind> {
        self.focused_workspace_pane
            .filter(|pane| self.group_for_pane(*pane).is_some())
    }

    pub(super) fn has_saved_project(&self) -> bool {
        self.project_root.is_some()
    }

    pub(super) fn is_tooltip_open(&self, key: &str) -> bool {
        self.open_tooltip_key.as_deref() == Some(key)
    }

    pub(super) fn project_title(&self) -> String {
        self.project_name
            .as_ref()
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| "Unsaved Project".to_string())
    }

    pub(super) fn spinner_active(&self) -> bool {
        self.compile_requested
            || self.compile_outputs_loading
            || self.compile_session.is_some()
            || matches!(self.lilypond_status, LilypondStatus::Checking)
            || self.plugin_scan.is_active()
    }

    pub(super) fn spinner_frame(&self) -> &'static str {
        if self.spinner_active() {
            SPINNER_FRAMES[self.spinner_step % SPINNER_FRAMES.len()]
        } else {
            " "
        }
    }

    pub(super) fn is_workspace_group_focused(&self, group_id: DockGroupId) -> bool {
        let Some(focused_pane) = self.focused_workspace_pane() else {
            return false;
        };

        self.workspace_group(group_id)
            .is_some_and(|group| group.active == focused_pane)
    }

    pub(super) fn is_pane_folded(&self, pane: WorkspacePaneKind) -> bool {
        self.folded_panes.iter().any(|folded| folded.pane == pane)
    }

    pub(super) fn workspace_visible_pane_count(&self) -> usize {
        self.dock_groups
            .values()
            .map(|group| group.tabs.len())
            .sum()
    }

    pub(super) fn workspace_area_size(&self) -> Size {
        Size::new(self.window_width.max(1.0), self.workspace_height())
    }

    pub(super) fn workspace_bounds(&self) -> Rectangle {
        let size = self.workspace_area_size();
        Rectangle {
            x: 0.0,
            y: 0.0,
            width: size.width,
            height: size.height,
        }
    }

    fn workspace_height(&self) -> f32 {
        let reserved_height =
            crate::status_bar::HEIGHT + transport_bar::HEIGHT + dock_view::TOOLBAR_HEIGHT;

        (self.window_height - reserved_height).max(1.0)
    }
}

fn build_dock_runtime(
    root: Option<&DockNodeSettings>,
) -> (
    Option<DockNode>,
    HashMap<DockGroupId, DockGroup>,
    DockGroupId,
    pane_grid::State<DockGroupId>,
) {
    let mut next_id = 1;
    let mut groups = HashMap::new();
    let layout = root.map(|root| dock_node_from_settings(root, &mut next_id, &mut groups));
    let workspace_panes = build_workspace_panes(layout.as_ref());

    (layout, groups, next_id, workspace_panes)
}

fn first_active_workspace_pane(
    node: &DockNode,
    groups: &HashMap<DockGroupId, DockGroup>,
) -> Option<WorkspacePaneKind> {
    match node {
        DockNode::Group(group_id) => groups.get(group_id).map(|group| group.active),
        DockNode::Split { first, second, .. } => first_active_workspace_pane(first, groups)
            .or_else(|| first_active_workspace_pane(second, groups)),
    }
}

fn dock_node_from_settings(
    node: &DockNodeSettings,
    next_id: &mut DockGroupId,
    groups: &mut HashMap<DockGroupId, DockGroup>,
) -> DockNode {
    match node {
        DockNodeSettings::Group(group) => {
            let group_id = *next_id;
            *next_id = next_id.saturating_add(1);
            let mut tabs = group.tabs.clone();
            if tabs.is_empty() {
                tabs.push(WorkspacePaneKind::Score);
            }
            let active = if tabs.contains(&group.active) {
                group.active
            } else {
                tabs[0]
            };
            groups.insert(group_id, DockGroup { tabs, active });
            DockNode::Group(group_id)
        }
        DockNodeSettings::Split {
            axis,
            ratio,
            first,
            second,
        } => DockNode::Split {
            axis: pane_grid_axis_from_settings(*axis),
            ratio: ratio.clamp(0.05, 0.95),
            first: Box::new(dock_node_from_settings(first, next_id, groups)),
            second: Box::new(dock_node_from_settings(second, next_id, groups)),
        },
    }
}

fn build_workspace_panes(layout: Option<&DockNode>) -> pane_grid::State<DockGroupId> {
    let Some(layout) = layout else {
        return pane_grid::State::new(0).0;
    };
    let configuration = configuration_from_dock_node(layout);

    match configuration {
        pane_grid::Configuration::Pane(group_id) => pane_grid::State::new(group_id).0,
        configuration => pane_grid::State::with_configuration(configuration),
    }
}

fn configuration_from_dock_node(layout: &DockNode) -> pane_grid::Configuration<DockGroupId> {
    match layout {
        DockNode::Group(group_id) => pane_grid::Configuration::Pane(*group_id),
        DockNode::Split {
            axis,
            ratio,
            first,
            second,
        } => pane_grid::Configuration::Split {
            axis: *axis,
            ratio: *ratio,
            a: Box::new(configuration_from_dock_node(first)),
            b: Box::new(configuration_from_dock_node(second)),
        },
    }
}

fn dock_node_from_workspace_state(state: &pane_grid::State<DockGroupId>) -> Option<DockNode> {
    dock_node_from_layout_node(state, state.layout())
}

fn dock_node_from_layout_node(
    state: &pane_grid::State<DockGroupId>,
    node: &pane_grid::Node,
) -> Option<DockNode> {
    match node {
        pane_grid::Node::Pane(pane) => Some(DockNode::Group(*state.get(*pane)?)),
        pane_grid::Node::Split {
            axis, ratio, a, b, ..
        } => Some(DockNode::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(dock_node_from_layout_node(state, a.as_ref())?),
            second: Box::new(dock_node_from_layout_node(state, b.as_ref())?),
        }),
    }
}

fn pane_grid_axis_from_settings(axis: DockAxis) -> pane_grid::Axis {
    match axis {
        DockAxis::Horizontal => pane_grid::Axis::Horizontal,
        DockAxis::Vertical => pane_grid::Axis::Vertical,
    }
}

fn folded_pane_from_settings(settings: FoldedPaneSettings) -> FoldedPaneState {
    FoldedPaneState {
        pane: settings.pane,
        restore: match settings.restore {
            FoldedPaneRestoreSettings::Tab { anchor } => FoldedPaneRestore::Tab { anchor },
            FoldedPaneRestoreSettings::Standalone => FoldedPaneRestore::Standalone,
            FoldedPaneRestoreSettings::Split {
                anchor,
                axis,
                ratio,
                insert_first,
                sibling_panes,
            } => FoldedPaneRestore::Split {
                anchor,
                axis: pane_grid_axis_from_settings(axis),
                ratio,
                insert_first,
                sibling_panes,
            },
        },
    }
}

fn folded_pane_to_settings(state: FoldedPaneState) -> FoldedPaneSettings {
    FoldedPaneSettings {
        pane: state.pane,
        restore: match state.restore {
            FoldedPaneRestore::Tab { anchor } => FoldedPaneRestoreSettings::Tab { anchor },
            FoldedPaneRestore::Standalone => FoldedPaneRestoreSettings::Standalone,
            FoldedPaneRestore::Split {
                anchor,
                axis,
                ratio,
                insert_first,
                sibling_panes,
            } => FoldedPaneRestoreSettings::Split {
                anchor,
                axis: dock_axis_to_settings(axis),
                ratio,
                insert_first,
                sibling_panes,
            },
        },
    }
}

fn migrate_workspace_layout(
    root: &mut Option<DockNodeSettings>,
    folded_panes: &[FoldedPaneSettings],
) {
    if root
        .as_ref()
        .is_some_and(|root| dock_node_settings_contains_pane(root, WorkspacePaneKind::Logger))
        || folded_panes
            .iter()
            .any(|folded| folded.pane == WorkspacePaneKind::Logger)
    {
        return;
    }

    let previous_root = root.take().unwrap_or_default();
    *root = Some(DockNodeSettings::Split {
        axis: DockAxis::Horizontal,
        ratio: 0.74,
        first: Box::new(previous_root),
        second: Box::new(DockNodeSettings::Group(settings::DockGroupSettings {
            tabs: vec![WorkspacePaneKind::Logger],
            active: WorkspacePaneKind::Logger,
        })),
    });
}

fn dock_node_settings_contains_pane(node: &DockNodeSettings, pane: WorkspacePaneKind) -> bool {
    match node {
        DockNodeSettings::Group(group) => group.tabs.contains(&pane),
        DockNodeSettings::Split { first, second, .. } => {
            dock_node_settings_contains_pane(first, pane)
                || dock_node_settings_contains_pane(second, pane)
        }
    }
}

pub(super) fn dock_axis_to_settings(axis: pane_grid::Axis) -> DockAxis {
    match axis {
        pane_grid::Axis::Horizontal => DockAxis::Horizontal,
        pane_grid::Axis::Vertical => DockAxis::Vertical,
    }
}

fn contains_group(node: &DockNode, group_id: DockGroupId) -> bool {
    match node {
        DockNode::Group(candidate) => *candidate == group_id,
        DockNode::Split { first, second, .. } => {
            contains_group(first, group_id) || contains_group(second, group_id)
        }
    }
}

fn selected_score_from_path(path: PathBuf) -> Result<SelectedScore, String> {
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let file_name = path
        .file_name()
        .ok_or_else(|| "Selected path has no file name".to_string())?
        .to_str()
        .ok_or_else(|| "Selected file name is not valid UTF-8".to_string())?
        .to_string();

    Ok(SelectedScore { path, file_name })
}

fn selected_score_stem(selected_file_name: &str) -> Result<&str, String> {
    std::path::Path::new(selected_file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "Selected score name has no valid stem".to_string())
}

fn default_project_name(project_root: &std::path::Path) -> String {
    project_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "Untitled Project".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts;
    use iced::event;
    use iced_test::simulator;
    use std::fs;

    fn test_app() -> Lilypalooza {
        let (mut app, _task) = new_with_default_test_state();
        let _ = update(
            &mut app,
            Message::Shortcuts(messages::ShortcutsMessage::OpenDialog),
        );
        app
    }

    fn test_editor_app() -> Lilypalooza {
        let (app, _task) = new_with_default_test_state();
        app
    }

    fn apply_messages(app: &mut Lilypalooza, messages: Vec<Message>) {
        for message in messages {
            let _ = update(app, message);
        }
    }

    fn hide_workspace_pane(app: &mut Lilypalooza, pane: WorkspacePaneKind) {
        for group in app.dock_groups.values_mut() {
            group.tabs.retain(|candidate| *candidate != pane);
            if group.active == pane && !group.tabs.is_empty() {
                group.active = group.tabs[0];
            }
        }
    }

    fn named_key_press(named: keyboard::key::Named, code: keyboard::key::Code) -> Message {
        Message::KeyPressed(KeyPress {
            status: event::Status::Ignored,
            key: keyboard::Key::Named(named),
            physical_key: keyboard::key::Physical::Code(code),
            modifiers: keyboard::Modifiers::default(),
        })
    }

    #[test]
    fn app_editor_frame_takes_chrome_colors_from_iced_theme() {
        let theme = iced::Theme::Dark;
        let palette = theme.extended_palette();
        let frame = AppEditorFrame::from_theme(&theme);

        assert_eq!(
            frame.style.frame_color,
            egui_color(palette.background.base.color)
        );
        assert_eq!(
            frame.style.titlebar_color,
            egui_color(mix_iced_color(
                palette.background.weak.color,
                Color::WHITE,
                0.04
            ))
        );
        assert_eq!(
            frame.style.title_color,
            egui_color(palette.background.base.text)
        );
    }

    #[test]
    fn app_editor_frame_close_button_has_hover_feedback() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);

        assert_ne!(
            frame.style.close_background,
            frame.style.close_background_hovered
        );
        assert_ne!(frame.style.close_icon, frame.style.close_icon_hovered);
    }

    #[test]
    fn app_editor_frame_border_is_thin() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);

        assert_eq!(frame.frame_thickness, EDITOR_FRAME_THICKNESS);
        assert!(frame.frame_thickness < 4.0);
        assert!(frame.border_width < 1.0);
    }

    #[test]
    fn app_editor_frame_starts_compact() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);

        assert_eq!(frame.titlebar_height, EDITOR_FRAME_COMPACT_CHROME_HEIGHT);
    }

    #[test]
    fn app_editor_frame_rect_uses_accepted_content_size_not_live_window_size() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let state = editor_host::EditorHostState {
            title: "Editor".to_string(),
            resizable: true,
            close_requested: false,
            content_size: editor_host::Size {
                width: 640.0,
                height: 480.0,
            },
            preset: None,
        };
        let live_rect = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(900.0, 700.0),
        );

        let rect = frame.frame_rect(live_rect, &state);

        assert_eq!(rect.width(), 640.0 + (EDITOR_FRAME_THICKNESS * 2.0) as f32);
        assert_eq!(
            rect.height(),
            480.0
                + EDITOR_FRAME_COMPACT_CHROME_HEIGHT as f32
                + (EDITOR_FRAME_THICKNESS * 2.0) as f32
        );
        assert!(rect.width() < live_rect.width());
        assert!(rect.height() < live_rect.height());
    }

    #[test]
    fn app_editor_frame_resize_grip_stays_at_bottom_right() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let rect = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(640.0, 480.0),
        );

        let grip = frame.resize_grip_rect(rect);

        assert_eq!(grip.right(), rect.right());
        assert_eq!(grip.bottom(), rect.bottom());
    }

    #[test]
    fn app_editor_frame_resize_command_preserves_content_aspect() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let command = frame.resize_command_for_drag(
            editor_host::Size {
                width: 640.0,
                height: 480.0,
            },
            editor_host::egui::vec2(120.0, 40.0),
        );

        assert_eq!(
            command,
            editor_host::EditorFrameCommand::ResizeContent {
                width: 760,
                height: 570,
            }
        );
    }

    #[test]
    fn app_editor_frame_dropdown_stays_inside_reserved_chrome() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let layout = frame.preset_layout(editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(640.0, EDITOR_FRAME_EXPANDED_CHROME_HEIGHT as f32),
        ));

        assert!(layout.browser.bottom() <= EDITOR_FRAME_EXPANDED_CHROME_HEIGHT as f32);
    }

    #[test]
    fn app_editor_frame_browser_fits_view_width() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let titlebar = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(640.0, EDITOR_FRAME_EXPANDED_CHROME_HEIGHT as f32),
        );
        let layout = frame.preset_layout(titlebar);

        assert!(layout.browser.width() > layout.preset_row.width());
        assert_eq!(layout.browser.left(), titlebar.left() + 8.0);
        assert_eq!(layout.browser.right(), titlebar.right() - 8.0);
    }

    #[test]
    fn app_editor_frame_browser_list_starts_below_save_button() {
        let browser = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(8.0, 38.0),
            editor_host::egui::vec2(624.0, EDITOR_FRAME_PRESET_BROWSER_HEIGHT),
        );
        let save = AppEditorFrame::preset_browser_save_rect(browser);
        let list = AppEditorFrame::preset_browser_list_rect(browser);

        assert!(list.top() > save.bottom());
        assert!(browser.bottom() - list.bottom() >= 10.0);
        assert!(list.height() >= 72.0);
    }

    #[test]
    fn app_editor_frame_icons_render_from_lucide_assets() {
        for icon in [
            AppEditorFrameIcon::ChevronLeft,
            AppEditorFrameIcon::ChevronRight,
            AppEditorFrameIcon::ChevronDown,
            AppEditorFrameIcon::ChevronUp,
            AppEditorFrameIcon::Pencil,
            AppEditorFrameIcon::Save,
            AppEditorFrameIcon::Trash,
        ] {
            let svg = std::str::from_utf8(icon.svg_bytes()).expect("icon should be utf8 svg");
            assert!(svg.contains("<svg"));
            assert!(render_lucide_icon(icon).is_some());
        }
    }

    #[test]
    fn app_editor_frame_uses_stateful_preset_chevron() {
        assert_eq!(
            AppEditorFrame::preset_menu_icon(false),
            AppEditorFrameIcon::ChevronUp
        );
        assert_eq!(
            AppEditorFrame::preset_menu_icon(true),
            AppEditorFrameIcon::ChevronDown
        );
    }

    #[test]
    fn app_editor_frame_cuts_preset_text_to_fixed_width() {
        let text = ellipsize_for_width("Extremely Long Preset Name", 72.0, 13.0);

        assert!(text.ends_with("..."));
        assert!(text.len() < "Extremely Long Preset Name".len());
    }

    #[test]
    fn app_editor_frame_delete_requires_confirmation() {
        let mut frame = AppEditorFrame::from_theme(&iced::Theme::Dark);

        assert_eq!(frame.request_preset_delete("preset-1"), None);
        assert_eq!(
            frame.delete_confirmation_preset_id.as_deref(),
            Some("preset-1")
        );
        assert_eq!(
            frame.confirm_preset_delete("preset-1"),
            Some(editor_host::EditorFrameCommand::DeletePreset(
                "preset-1".to_string()
            ))
        );
        assert_eq!(frame.delete_confirmation_preset_id, None);
    }

    #[test]
    fn app_editor_frame_delete_confirmation_cuts_long_preset_name() {
        let message = AppEditorFrame::delete_confirmation_label(
            "Very Long Evolving Pad With Too Much Text",
            128.0,
        );

        assert!(message.starts_with("Remove \""));
        assert!(message.ends_with("\"?"));
        assert!(message.contains("..."));
        assert!(message.len() < "Remove \"Very Long Evolving Pad With Too Much Text\"?".len());
    }

    #[test]
    fn app_editor_frame_rename_can_cancel_without_command() {
        let mut frame = AppEditorFrame::from_theme(&iced::Theme::Dark);

        frame.begin_preset_rename("preset-1", "Warm Piano");
        frame.cancel_preset_rename();

        assert_eq!(frame.renaming_preset_id, None);
        assert!(frame.renaming_preset_value.is_empty());
    }

    #[test]
    fn app_editor_frame_compact_preset_controls_live_in_title_row() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let layout = frame.preset_layout(editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(640.0, EDITOR_FRAME_COMPACT_CHROME_HEIGHT as f32),
        ));

        assert!(layout.title_row.contains_rect(layout.preset_row));
        assert!(layout.preset_row.right() <= layout.title_text.left());
        assert!(layout.title_text.right() <= layout.close_button.left());
        assert!(layout.browser.top() >= layout.title_row.bottom());
    }

    #[test]
    fn app_editor_frame_header_orders_presets_title_and_close_button() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let titlebar = editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(640.0, EDITOR_FRAME_COMPACT_CHROME_HEIGHT as f32),
        );
        let layout = frame.preset_layout(titlebar);

        assert!(layout.preset_row.left() < layout.title_text.left());
        assert!(layout.title_text.right() < layout.close_button.left());
        assert!(layout.title_text.width() <= 180.0);
        assert_eq!(layout.close_button.center().x, titlebar.right() - 18.0);
    }

    #[test]
    fn app_editor_frame_previous_button_does_not_overlap_dropdown() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let layout = frame.preset_layout(editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(640.0, EDITOR_FRAME_EXPANDED_CHROME_HEIGHT as f32),
        ));

        assert!(!layout.previous.intersects(layout.browser));
    }

    #[test]
    fn app_editor_frame_close_button_stays_in_title_row_when_presets_expand() {
        let frame = AppEditorFrame::from_theme(&iced::Theme::Dark);
        let layout = frame.preset_layout(editor_host::egui::Rect::from_min_size(
            editor_host::egui::pos2(0.0, 0.0),
            editor_host::egui::vec2(640.0, EDITOR_FRAME_EXPANDED_CHROME_HEIGHT as f32),
        ));

        assert!(layout.title_row.contains_rect(layout.close_button));
        assert!(!layout.close_button.intersects(layout.browser));
    }

    #[test]
    fn processor_preset_library_saves_user_preset_for_processor_kind() {
        let mut library = crate::app::processor_presets::ProcessorPresetLibrary::default();
        let kind = lilypalooza_audio::ProcessorKind::BuiltIn {
            processor_id: lilypalooza_audio::BUILTIN_SOUNDFONT_ID.to_string(),
        };
        let state = lilypalooza_audio::ProcessorState(vec![1, 2, 3]);

        let id = library.save_user_preset("Warm Piano", kind.clone(), state.clone());

        let presets = library.presets_for(&kind);
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].id, id);
        assert_eq!(presets[0].name, "Warm Piano");
        assert_eq!(library.state_for(&kind, &id), Some(&state));
    }

    #[test]
    fn processor_preset_library_renames_user_preset() {
        let mut library = crate::app::processor_presets::ProcessorPresetLibrary::default();
        let kind = lilypalooza_audio::ProcessorKind::BuiltIn {
            processor_id: lilypalooza_audio::BUILTIN_SOUNDFONT_ID.to_string(),
        };
        let id = library.save_user_preset(
            "Warm Piano",
            kind.clone(),
            lilypalooza_audio::ProcessorState(vec![]),
        );

        assert!(library.rename_user_preset(&kind, &id, "Soft Piano"));

        assert_eq!(library.presets_for(&kind)[0].name, "Soft Piano");
    }

    #[test]
    fn processor_preset_library_deletes_user_preset() {
        let mut library = crate::app::processor_presets::ProcessorPresetLibrary::default();
        let kind = lilypalooza_audio::ProcessorKind::BuiltIn {
            processor_id: lilypalooza_audio::BUILTIN_SOUNDFONT_ID.to_string(),
        };
        let id = library.save_user_preset(
            "Warm Piano",
            kind.clone(),
            lilypalooza_audio::ProcessorState(vec![]),
        );

        assert!(library.delete_user_preset(&kind, &id));

        assert!(library.presets_for(&kind).is_empty());
    }

    #[test]
    fn project_state_roundtrips_processor_presets() {
        let mut state = ProjectState::default();
        let kind = lilypalooza_audio::ProcessorKind::BuiltIn {
            processor_id: lilypalooza_audio::BUILTIN_SOUNDFONT_ID.to_string(),
        };
        let preset_state = lilypalooza_audio::ProcessorState(vec![7, 8, 9]);
        state
            .processor_presets
            .save_user_preset("Saved", kind.clone(), preset_state.clone());

        let serialized = ron::to_string(&state).expect("state should serialize");
        let parsed: ProjectState = ron::from_str(&serialized).expect("state should parse");

        assert_eq!(
            parsed.processor_presets.presets_for(&kind)[0].state,
            preset_state
        );
    }

    #[test]
    fn startup_restores_global_processor_presets() {
        let kind = lilypalooza_audio::ProcessorKind::BuiltIn {
            processor_id: lilypalooza_audio::BUILTIN_SOUNDFONT_ID.to_string(),
        };
        let mut stored_state = GlobalState::default();
        stored_state.processor_presets.save_user_preset(
            "Saved",
            kind.clone(),
            lilypalooza_audio::ProcessorState(vec![4, 5, 6]),
        );

        let (app, _) = new_with_loaded_state(
            None,
            None,
            false,
            settings::AppSettings::default(),
            None,
            stored_state,
            None,
        );

        assert_eq!(app.processor_presets.presets_for(&kind)[0].name, "Saved");
    }

    fn char_key_press(value: &str, code: keyboard::key::Code) -> Message {
        Message::KeyPressed(KeyPress {
            status: event::Status::Ignored,
            key: keyboard::Key::Character(value.into()),
            physical_key: keyboard::key::Physical::Code(code),
            modifiers: keyboard::Modifiers::default(),
        })
    }

    fn primary_char_key_press(value: &str, code: keyboard::key::Code) -> Message {
        let mut modifiers = keyboard::Modifiers::default();
        if cfg!(target_os = "macos") {
            modifiers.insert(keyboard::Modifiers::COMMAND);
        } else {
            modifiers.insert(keyboard::Modifiers::CTRL);
        }
        Message::KeyPressed(KeyPress {
            status: event::Status::Ignored,
            key: keyboard::Key::Character(value.into()),
            physical_key: keyboard::key::Physical::Code(code),
            modifiers,
        })
    }

    fn active_browser_column_index(app: &Lilypalooza) -> Option<usize> {
        Some(app.editor.file_browser_active_column_index())
    }

    fn selected_browser_entry_name(app: &Lilypalooza, column_index: usize) -> Option<String> {
        app.editor
            .file_browser_columns()
            .get(column_index)
            .and_then(|column| match column {
                editor::EditorBrowserColumnSummary::Directory { entries } => entries
                    .iter()
                    .find(|entry| entry.selected)
                    .map(|entry| entry.name.clone()),
                editor::EditorBrowserColumnSummary::FilePreview { .. } => None,
            })
    }

    fn file_preview_name(app: &Lilypalooza, column_index: usize) -> Option<String> {
        app.editor
            .file_browser_columns()
            .get(column_index)
            .and_then(|column| match column {
                editor::EditorBrowserColumnSummary::FilePreview { metadata } => {
                    Some(metadata.name.clone())
                }
                editor::EditorBrowserColumnSummary::Directory { .. } => None,
            })
    }

    fn browser_entry_names(app: &Lilypalooza, column_index: usize) -> Vec<String> {
        app.editor
            .file_browser_columns()
            .get(column_index)
            .and_then(|column| match column {
                editor::EditorBrowserColumnSummary::Directory { entries } => Some(
                    entries
                        .iter()
                        .map(|entry| entry.name.clone())
                        .collect::<Vec<_>>(),
                ),
                editor::EditorBrowserColumnSummary::FilePreview { .. } => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn playback_poll_interval_is_visibility_aware() {
        let (mut app, _task) = new_with_default_test_state();
        app.playback = Some(
            AudioEngine::start_cpal(
                MixerState::new(),
                audio_engine_options(&app.playback_settings),
            )
            .expect("test audio engine should start"),
        );
        app.piano_roll.set_playback_position(0, 1, true);

        assert_eq!(
            app.playback_poll_interval(),
            Some(ACTIVE_PLAYBACK_POLL_INTERVAL)
        );

        hide_workspace_pane(&mut app, WorkspacePaneKind::Score);
        assert_eq!(
            app.playback_poll_interval(),
            Some(ACTIVE_PLAYBACK_POLL_INTERVAL)
        );

        hide_workspace_pane(&mut app, WorkspacePaneKind::PianoRoll);
        let _ = app.unfold_workspace_pane(WorkspacePaneKind::Mixer);
        assert_eq!(
            app.playback_poll_interval(),
            Some(ACTIVE_PLAYBACK_POLL_INTERVAL)
        );

        hide_workspace_pane(&mut app, WorkspacePaneKind::Mixer);
        assert_eq!(
            app.playback_poll_interval(),
            Some(PASSIVE_PLAYBACK_POLL_INTERVAL)
        );
    }

    #[test]
    fn actions_palette_search_input_filters_actions() {
        let mut app = test_app();
        let mut ui = simulator(view(&app));

        ui.click("Search actions")
            .expect("search input should be clickable");
        let _ = ui.typewrite("settings");
        let messages: Vec<_> = ui.into_messages().collect();
        apply_messages(&mut app, messages);

        assert_eq!(app.shortcuts_search_query, "settings");
        assert!(
            shortcuts::filtered_action_metadata(&app.shortcuts_search_query)
                .iter()
                .any(|action| action.id == settings::ShortcutActionId::OpenSettingsFile)
        );
    }

    #[test]
    fn actions_palette_clicking_action_emits_activation_message() {
        let mut app = test_app();
        let mut ui = simulator(view(&app));

        ui.click("Search actions")
            .expect("search input should be clickable");
        let _ = ui.typewrite("settings");
        let messages: Vec<_> = ui.into_messages().collect();
        apply_messages(&mut app, messages);

        let mut ui = simulator(view(&app));
        ui.click("Open Settings File")
            .expect("open settings action should be clickable");

        assert!(ui.into_messages().any(|message| matches!(
            message,
            Message::Shortcuts(messages::ShortcutsMessage::ActivateAction(
                settings::ShortcutActionId::OpenSettingsFile
            ))
        )));
    }

    #[test]
    fn plugin_validator_path_uses_sibling_helper_for_dev_binary() {
        let path = plugin_validator_path_for_exe(Path::new("/repo/target/debug/lilypalooza"));

        assert_eq!(
            path,
            PathBuf::from("/repo/target/debug/lilypalooza-plugin-validator")
        );
    }

    #[test]
    fn plugin_validator_path_uses_app_bundle_macos_directory() {
        let path = plugin_validator_path_for_exe(Path::new(
            "/Applications/Lilypalooza.app/Contents/MacOS/lilypalooza",
        ));

        assert_eq!(
            path,
            PathBuf::from(
                "/Applications/Lilypalooza.app/Contents/MacOS/lilypalooza-plugin-validator"
            )
        );
    }

    #[test]
    fn starting_plugin_scan_logs_immediately() {
        let mut app = test_app();
        app.logger.clear();
        app.plugin_search_paths.clear();

        app.start_plugin_scan_with_validator(std::env::current_exe().expect("test exe"));

        assert_eq!(
            app.logger.last_line(),
            Some("Scanning plugins from 0 path(s)")
        );
        assert!(app.plugin_scan.is_active());
    }

    #[test]
    fn open_settings_file_does_not_log_editor_open_noise() {
        let mut app = test_editor_app();
        app.logger.clear();

        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::OpenSettingsFile);

        assert!(
            app.logger
                .last_line()
                .is_none_or(|line| !line.contains("editor file"))
        );
    }

    #[test]
    fn project_dirty_only_tracks_project_state_changes() {
        let (mut app, _task) = new_with_default_test_state();
        let temp = tempfile::tempdir().expect("temp dir should exist");

        app.apply_project_state(temp.path().to_path_buf(), ProjectState::default());
        assert!(!app.project_is_dirty());

        app.editor_recent_files.push(PathBuf::from("/tmp/test.ly"));
        assert!(!app.project_is_dirty());

        app.metronome.enabled = true;
        assert!(app.project_is_dirty());
    }

    #[test]
    fn window_close_prompts_for_dirty_project_changes() {
        let (mut app, _task) = new_with_default_test_state();
        let temp = tempfile::tempdir().expect("temp dir should exist");

        app.apply_project_state(temp.path().to_path_buf(), ProjectState::default());
        app.metronome.enabled = true;

        let _ = app.handle_window_close_requested(app.main_window_id);

        assert!(matches!(
            app.pending_editor_action,
            Some(PendingEditorAction::ResolveDirtyProject {
                continuation: EditorContinuation::ExitApp
            })
        ));
        assert!(matches!(
            app.error_prompt.as_ref().map(|prompt| prompt.buttons()),
            Some(crate::error_prompt::PromptButtons::SaveDiscardCancel)
        ));
    }

    #[test]
    fn discard_dirty_project_prompt_clears_pending_action() {
        let (mut app, _task) = new_with_default_test_state();
        let temp = tempfile::tempdir().expect("temp dir should exist");

        app.apply_project_state(temp.path().to_path_buf(), ProjectState::default());
        app.metronome.enabled = true;
        let _ = app.handle_window_close_requested(app.main_window_id);

        let _ = app.handle_prompt_message(PromptMessage::Discard);

        assert!(app.pending_editor_action.is_none());
        assert!(app.error_prompt.is_none());
    }

    #[test]
    fn metronome_shortcut_toggles_outside_editor() {
        let mut app = test_editor_app();
        app.metronome.enabled = false;
        app.focused_workspace_pane = Some(WorkspacePaneKind::PianoRoll);

        let _ = update(
            &mut app,
            primary_char_key_press("k", keyboard::key::Code::KeyK),
        );

        assert!(app.metronome.enabled);
    }

    #[test]
    fn metronome_shortcut_is_suppressed_in_editor() {
        let mut app = test_editor_app();
        app.metronome.enabled = false;
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::OpenSettingsFile);

        let _ = update(
            &mut app,
            primary_char_key_press("k", keyboard::key::Code::KeyK),
        );

        assert!(!app.metronome.enabled);
    }

    #[test]
    fn metronome_popup_escape_closes_menu() {
        let mut app = test_editor_app();
        app.metronome_menu_open = true;

        let _ = update(
            &mut app,
            named_key_press(keyboard::key::Named::Escape, keyboard::key::Code::Escape),
        );

        assert!(!app.metronome_menu_open);
    }

    #[test]
    fn instrument_browser_escape_closes_menu() {
        let mut app = test_editor_app();
        app.open_processor_browser_target = Some(super::processor_editor_windows::EditorTarget {
            strip_index: 1,
            slot_index: 0,
        });

        let _ = update(
            &mut app,
            named_key_press(keyboard::key::Named::Escape, keyboard::key::Code::Escape),
        );

        assert_eq!(app.open_processor_browser_target, None);
    }

    #[test]
    fn metronome_transport_messages_toggle_and_close_menu() {
        let mut app = test_editor_app();

        let _ = update(
            &mut app,
            Message::PianoRoll(PianoRollMessage::TransportOpenMetronomeMenu),
        );
        assert!(app.metronome_menu_open);

        let _ = update(
            &mut app,
            Message::PianoRoll(PianoRollMessage::TransportToggleMetronome),
        );
        assert!(app.metronome.enabled);

        let _ = update(
            &mut app,
            Message::PianoRoll(PianoRollMessage::TransportCloseMetronomeMenu),
        );
        assert!(!app.metronome_menu_open);
    }

    #[test]
    fn browser_arrow_keys_navigate_columns_after_browser_focus_click() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("alpha")).expect("alpha dir");
        fs::create_dir(root.path().join("alpha").join("child")).expect("child dir");
        fs::write(
            root.path().join("alpha").join("child").join("inner.txt"),
            "inner",
        )
        .expect("inner file");
        fs::write(root.path().join("alpha").join("b.txt"), "b").expect("b file");
        fs::create_dir(root.path().join("beta")).expect("beta dir");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("alpha"),
                is_dir: true,
            }),
        );

        assert!(app.editor_file_browser_focused);
        assert_eq!(app.editor.file_browser_columns().len(), 2);
        assert_eq!(active_browser_column_index(&app), Some(0));
        assert_eq!(
            selected_browser_entry_name(&app, 0).as_deref(),
            Some("alpha")
        );
        assert_eq!(selected_browser_entry_name(&app, 1), None);

        let _ = update(
            &mut app,
            named_key_press(
                keyboard::key::Named::ArrowRight,
                keyboard::key::Code::ArrowRight,
            ),
        );
        assert_eq!(active_browser_column_index(&app), Some(1));
        assert_eq!(
            selected_browser_entry_name(&app, 0).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            selected_browser_entry_name(&app, 1).as_deref(),
            Some("child")
        );
        assert_eq!(app.editor.file_browser_columns().len(), 3);
        assert_eq!(selected_browser_entry_name(&app, 2), None);

        let _ = update(
            &mut app,
            named_key_press(
                keyboard::key::Named::ArrowDown,
                keyboard::key::Code::ArrowDown,
            ),
        );
        assert_eq!(
            selected_browser_entry_name(&app, 1).as_deref(),
            Some("b.txt")
        );
        assert_eq!(app.editor.file_browser_columns().len(), 3);
        assert_eq!(file_preview_name(&app, 2).as_deref(), Some("b.txt"));

        let _ = update(
            &mut app,
            named_key_press(
                keyboard::key::Named::ArrowLeft,
                keyboard::key::Code::ArrowLeft,
            ),
        );
        assert_eq!(active_browser_column_index(&app), Some(0));
        assert_eq!(
            selected_browser_entry_name(&app, 0).as_deref(),
            Some("alpha")
        );
        assert_eq!(selected_browser_entry_name(&app, 1), None);

        let _ = update(
            &mut app,
            named_key_press(
                keyboard::key::Named::ArrowRight,
                keyboard::key::Code::ArrowRight,
            ),
        );
        assert_eq!(active_browser_column_index(&app), Some(1));
        assert_eq!(
            selected_browser_entry_name(&app, 0).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            selected_browser_entry_name(&app, 1).as_deref(),
            Some("child")
        );
        assert_eq!(app.editor.file_browser_columns().len(), 3);
        assert_eq!(selected_browser_entry_name(&app, 2), None);
    }

    #[test]
    fn browser_focus_blocks_editor_text_input() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("alpha")).expect("alpha dir");
        let file_path = root.path().join("note.txt");
        fs::write(&file_path, "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.set_focused_workspace_pane(WorkspacePaneKind::Editor);
        let _ = app.open_editor_file_in_editor(&file_path);
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("alpha"),
                is_dir: true,
            }),
        );

        let before = app.editor.active_content().expect("active content");
        assert!(app.editor_file_browser_focused);
        assert!(!app.editor.active_editor_is_focused());

        let _ = update(&mut app, char_key_press("x", keyboard::key::Code::KeyX));

        assert_eq!(
            app.editor.active_content().as_deref(),
            Some(before.as_str())
        );
        assert!(!app.editor.active_editor_is_focused());
    }

    #[test]
    fn point_and_click_unfolds_and_focuses_editor_pane() {
        let root = TempDir::new().expect("tempdir");
        let score_path = root.path().join("score.ly");
        let target_path = root.path().join("part.ly");
        let other_path = root.path().join("other.ly");
        fs::write(&score_path, "\\version \"2.24.0\"").expect("score file");
        fs::write(&target_path, "{ c'4 }").expect("target file");
        fs::write(&other_path, "{ d'4 }").expect("other file");

        let svg_source = format!(
            r#"<svg width="100" height="100" viewBox="0 0 100 100"><a xlink:href="textedit://{}:3:2:3"><g transform="translate(20,30)"></g></a></svg>"#,
            target_path.display()
        );
        let note_anchors = score_cursor::parse_svg_note_anchors(&svg_source, 0);
        assert_eq!(note_anchors.len(), 1);

        let mut app = test_editor_app();
        app.current_score = Some(SelectedScore {
            path: score_path,
            file_name: "score.ly".into(),
        });
        let svg_bytes = svg_source.clone().into_bytes();
        app.rendered_score = Some(RenderedScore {
            pages: vec![RenderedPage {
                handle: svg::Handle::from_memory(svg_bytes.clone()),
                svg_bytes: Bytes::from(svg_bytes),
                display_size: SvgSize {
                    width: 100.0,
                    height: 100.0,
                },
                coord_size: SvgSize {
                    width: 100.0,
                    height: 100.0,
                },
                note_anchors,
                system_bands: Vec::new(),
            }],
            current_page: 0,
        });
        let scale = score_view::score_base_scale() * app.svg_zoom;
        let inset = f32::from(crate::ui_style::PADDING_SM);
        app.score_viewport_cursor =
            Some(iced::Point::new(20.0 * scale + inset, 30.0 * scale + inset));

        if !app.is_pane_folded(WorkspacePaneKind::Editor) {
            assert!(app.fold_workspace_pane(WorkspacePaneKind::Editor));
        }
        assert!(app.is_pane_folded(WorkspacePaneKind::Editor));

        let _ = update(&mut app, Message::Viewer(ViewerMessage::OpenPointAndClick));

        assert!(!app.is_pane_folded(WorkspacePaneKind::Editor));
        assert_eq!(app.focused_workspace_pane, Some(WorkspacePaneKind::Editor));
        assert!(app.editor.find_tab_by_path(&target_path).is_some());
    }

    #[test]
    fn point_and_click_activates_editor_pane_tab_and_target_editor_tab() {
        let root = TempDir::new().expect("tempdir");
        let score_path = root.path().join("score.ly");
        let target_path = root.path().join("part.ly");
        let other_path = root.path().join("other.ly");
        fs::write(&score_path, "\\version \"2.24.0\"").expect("score file");
        fs::write(&target_path, "{ c'4 }").expect("target file");
        fs::write(&other_path, "{ d'4 }").expect("other file");

        let svg_source = format!(
            r#"<svg width="100" height="100" viewBox="0 0 100 100"><a xlink:href="textedit://{}:3:2:3"><g transform="translate(20,30)"></g></a></svg>"#,
            target_path.display()
        );
        let note_anchors = score_cursor::parse_svg_note_anchors(&svg_source, 0);

        let mut app = test_editor_app();
        app.current_score = Some(SelectedScore {
            path: score_path,
            file_name: "score.ly".into(),
        });
        let svg_bytes = svg_source.clone().into_bytes();
        app.rendered_score = Some(RenderedScore {
            pages: vec![RenderedPage {
                handle: svg::Handle::from_memory(svg_bytes.clone()),
                svg_bytes: Bytes::from(svg_bytes),
                display_size: SvgSize {
                    width: 100.0,
                    height: 100.0,
                },
                coord_size: SvgSize {
                    width: 100.0,
                    height: 100.0,
                },
                note_anchors,
                system_bands: Vec::new(),
            }],
            current_page: 0,
        });
        let _ = app.unfold_workspace_pane(WorkspacePaneKind::Editor);
        let _ = app.open_editor_file_in_editor(&other_path);

        if let Some(group_id) = app.group_for_pane(WorkspacePaneKind::Editor)
            && let Some(group) = app.dock_groups.get_mut(&group_id)
            && let Some(other_pane) = group
                .tabs
                .iter()
                .copied()
                .find(|pane| *pane != WorkspacePaneKind::Editor)
        {
            group.active = other_pane;
        }

        let scale = score_view::score_base_scale() * app.svg_zoom;
        let inset = f32::from(crate::ui_style::PADDING_SM);
        app.score_viewport_cursor =
            Some(iced::Point::new(20.0 * scale + inset, 30.0 * scale + inset));

        let _ = update(&mut app, Message::Viewer(ViewerMessage::OpenPointAndClick));

        let group_id = app
            .group_for_pane(WorkspacePaneKind::Editor)
            .expect("editor group");
        let group = app.dock_groups.get(&group_id).expect("editor group state");

        assert_eq!(group.active, WorkspacePaneKind::Editor);
        assert_eq!(
            app.editor
                .active_file_backed_tab_path()
                .as_ref()
                .map(|path| state::normalize_path(path)),
            Some(state::normalize_path(&target_path))
        );
    }

    #[test]
    fn browser_single_click_selects_file_and_double_click_opens_it() {
        let root = TempDir::new().expect("tempdir");
        let file_path = root.path().join("note.txt");
        fs::write(&file_path, "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let before = app.editor.active_content();

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: file_path.clone(),
                is_dir: false,
            }),
        );

        assert_eq!(
            selected_browser_entry_name(&app, 0).as_deref(),
            Some("note.txt")
        );
        assert_eq!(file_preview_name(&app, 1).as_deref(), Some("note.txt"));
        assert_eq!(app.editor.active_content(), before);

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryDoublePressed {
                column_index: 0,
                path: file_path,
                is_dir: false,
            }),
        );

        assert_eq!(app.editor.active_content().as_deref(), Some("hello"));
    }

    #[test]
    fn browser_rename_action_starts_inline_rename() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );

        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserRename);

        assert!(matches!(
            app.browser_inline_edit.as_ref().map(|edit| edit.kind),
            Some(BrowserInlineEditKind::Rename)
        ));
    }

    #[test]
    fn browser_inline_rename_enter_commits_edit() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserRename);
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserInlineEditChanged(
                "renamed.txt".to_string(),
            )),
        );

        let _ = update(
            &mut app,
            named_key_press(keyboard::key::Named::Enter, keyboard::key::Code::Enter),
        );

        assert!(app.browser_inline_edit.is_none());
        assert!(root.path().join("renamed.txt").exists());
    }

    #[test]
    fn browser_captured_enter_after_commit_does_not_restart_rename() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserRename);
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserInlineEditChanged(
                "renamed.txt".to_string(),
            )),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::CommitFileBrowserInlineEdit),
        );

        let _ = update(
            &mut app,
            Message::KeyPressed(KeyPress {
                status: event::Status::Captured,
                key: keyboard::Key::Named(keyboard::key::Named::Enter),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
                modifiers: keyboard::Modifiers::default(),
            }),
        );

        assert!(app.browser_inline_edit.is_none());
        assert!(root.path().join("renamed.txt").exists());
    }

    #[test]
    fn browser_delete_action_opens_delete_prompt() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );

        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserDelete);

        assert!(app.error_prompt.is_some());
        assert!(matches!(
            app.prompt_ok_action,
            Some(PromptOkAction::DeleteBrowserPath(_))
        ));
    }

    #[test]
    fn prompt_enter_runs_selected_button_action() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserDelete);

        let _ = update(
            &mut app,
            Message::KeyPressed(KeyPress {
                status: event::Status::Ignored,
                key: keyboard::Key::Named(keyboard::key::Named::Enter),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
                modifiers: keyboard::Modifiers::default(),
            }),
        );

        assert!(app.error_prompt.is_none());
        assert!(app.browser_inline_edit.is_none());
        assert!(!root.path().join("note.txt").exists());
    }

    #[test]
    fn browser_copy_paste_copies_selected_entry() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("folder")).expect("folder dir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserCopy);
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("folder"),
                is_dir: true,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserPaste);

        assert!(root.path().join("note.txt").exists());
        assert!(root.path().join("folder").join("note.txt").exists());
    }

    #[test]
    fn browser_cut_paste_moves_selected_entry() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("folder")).expect("folder dir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserCut);
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("folder"),
                is_dir: true,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserPaste);

        assert!(!root.path().join("note.txt").exists());
        assert!(root.path().join("folder").join("note.txt").exists());
        assert!(app.browser_clipboard.is_none());
    }

    #[test]
    fn browser_drag_release_moves_item_into_directory() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("folder")).expect("folder dir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserDragMoved(Point::new(
                0.0, 0.0,
            ))),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserDragMoved(Point::new(
                24.0, 0.0,
            ))),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryHovered {
                column_index: 0,
                path: root.path().join("folder"),
                is_dir: true,
            }),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserDragReleased),
        );

        assert!(!root.path().join("note.txt").exists());
        assert!(root.path().join("folder").join("note.txt").exists());
    }

    #[test]
    fn browser_delete_undo_redo_restores_item() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );

        let _ = app.delete_browser_path_with_history(&root.path().join("note.txt"));
        assert!(!root.path().join("note.txt").exists());

        let _ = app.undo_browser_operation();
        assert!(root.path().join("note.txt").exists());

        let _ = app.redo_browser_operation();
        assert!(!root.path().join("note.txt").exists());
    }

    #[test]
    fn browser_cut_paste_undo_redo_moves_item_back_and_forth() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("folder")).expect("folder dir");
        fs::write(root.path().join("note.txt"), "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("note.txt"),
                is_dir: false,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserCut);
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("folder"),
                is_dir: true,
            }),
        );
        let _ = app.handle_shortcut_action(shortcuts::ShortcutAction::FileBrowserPaste);

        assert!(!root.path().join("note.txt").exists());
        assert!(root.path().join("folder").join("note.txt").exists());

        let _ = app.undo_browser_operation();
        assert!(root.path().join("note.txt").exists());
        assert!(!root.path().join("folder").join("note.txt").exists());

        let _ = app.redo_browser_operation();
        assert!(!root.path().join("note.txt").exists());
        assert!(root.path().join("folder").join("note.txt").exists());
    }

    #[test]
    fn browser_focus_message_moves_focus_from_editor() {
        let root = TempDir::new().expect("tempdir");
        let file_path = root.path().join("note.txt");
        fs::write(&file_path, "hello").expect("note file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        app.set_focused_workspace_pane(WorkspacePaneKind::Editor);
        let _ = app.open_editor_file_in_editor(&file_path);
        app.editor.request_focus();

        assert!(app.editor.active_editor_is_focused());

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserFocused),
        );

        assert!(app.editor_file_browser_focused);
        assert!(!app.editor.active_editor_is_focused());
    }

    #[test]
    fn browser_toolbar_hidden_toggle_updates_entries() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("alpha")).expect("alpha dir");
        fs::write(root.path().join("beta.txt"), "b").expect("beta file");
        fs::write(root.path().join(".hidden.txt"), "h").expect("hidden file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();

        assert_eq!(browser_entry_names(&app, 0), vec!["alpha", "beta.txt"]);
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserToggleHiddenRequested),
        );
        assert_eq!(
            browser_entry_names(&app, 0),
            vec!["alpha", ".hidden.txt", "beta.txt"]
        );
    }

    #[test]
    fn browser_new_file_requested_starts_inline_edit() {
        let root = TempDir::new().expect("tempdir");
        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserNewFileRequested),
        );

        assert!(matches!(
            app.browser_inline_edit.as_ref().map(|edit| edit.kind),
            Some(BrowserInlineEditKind::NewFile)
        ));
        assert_eq!(app.browser_inline_edit_value, "untitled");
        assert!(app.editor_file_browser_focused);
        assert!(!app.editor.active_editor_is_focused());
    }

    #[test]
    fn browser_commit_new_file_creates_and_selects_file() {
        let root = TempDir::new().expect("tempdir");
        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserNewFileRequested),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserInlineEditChanged(
                "created.txt".to_string(),
            )),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::CommitFileBrowserInlineEdit),
        );

        assert!(root.path().join("created.txt").exists());
        assert_eq!(
            selected_browser_entry_name(&app, 0).as_deref(),
            Some("created.txt")
        );
        assert_eq!(file_preview_name(&app, 1).as_deref(), Some("created.txt"));
    }

    #[test]
    fn browser_commit_rename_renames_selected_entry() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("old.txt"), "old").expect("old file");

        let mut app = test_editor_app();
        app.project_root = Some(root.path().to_path_buf());
        app.editor.set_project_root(Some(root.path().to_path_buf()));
        app.editor.toggle_file_browser();
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserEntryPressed {
                column_index: 0,
                path: root.path().join("old.txt"),
                is_dir: false,
            }),
        );

        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserRenameRequested),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::FileBrowserInlineEditChanged(
                "renamed.txt".to_string(),
            )),
        );
        let _ = update(
            &mut app,
            Message::Editor(messages::EditorMessage::CommitFileBrowserInlineEdit),
        );

        assert!(!root.path().join("old.txt").exists());
        assert!(root.path().join("renamed.txt").exists());
        assert_eq!(
            selected_browser_entry_name(&app, 0).as_deref(),
            Some("renamed.txt")
        );
        assert_eq!(file_preview_name(&app, 1).as_deref(), Some("renamed.txt"));
    }
}
