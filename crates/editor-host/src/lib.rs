#![allow(missing_docs)]

use std::ffi::c_void;
use std::num::{NonZeroIsize, NonZeroU32};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub use egui;
use lilypalooza_egui_baseview::{EguiApp, EguiWindowHandle, EguiWindowOptions, open_parented};
use raw_window_handle::{
    AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle, Win32WindowHandle, XcbDisplayHandle,
    XcbWindowHandle, XlibDisplayHandle, XlibWindowHandle,
};

#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowHandleSnapshot {
    AppKit { ns_view: usize },
    Win32 { hwnd: isize },
    Xcb { window: u32 },
    Xlib { window: u64 },
    Wayland { surface: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayHandleSnapshot {
    AppKit,
    Xcb {
        connection: Option<usize>,
        screen: i32,
    },
    Xlib {
        display: Option<usize>,
        screen: i32,
    },
    Wayland {
        display: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub window: WindowHandleSnapshot,
    pub display: Option<DisplayHandleSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditorHostOptions {
    pub title: String,
    pub resizable: bool,
    pub owner: Option<WindowSnapshot>,
}

impl EditorHostOptions {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            resizable: true,
            owner: None,
        }
    }

    #[must_use]
    pub fn with_resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    #[must_use]
    pub fn with_owner(mut self, owner: WindowSnapshot) -> Self {
        self.owner = Some(owner);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditorFrameLayout {
    pub outer_width: f64,
    pub outer_height: f64,
    pub titlebar: Rect,
    pub content: Rect,
}

#[must_use]
pub fn host_layout(
    content_width: f64,
    content_height: f64,
    titlebar_height: f64,
    frame_thickness: f64,
) -> EditorFrameLayout {
    let frame = frame_thickness.max(0.0);
    let titlebar_height = titlebar_height.max(20.0);
    let outer_width = content_width + frame * 2.0;
    let outer_height = content_height + titlebar_height + frame * 2.0;

    EditorFrameLayout {
        outer_width,
        outer_height,
        titlebar: Rect {
            x: frame,
            y: frame + content_height,
            width: content_width,
            height: titlebar_height,
        },
        content: Rect {
            x: frame,
            y: frame,
            width: content_width,
            height: content_height,
        },
    }
}

#[must_use]
pub fn content_size_from_outer_size(
    outer_size: Size,
    titlebar_height: f64,
    frame_thickness: f64,
) -> Size {
    let frame = frame_thickness.max(0.0);
    let titlebar_height = titlebar_height.max(20.0);
    Size {
        width: (outer_size.width - frame * 2.0).max(1.0),
        height: (outer_size.height - titlebar_height - frame * 2.0).max(1.0),
    }
}

fn same_size(a: Size, b: Size) -> bool {
    (a.width - b.width).abs() < 0.5 && (a.height - b.height).abs() < 0.5
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditorHostState {
    pub title: String,
    pub resizable: bool,
    pub close_requested: bool,
    pub content_size: Size,
    pub preset: Option<EditorPresetState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorPresetState {
    pub current_name: String,
    pub selected_id: Option<String>,
    pub expanded: bool,
    pub items: Vec<EditorPresetItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorPresetItem {
    pub id: String,
    pub name: String,
    pub origin: EditorPresetOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorPresetOrigin {
    User,
    Factory,
    PluginNative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorFrameAction {
    None,
    Close,
    DragWindow,
    Command(EditorFrameCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorFrameCommand {
    PreviousPreset,
    NextPreset,
    LoadPreset(String),
    RenamePreset { id: String, name: String },
    DeletePreset(String),
    SavePreset,
    TogglePresetBrowser,
    ResizeContent { width: u32, height: u32 },
}

pub trait EditorFrame {
    fn layout(&self, content_size: Size) -> EditorFrameLayout;

    fn render(&mut self, ui: &mut egui::Ui, state: &EditorHostState) -> EditorFrameAction;
}

pub struct InstalledHost {
    host: WindowSnapshot,
    content: WindowSnapshot,
    close_requested: Arc<AtomicBool>,
    title: Arc<Mutex<String>>,
    preset_state: Arc<Mutex<Option<EditorPresetState>>>,
    frame_commands: Arc<Mutex<Vec<EditorFrameCommand>>>,
    frame_window: Option<EguiWindowHandle>,
    frame_content_size: Arc<Mutex<Size>>,
    content_size: Size,
    frame_thickness: f64,
}

impl std::fmt::Debug for InstalledHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstalledHost")
            .field("host", &self.host)
            .field("content", &self.content)
            .field("close_requested", &self.close_requested())
            .finish_non_exhaustive()
    }
}

impl Drop for InstalledHost {
    fn drop(&mut self) {
        if let Some(mut window) = self.frame_window.take() {
            window.close();
        }
    }
}

impl InstalledHost {
    #[must_use]
    pub fn host(&self) -> WindowSnapshot {
        self.host
    }

    #[must_use]
    pub fn content(&self) -> WindowSnapshot {
        self.content
    }

    #[must_use]
    pub fn content_size(&self) -> Size {
        self.content_size
    }

    #[must_use]
    pub fn close_requested(&self) -> bool {
        self.close_requested.load(Ordering::Relaxed)
    }

    pub fn clear_close_requested(&self) {
        self.close_requested.store(false, Ordering::Relaxed);
    }

    pub fn set_preset_state(&mut self, preset: Option<EditorPresetState>) {
        if let Ok(mut current) = self.preset_state.lock() {
            *current = preset;
        }
        if self.frame_window.is_some() {
            let _ = resize_installed_host(
                &self.host,
                &self.content,
                self.content_size,
                self.titlebar_height(),
                self.frame_thickness,
                self.frame_window.as_mut(),
                ResizeAnchor::Bottom,
            );
        }
    }

    pub fn resize_content(&mut self, content_size: Size) -> Result<(), Error> {
        self.resize_content_with_anchor(content_size, ResizeAnchor::Bottom)
    }

    pub fn resize_content_from_top(&mut self, content_size: Size) -> Result<(), Error> {
        self.resize_content_with_anchor(content_size, ResizeAnchor::Top)
    }

    fn resize_content_with_anchor(
        &mut self,
        content_size: Size,
        anchor: ResizeAnchor,
    ) -> Result<(), Error> {
        if same_size(self.content_size, content_size) {
            trace_editor_host(|| {
                format!(
                    "installed-host resize_content ignored same size current={:?} requested={:?} anchor={anchor:?}",
                    self.content_size, content_size
                )
            });
            return Ok(());
        }
        let previous = self.content_size;
        self.set_content_size(content_size);
        if self.frame_window.is_none() {
            trace_editor_host(|| {
                format!(
                    "installed-host resize_content stored without frame current={previous:?} requested={content_size:?} anchor={anchor:?}"
                )
            });
            return Ok(());
        }
        trace_editor_host(|| {
            format!(
                "installed-host resize_content applying current={previous:?} requested={content_size:?} anchor={anchor:?}"
            )
        });
        resize_installed_host(
            &self.host,
            &self.content,
            self.content_size,
            self.titlebar_height(),
            self.frame_thickness,
            self.frame_window.as_mut(),
            anchor,
        )
    }

    fn set_content_size(&mut self, content_size: Size) {
        self.content_size = content_size;
        if let Ok(mut frame_content_size) = self.frame_content_size.lock() {
            *frame_content_size = content_size;
        }
    }

    pub fn resize_outer(&mut self, outer_size: Size) -> Result<Size, Error> {
        let content_size =
            content_size_from_outer_size(outer_size, self.titlebar_height(), self.frame_thickness);
        self.resize_content(content_size)?;
        Ok(content_size)
    }

    #[must_use]
    pub fn content_size_from_outer_size(&self, outer_size: Size) -> Size {
        content_size_from_outer_size(outer_size, self.titlebar_height(), self.frame_thickness)
    }

    fn titlebar_height(&self) -> f64 {
        self.preset_state()
            .as_ref()
            .map_or(34.0, |preset| if preset.expanded { 160.0 } else { 34.0 })
    }

    #[must_use]
    pub fn preset_state(&self) -> Option<EditorPresetState> {
        self.preset_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn drain_frame_commands(&mut self) -> Vec<EditorFrameCommand> {
        self.frame_commands
            .lock()
            .map(|mut commands| commands.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn set_visible(&mut self, visible: bool) -> Result<(), Error> {
        set_host_window_visible(&self.host, visible)
    }

    pub fn set_title(&mut self, title: impl Into<String>) -> Result<(), Error> {
        let title = title.into();
        if let Ok(mut current) = self.title.lock() {
            *current = title.clone();
        }
        set_host_window_title(&self.host, &title)
    }

    pub fn raise(&mut self) -> Result<(), Error> {
        raise_host_window(&self.host)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeAnchor {
    Top,
    Bottom,
}

fn trace_editor_host(message: impl FnOnce() -> String) {
    if std::env::var_os("LILYPALOOZA_EDITOR_HOST_TRACE").is_some() {
        eprintln!("[editor-host] {}", message());
    }
}

#[cfg(test)]
impl InstalledHost {
    fn test_with_frame_commands(
        commands: impl IntoIterator<Item = EditorFrameCommand>,
    ) -> (Self, Vec<EditorFrameCommand>) {
        let commands = commands.into_iter().collect::<Vec<_>>();
        let frame_commands = Arc::new(Mutex::new(commands.clone()));
        let content_size = Size {
            width: 440.0,
            height: 360.0,
        };
        let host = WindowSnapshot {
            window: WindowHandleSnapshot::AppKit { ns_view: 1 },
            display: Some(DisplayHandleSnapshot::AppKit),
        };
        (
            Self {
                host,
                content: host,
                close_requested: Arc::new(AtomicBool::new(false)),
                title: Arc::new(Mutex::new("Test".to_string())),
                preset_state: Arc::new(Mutex::new(None)),
                frame_commands,
                frame_window: None,
                frame_content_size: Arc::new(Mutex::new(content_size)),
                content_size,
                frame_thickness: 4.0,
            },
            commands,
        )
    }
}

impl WindowSnapshot {
    pub fn capture(
        window: RawWindowHandle,
        display: Option<RawDisplayHandle>,
    ) -> Result<Self, Error> {
        let window = match window {
            RawWindowHandle::AppKit(handle) => WindowHandleSnapshot::AppKit {
                ns_view: handle.ns_view.as_ptr() as usize,
            },
            RawWindowHandle::Win32(handle) => WindowHandleSnapshot::Win32 {
                hwnd: handle.hwnd.get(),
            },
            RawWindowHandle::Xcb(handle) => WindowHandleSnapshot::Xcb {
                window: handle.window.get(),
            },
            RawWindowHandle::Xlib(handle) => WindowHandleSnapshot::Xlib {
                window: handle.window,
            },
            RawWindowHandle::Wayland(handle) => WindowHandleSnapshot::Wayland {
                surface: handle.surface.as_ptr() as usize,
            },
            other => {
                return Err(Error::Message(format!(
                    "unsupported window handle: {other:?}"
                )));
            }
        };
        let display = display
            .map(|display| match display {
                RawDisplayHandle::AppKit(_) => Ok(DisplayHandleSnapshot::AppKit),
                RawDisplayHandle::Xcb(handle) => Ok(DisplayHandleSnapshot::Xcb {
                    connection: handle
                        .connection
                        .map(|connection| connection.as_ptr() as usize),
                    screen: handle.screen,
                }),
                RawDisplayHandle::Xlib(handle) => Ok(DisplayHandleSnapshot::Xlib {
                    display: handle.display.map(|display| display.as_ptr() as usize),
                    screen: handle.screen,
                }),
                RawDisplayHandle::Wayland(handle) => Ok(DisplayHandleSnapshot::Wayland {
                    display: handle.display.as_ptr() as usize,
                }),
                other => Err(Error::Message(format!(
                    "unsupported display handle: {other:?}"
                ))),
            })
            .transpose()?;

        Ok(Self { window, display })
    }

    pub fn raw_window_handle(&self) -> Result<RawWindowHandle, Error> {
        match self.window {
            WindowHandleSnapshot::AppKit { ns_view } => Ok(RawWindowHandle::AppKit(
                AppKitWindowHandle::new(non_null_ptr(ns_view, "ns_view")?),
            )),
            WindowHandleSnapshot::Win32 { hwnd } => Ok(RawWindowHandle::Win32(
                Win32WindowHandle::new(non_zero_isize(hwnd, "hwnd")?),
            )),
            WindowHandleSnapshot::Xcb { window } => Ok(RawWindowHandle::Xcb(XcbWindowHandle::new(
                non_zero_u32(window, "xcb window")?,
            ))),
            WindowHandleSnapshot::Xlib { window } => {
                Ok(RawWindowHandle::Xlib(XlibWindowHandle::new(window)))
            }
            WindowHandleSnapshot::Wayland { surface } => Ok(RawWindowHandle::Wayland(
                WaylandWindowHandle::new(non_null_ptr(surface, "wayland surface")?),
            )),
        }
    }

    pub fn raw_display_handle(&self) -> Result<Option<RawDisplayHandle>, Error> {
        self.display
            .map(|display| match display {
                DisplayHandleSnapshot::AppKit => Ok::<RawDisplayHandle, Error>(
                    RawDisplayHandle::AppKit(AppKitDisplayHandle::new()),
                ),
                DisplayHandleSnapshot::Xcb { connection, screen } => {
                    Ok::<RawDisplayHandle, Error>(RawDisplayHandle::Xcb(XcbDisplayHandle::new(
                        connection.map(non_null_ptr_unchecked),
                        screen,
                    )))
                }
                DisplayHandleSnapshot::Xlib { display, screen } => {
                    Ok::<RawDisplayHandle, Error>(RawDisplayHandle::Xlib(XlibDisplayHandle::new(
                        display.map(non_null_ptr_unchecked),
                        screen,
                    )))
                }
                DisplayHandleSnapshot::Wayland { display } => {
                    Ok::<RawDisplayHandle, Error>(RawDisplayHandle::Wayland(
                        WaylandDisplayHandle::new(non_null_ptr(display, "wayland display")?),
                    ))
                }
            })
            .transpose()
    }
}

pub fn prepare_process() -> Result<(), Error> {
    #[cfg(target_os = "macos")]
    {
        macos::prepare_process()
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub fn route_app_quit_to_window_close(window: &WindowSnapshot) -> Result<(), Error> {
    macos::route_app_quit_to_window_close(window)
}

#[cfg(not(target_os = "macos"))]
pub fn route_app_quit_to_window_close(window: &WindowSnapshot) -> Result<(), Error> {
    window.raw_window_handle()?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn install_editor_host(
    host: &WindowSnapshot,
    options: &EditorHostOptions,
    frame: impl EditorFrame + Send + 'static,
) -> Result<InstalledHost, Error> {
    macos::install_editor_host(host, options, frame)
}

#[cfg(not(target_os = "macos"))]
pub fn install_editor_host(
    host: &WindowSnapshot,
    options: &EditorHostOptions,
    frame: impl EditorFrame + Send + 'static,
) -> Result<InstalledHost, Error> {
    let _ = frame;
    host.raw_window_handle()?;
    if options.title.contains('\0') {
        return Err(Error::Message("host title contains a NUL byte".to_string()));
    }
    Err(Error::Message(
        "editor host framing backend is only implemented on macOS".to_string(),
    ))
}

#[cfg(target_os = "macos")]
pub fn set_host_window_visible(host: &WindowSnapshot, visible: bool) -> Result<(), Error> {
    macos::set_host_window_visible(host, visible)
}

#[cfg(not(target_os = "macos"))]
pub fn set_host_window_visible(host: &WindowSnapshot, visible: bool) -> Result<(), Error> {
    host.raw_window_handle()?;
    if visible {
        return Ok(());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_host_window_title(host: &WindowSnapshot, title: &str) -> Result<(), Error> {
    macos::set_host_window_title(host, title)
}

#[cfg(not(target_os = "macos"))]
fn set_host_window_title(host: &WindowSnapshot, _title: &str) -> Result<(), Error> {
    host.raw_window_handle()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn raise_host_window(host: &WindowSnapshot) -> Result<(), Error> {
    macos::raise_host_window(host)
}

#[cfg(not(target_os = "macos"))]
fn raise_host_window(host: &WindowSnapshot) -> Result<(), Error> {
    host.raw_window_handle()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn begin_host_window_drag(host: &WindowSnapshot) -> Result<(), Error> {
    macos::begin_host_window_drag(host)
}

#[cfg(not(target_os = "macos"))]
fn begin_host_window_drag(host: &WindowSnapshot) -> Result<(), Error> {
    host.raw_window_handle()?;
    Ok(())
}

fn non_null_ptr(value: usize, name: &str) -> Result<NonNull<c_void>, Error> {
    NonNull::new(value as *mut c_void).ok_or_else(|| Error::Message(format!("{name} is null")))
}

fn non_null_ptr_unchecked(value: usize) -> NonNull<c_void> {
    NonNull::new(value as *mut c_void).expect("stored non-null pointer")
}

fn non_zero_isize(value: isize, name: &str) -> Result<NonZeroIsize, Error> {
    NonZeroIsize::new(value).ok_or_else(|| Error::Message(format!("{name} is zero")))
}

fn non_zero_u32(value: u32, name: &str) -> Result<NonZeroU32, Error> {
    NonZeroU32::new(value).ok_or_else(|| Error::Message(format!("{name} is zero")))
}

pub(crate) fn open_egui_frame(
    parent: WindowSnapshot,
    options: &EditorHostOptions,
    content_size: Size,
    outer_width: f64,
    outer_height: f64,
    frame: impl EditorFrame + Send + 'static,
) -> Result<EguiFrameHost, Error> {
    let close_requested = Arc::new(AtomicBool::new(false));
    let title = Arc::new(Mutex::new(options.title.clone()));
    let preset_state = Arc::new(Mutex::new(None));
    let frame_content_size = Arc::new(Mutex::new(content_size));
    let frame_commands = Arc::new(Mutex::new(Vec::new()));
    let app = FrameApp {
        frame,
        host: parent,
        title: Arc::clone(&title),
        preset_state: Arc::clone(&preset_state),
        content_size: Arc::clone(&frame_content_size),
        state: EditorHostState {
            title: options.title.clone(),
            resizable: options.resizable,
            close_requested: false,
            content_size,
            preset: None,
        },
        close_requested: Arc::clone(&close_requested),
        frame_commands: Arc::clone(&frame_commands),
    };
    let window = open_parented(
        parent.raw_window_handle()?,
        EguiWindowOptions {
            title: options.title.clone(),
            width: outer_width,
            height: outer_height,
        },
        move || app,
    )
    .map_err(|error| Error::Message(error.to_string()))?;
    Ok(EguiFrameHost {
        window,
        close_requested,
        title,
        preset_state,
        content_size: frame_content_size,
        frame_commands,
    })
}

pub(crate) struct EguiFrameHost {
    pub(crate) window: EguiWindowHandle,
    pub(crate) close_requested: Arc<AtomicBool>,
    pub(crate) title: Arc<Mutex<String>>,
    pub(crate) preset_state: Arc<Mutex<Option<EditorPresetState>>>,
    pub(crate) content_size: Arc<Mutex<Size>>,
    pub(crate) frame_commands: Arc<Mutex<Vec<EditorFrameCommand>>>,
}

struct FrameApp<F> {
    frame: F,
    host: WindowSnapshot,
    title: Arc<Mutex<String>>,
    preset_state: Arc<Mutex<Option<EditorPresetState>>>,
    content_size: Arc<Mutex<Size>>,
    state: EditorHostState,
    close_requested: Arc<AtomicBool>,
    frame_commands: Arc<Mutex<Vec<EditorFrameCommand>>>,
}

impl<F: EditorFrame> EguiApp for FrameApp<F> {
    fn update(&mut self, ctx: &egui::Context) {
        self.state.close_requested = self.close_requested.load(Ordering::Relaxed);
        if let Ok(title) = self.title.lock() {
            self.state.title.clone_from(&title);
        }
        if let Ok(preset_state) = self.preset_state.lock() {
            self.state.preset.clone_from(&preset_state);
        }
        if let Ok(content_size) = self.content_size.lock() {
            self.state.content_size = *content_size;
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| match self.frame.render(ui, &self.state) {
                EditorFrameAction::None => {}
                EditorFrameAction::Close => {
                    self.close_requested.store(true, Ordering::Relaxed);
                }
                EditorFrameAction::DragWindow => {
                    let _ = begin_host_window_drag(&self.host);
                }
                EditorFrameAction::Command(command) => {
                    if let Ok(mut commands) = self.frame_commands.lock() {
                        commands.push(command);
                    }
                }
            });
        ctx.request_repaint();
    }
}

#[cfg(target_os = "macos")]
fn resize_installed_host(
    host: &WindowSnapshot,
    content: &WindowSnapshot,
    content_size: Size,
    titlebar_height: f64,
    frame_thickness: f64,
    frame_window: Option<&mut EguiWindowHandle>,
    anchor: ResizeAnchor,
) -> Result<(), Error> {
    let layout = host_layout(
        content_size.width,
        content_size.height,
        titlebar_height,
        frame_thickness,
    );
    macos::resize_installed_host(
        host,
        content,
        content_size,
        titlebar_height,
        frame_thickness,
        anchor,
    )?;
    if let Some(frame_window) = frame_window {
        trace_editor_host(|| {
            format!(
                "egui frame resize outer={}x{} content={content_size:?}",
                layout.outer_width, layout.outer_height
            )
        });
        frame_window.resize(layout.outer_width, layout.outer_height);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn resize_installed_host(
    _host: &WindowSnapshot,
    _content: &WindowSnapshot,
    content_size: Size,
    titlebar_height: f64,
    frame_thickness: f64,
    frame_window: Option<&mut EguiWindowHandle>,
    _anchor: ResizeAnchor,
) -> Result<(), Error> {
    let layout = host_layout(
        content_size.width,
        content_size.height,
        titlebar_height,
        frame_thickness,
    );
    if let Some(frame_window) = frame_window {
        frame_window.resize(layout.outer_width, layout.outer_height);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use raw_window_handle::{
        AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle,
    };

    use super::{
        EditorFrame, EditorFrameAction, EditorHostOptions, EditorHostState, Size, WindowSnapshot,
        host_layout,
    };

    #[test]
    fn window_snapshot_roundtrips_appkit() {
        let snapshot = WindowSnapshot::capture(
            RawWindowHandle::AppKit(AppKitWindowHandle::new(
                NonNull::<std::ffi::c_void>::dangling(),
            )),
            Some(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
        )
        .expect("snapshot should capture appkit");

        let window = snapshot
            .raw_window_handle()
            .expect("snapshot should restore appkit window");
        let display = snapshot
            .raw_display_handle()
            .expect("snapshot should restore appkit display");

        assert!(matches!(window, RawWindowHandle::AppKit(_)));
        assert!(matches!(display, Some(RawDisplayHandle::AppKit(_))));
    }

    #[test]
    fn host_layout_puts_titlebar_above_content() {
        let layout = host_layout(440.0, 360.0, 30.0, 4.0);

        assert_eq!(layout.content.height, 360.0);
        assert!(layout.titlebar.y >= layout.content.y + layout.content.height);
        assert!(layout.outer_height > 360.0);
    }

    #[test]
    fn host_layout_keeps_content_unclipped_inside_frame() {
        let layout = host_layout(440.0, 360.0, 30.0, 4.0);

        assert_eq!(layout.content.width, 440.0);
        assert_eq!(layout.content.height, 360.0);
        assert_eq!(layout.content.x, 4.0);
        assert_eq!(layout.content.y, 4.0);
    }

    #[test]
    fn host_layout_adds_frame_to_content_slot() {
        let layout = host_layout(820.0, 456.0, 30.0, 4.0);

        assert_eq!(layout.outer_width, 828.0);
        assert_eq!(layout.outer_height, 494.0);
        assert_eq!(layout.content.width, 820.0);
        assert_eq!(layout.content.height, 456.0);
        assert_eq!(layout.titlebar.height, 30.0);
        assert_eq!(layout.titlebar.y, 460.0);
    }

    #[test]
    fn content_size_from_outer_size_removes_frame_and_titlebar() {
        assert_eq!(
            super::content_size_from_outer_size(
                Size {
                    width: 828.0,
                    height: 494.0,
                },
                30.0,
                4.0,
            ),
            Size {
                width: 820.0,
                height: 456.0,
            }
        );
    }

    #[test]
    fn editor_frame_trait_is_the_only_frame_customization_api() {
        struct TestFrame;

        impl EditorFrame for TestFrame {
            fn layout(&self, content_size: Size) -> super::EditorFrameLayout {
                host_layout(content_size.width, content_size.height, 30.0, 4.0)
            }

            fn render(
                &mut self,
                _ui: &mut egui::Ui,
                _state: &EditorHostState,
            ) -> EditorFrameAction {
                EditorFrameAction::None
            }
        }

        let frame = TestFrame;
        let layout = frame.layout(Size {
            width: 440.0,
            height: 360.0,
        });
        assert_eq!(layout.content.width, 440.0);
        assert_eq!(layout.content.height, 360.0);
    }

    #[test]
    fn installed_host_exposes_frame_commands() {
        let (mut host, commands) = super::InstalledHost::test_with_frame_commands([
            super::EditorFrameCommand::PreviousPreset,
            super::EditorFrameCommand::LoadPreset("preset-1".to_string()),
        ]);

        assert_eq!(host.drain_frame_commands(), commands);
        assert!(host.drain_frame_commands().is_empty());
    }

    #[test]
    fn installed_host_stores_preset_state_for_frame() {
        let (mut host, _) = super::InstalledHost::test_with_frame_commands([]);
        let state = super::EditorPresetState {
            current_name: "Warm Piano".to_string(),
            selected_id: Some("preset-1".to_string()),
            expanded: false,
            items: vec![super::EditorPresetItem {
                id: "preset-1".to_string(),
                name: "Warm Piano".to_string(),
                origin: super::EditorPresetOrigin::User,
            }],
        };

        host.set_preset_state(Some(state.clone()));

        assert_eq!(host.preset_state(), Some(state));
    }

    #[test]
    fn installed_host_derives_content_size_from_current_chrome() {
        let (host, _) = super::InstalledHost::test_with_frame_commands([]);

        assert_eq!(
            host.content_size_from_outer_size(Size {
                width: 648.0,
                height: 522.0,
            }),
            Size {
                width: 640.0,
                height: 480.0,
            }
        );
    }

    #[test]
    fn installed_host_updates_frame_content_size_when_content_resizes() {
        let (mut host, _) = super::InstalledHost::test_with_frame_commands([]);

        host.resize_content(Size {
            width: 512.0,
            height: 384.0,
        })
        .expect("test host without native window should still update state");

        assert_eq!(
            *host
                .frame_content_size
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Size {
                width: 512.0,
                height: 384.0,
            }
        );
    }

    #[test]
    fn installed_host_ignores_same_content_size_resize() {
        let (mut host, _) = super::InstalledHost::test_with_frame_commands([]);

        host.resize_content(Size {
            width: 440.25,
            height: 359.75,
        })
        .expect("same size resize should be a no-op");

        assert_eq!(
            *host
                .frame_content_size
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Size {
                width: 440.0,
                height: 360.0,
            }
        );
    }

    #[test]
    fn host_options_default_to_resizable() {
        assert!(EditorHostOptions::new("Editor").resizable);
    }

    #[test]
    fn host_options_default_to_no_owner() {
        assert_eq!(EditorHostOptions::new("Editor").owner, None);
    }

    #[test]
    fn host_options_can_disable_resizing() {
        assert!(
            !EditorHostOptions::new("Editor")
                .with_resizable(false)
                .resizable
        );
    }

    #[test]
    fn host_options_can_store_owner_window() {
        let owner = WindowSnapshot::capture(
            RawWindowHandle::AppKit(AppKitWindowHandle::new(
                NonNull::<std::ffi::c_void>::dangling(),
            )),
            Some(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
        )
        .expect("snapshot should capture appkit");

        assert_eq!(
            EditorHostOptions::new("Editor").with_owner(owner).owner,
            Some(owner)
        );
    }
}
