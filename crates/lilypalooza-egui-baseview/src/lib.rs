//! Minimal egui host on top of parented baseview windows.

use std::sync::Arc;
use std::sync::Mutex;

pub use baseview::Size;
use baseview::gl::GlConfig;
use baseview::{
    Event, EventStatus, MouseButton, MouseEvent, ScrollDelta, Window, WindowHandle, WindowHandler,
    WindowInfo, WindowOpenOptions, WindowScalePolicy,
};
use egui::{Pos2, RawInput, Rect, Vec2};
use egui_glow::Painter;
use glow::HasContext;
use keyboard_types::{Key, KeyState, KeyboardEvent, Modifiers};
use raw_window_handle::{
    AppKitWindowHandle, HasRawWindowHandle, RawWindowHandle, WaylandWindowHandle,
    Win32WindowHandle, XcbWindowHandle, XlibWindowHandle,
};
use raw_window_handle_06 as rwh06;

/// Re-exported egui API for editor implementations.
pub use egui;

/// Editor window open options.
#[derive(Debug, Clone)]
pub struct EguiWindowOptions {
    /// Window title.
    pub title: String,
    /// Logical content width.
    pub width: f64,
    /// Logical content height.
    pub height: f64,
}

/// Live parented egui editor handle.
pub struct EguiWindowHandle {
    window: WindowHandle,
    pending_resize: Arc<Mutex<Option<Size>>>,
}

impl EguiWindowHandle {
    /// Closes and destroys the parented editor view.
    pub fn close(&mut self) {
        self.window.close();
    }

    /// Resizes the parented editor view.
    pub fn resize(&mut self, width: f64, height: f64) {
        if let Ok(mut pending) = self.pending_resize.lock() {
            *pending = Some(Size::new(width, height));
        }
    }
}

/// Errors returned by the egui/baseview bridge.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Parent handle type is not supported by baseview.
    #[error("unsupported parent window handle: {0}")]
    UnsupportedParent(String),
}

/// Opens a parented egui editor view.
///
/// # Errors
///
/// Returns an error when the parent raw window handle cannot be converted to baseview's raw-window-handle version.
pub fn open_parented<App, Build>(
    parent: rwh06::RawWindowHandle,
    options: EguiWindowOptions,
    build: Build,
) -> Result<EguiWindowHandle, Error>
where
    App: EguiApp + 'static,
    Build: FnOnce() -> App + Send + 'static,
{
    let parent = ParentWindow::from_raw(parent)?;
    let pending_resize = Arc::new(Mutex::new(None));
    let open_options = WindowOpenOptions {
        title: options.title,
        size: Size::new(options.width, options.height),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(GlConfig {
            alpha_bits: 8,
            ..GlConfig::default()
        }),
    };
    let window_pending_resize = Arc::clone(&pending_resize);
    let initial_size = Size::new(options.width, options.height);
    let window = Window::open_parented(&parent, open_options, move |window| {
        EguiWindow::new(window, build(), window_pending_resize, initial_size)
    });
    Ok(EguiWindowHandle {
        window,
        pending_resize,
    })
}

/// egui editor application.
pub trait EguiApp {
    /// Draws one egui frame.
    fn update(&mut self, ctx: &egui::Context);
}

struct ParentWindow {
    raw: RawWindowHandle,
}

impl ParentWindow {
    fn from_raw(raw: rwh06::RawWindowHandle) -> Result<Self, Error> {
        let raw = match raw {
            rwh06::RawWindowHandle::AppKit(handle) => {
                let mut converted = AppKitWindowHandle::empty();
                converted.ns_view = handle.ns_view.as_ptr();
                RawWindowHandle::AppKit(converted)
            }
            rwh06::RawWindowHandle::Win32(handle) => {
                let mut converted = Win32WindowHandle::empty();
                converted.hwnd = handle.hwnd.get() as *mut _;
                converted.hinstance = handle
                    .hinstance
                    .map_or(std::ptr::null_mut(), |value| value.get() as *mut _);
                RawWindowHandle::Win32(converted)
            }
            rwh06::RawWindowHandle::Xcb(handle) => {
                let mut converted = XcbWindowHandle::empty();
                converted.window = handle.window.get();
                converted.visual_id = handle.visual_id.map_or(0, |id| id.get());
                RawWindowHandle::Xcb(converted)
            }
            rwh06::RawWindowHandle::Xlib(handle) => {
                let mut converted = XlibWindowHandle::empty();
                converted.window = handle.window;
                converted.visual_id = handle.visual_id;
                RawWindowHandle::Xlib(converted)
            }
            rwh06::RawWindowHandle::Wayland(handle) => {
                let mut converted = WaylandWindowHandle::empty();
                converted.surface = handle.surface.as_ptr();
                RawWindowHandle::Wayland(converted)
            }
            other => return Err(Error::UnsupportedParent(format!("{other:?}"))),
        };
        Ok(Self { raw })
    }
}

// SAFETY: `ParentWindow` stores a raw window handle captured from the host while the parent view is
// alive. The bridge only hands it to baseview during immediate parented-window creation.
unsafe impl HasRawWindowHandle for ParentWindow {
    fn raw_window_handle(&self) -> RawWindowHandle {
        self.raw
    }
}

struct EguiWindow<App> {
    app: App,
    ctx: egui::Context,
    painter: Painter,
    gl: Arc<glow::Context>,
    input: RawInput,
    window_info: WindowInfo,
    pointer_pos: Option<Pos2>,
    pending_resize: Arc<Mutex<Option<Size>>>,
}

impl<App: EguiApp> EguiWindow<App> {
    fn new(
        window: &mut Window<'_>,
        app: App,
        pending_resize: Arc<Mutex<Option<Size>>>,
        initial_size: Size,
    ) -> Self {
        let Some(context) = window.gl_context() else {
            panic!("baseview did not create an OpenGL context");
        };
        // SAFETY: baseview created the OpenGL context for this window and this callback runs on
        // the window thread while the context is valid.
        unsafe {
            context.make_current();
        }
        // SAFETY: the loader function is provided by the current baseview OpenGL context and stays
        // valid for the lifetime of that context.
        let gl = unsafe {
            glow::Context::from_loader_function(|name| context.get_proc_address(name) as *const _)
        };
        let gl = Arc::new(gl);
        let painter = match Painter::new(Arc::clone(&gl), "", None, false) {
            Ok(painter) => painter,
            Err(error) => panic!("egui glow painter failed to initialize: {error}"),
        };
        // SAFETY: the current thread owns the baseview OpenGL context for this callback.
        unsafe {
            context.make_not_current();
        }
        let mut window = Self {
            app,
            ctx: egui::Context::default(),
            painter,
            gl,
            input: RawInput::default(),
            window_info: initial_window_info(initial_size),
            pointer_pos: None,
            pending_resize,
        };
        window.set_window_info(initial_window_info(initial_size));
        window
    }

    fn set_window_info(&mut self, window_info: WindowInfo) {
        self.window_info = window_info;
        let logical = window_info.logical_size();
        self.input.screen_rect = Some(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(logical.width as f32, logical.height as f32),
        ));
        if let Some(viewport) = self.input.viewports.get_mut(&egui::ViewportId::ROOT) {
            viewport.native_pixels_per_point = Some(window_info.scale() as f32);
        }
    }

    fn push_event(&mut self, event: egui::Event) {
        self.input.events.push(event);
    }

    fn render(&mut self, window: &mut Window<'_>) {
        let Some(context) = window.gl_context() else {
            panic!("baseview lost its OpenGL context");
        };
        // SAFETY: baseview invokes rendering on the window thread with a live OpenGL context.
        unsafe {
            context.make_current();
        }

        let raw_input = std::mem::take(&mut self.input);
        let output = self.ctx.run(raw_input, |ctx| self.app.update(ctx));
        self.input = RawInput::default();
        self.set_window_info(self.window_info);

        let clipped_primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let physical = self.window_info.physical_size();
        let screen_size = [physical.width, physical.height];
        let [r, g, b, a] = clear_color();
        // SAFETY: the glow context is current for this window during this render call.
        unsafe {
            self.gl.clear_color(r, g, b, a);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.painter.paint_and_update_textures(
            screen_size,
            output.pixels_per_point,
            &clipped_primitives,
            &output.textures_delta,
        );
        context.swap_buffers();
        // SAFETY: the current thread owns the baseview OpenGL context for this callback.
        unsafe {
            context.make_not_current();
        }
    }
}

fn clear_color() -> [f32; 4] {
    [0.0, 0.0, 0.0, 0.0]
}

fn initial_window_info(size: Size) -> WindowInfo {
    WindowInfo::from_logical_size(size, 1.0)
}

impl<App> Drop for EguiWindow<App> {
    fn drop(&mut self) {
        self.painter.destroy();
    }
}

impl<App: EguiApp> WindowHandler for EguiWindow<App> {
    fn on_frame(&mut self, window: &mut Window<'_>) {
        let requested_size = self
            .pending_resize
            .lock()
            .ok()
            .and_then(|mut pending| pending.take());
        if let Some(size) = requested_size {
            window.resize(size);
            self.set_window_info(WindowInfo::from_logical_size(
                size,
                self.window_info.scale(),
            ));
        }
        self.render(window);
    }

    fn on_event(&mut self, _window: &mut Window<'_>, event: Event) -> EventStatus {
        match event {
            Event::Window(baseview::WindowEvent::Resized(info)) => {
                self.set_window_info(info);
            }
            Event::Window(baseview::WindowEvent::Focused) => {
                self.input.focused = true;
            }
            Event::Window(baseview::WindowEvent::Unfocused) => {
                self.input.focused = false;
            }
            Event::Window(baseview::WindowEvent::WillClose) => {}
            Event::Mouse(mouse_event) => self.handle_mouse(_window, mouse_event),
            Event::Keyboard(key_event) => {
                if is_command_quit(&key_event) {
                    return EventStatus::Ignored;
                }
                self.handle_keyboard(key_event);
            }
        }
        EventStatus::Captured
    }
}

impl<App: EguiApp> EguiWindow<App> {
    fn handle_mouse(&mut self, window: &mut Window<'_>, event: MouseEvent) {
        match event {
            MouseEvent::CursorMoved { position, .. } => {
                let pos = Pos2::new(position.x as f32, position.y as f32);
                self.pointer_pos = Some(pos);
                self.push_event(egui::Event::PointerMoved(pos));
            }
            MouseEvent::ButtonPressed { button, .. } => {
                window.focus();
                self.input.focused = true;
                if let (Some(pos), Some(button)) = (self.pointer_pos, pointer_button(button)) {
                    self.push_event(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    });
                }
            }
            MouseEvent::ButtonReleased { button, .. } => {
                if let (Some(pos), Some(button)) = (self.pointer_pos, pointer_button(button)) {
                    self.push_event(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    });
                }
            }
            MouseEvent::WheelScrolled { delta, .. } => {
                let delta = match delta {
                    ScrollDelta::Lines { x, y } => Vec2::new(x, y) * 24.0,
                    ScrollDelta::Pixels { x, y } => Vec2::new(x, y),
                };
                self.push_event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta,
                    modifiers: egui::Modifiers::default(),
                });
            }
            MouseEvent::CursorLeft => {
                self.pointer_pos = None;
                self.push_event(egui::Event::PointerGone);
            }
            MouseEvent::CursorEntered
            | MouseEvent::DragEntered { .. }
            | MouseEvent::DragMoved { .. }
            | MouseEvent::DragLeft
            | MouseEvent::DragDropped { .. } => {}
        }
    }

    fn handle_keyboard(&mut self, event: KeyboardEvent) {
        let modifiers = egui_modifiers(event.modifiers);
        self.input.modifiers = modifiers;
        let pressed = event.state == KeyState::Down;

        if pressed
            && !event.is_composing
            && !modifiers.command
            && !modifiers.ctrl
            && let Key::Character(text) = &event.key
            && is_text_input(text)
        {
            self.push_event(egui::Event::Text(text.clone()));
        }

        if let Some(key) = egui_key(&event.key) {
            self.push_event(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: event.repeat,
                modifiers,
            });
        }
    }
}

fn egui_modifiers(modifiers: Modifiers) -> egui::Modifiers {
    egui::Modifiers {
        alt: modifiers.alt(),
        ctrl: modifiers.ctrl(),
        shift: modifiers.shift(),
        mac_cmd: modifiers.meta(),
        command: if cfg!(target_os = "macos") {
            modifiers.meta()
        } else {
            modifiers.ctrl()
        },
    }
}

fn is_command_quit(event: &KeyboardEvent) -> bool {
    event.state == KeyState::Down
        && event.modifiers.meta()
        && matches!(&event.key, Key::Character(value) if value.eq_ignore_ascii_case("q"))
}

fn is_text_input(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn egui_key(key: &Key) -> Option<egui::Key> {
    Some(match key {
        Key::ArrowDown => egui::Key::ArrowDown,
        Key::ArrowLeft => egui::Key::ArrowLeft,
        Key::ArrowRight => egui::Key::ArrowRight,
        Key::ArrowUp => egui::Key::ArrowUp,
        Key::Escape => egui::Key::Escape,
        Key::Tab => egui::Key::Tab,
        Key::Backspace => egui::Key::Backspace,
        Key::Enter => egui::Key::Enter,
        Key::Delete => egui::Key::Delete,
        Key::Home => egui::Key::Home,
        Key::End => egui::Key::End,
        Key::PageUp => egui::Key::PageUp,
        Key::PageDown => egui::Key::PageDown,
        Key::Character(value) if value.len() == 1 => match value.as_bytes()[0] {
            b'0' => egui::Key::Num0,
            b'1' => egui::Key::Num1,
            b'2' => egui::Key::Num2,
            b'3' => egui::Key::Num3,
            b'4' => egui::Key::Num4,
            b'5' => egui::Key::Num5,
            b'6' => egui::Key::Num6,
            b'7' => egui::Key::Num7,
            b'8' => egui::Key::Num8,
            b'9' => egui::Key::Num9,
            b'a' | b'A' => egui::Key::A,
            b'c' | b'C' => egui::Key::C,
            b'v' | b'V' => egui::Key::V,
            b'x' | b'X' => egui::Key::X,
            b'z' | b'Z' => egui::Key::Z,
            _ => return None,
        },
        _ => return None,
    })
}

fn pointer_button(button: MouseButton) -> Option<egui::PointerButton> {
    match button {
        MouseButton::Left => Some(egui::PointerButton::Primary),
        MouseButton::Right => Some(egui::PointerButton::Secondary),
        MouseButton::Middle => Some(egui::PointerButton::Middle),
        MouseButton::Back => Some(egui::PointerButton::Extra1),
        MouseButton::Forward => Some(egui::PointerButton::Extra2),
        MouseButton::Other(_) => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clears_parented_egui_view_to_transparent() {
        assert_eq!(super::clear_color(), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn initializes_parented_egui_view_to_requested_size() {
        let info = super::initial_window_info(super::Size::new(640.0, 514.0));

        assert_eq!(info.logical_size(), super::Size::new(640.0, 514.0));
        assert_eq!(info.physical_size().width, 640);
        assert_eq!(info.physical_size().height, 514);
    }
}
