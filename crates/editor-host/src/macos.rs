use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSColor, NSView, NSWindow, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowOrderingMode, NSWindowStyleMask, NSWindowTabbingMode,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CALayer;
use raw_window_handle::RawWindowHandle;

use crate::{
    EditorFrame, EditorHostOptions, Error, InstalledHost, Size, WindowHandleSnapshot,
    WindowSnapshot, host_layout, open_egui_frame,
};

pub fn prepare_process() -> Result<(), Error> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(Error::Message(
            "editor host configuration must run on the main thread".to_string(),
        ));
    };
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
    Ok(())
}

pub fn install_editor_host(
    host: &WindowSnapshot,
    options: &EditorHostOptions,
    frame: impl EditorFrame + Send + 'static,
) -> Result<InstalledHost, Error> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(Error::Message(
            "editor host configuration must run on the main thread".to_string(),
        ));
    };

    let host_view = ns_view_from_snapshot(host)?;
    let host_window = host_view
        .window()
        .ok_or_else(|| Error::Message("host window is missing".to_string()))?;

    configure_window(&host_window, options, mtm);
    if let Some(owner) = options.owner {
        attach_child_window_to_owner(&host_window, &owner)?;
    }

    let requested_bounds = host_view.bounds();
    let layout = frame.layout(Size {
        width: requested_bounds.size.width,
        height: requested_bounds.size.height,
    });

    resize_host_window_from_top_clamped_to_screen(
        &host_window,
        layout.outer_width,
        layout.outer_height,
    );
    host_view.setFrameSize(NSSize::new(layout.outer_width, layout.outer_height));
    host_window.setOpaque(false);
    host_window.setBackgroundColor(Some(&NSColor::clearColor()));
    style_host_view(&host_view);

    let frame_host = open_egui_frame(
        *host,
        options,
        Size {
            width: requested_bounds.size.width,
            height: requested_bounds.size.height,
        },
        layout.outer_width,
        layout.outer_height,
        frame,
    )?;

    let content_frame = content_frame_for_host(layout, host_view.isFlipped());
    let content = build_content_view(content_frame, mtm);
    host_view.addSubview(&content);
    let content = WindowSnapshot::capture(
        RawWindowHandle::AppKit(raw_window_handle::AppKitWindowHandle::new(
            NonNull::new(Retained::as_ptr(&content) as *mut std::ffi::c_void)
                .expect("retained appkit content view pointer is non-null"),
        )),
        host.display.map(|_| {
            raw_window_handle::RawDisplayHandle::AppKit(
                raw_window_handle::AppKitDisplayHandle::new(),
            )
        }),
    )?;

    Ok(InstalledHost {
        host: *host,
        content,
        close_requested: frame_host.close_requested,
        title: frame_host.title,
        preset_state: frame_host.preset_state,
        frame_content_size: frame_host.content_size,
        frame_commands: frame_host.frame_commands,
        frame_window: Some(frame_host.window),
        content_size: Size {
            width: requested_bounds.size.width,
            height: requested_bounds.size.height,
        },
        frame_thickness: layout.content.x,
    })
}

pub fn resize_installed_host(
    host: &WindowSnapshot,
    content: &WindowSnapshot,
    content_size: Size,
    titlebar_height: f64,
    frame_thickness: f64,
) -> Result<(), Error> {
    let layout = host_layout(
        content_size.width,
        content_size.height,
        titlebar_height,
        frame_thickness,
    );
    let host_view = ns_view_from_snapshot(host)?;
    let host_window = host_view
        .window()
        .ok_or_else(|| Error::Message("host window is missing".to_string()))?;
    resize_host_window_from_bottom_clamped_to_screen(
        &host_window,
        layout.outer_width,
        layout.outer_height,
    );
    host_view.setFrameSize(NSSize::new(layout.outer_width, layout.outer_height));

    let content_view = ns_view_from_snapshot(content)?;
    content_view.setFrame(ns_rect(content_frame_for_host(
        layout,
        host_view.isFlipped(),
    )));
    Ok(())
}

pub fn set_host_window_visible(host: &WindowSnapshot, visible: bool) -> Result<(), Error> {
    let host_view = ns_view_from_snapshot(host)?;
    let host_window = host_view
        .window()
        .ok_or_else(|| Error::Message("host window is missing".to_string()))?;

    if visible {
        host_window.makeKeyAndOrderFront(None);
    } else {
        host_window.orderOut(None);
    }

    Ok(())
}

pub fn set_host_window_title(host: &WindowSnapshot, title: &str) -> Result<(), Error> {
    let host_view = ns_view_from_snapshot(host)?;
    let host_window = host_view
        .window()
        .ok_or_else(|| Error::Message("host window is missing".to_string()))?;
    host_window.setTitle(&NSString::from_str(title));
    Ok(())
}

pub fn raise_host_window(host: &WindowSnapshot) -> Result<(), Error> {
    let host_view = ns_view_from_snapshot(host)?;
    let host_window = host_view
        .window()
        .ok_or_else(|| Error::Message("host window is missing".to_string()))?;

    if let Some(parent_window) = host_window.parentWindow() {
        parent_window.removeChildWindow(&host_window);
        // SAFETY: Both windows are live AppKit windows on the main thread. Re-adding the editor
        // child above the app window updates sibling editor stacking order.
        unsafe {
            parent_window.addChildWindow_ordered(&host_window, NSWindowOrderingMode::Above);
        }
    }
    host_window.makeKeyAndOrderFront(None);
    Ok(())
}

fn attach_child_window_to_owner(
    child_window: &NSWindow,
    owner: &WindowSnapshot,
) -> Result<(), Error> {
    let owner_view = ns_view_from_snapshot(owner)?;
    let owner_window = owner_view
        .window()
        .ok_or_else(|| Error::Message("owner window is missing".to_string()))?;

    // SAFETY: Both windows are live AppKit windows on the main thread. AppKit uses this
    // relationship to keep auxiliary editor windows above and with the owning app window.
    unsafe {
        owner_window.addChildWindow_ordered(child_window, NSWindowOrderingMode::Above);
    }
    Ok(())
}

pub fn route_app_quit_to_window_close(window: &WindowSnapshot) -> Result<(), Error> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(Error::Message(
            "app quit routing must run on the main thread".to_string(),
        ));
    };
    let view = ns_view_from_snapshot(window)?;
    let window = view
        .window()
        .ok_or_else(|| Error::Message("main window is missing".to_string()))?;
    let app = NSApplication::sharedApplication(mtm);
    let Some(main_menu) = app.mainMenu() else {
        return Err(Error::Message("app menu is missing".to_string()));
    };
    let Some(app_menu_item) = main_menu.itemAtIndex(0) else {
        return Err(Error::Message("app menu item is missing".to_string()));
    };
    let Some(app_menu) = app_menu_item.submenu() else {
        return Err(Error::Message("app submenu is missing".to_string()));
    };
    let item_count = app_menu.numberOfItems();
    if item_count == 0 {
        return Err(Error::Message("app submenu is empty".to_string()));
    }
    let Some(quit_item) = app_menu.itemAtIndex(item_count - 1) else {
        return Err(Error::Message("quit menu item is missing".to_string()));
    };

    // SAFETY: The menu item and main window are live AppKit objects on the main thread.
    unsafe {
        quit_item.setTarget(Some(&window));
        quit_item.setAction(Some(sel!(performClose:)));
    }

    Ok(())
}

fn ns_view_from_snapshot(snapshot: &WindowSnapshot) -> Result<Retained<NSView>, Error> {
    let WindowHandleSnapshot::AppKit { ns_view } = snapshot.window else {
        return Err(Error::Message(
            "auxiliary editor host currently requires an AppKit view".to_string(),
        ));
    };

    // SAFETY: The snapshot stores an AppKit NSView pointer captured from a live window handle on
    // the main thread. We only use it immediately on the main thread to configure that window.
    let view = (ns_view as *mut AnyObject).cast::<NSView>();
    if view.is_null() {
        return Err(Error::Message("host AppKit view is null".to_string()));
    }
    // SAFETY: The pointer came from a live AppKit window handle and is still owned by that window.
    unsafe { Retained::retain(view) }
        .ok_or_else(|| Error::Message("host AppKit view is unavailable".to_string()))
}

fn configure_window(window: &NSWindow, options: &EditorHostOptions, mtm: MainThreadMarker) {
    let mut style_mask = window.styleMask();
    style_mask.insert(NSWindowStyleMask::Closable);
    style_mask.remove(NSWindowStyleMask::Miniaturizable);
    // Resizable editors use the app-drawn bottom-right grip. Native borderless resize zones resize
    // from any edge and fight plugin resize negotiation.
    style_mask.remove(NSWindowStyleMask::Resizable);

    let collection_behavior = window.collectionBehavior()
        | NSWindowCollectionBehavior::Auxiliary
        | NSWindowCollectionBehavior::FullScreenAuxiliary
        | NSWindowCollectionBehavior::FullScreenDisallowsTiling;

    window.setStyleMask(style_mask);
    window.setTitle(&NSString::from_str(&options.title));
    window.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
    window.setHasShadow(true);
    window.setMovableByWindowBackground(true);
    window.setAnimationBehavior(NSWindowAnimationBehavior::UtilityWindow);
    window.setCollectionBehavior(collection_behavior);
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    window.invalidateShadow();
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
}

pub fn begin_host_window_drag(host: &WindowSnapshot) -> Result<(), Error> {
    let host_view = ns_view_from_snapshot(host)?;
    let host_window = host_view
        .window()
        .ok_or_else(|| Error::Message("host window is missing".to_string()))?;
    let event = host_window
        .currentEvent()
        .ok_or_else(|| Error::Message("host window drag event is missing".to_string()))?;

    host_window.performWindowDragWithEvent(&event);
    host_window.invalidateShadow();
    Ok(())
}

fn resize_host_window_from_top_clamped_to_screen(window: &NSWindow, width: f64, height: f64) {
    let frame = host_window_frame_resized_from_top(window.frame(), width, height);
    let frame = window
        .screen()
        .map(|screen| host_window_frame_clamped_to_visible_top(frame, screen.visibleFrame()))
        .unwrap_or(frame);
    window.setFrame_display(frame, false);
}

fn resize_host_window_from_bottom_clamped_to_screen(window: &NSWindow, width: f64, height: f64) {
    let frame = host_window_frame_resized_from_bottom(window.frame(), width, height);
    let frame = window
        .screen()
        .map(|screen| host_window_frame_clamped_to_visible_top(frame, screen.visibleFrame()))
        .unwrap_or(frame);
    window.setFrame_display(frame, false);
}

fn host_window_frame_resized_from_top(mut frame: NSRect, width: f64, height: f64) -> NSRect {
    let delta_height = height - frame.size.height;
    frame.origin.y -= delta_height;
    frame.size = NSSize::new(width, height);
    frame
}

fn host_window_frame_resized_from_bottom(mut frame: NSRect, width: f64, height: f64) -> NSRect {
    frame.size = NSSize::new(width, height);
    frame
}

#[cfg(test)]
fn host_window_frame_resized_from_bottom_clamped(
    frame: NSRect,
    width: f64,
    height: f64,
    visible: NSRect,
) -> NSRect {
    host_window_frame_clamped_to_visible_top(
        host_window_frame_resized_from_bottom(frame, width, height),
        visible,
    )
}

fn host_window_frame_clamped_to_visible_top(mut frame: NSRect, visible: NSRect) -> NSRect {
    let top = frame.origin.y + frame.size.height;
    let visible_top = visible.origin.y + visible.size.height;
    if top > visible_top {
        frame.origin.y = visible_top - frame.size.height;
    }
    frame
}

fn build_content_view(frame: crate::Rect, mtm: MainThreadMarker) -> Retained<NSView> {
    let content = NSView::initWithFrame(NSView::alloc(mtm), ns_rect(frame));
    content.setAutoresizingMask(content_view_autoresizing_mask());
    style_view(
        &content,
        &NSColor::clearColor(),
        None,
        0.0,
        0.0,
        content_view_masks_to_bounds(),
    );
    content
}

fn content_view_autoresizing_mask() -> NSAutoresizingMaskOptions {
    NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable
}

fn content_view_masks_to_bounds() -> bool {
    false
}

fn content_frame_for_host(layout: crate::EditorFrameLayout, host_is_flipped: bool) -> crate::Rect {
    if !host_is_flipped {
        return layout.content;
    }

    crate::Rect {
        y: layout.outer_height - layout.content.y - layout.content.height,
        ..layout.content
    }
}

fn style_host_view(view: &NSView) {
    style_view(view, &NSColor::clearColor(), None, 0.0, 0.0, false);
}

fn style_view(
    view: &NSView,
    background: &NSColor,
    border: Option<&NSColor>,
    corner_radius: f64,
    border_width: f64,
    masks_to_bounds: bool,
) {
    view.setWantsLayer(true);
    let layer = CALayer::layer();
    let background = background.CGColor();
    layer.setBackgroundColor(Some(&background));
    if let Some(border) = border {
        let border = border.CGColor();
        layer.setBorderColor(Some(&border));
    }
    layer.setBorderWidth(border_width);
    layer.setCornerRadius(corner_radius);
    layer.setMasksToBounds(masks_to_bounds || corner_radius > 0.0);
    view.setLayer(Some(&layer));
}

fn ns_rect(frame: crate::Rect) -> NSRect {
    NSRect::new(
        NSPoint::new(frame.x, frame.y),
        NSSize::new(frame.width, frame.height),
    )
}

#[cfg(test)]
mod tests {
    use crate::{Rect, host_layout};
    use objc2_app_kit::NSAutoresizingMaskOptions;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    use super::{
        content_frame_for_host, content_view_autoresizing_mask, content_view_masks_to_bounds,
        host_window_frame_resized_from_bottom, host_window_frame_resized_from_bottom_clamped,
        host_window_frame_resized_from_top,
    };

    #[test]
    fn content_frame_uses_bottom_left_coordinates_for_normal_appkit_views() {
        let layout = host_layout(820.0, 456.0, 30.0, 4.0);

        assert_eq!(
            content_frame_for_host(layout, false),
            Rect {
                x: 4.0,
                y: 4.0,
                width: 820.0,
                height: 456.0,
            }
        );
    }

    #[test]
    fn content_frame_moves_below_titlebar_for_flipped_appkit_views() {
        let layout = host_layout(820.0, 456.0, 30.0, 4.0);

        assert_eq!(
            content_frame_for_host(layout, true),
            Rect {
                x: 4.0,
                y: 34.0,
                width: 820.0,
                height: 456.0,
            }
        );
    }

    #[test]
    fn live_resize_grows_host_upward_to_keep_content_position() {
        let frame = NSRect::new(NSPoint::new(100.0, 200.0), NSSize::new(440.0, 400.0));

        let resized = host_window_frame_resized_from_bottom(frame, 440.0, 500.0);

        assert_eq!(resized.origin.y, 200.0);
        assert_eq!(resized.size.height, 500.0);
    }

    #[test]
    fn live_resize_clamps_header_top_to_visible_screen() {
        let frame = NSRect::new(NSPoint::new(100.0, 200.0), NSSize::new(440.0, 400.0));
        let visible = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0));

        let resized = host_window_frame_resized_from_bottom_clamped(frame, 440.0, 1000.0, visible);

        assert_eq!(resized.origin.y + resized.size.height, 900.0);
    }

    #[test]
    fn content_host_view_autoresizes_with_plugin_owned_resize() {
        let mask = content_view_autoresizing_mask();

        assert!(mask.contains(NSAutoresizingMaskOptions::ViewWidthSizable));
        assert!(mask.contains(NSAutoresizingMaskOptions::ViewHeightSizable));
    }

    #[test]
    fn content_host_view_does_not_clip_plugin_owned_resize() {
        assert!(!content_view_masks_to_bounds());
    }

    #[test]
    fn initial_resize_keeps_existing_top_anchor() {
        let frame = NSRect::new(NSPoint::new(100.0, 200.0), NSSize::new(440.0, 400.0));

        let resized = host_window_frame_resized_from_top(frame, 440.0, 500.0);

        assert_eq!(resized.origin.y, 100.0);
        assert_eq!(resized.size.height, 500.0);
    }
}
