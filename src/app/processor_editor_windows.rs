use std::collections::HashMap;

use editor_host::{EditorFrameCommand, EditorPresetState, InstalledHost, WindowSnapshot};
use iced::window;
use lilypalooza_audio::{EditorError, EditorParent, EditorSession, EditorSize};

/// Mixer-strip processor target.
///
/// `strip_index` follows the visible mixer strip order:
/// - `0` is the master strip
/// - `1..=track_count` are instrument tracks
/// - the remaining indices are bus strips
///
/// `slot_index` follows one shared convention on every strip:
/// - `0` is the instrument slot
/// - `1..` are effect slots
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct EditorTarget {
    pub(super) strip_index: usize,
    pub(super) slot_index: usize,
}

pub(super) struct EditorWindow {
    pub(super) title: String,
    pub(super) resizable: bool,
    pub(super) host_window_id: window::Id,
    pub(super) host: Option<InstalledHost>,
    pub(super) session: Box<dyn EditorSession>,
    pub(super) visible: bool,
}

pub(super) struct PendingEditorWindow {
    pub(super) target: EditorTarget,
    pub(super) title: String,
    pub(super) resizable: bool,
    pub(super) host_window_id: window::Id,
    pub(super) session: Box<dyn EditorSession>,
}

#[derive(Default)]
pub(super) struct EditorWindowManager {
    windows: HashMap<EditorTarget, EditorWindow>,
    pending: HashMap<window::Id, PendingEditorWindow>,
    windows_by_id: HashMap<window::Id, EditorTarget>,
    focused: Option<EditorTarget>,
}

pub(super) fn snapshot_into_editor_parent(
    snapshot: WindowSnapshot,
) -> Result<EditorParent, String> {
    let window = snapshot
        .raw_window_handle()
        .map_err(|error| error.to_string())?;
    let display = snapshot
        .raw_display_handle()
        .map_err(|error| error.to_string())?;
    Ok(EditorParent { window, display })
}

fn editor_size_from_host_size(size: editor_host::Size) -> EditorSize {
    EditorSize {
        width: size.width.round().max(1.0) as u32,
        height: size.height.round().max(1.0) as u32,
    }
}

fn host_size_from_editor_size(size: EditorSize) -> editor_host::Size {
    editor_host::Size {
        width: f64::from(size.width),
        height: f64::from(size.height),
    }
}

fn negotiate_editor_content_resize(
    session: &mut dyn EditorSession,
    requested: editor_host::Size,
) -> Result<editor_host::Size, EditorError> {
    session
        .resize(editor_size_from_host_size(requested))
        .map(host_size_from_editor_size)
}

impl EditorWindowManager {
    pub(super) fn focus_existing(&mut self, target: EditorTarget) -> Option<window::Id> {
        if let Some(window) = self.windows.get(&target) {
            self.focused = Some(target);
            return Some(window.host_window_id);
        }
        if let Some((window_id, _)) = self
            .pending
            .iter()
            .find(|(_, window)| window.target == target)
        {
            self.focused = Some(target);
            return Some(*window_id);
        }
        None
    }

    pub(super) fn begin_open(
        &mut self,
        target: EditorTarget,
        title: String,
        resizable: bool,
        session: Box<dyn EditorSession>,
        window_id: window::Id,
    ) {
        self.pending.insert(
            window_id,
            PendingEditorWindow {
                target,
                title,
                resizable,
                host_window_id: window_id,
                session,
            },
        );
        self.focused = Some(target);
    }

    pub(super) fn attach(
        &mut self,
        window_id: window::Id,
        mut host: Option<InstalledHost>,
        parent: EditorParent,
    ) -> Result<(), EditorError> {
        let Some(mut pending) = self.pending.remove(&window_id) else {
            return Err(EditorError::HostUnavailable(format!(
                "pending editor window `{window_id:?}` is missing"
            )));
        };
        pending.session.attach(parent)?;
        if let Some(host) = host.as_mut()
            && let Ok(Some(size)) = pending.session.initial_size()
        {
            let _ = host.resize_content(editor_host::Size {
                width: f64::from(size.width),
                height: f64::from(size.height),
            });
        }
        self.focused = Some(pending.target);
        self.windows.insert(
            pending.target,
            EditorWindow {
                title: pending.title,
                resizable: pending.resizable,
                host_window_id: pending.host_window_id,
                host,
                session: pending.session,
                visible: true,
            },
        );
        self.windows_by_id.insert(window_id, pending.target);
        Ok(())
    }

    pub(super) fn pending_contains(&self, window_id: window::Id) -> bool {
        self.pending.contains_key(&window_id)
    }

    pub(super) fn window_title(&self, window_id: window::Id) -> Option<&str> {
        self.windows_by_id
            .get(&window_id)
            .and_then(|target| self.windows.get(target).map(|window| window.title.as_str()))
            .or_else(|| {
                self.pending
                    .get(&window_id)
                    .map(|window| window.title.as_str())
            })
    }

    pub(super) fn window_resizable(&self, window_id: window::Id) -> Option<bool> {
        self.windows_by_id
            .get(&window_id)
            .and_then(|target| self.windows.get(target).map(|window| window.resizable))
            .or_else(|| self.pending.get(&window_id).map(|window| window.resizable))
    }

    pub(super) fn target_for_window(&self, window_id: window::Id) -> Option<EditorTarget> {
        self.windows_by_id.get(&window_id).copied()
    }

    pub(super) fn window_for_target(&self, target: EditorTarget) -> Option<window::Id> {
        self.windows
            .get(&target)
            .map(|window| window.host_window_id)
            .or_else(|| {
                self.pending.iter().find_map(|(window_id, pending)| {
                    (pending.target == target).then_some(*window_id)
                })
            })
    }

    pub(super) fn focus_window(&mut self, window_id: window::Id) -> Vec<String> {
        let Some(target) = self.windows_by_id.get(&window_id).copied() else {
            return Vec::new();
        };
        let Some(window) = self.windows.get_mut(&target) else {
            return Vec::new();
        };
        self.focused = Some(target);
        let mut errors = Vec::new();
        if let Some(host) = window.host.as_mut()
            && let Err(error) = host.raise()
        {
            errors.push(error.to_string());
        }
        errors
    }

    pub(super) fn remove_window(
        &mut self,
        window_id: window::Id,
    ) -> Option<(EditorTarget, Box<dyn EditorSession>)> {
        if let Some(pending) = self.pending.remove(&window_id) {
            if self.focused == Some(pending.target) {
                self.focused = None;
            }
            return Some((pending.target, pending.session));
        }

        let target = self.windows_by_id.remove(&window_id)?;
        let window = self.windows.remove(&target)?;
        if self.focused == Some(target) {
            self.focused = None;
        }
        Some((target, window.session))
    }

    pub(super) fn remove_target(
        &mut self,
        target: EditorTarget,
    ) -> Option<(window::Id, Box<dyn EditorSession>)> {
        if let Some(window) = self.windows.remove(&target) {
            self.windows_by_id.remove(&window.host_window_id);
            if self.focused == Some(target) {
                self.focused = None;
            }
            return Some((window.host_window_id, window.session));
        }
        let window_id = self
            .pending
            .iter()
            .find_map(|(window_id, pending)| (pending.target == target).then_some(*window_id))?;
        let pending = self.pending.remove(&window_id)?;
        if self.focused == Some(target) {
            self.focused = None;
        }
        Some((pending.host_window_id, pending.session))
    }

    pub(super) fn shift_targets_after_removed_strip(&mut self, removed_strip_index: usize) {
        let targets_to_shift = self
            .windows
            .keys()
            .copied()
            .filter(|target| target.strip_index > removed_strip_index)
            .collect::<Vec<_>>();
        for target in targets_to_shift {
            if let Some(window) = self.windows.remove(&target) {
                let shifted = EditorTarget {
                    strip_index: target.strip_index - 1,
                    slot_index: target.slot_index,
                };
                self.windows_by_id.insert(window.host_window_id, shifted);
                self.windows.insert(shifted, window);
            }
        }

        for pending in self.pending.values_mut() {
            if pending.target.strip_index > removed_strip_index {
                pending.target.strip_index -= 1;
            }
        }

        if let Some(target) = self.focused
            && target.strip_index > removed_strip_index
        {
            self.focused = Some(EditorTarget {
                strip_index: target.strip_index - 1,
                slot_index: target.slot_index,
            });
        }
    }

    pub(super) fn move_slot_targets_within_strip(
        &mut self,
        strip_index: usize,
        from_slot_index: usize,
        to_slot_index: usize,
    ) {
        if from_slot_index == to_slot_index {
            return;
        }

        let shift = |target: EditorTarget| -> EditorTarget {
            if target.strip_index != strip_index {
                return target;
            }
            let slot_index = if target.slot_index == from_slot_index {
                to_slot_index
            } else if from_slot_index < to_slot_index
                && target.slot_index > from_slot_index
                && target.slot_index <= to_slot_index
            {
                target.slot_index - 1
            } else if from_slot_index > to_slot_index
                && target.slot_index >= to_slot_index
                && target.slot_index < from_slot_index
            {
                target.slot_index + 1
            } else {
                target.slot_index
            };
            EditorTarget {
                slot_index,
                ..target
            }
        };

        let moved_windows = self.windows.drain().collect::<Vec<_>>();
        self.windows_by_id.clear();
        for (target, window) in moved_windows {
            let moved = shift(target);
            self.windows_by_id.insert(window.host_window_id, moved);
            self.windows.insert(moved, window);
        }

        for pending in self.pending.values_mut() {
            pending.target = shift(pending.target);
        }

        if let Some(target) = self.focused {
            self.focused = Some(shift(target));
        }
    }

    pub(super) fn remove_all_windows(&mut self) -> Vec<(window::Id, Box<dyn EditorSession>)> {
        let windows = self
            .windows
            .drain()
            .map(|(target, window)| {
                if self.focused == Some(target) {
                    self.focused = None;
                }
                (window.host_window_id, window.session)
            })
            .collect::<Vec<_>>();
        self.windows_by_id.clear();
        self.pending.clear();
        windows
    }

    pub(super) fn hide_window(
        &mut self,
        window_id: window::Id,
    ) -> Option<(EditorTarget, Vec<String>)> {
        let target = *self.windows_by_id.get(&window_id)?;
        let window = self.windows.get_mut(&target)?;
        let mut errors = Vec::new();
        if self.focused == Some(target) {
            self.focused = None;
        }
        window.visible = false;
        if let Err(error) = window.session.set_visible(false) {
            errors.push(error.to_string());
        }
        if let Some(host) = window.host.as_mut()
            && let Err(error) = host.set_visible(false)
        {
            errors.push(error.to_string());
        } else if let Some(host) = window.host.as_ref() {
            host.clear_close_requested();
        }
        Some((target, errors))
    }

    pub(super) fn hide_all_windows(&mut self) -> Vec<Vec<String>> {
        self.focused = None;
        self.windows
            .values_mut()
            .map(|window| {
                let mut errors = Vec::new();
                window.visible = false;
                if let Err(error) = window.session.set_visible(false) {
                    errors.push(error.to_string());
                }
                if let Some(host) = window.host.as_mut()
                    && let Err(error) = host.set_visible(false)
                {
                    errors.push(error.to_string());
                }
                errors
            })
            .collect()
    }

    pub(super) fn show_window(&mut self, window_id: window::Id) -> Vec<String> {
        let Some(target) = self.windows_by_id.get(&window_id).copied() else {
            return Vec::new();
        };
        let Some(window) = self.windows.get_mut(&target) else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        if let Some(host) = window.host.as_mut() {
            host.clear_close_requested();
            if let Err(error) = host.set_visible(true) {
                errors.push(error.to_string());
            }
        }
        if let Err(error) = window.session.set_visible(true) {
            errors.push(error.to_string());
        }
        window.visible = true;
        self.focused = Some(target);
        errors
    }

    pub(super) fn window_visible(&self, window_id: window::Id) -> bool {
        self.windows_by_id
            .get(&window_id)
            .and_then(|target| self.windows.get(target))
            .is_some_and(|window| window.visible)
    }

    pub(super) fn targets_for_strip(&self, strip_index: usize) -> Vec<EditorTarget> {
        self.windows
            .keys()
            .chain(self.pending.values().map(|window| &window.target))
            .filter(|target| target.strip_index == strip_index)
            .copied()
            .collect()
    }

    pub(super) fn set_window_title(&mut self, target: EditorTarget, title: String) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(window) = self.windows.get_mut(&target) {
            window.title.clone_from(&title);
            if let Some(host) = window.host.as_mut()
                && let Err(error) = host.set_title(title.clone())
            {
                errors.push(error.to_string());
            }
        }
        for pending in self
            .pending
            .values_mut()
            .filter(|pending| pending.target == target)
        {
            pending.title.clone_from(&title);
        }
        errors
    }

    pub(super) fn set_preset_state(
        &mut self,
        target: EditorTarget,
        state: Option<EditorPresetState>,
    ) {
        if let Some(window) = self.windows.get_mut(&target)
            && let Some(host) = window.host.as_mut()
        {
            host.set_preset_state(state);
        }
    }

    pub(super) fn preset_state(&self, target: EditorTarget) -> Option<EditorPresetState> {
        self.windows
            .get(&target)
            .and_then(|window| window.host.as_ref())
            .and_then(InstalledHost::preset_state)
    }

    pub(super) fn drain_frame_commands(&mut self) -> Vec<(EditorTarget, EditorFrameCommand)> {
        let mut commands = Vec::new();
        for (target, window) in &mut self.windows {
            let Some(host) = window.host.as_mut() else {
                continue;
            };
            commands.extend(
                host.drain_frame_commands()
                    .into_iter()
                    .map(|command| (*target, command)),
            );
        }
        commands
    }

    pub(super) fn apply_requested_content_resizes(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        for window in self.windows.values_mut() {
            match window.session.requested_size() {
                Ok(Some(size)) => {
                    if let Some(host) = window.host.as_mut()
                        && let Err(error) = host.resize_content(editor_host::Size {
                            width: f64::from(size.width),
                            height: f64::from(size.height),
                        })
                    {
                        errors.push(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    errors.push(error.to_string());
                }
            };
        }
        errors
    }

    pub(super) fn resize_window(&mut self, window_id: window::Id, size: iced::Size) -> Vec<String> {
        let Some(target) = self.windows_by_id.get(&window_id).copied() else {
            return Vec::new();
        };
        let Some(window) = self.windows.get_mut(&target) else {
            return Vec::new();
        };
        if !window.resizable {
            return Vec::new();
        }
        let Some(host) = window.host.as_mut() else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        let requested_content_size = host.content_size_from_outer_size(editor_host::Size {
            width: f64::from(size.width),
            height: f64::from(size.height),
        });
        match negotiate_editor_content_resize(window.session.as_mut(), requested_content_size) {
            Ok(content_size) => {
                if let Err(error) = host.resize_content(content_size) {
                    errors.push(error.to_string());
                }
            }
            Err(error) => errors.push(error.to_string()),
        }
        errors
    }

    pub(super) fn resize_target_content(
        &mut self,
        target: EditorTarget,
        requested_content_size: editor_host::Size,
    ) -> Vec<String> {
        let Some(window) = self.windows.get_mut(&target) else {
            return Vec::new();
        };
        if !window.resizable {
            return Vec::new();
        }
        let Some(host) = window.host.as_mut() else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        match negotiate_editor_content_resize(window.session.as_mut(), requested_content_size) {
            Ok(content_size) => {
                if let Err(error) = host.resize_content(content_size) {
                    errors.push(error.to_string());
                }
            }
            Err(error) => errors.push(error.to_string()),
        }
        errors
    }

    pub(super) fn close_requested_windows(&self) -> Vec<window::Id> {
        self.windows
            .values()
            .filter(|window| {
                window
                    .host
                    .as_ref()
                    .is_some_and(InstalledHost::close_requested)
            })
            .map(|window| window.host_window_id)
            .collect()
    }

    pub(super) fn has_installed_hosts(&self) -> bool {
        self.windows.values().any(|window| window.host.is_some())
    }
}

#[cfg(test)]
impl EditorWindowManager {
    pub(super) fn contains_window(&self, target: EditorTarget) -> bool {
        self.windows.contains_key(&target)
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use editor_host::WindowSnapshot;
    use iced::window;
    use lilypalooza_audio::{EditorError, EditorParent, EditorSession, EditorSize};

    use super::{EditorTarget, EditorWindowManager, snapshot_into_editor_parent};

    struct FakeEditorSession;
    struct RequestedSizeEditorSession {
        calls: Arc<AtomicUsize>,
    }
    struct AdjustingResizeEditorSession {
        requested: Arc<std::sync::Mutex<Vec<EditorSize>>>,
        accepted: EditorSize,
    }

    impl EditorSession for FakeEditorSession {
        fn attach(&mut self, _parent: EditorParent) -> Result<(), EditorError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), EditorError> {
            Ok(())
        }

        fn set_visible(&mut self, _visible: bool) -> Result<(), EditorError> {
            Ok(())
        }

        fn resize(&mut self, size: EditorSize) -> Result<EditorSize, EditorError> {
            Ok(size)
        }
    }

    impl EditorSession for RequestedSizeEditorSession {
        fn requested_size(&mut self) -> Result<Option<EditorSize>, EditorError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(Some(EditorSize {
                width: 640,
                height: 480,
            }))
        }

        fn attach(&mut self, _parent: EditorParent) -> Result<(), EditorError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), EditorError> {
            Ok(())
        }

        fn set_visible(&mut self, _visible: bool) -> Result<(), EditorError> {
            Ok(())
        }

        fn resize(&mut self, size: EditorSize) -> Result<EditorSize, EditorError> {
            Ok(size)
        }
    }

    impl EditorSession for AdjustingResizeEditorSession {
        fn attach(&mut self, _parent: EditorParent) -> Result<(), EditorError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), EditorError> {
            Ok(())
        }

        fn set_visible(&mut self, _visible: bool) -> Result<(), EditorError> {
            Ok(())
        }

        fn resize(&mut self, size: EditorSize) -> Result<EditorSize, EditorError> {
            self.requested
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(size);
            Ok(self.accepted)
        }
    }

    #[test]
    fn processor_editor_window_manager_reuses_existing_target_window() {
        let mut manager = EditorWindowManager::default();
        let target = EditorTarget {
            strip_index: 3,
            slot_index: 0,
        };

        let first_id = window::Id::unique();
        manager.begin_open(
            target,
            "Track 4".to_string(),
            true,
            Box::new(FakeEditorSession),
            first_id,
        );
        let second_token = manager.focus_existing(target);

        assert_eq!(Some(first_id), second_token);
        assert_eq!(manager.focused, Some(target));
    }

    #[test]
    fn processor_editor_window_manager_attaches_pending_session_once_parent_arrives() {
        let mut manager = EditorWindowManager::default();
        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };

        let window_id = window::Id::unique();
        manager.begin_open(
            target,
            "Track 1".to_string(),
            true,
            Box::new(FakeEditorSession),
            window_id,
        );

        manager
            .attach(
                window_id,
                None,
                EditorParent {
                    window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                        iced::window::raw_window_handle::AppKitWindowHandle::new(
                            std::ptr::NonNull::<std::ffi::c_void>::dangling(),
                        ),
                    ),
                    display: None,
                },
            )
            .expect("attach should succeed");
        assert!(manager.windows.contains_key(&target));
        assert!(manager.window_visible(window_id));
    }

    #[test]
    fn processor_editor_window_manager_tracks_visibility_for_toggle() {
        let mut manager = EditorWindowManager::default();
        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let window_id = window::Id::unique();
        manager.begin_open(
            target,
            "Track 1".to_string(),
            true,
            Box::new(FakeEditorSession),
            window_id,
        );
        manager
            .attach(
                window_id,
                None,
                EditorParent {
                    window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                        iced::window::raw_window_handle::AppKitWindowHandle::new(
                            std::ptr::NonNull::<std::ffi::c_void>::dangling(),
                        ),
                    ),
                    display: None,
                },
            )
            .expect("attach should succeed");

        assert!(manager.window_visible(window_id));
        manager.hide_window(window_id).expect("window should hide");
        assert!(!manager.window_visible(window_id));
        assert!(manager.show_window(window_id).is_empty());
        assert!(manager.window_visible(window_id));
    }

    #[test]
    fn processor_editor_window_manager_polls_requested_editor_resize() {
        let mut manager = EditorWindowManager::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let window_id = window::Id::unique();
        manager.begin_open(
            EditorTarget {
                strip_index: 1,
                slot_index: 0,
            },
            "Track 1".to_string(),
            true,
            Box::new(RequestedSizeEditorSession {
                calls: Arc::clone(&calls),
            }),
            window_id,
        );
        manager
            .attach(
                window_id,
                None,
                EditorParent {
                    window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                        iced::window::raw_window_handle::AppKitWindowHandle::new(
                            std::ptr::NonNull::<std::ffi::c_void>::dangling(),
                        ),
                    ),
                    display: None,
                },
            )
            .expect("attach should succeed");

        assert!(manager.apply_requested_content_resizes().is_empty());
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn processor_editor_resize_negotiation_uses_session_accepted_size() {
        let requested = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut session = AdjustingResizeEditorSession {
            requested: Arc::clone(&requested),
            accepted: EditorSize {
                width: 512,
                height: 384,
            },
        };

        let accepted = super::negotiate_editor_content_resize(
            &mut session,
            editor_host::Size {
                width: 640.0,
                height: 480.0,
            },
        )
        .expect("resize should be accepted");

        assert_eq!(
            *requested
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![EditorSize {
                width: 640,
                height: 480,
            }]
        );
        assert_eq!(
            accepted,
            editor_host::Size {
                width: 512.0,
                height: 384.0,
            }
        );
    }

    #[test]
    fn processor_editor_resize_command_is_ignored_for_fixed_size_window() {
        let mut manager = EditorWindowManager::default();
        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let window_id = window::Id::unique();
        manager.begin_open(
            target,
            "Track 1".to_string(),
            false,
            Box::new(FakeEditorSession),
            window_id,
        );
        manager
            .attach(
                window_id,
                None,
                EditorParent {
                    window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                        iced::window::raw_window_handle::AppKitWindowHandle::new(
                            std::ptr::NonNull::<std::ffi::c_void>::dangling(),
                        ),
                    ),
                    display: None,
                },
            )
            .expect("attach should succeed");

        assert!(
            manager
                .resize_target_content(
                    target,
                    editor_host::Size {
                        width: 640.0,
                        height: 480.0,
                    },
                )
                .is_empty()
        );
    }

    #[test]
    fn processor_editor_window_manager_updates_titles_for_open_and_pending_windows() {
        let mut manager = EditorWindowManager::default();
        let open_target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let pending_target = EditorTarget {
            strip_index: 1,
            slot_index: 1,
        };
        let open_id = window::Id::unique();
        let pending_id = window::Id::unique();
        manager.begin_open(
            open_target,
            "Old".to_string(),
            true,
            Box::new(FakeEditorSession),
            open_id,
        );
        manager
            .attach(
                open_id,
                None,
                EditorParent {
                    window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                        iced::window::raw_window_handle::AppKitWindowHandle::new(
                            std::ptr::NonNull::<std::ffi::c_void>::dangling(),
                        ),
                    ),
                    display: None,
                },
            )
            .expect("attach should succeed");
        manager.begin_open(
            pending_target,
            "Old pending".to_string(),
            true,
            Box::new(FakeEditorSession),
            pending_id,
        );

        assert!(
            manager
                .set_window_title(open_target, "New".to_string())
                .is_empty()
        );
        assert!(
            manager
                .set_window_title(pending_target, "New pending".to_string())
                .is_empty()
        );

        assert_eq!(manager.window_title(open_id), Some("New"));
        assert_eq!(manager.window_title(pending_id), Some("New pending"));
    }

    #[test]
    fn editor_parent_snapshot_roundtrips_appkit_window_handle() {
        let snapshot = WindowSnapshot::capture(
            iced::window::raw_window_handle::RawWindowHandle::AppKit(
                iced::window::raw_window_handle::AppKitWindowHandle::new(
                    NonNull::<std::ffi::c_void>::dangling(),
                ),
            ),
            Some(iced::window::raw_window_handle::RawDisplayHandle::AppKit(
                iced::window::raw_window_handle::AppKitDisplayHandle::new(),
            )),
        )
        .expect("snapshot should capture appkit");

        let parent = snapshot_into_editor_parent(snapshot).expect("snapshot should restore appkit");

        assert!(matches!(
            parent.window,
            iced::window::raw_window_handle::RawWindowHandle::AppKit(_)
        ));
        assert!(matches!(
            parent.display,
            Some(iced::window::raw_window_handle::RawDisplayHandle::AppKit(_))
        ));
    }

    #[test]
    fn processor_editor_window_manager_removes_window_by_host_id() {
        let mut manager = EditorWindowManager::default();
        let target = EditorTarget {
            strip_index: 2,
            slot_index: 1,
        };
        let window_id = window::Id::unique();
        manager.begin_open(
            target,
            "Track 2".to_string(),
            true,
            Box::new(FakeEditorSession),
            window_id,
        );
        manager
            .attach(
                window_id,
                None,
                EditorParent {
                    window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                        iced::window::raw_window_handle::AppKitWindowHandle::new(
                            std::ptr::NonNull::<std::ffi::c_void>::dangling(),
                        ),
                    ),
                    display: None,
                },
            )
            .expect("attach should succeed");

        let removed = manager.remove_window(window_id);

        assert!(removed.is_some());
        assert!(!manager.windows.contains_key(&target));
    }

    #[test]
    fn processor_editor_window_manager_moves_effect_slot_targets_with_reorder() {
        let mut manager = EditorWindowManager::default();
        let targets = [
            EditorTarget {
                strip_index: 2,
                slot_index: 1,
            },
            EditorTarget {
                strip_index: 2,
                slot_index: 2,
            },
            EditorTarget {
                strip_index: 2,
                slot_index: 3,
            },
        ];
        for target in targets {
            let window_id = window::Id::unique();
            manager.begin_open(
                target,
                format!("Slot {}", target.slot_index),
                true,
                Box::new(FakeEditorSession),
                window_id,
            );
            manager
                .attach(
                    window_id,
                    None,
                    EditorParent {
                        window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                            iced::window::raw_window_handle::AppKitWindowHandle::new(
                                std::ptr::NonNull::<std::ffi::c_void>::dangling(),
                            ),
                        ),
                        display: None,
                    },
                )
                .expect("attach should succeed");
        }

        manager.move_slot_targets_within_strip(2, 1, 3);

        assert!(manager.windows.contains_key(&EditorTarget {
            strip_index: 2,
            slot_index: 1,
        }));
        assert!(manager.windows.contains_key(&EditorTarget {
            strip_index: 2,
            slot_index: 2,
        }));
        assert!(manager.windows.contains_key(&EditorTarget {
            strip_index: 2,
            slot_index: 3,
        }));
        assert_eq!(
            manager
                .windows
                .get(&EditorTarget {
                    strip_index: 2,
                    slot_index: 3,
                })
                .map(|window| window.title.as_str()),
            Some("Slot 1")
        );
    }
}
