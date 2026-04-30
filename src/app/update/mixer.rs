use editor_host::{
    EditorFrameCommand, EditorHostOptions, EditorPresetItem, EditorPresetOrigin, EditorPresetState,
    WindowSnapshot, route_app_quit_to_window_close,
};
use lilypalooza_audio::{BUILTIN_SOUNDFONT_ID, BusId, BusSend, ProcessorKind, SlotState, TrackId};
use lilypalooza_builtins::soundfont_synth::{self, SoundfontProcessorState};

use super::super::messages::MixerMessage;
use super::*;
use crate::app::processor_editor_windows::{EditorTarget, snapshot_into_editor_parent};
use iced::window;

fn processor_editor_window_settings(
    descriptor: lilypalooza_audio::EditorDescriptor,
    initial_size: Option<lilypalooza_audio::EditorSize>,
) -> window::Settings {
    let size = initial_size.unwrap_or(descriptor.default_size);
    window::Settings {
        size: Size::new(size.width as f32, size.height as f32),
        min_size: descriptor
            .min_size
            .map(|size| Size::new(size.width as f32, size.height as f32)),
        resizable: descriptor.resizable,
        closeable: true,
        minimizable: false,
        decorations: false,
        exit_on_close_request: false,
        ..window::Settings::default()
    }
}

impl Lilypalooza {
    pub(in crate::app) fn log_processor_editor_error(
        &mut self,
        action: &str,
        error: impl std::fmt::Display,
    ) {
        self.logger
            .push(format!("Processor editor {action} failed: {error}"));
    }

    fn toggle_processor_browser(&mut self, target: EditorTarget) -> Task<Message> {
        if self.open_processor_browser_target == Some(target) {
            self.close_processor_browser();
            return Task::none();
        }
        self.open_processor_browser_target = Some(target);
        self.open_instrument_browser_track =
            (target.slot_index == 0 && target.strip_index > 0).then(|| target.strip_index - 1);
        self.instrument_browser_search.clear();
        iced::widget::operation::focus(self.instrument_browser_search_input_id.clone())
    }

    fn close_processor_browser(&mut self) {
        self.open_processor_browser_target = None;
        self.open_instrument_browser_track = None;
        self.instrument_browser_search.clear();
    }

    pub(in crate::app) fn destroy_editor_target(&mut self, target: EditorTarget) -> Task<Message> {
        let Some((window_id, mut session)) = self.processor_editor_windows.remove_target(target)
        else {
            return Task::none();
        };
        if let Err(error) = session.detach() {
            self.log_processor_editor_error("detach", error);
        }
        window::close(window_id)
    }

    fn destroy_editor_strip_and_shift_later(&mut self, strip_index: usize) -> Task<Message> {
        let tasks = self
            .processor_editor_windows
            .targets_for_strip(strip_index)
            .into_iter()
            .map(|target| self.destroy_editor_target(target))
            .collect::<Vec<_>>();
        self.processor_editor_windows
            .shift_targets_after_removed_strip(strip_index);
        Task::batch(tasks)
    }

    pub(in crate::app) fn request_remove_bus_confirmation(&mut self, id: u16) -> Task<Message> {
        let name = self
            .playback
            .as_ref()
            .and_then(|playback| playback.mixer_state().bus(BusId(id)).ok())
            .map(|bus| bus.name.clone())
            .unwrap_or_else(|| format!("Bus {id}"));
        self.show_prompt(
            ErrorPrompt::new(
                format!("Remove {name}?"),
                "This removes the bus and clears routes or sends that target it.",
                ErrorFatality::Recoverable,
                PromptButtons::OkCancel,
            ),
            Some(PromptOkAction::RemoveBus(id)),
        );
        Task::none()
    }

    pub(in crate::app) fn remove_bus_confirmed(&mut self, id: u16) -> Task<Message> {
        self.remove_bus(id, true)
    }

    fn remove_bus(&mut self, id: u16, confirmed: bool) -> Task<Message> {
        if !confirmed {
            return self.request_remove_bus_confirmation(id);
        }

        let mut editor_cleanup = Task::none();
        let removed_bus_strip_index = self.playback.as_ref().and_then(|playback| {
            playback
                .mixer_state()
                .buses()
                .iter()
                .position(|bus| bus.bus_id == Some(BusId(id)))
                .map(|index| 1 + playback.mixer_state().track_count() + index)
        });
        if let Some(strip_index) = removed_bus_strip_index {
            editor_cleanup = Task::batch([
                editor_cleanup,
                self.destroy_editor_strip_and_shift_later(strip_index),
            ]);
        }

        let Some(playback) = self.playback.as_mut() else {
            return editor_cleanup;
        };
        let mut mixer = playback.mixer();
        if let Err(error) = mixer.remove_bus(BusId(id)) {
            self.logger.push(error.to_string());
        } else {
            self.piano_roll
                .set_global_solo_active(mixer_has_any_solo(&mixer));
        }
        editor_cleanup
    }

    pub(in crate::app) fn destroy_all_editor_windows(&mut self) -> Task<Message> {
        let removed = self.processor_editor_windows.remove_all_windows();
        if removed.is_empty() {
            return Task::none();
        }

        let mut tasks = Vec::with_capacity(removed.len());
        for (window_id, mut session) in removed {
            if let Err(error) = session.detach() {
                self.log_processor_editor_error("detach", error);
            }
            tasks.push(window::close(window_id));
        }
        Task::batch(tasks)
    }

    pub(in crate::app) fn hide_all_editor_windows(&mut self) -> Task<Message> {
        for errors in self.processor_editor_windows.hide_all_windows() {
            for error in errors {
                self.log_processor_editor_error("hide", error);
            }
        }
        Task::none()
    }

    pub(in crate::app) fn handle_window_opened(&mut self, window_id: window::Id) -> Task<Message> {
        if window_id == self.main_window_id {
            return window::run(window_id, move |window| {
                let parent = window
                    .window_handle()
                    .map_err(|error| error.to_string())
                    .and_then(|handle| {
                        WindowSnapshot::capture(
                            handle.as_raw(),
                            window.display_handle().ok().map(|display| display.as_raw()),
                        )
                        .map_err(|error| error.to_string())
                    });

                Message::WindowSnapshotCaptured {
                    window_id,
                    host: parent.clone(),
                    parent,
                }
            });
        }

        if !self.processor_editor_windows.pending_contains(window_id) {
            return Task::none();
        }

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
            Message::WindowSnapshotCaptured {
                window_id,
                host,
                parent: Err("editor host is installed in app state".to_string()),
            }
        })
    }

    pub(in crate::app) fn handle_window_closed(&mut self, window_id: window::Id) -> Task<Message> {
        if let Some((_target, mut session)) = self.processor_editor_windows.remove_window(window_id)
            && let Err(error) = session.detach()
        {
            self.log_processor_editor_error("detach", error);
        }
        Task::none()
    }

    pub(in crate::app) fn handle_processor_editor_attached(
        &mut self,
        window_id: window::Id,
        host: Result<WindowSnapshot, String>,
        parent: Result<WindowSnapshot, String>,
    ) -> Task<Message> {
        if window_id == self.main_window_id {
            match parent {
                Ok(snapshot) => {
                    if let Err(error) = route_app_quit_to_window_close(&snapshot) {
                        self.log_processor_editor_error("route app quit", error);
                    }
                    self.main_window_snapshot = Some(snapshot);
                }
                Err(error) => {
                    self.log_processor_editor_error("capture main window", error);
                    self.main_window_snapshot = None;
                }
            }
            return Task::none();
        }

        let host = match host {
            Ok(host) => host,
            Err(error) => {
                self.log_processor_editor_error("capture host window", error);
                return Task::none();
            }
        };
        let resizable = self
            .processor_editor_windows
            .window_resizable(window_id)
            .unwrap_or(true);
        let title = self
            .processor_editor_windows
            .window_title(window_id)
            .unwrap_or("Processor Editor")
            .to_string();
        let mut options = EditorHostOptions::new(title).with_resizable(resizable);
        if let Some(owner) = self.main_window_snapshot {
            options = options.with_owner(owner);
        }
        let installed_host = match editor_host::install_editor_host(
            &host,
            &options,
            crate::app::AppEditorFrame::from_theme(&self.theme),
        ) {
            Ok(host) => host,
            Err(error) => {
                self.log_processor_editor_error("install host", error);
                return Task::none();
            }
        };
        let parent = match snapshot_into_editor_parent(installed_host.content()) {
            Ok(parent) => parent,
            Err(error) => {
                self.log_processor_editor_error("capture content window", error);
                return Task::none();
            }
        };
        if let Err(error) =
            self.processor_editor_windows
                .attach(window_id, Some(installed_host), parent)
        {
            self.log_processor_editor_error("attach", error);
        } else if let Some(target) = self.processor_editor_windows.target_for_window(window_id) {
            self.refresh_editor_preset_state(target, None);
        }
        Task::none()
    }

    pub(in crate::app) fn handle_processor_editor_close_requested(
        &mut self,
        window_id: window::Id,
    ) -> Task<Message> {
        let Some((_target, errors)) = self.processor_editor_windows.hide_window(window_id) else {
            return Task::none();
        };
        for error in errors {
            self.log_processor_editor_error("hide", error);
        }
        Task::none()
    }

    pub(in crate::app) fn handle_processor_editor_focused(
        &mut self,
        window_id: window::Id,
    ) -> Task<Message> {
        for error in self.processor_editor_windows.focus_window(window_id) {
            self.log_processor_editor_error("raise", error);
        }
        Task::none()
    }

    pub(in crate::app) fn handle_primary_mouse_pressed(&mut self, pressed: bool) -> Task<Message> {
        self.primary_mouse_pressed = pressed;
        if pressed {
            self.begin_effect_drag_from_hover();
            return Task::none();
        }

        let effect_drag_task = self.finish_effect_drag(None);
        self.commit_pending_mixer_history();
        if self.renaming_target.is_some() {
            return Task::batch([
                effect_drag_task,
                iced::widget::operation::is_focused(super::super::TRACK_RENAME_INPUT_ID)
                    .map(Message::TrackRenameFocusChanged),
            ]);
        }
        effect_drag_task
    }

    pub(in crate::app) fn undo_mixer_operation(&mut self) -> Task<Message> {
        let close_editors = self.destroy_all_editor_windows();
        self.pending_mixer_undo_snapshot = None;
        let Some(previous) = self.mixer_undo_stack.pop() else {
            return close_editors;
        };
        let Some(playback) = self.playback.as_mut() else {
            return close_editors;
        };

        let current = playback.mixer_state().clone();
        let restored_state = previous.clone();
        let mut mixer = playback.mixer();
        if mixer.replace_state(previous).is_ok() {
            self.mixer_redo_stack.push(current);
            if let Err(error) =
                sync_piano_roll_mix_from_mixer_state(&mut self.piano_roll, &restored_state)
            {
                self.logger.push(format!("Piano roll sync failed: {error}"));
            }
        }
        close_editors
    }

    pub(in crate::app) fn redo_mixer_operation(&mut self) -> Task<Message> {
        let close_editors = self.destroy_all_editor_windows();
        self.pending_mixer_undo_snapshot = None;
        let Some(next) = self.mixer_redo_stack.pop() else {
            return close_editors;
        };
        let Some(playback) = self.playback.as_mut() else {
            return close_editors;
        };

        let current = playback.mixer_state().clone();
        let restored_state = next.clone();
        let mut mixer = playback.mixer();
        if mixer.replace_state(next).is_ok() {
            self.mixer_undo_stack.push(current);
            if let Err(error) =
                sync_piano_roll_mix_from_mixer_state(&mut self.piano_roll, &restored_state)
            {
                self.logger.push(format!("Piano roll sync failed: {error}"));
            }
        }
        close_editors
    }

    pub(in crate::app) fn handle_mixer_message(&mut self, message: MixerMessage) -> Task<Message> {
        if !matches!(
            message,
            MixerMessage::SelectTrack(_)
                | MixerMessage::InstrumentViewportScrolled(_)
                | MixerMessage::BusViewportScrolled(_)
        ) {
            self.set_focused_workspace_pane(WorkspacePaneKind::Mixer);
        }

        let history_mode = mixer_message_history_mode(&message, self.primary_mouse_pressed);
        match history_mode {
            MixerHistoryMode::None => {}
            MixerHistoryMode::Immediate => {
                self.commit_pending_mixer_history();
                if let Some(snapshot) = self
                    .playback
                    .as_ref()
                    .map(|playback| playback.mixer_state().clone())
                {
                    self.mixer_undo_stack.push(snapshot);
                    self.mixer_redo_stack.clear();
                }
            }
            MixerHistoryMode::Gesture => {
                if self.pending_mixer_undo_snapshot.is_none()
                    && let Some(snapshot) = self
                        .playback
                        .as_ref()
                        .map(|playback| playback.mixer_state().clone())
                {
                    self.pending_mixer_undo_snapshot = Some(snapshot);
                }
            }
        }

        match message {
            MixerMessage::SelectTrack(track_index) => {
                return self.select_track(
                    track_index,
                    super::track_selection::TrackSelectionOrigin::Mixer,
                );
            }
            MixerMessage::ToggleProcessorBrowser(target) => {
                return self.toggle_processor_browser(target);
            }
            MixerMessage::CloseProcessorBrowser => {
                self.close_processor_browser();
                return Task::none();
            }
            MixerMessage::ProcessorBrowserSearchChanged(value) => {
                self.instrument_browser_search = value;
                return Task::none();
            }
            MixerMessage::SelectProcessorBrowserBackend(backend) => {
                self.instrument_browser_backend = backend;
                return Task::none();
            }
            MixerMessage::ToggleMixerEffectRack(strip_index) => {
                if let Some(index) = self
                    .open_mixer_effect_rack_tracks
                    .iter()
                    .position(|open_strip| *open_strip == strip_index)
                {
                    self.open_mixer_effect_rack_tracks.remove(index);
                } else {
                    self.open_mixer_effect_rack_tracks.push(strip_index);
                    self.open_mixer_effect_rack_tracks.sort_unstable();
                    self.open_mixer_effect_rack_tracks.dedup();
                }
                return Task::none();
            }
            MixerMessage::SetProcessorSlotHovered(target) => {
                if let Some((target, _segment)) = target {
                    if target.slot_index > 0 {
                        self.effect_rack_hovered_effect =
                            Some((target.strip_index, target.slot_index - 1));
                    }
                } else if self.effect_drag_source.is_none() {
                    self.effect_rack_hovered_effect = None;
                }
                self.hovered_processor_slot = target;
                return Task::none();
            }
            MixerMessage::StartTrackEffectDrag {
                strip_index,
                effect_index,
            } => {
                self.effect_drag_source = Some((strip_index, effect_index));
                self.effect_drag_target = Some((strip_index, effect_index));
                return Task::none();
            }
            MixerMessage::DropTrackEffect {
                strip_index,
                effect_index,
            } => {
                return self.finish_effect_drag(Some((strip_index, effect_index)));
            }
            MixerMessage::TrackEffectDragMoved { strip_index, y } => {
                self.update_effect_rack_drag_position(strip_index, y);
                return self.tick_effect_rack_autoscroll();
            }
            MixerMessage::EffectRackCursorLeft(strip_index) => {
                if self
                    .effect_rack_hovered_effect
                    .is_some_and(|(hovered_strip_index, _)| hovered_strip_index == strip_index)
                {
                    self.effect_rack_hovered_effect = None;
                }
                if self
                    .effect_drag_source
                    .is_some_and(|(source_strip_index, _)| source_strip_index == strip_index)
                {
                    self.effect_rack_autoscroll_direction = 0;
                    self.effect_rack_drag_pointer_y = None;
                }
                return Task::none();
            }
            MixerMessage::EffectRackViewportScrolled {
                strip_index,
                viewport,
            } => {
                self.effect_rack_scroll_y
                    .insert(strip_index, viewport.absolute_offset().y);
                self.effect_rack_viewport_height
                    .insert(strip_index, viewport.bounds().height);
                return Task::none();
            }
            MixerMessage::ToggleTrackInstrumentBrowser(track_index) => {
                return self.toggle_processor_browser(EditorTarget {
                    strip_index: track_index + 1,
                    slot_index: 0,
                });
            }
            MixerMessage::CloseTrackInstrumentBrowser => {
                self.close_processor_browser();
                return Task::none();
            }
            MixerMessage::InstrumentBrowserSearchChanged(value) => {
                self.instrument_browser_search = value;
                return Task::none();
            }
            MixerMessage::SelectInstrumentBrowserBackend(backend) => {
                self.instrument_browser_backend = backend;
                return Task::none();
            }
            MixerMessage::StartTrackRename(track_index) => {
                return self.start_track_rename(track_index, WorkspacePaneKind::Mixer);
            }
            MixerMessage::StartBusRename(bus_id) => {
                let Some(name) = self
                    .playback
                    .as_ref()
                    .and_then(|playback| playback.mixer_state().bus(BusId(bus_id)).ok())
                    .map(|bus| bus.name.clone())
                else {
                    return Task::none();
                };
                return self.start_bus_rename(bus_id, WorkspacePaneKind::Mixer, name);
            }
            MixerMessage::OpenEditor(target) => {
                return self.open_editor_target(target);
            }
            MixerMessage::TrackRenameInputChanged(value) => {
                self.update_track_rename_value(value);
                return Task::none();
            }
            MixerMessage::OpenTrackColorPicker => {
                self.open_track_color_picker();
                return Task::none();
            }
            MixerMessage::SubmitTrackColor(color) => {
                self.submit_track_color(color);
                return Task::none();
            }
            MixerMessage::PreviewTrackColor(color) => {
                self.preview_track_color(color);
                return Task::none();
            }
            MixerMessage::CommitTrackRename => return self.commit_track_rename(),
            MixerMessage::CancelTrackRename => {
                self.cancel_track_rename();
                return Task::none();
            }
            _ => {}
        }

        if matches!(
            message,
            MixerMessage::SelectTrackInstrument(_, _) | MixerMessage::SelectProcessor(_, _)
        ) {
            self.close_processor_browser();
        }

        let editor_cleanup = match &message {
            MixerMessage::SelectTrackInstrument(index, _) => {
                self.destroy_editor_target(EditorTarget {
                    strip_index: index + 1,
                    slot_index: 0,
                })
            }
            MixerMessage::SelectProcessor(target, _) => self.destroy_editor_target(*target),
            _ => Task::none(),
        };

        if let MixerMessage::RemoveBus(id) = message {
            return Task::batch([editor_cleanup, self.remove_bus(id, false)]);
        }

        let Some(playback) = self.playback.as_mut() else {
            return editor_cleanup;
        };
        let mut editor_target_to_open = None;
        let mut mixer_error = None;

        {
            let mut mixer = playback.mixer();

            match message {
                MixerMessage::AddBus => {
                    if let Err(error) = mixer.add_bus(format!("Bus {}", mixer.bus_count() + 1)) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::RemoveBus(_) => {
                    unreachable!("bus removal is handled before mixer borrow")
                }
                MixerMessage::InstrumentViewportScrolled(viewport) => {
                    self.mixer_instrument_scroll_x = viewport.absolute_offset().x;
                    self.mixer_instrument_viewport_width = viewport.bounds().width;
                }
                MixerMessage::BusViewportScrolled(viewport) => {
                    self.mixer_bus_scroll_x = viewport.absolute_offset().x;
                    self.mixer_bus_viewport_width = viewport.bounds().width;
                }
                MixerMessage::ResetMasterMeter => mixer.reset_master_meter(),
                MixerMessage::SetMasterGain(gain) => mixer.set_master_gain_db(gain),
                MixerMessage::SetMasterPan(pan) => mixer.set_master_pan(pan),
                MixerMessage::ResetTrackMeter(index) => {
                    if let Err(error) = mixer.reset_track_meter(TrackId(index as u16)) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::SetTrackGain(index, gain) => {
                    if let Err(error) = mixer.set_track_gain_db(TrackId(index as u16), gain) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::SetTrackPan(index, pan) => {
                    if let Err(error) = mixer.set_track_pan(TrackId(index as u16), pan) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::SetMainRoute(source, route) => {
                    let result = match source {
                        super::super::mixer::RoutingStrip::Track(index) => {
                            mixer.set_track_route(TrackId(index as u16), route)
                        }
                        super::super::mixer::RoutingStrip::Bus(id) => {
                            mixer.set_bus_route(BusId(id), route)
                        }
                    };
                    if let Err(error) = result {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::AddSend(source, bus_id) => {
                    let send = BusSend::new(BusId(bus_id), 0.0, false);
                    let result = match source {
                        super::super::mixer::RoutingStrip::Track(index) => {
                            mixer.add_track_send(TrackId(index as u16), send)
                        }
                        super::super::mixer::RoutingStrip::Bus(id) => {
                            mixer.add_bus_send(BusId(id), send)
                        }
                    };
                    if let Err(error) = result {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::SetSendDestination(source, send_index, bus_id) => {
                    let result = update_send(&mut mixer, source, send_index, |send| {
                        send.bus_id = BusId(bus_id);
                    });
                    if let Err(error) = result {
                        mixer_error = Some(error);
                    }
                }
                MixerMessage::SetSendGain(source, send_index, gain) => {
                    let result = update_send(&mut mixer, source, send_index, |send| {
                        send.gain_db = gain;
                    });
                    if let Err(error) = result {
                        mixer_error = Some(error);
                    }
                }
                MixerMessage::ToggleSendEnabled(source, send_index) => {
                    let result = update_send(&mut mixer, source, send_index, |send| {
                        send.enabled = !send.enabled;
                    });
                    if let Err(error) = result {
                        mixer_error = Some(error);
                    }
                }
                MixerMessage::ToggleSendPreFader(source, send_index) => {
                    let result = update_send(&mut mixer, source, send_index, |send| {
                        send.pre_fader = !send.pre_fader;
                    });
                    if let Err(error) = result {
                        mixer_error = Some(error);
                    }
                }
                MixerMessage::RemoveSend(source, send_index) => {
                    let result = match source {
                        super::super::mixer::RoutingStrip::Track(index) => mixer
                            .remove_track_send(TrackId(index as u16), send_index)
                            .map(drop),
                        super::super::mixer::RoutingStrip::Bus(id) => {
                            mixer.remove_bus_send(BusId(id), send_index).map(drop)
                        }
                    };
                    if let Err(error) = result {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::ToggleTrackMute(index) => {
                    let next = mixer
                        .track(TrackId(index as u16))
                        .map(|track| !track.state.muted)
                        .unwrap_or(false);
                    if let Err(error) = mixer.set_track_muted(TrackId(index as u16), next) {
                        mixer_error = Some(error.to_string());
                    } else {
                        self.piano_roll.set_track_muted(index, next);
                    }
                }
                MixerMessage::ToggleTrackSolo(index) => {
                    let next = mixer
                        .track(TrackId(index as u16))
                        .map(|track| !track.state.soloed)
                        .unwrap_or(false);
                    if let Err(error) = mixer.set_track_soloed(TrackId(index as u16), next) {
                        mixer_error = Some(error.to_string());
                    } else {
                        self.piano_roll.set_track_soloed(index, next);
                        self.piano_roll
                            .set_global_solo_active(mixer_has_any_solo(&mixer));
                    }
                }
                MixerMessage::SelectTrackInstrument(index, choice) => {
                    let open_editor_after_select = matches!(
                        &choice,
                        super::super::mixer::InstrumentChoice::Processor { .. }
                    );
                    let slot = match choice {
                        super::super::mixer::InstrumentChoice::None => SlotState::default(),
                        super::super::mixer::InstrumentChoice::Processor {
                            ref processor_id,
                            backend,
                            ..
                        } => default_track_instrument_slot(
                            &mixer,
                            TrackId(index as u16),
                            processor_id.as_str(),
                            backend,
                        ),
                    };
                    if let Err(error) = mixer.set_track_instrument(TrackId(index as u16), slot) {
                        mixer_error = Some(error.to_string());
                    } else if open_editor_after_select {
                        editor_target_to_open = Some(EditorTarget {
                            strip_index: index + 1,
                            slot_index: 0,
                        });
                    }
                }
                MixerMessage::SelectProcessor(target, choice) => {
                    if target.slot_index == 0 {
                        if target.strip_index == 0 || target.strip_index > mixer.track_count() {
                            return Task::none();
                        }
                        let track_id = TrackId((target.strip_index - 1) as u16);
                        let open_editor_after_select = matches!(
                            &choice,
                            super::super::mixer::ProcessorChoice::Processor { .. }
                        );
                        let slot = match choice {
                            super::super::mixer::ProcessorChoice::None => SlotState::default(),
                            super::super::mixer::ProcessorChoice::Processor {
                                ref processor_id,
                                backend,
                                ..
                            } => default_track_instrument_slot(
                                &mixer,
                                track_id,
                                processor_id,
                                backend,
                            ),
                        };
                        if let Err(error) = mixer.set_track_instrument(track_id, slot) {
                            mixer_error = Some(error.to_string());
                        } else if open_editor_after_select {
                            editor_target_to_open = Some(target);
                        }
                    } else {
                        let effect_index = target.slot_index - 1;
                        let Some(strip) = mixer.strip_by_index(target.strip_index) else {
                            return Task::none();
                        };
                        let bus_id = strip.bus_id;
                        let mut effects = strip.effects().to_vec();
                        match choice {
                            super::super::mixer::ProcessorChoice::None => {
                                if effect_index < effects.len() {
                                    effects.remove(effect_index);
                                }
                            }
                            super::super::mixer::ProcessorChoice::Processor {
                                ref processor_id,
                                ref name,
                                backend,
                                ..
                            } => {
                                let mut slot = processor_slot(processor_id, backend);
                                assign_effect_instance_label_index(
                                    &effects,
                                    effect_index,
                                    name,
                                    &mut slot,
                                );
                                if effect_index < effects.len() {
                                    effects[effect_index] = slot;
                                } else {
                                    effects.push(slot);
                                }
                                editor_target_to_open = Some(target);
                            }
                        }
                        let result = if target.strip_index == 0 {
                            mixer.set_master_effects(effects)
                        } else if target.strip_index <= mixer.track_count() {
                            mixer.set_track_effects(
                                TrackId((target.strip_index - 1) as u16),
                                effects,
                            )
                        } else if let Some(bus_id) = bus_id {
                            mixer.set_bus_effects(bus_id, effects)
                        } else {
                            return Task::none();
                        };
                        if let Err(error) = result {
                            mixer_error = Some(error.to_string());
                        }
                    }
                }
                MixerMessage::ToggleSlotBypass(target) => {
                    let address = lilypalooza_audio::SlotAddress {
                        strip_index: target.strip_index,
                        slot_index: target.slot_index,
                    };
                    let next = mixer
                        .slot(address)
                        .map(|slot| !slot.bypassed)
                        .unwrap_or(false);
                    if let Err(error) = mixer.set_slot_bypassed(address, next) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::MoveTrackEffect {
                    strip_index,
                    from_effect_index,
                    to_effect_index,
                } => {
                    let Some(strip) = mixer.strip_by_index(strip_index) else {
                        return Task::none();
                    };
                    let bus_id = strip.bus_id;
                    let mut effects = strip.effects().to_vec();
                    if from_effect_index < effects.len() && to_effect_index < effects.len() {
                        let effect = effects.remove(from_effect_index);
                        effects.insert(to_effect_index, effect);
                        let result = if strip_index == 0 {
                            mixer.set_master_effects(effects)
                        } else if strip_index <= mixer.track_count() {
                            mixer.set_track_effects(TrackId((strip_index - 1) as u16), effects)
                        } else if let Some(bus_id) = bus_id {
                            mixer.set_bus_effects(bus_id, effects)
                        } else {
                            return Task::none();
                        };
                        if let Err(error) = result {
                            mixer_error = Some(error.to_string());
                        } else {
                            self.processor_editor_windows
                                .move_slot_targets_within_strip(
                                    strip_index,
                                    from_effect_index + 1,
                                    to_effect_index + 1,
                                );
                        }
                    }
                }
                MixerMessage::ResetBusMeter(id) => {
                    if let Err(error) = mixer.reset_bus_meter(BusId(id)) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::SetBusGain(id, gain) => {
                    if let Err(error) = mixer.set_bus_gain_db(BusId(id), gain) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::SetBusPan(id, pan) => {
                    if let Err(error) = mixer.set_bus_pan(BusId(id), pan) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::ToggleBusMute(id) => {
                    let next = mixer
                        .bus(BusId(id))
                        .map(|bus| !bus.state.muted)
                        .unwrap_or(false);
                    if let Err(error) = mixer.set_bus_muted(BusId(id), next) {
                        mixer_error = Some(error.to_string());
                    }
                }
                MixerMessage::ToggleBusSolo(id) => {
                    let next = mixer
                        .bus(BusId(id))
                        .map(|bus| !bus.state.soloed)
                        .unwrap_or(false);
                    if let Err(error) = mixer.set_bus_soloed(BusId(id), next) {
                        mixer_error = Some(error.to_string());
                    } else {
                        self.piano_roll
                            .set_global_solo_active(mixer_has_any_solo(&mixer));
                    }
                }
                MixerMessage::SelectTrack(_) => {}
                MixerMessage::StartTrackRename(_)
                | MixerMessage::ToggleProcessorBrowser(_)
                | MixerMessage::CloseProcessorBrowser
                | MixerMessage::ProcessorBrowserSearchChanged(_)
                | MixerMessage::SelectProcessorBrowserBackend(_)
                | MixerMessage::ToggleMixerEffectRack(_)
                | MixerMessage::SetProcessorSlotHovered(_)
                | MixerMessage::StartTrackEffectDrag { .. }
                | MixerMessage::DropTrackEffect { .. }
                | MixerMessage::TrackEffectDragMoved { .. }
                | MixerMessage::EffectRackCursorLeft(_)
                | MixerMessage::EffectRackViewportScrolled { .. }
                | MixerMessage::ToggleTrackInstrumentBrowser(_)
                | MixerMessage::CloseTrackInstrumentBrowser
                | MixerMessage::InstrumentBrowserSearchChanged(_)
                | MixerMessage::SelectInstrumentBrowserBackend(_)
                | MixerMessage::StartBusRename(_)
                | MixerMessage::OpenEditor(_)
                | MixerMessage::TrackRenameInputChanged(_)
                | MixerMessage::OpenTrackColorPicker
                | MixerMessage::SubmitTrackColor(_)
                | MixerMessage::PreviewTrackColor(_)
                | MixerMessage::CommitTrackRename => {}
                MixerMessage::CancelTrackRename => {}
            }
        }

        if let Some(error) = mixer_error {
            self.logger.push(format!("Mixer update failed: {error}"));
        }
        self.project_mixer_state = playback.mixer_state().clone();
        if let Some(target) = editor_target_to_open {
            return Task::batch([editor_cleanup, self.open_editor_target(target)]);
        }
        editor_cleanup
    }
}

impl Lilypalooza {
    fn commit_pending_mixer_history(&mut self) {
        if let Some(snapshot) = self.pending_mixer_undo_snapshot.take() {
            self.mixer_undo_stack.push(snapshot);
            self.mixer_redo_stack.clear();
        }
    }

    fn begin_effect_drag_from_hover(&mut self) {
        let Some(source) = self.effect_rack_hovered_effect else {
            return;
        };
        self.effect_drag_source = Some(source);
        self.effect_drag_target = Some(source);
    }

    fn finish_effect_drag(&mut self, target: Option<(usize, usize)>) -> Task<Message> {
        let source = self.effect_drag_source.take();
        let target = target.or(self.effect_drag_target);
        self.effect_drag_target = None;
        self.effect_rack_autoscroll_direction = 0;
        self.effect_rack_drag_pointer_y = None;

        let Some((from_strip_index, from_effect_index)) = source else {
            return Task::none();
        };
        let Some((to_strip_index, to_effect_index)) = target else {
            return Task::none();
        };
        if from_strip_index != to_strip_index || from_effect_index == to_effect_index {
            return Task::none();
        }

        self.handle_mixer_message(MixerMessage::MoveTrackEffect {
            strip_index: from_strip_index,
            from_effect_index,
            to_effect_index,
        })
    }

    fn update_effect_rack_drag_position(&mut self, strip_index: usize, y: f32) {
        let hover = self.effect_rack_hovered_index_at_y(strip_index, y);
        self.effect_rack_hovered_effect = hover.map(|index| (strip_index, index));

        if self
            .effect_drag_source
            .is_some_and(|(source_strip, _)| source_strip == strip_index)
            && let Some(target_index) = self.effect_rack_drop_index_at_y(strip_index, y)
        {
            self.effect_drag_target = Some((strip_index, target_index));
            self.effect_rack_drag_pointer_y = Some(y);
            self.update_effect_rack_autoscroll_direction(strip_index, y);
        }
    }

    fn effect_rack_hovered_index_at_y(&self, strip_index: usize, y: f32) -> Option<usize> {
        let index = self.effect_rack_raw_index_at_y(strip_index, y);
        (index < self.effect_count_for_strip(strip_index)?).then_some(index)
    }

    fn effect_rack_drop_index_at_y(&self, strip_index: usize, y: f32) -> Option<usize> {
        let effect_count = self.effect_count_for_strip(strip_index)?;
        if effect_count == 0 {
            return None;
        }
        Some(
            self.effect_rack_raw_index_at_y(strip_index, y)
                .min(effect_count - 1),
        )
    }

    fn effect_rack_raw_index_at_y(&self, strip_index: usize, y: f32) -> usize {
        let scroll_y = self
            .effect_rack_scroll_y
            .get(&strip_index)
            .copied()
            .unwrap_or(0.0);
        ((scroll_y + y).max(0.0) / super::super::mixer::EFFECT_RACK_ROW_HEIGHT).floor() as usize
    }

    fn effect_count_for_strip(&self, strip_index: usize) -> Option<usize> {
        let mixer = self.playback.as_ref()?.mixer_state();
        mixer
            .strip_by_index(strip_index)
            .map(lilypalooza_audio::mixer::Track::effect_count)
    }

    fn update_effect_rack_autoscroll_direction(&mut self, strip_index: usize, y: f32) {
        let viewport_height = self
            .effect_rack_viewport_height
            .get(&strip_index)
            .copied()
            .unwrap_or(super::super::mixer::EFFECT_RACK_HEIGHT);
        let edge = super::super::mixer::EFFECT_RACK_EDGE_SCROLL_ZONE.min(viewport_height / 2.0);
        self.effect_rack_autoscroll_direction = if y <= edge {
            -1
        } else if y >= viewport_height - edge {
            1
        } else {
            0
        };
    }

    pub(in crate::app) fn tick_effect_rack_autoscroll(&mut self) -> Task<Message> {
        let Some((strip_index, _)) = self.effect_drag_source else {
            return Task::none();
        };
        if self.effect_rack_autoscroll_direction == 0 {
            return Task::none();
        }
        if let Some(y) = self.effect_rack_drag_pointer_y {
            self.effect_drag_target = self
                .effect_rack_drop_index_at_y(strip_index, y)
                .map(|index| (strip_index, index));
        }

        iced::widget::operation::scroll_by(
            super::super::mixer::effect_rack_scroll_id(strip_index),
            iced::widget::operation::AbsoluteOffset {
                x: 0.0,
                y: super::super::mixer::EFFECT_RACK_EDGE_SCROLL_STEP
                    * f32::from(self.effect_rack_autoscroll_direction),
            },
        )
    }
}

fn mixer_has_any_solo(mixer: &lilypalooza_audio::MixerHandle<'_>) -> bool {
    mixer.tracks().iter().any(|track| track.state.soloed)
        || mixer.buses().iter().any(|bus| bus.state.soloed)
}

fn update_send(
    mixer: &mut lilypalooza_audio::MixerHandle<'_>,
    source: super::super::mixer::RoutingStrip,
    send_index: usize,
    update: impl FnOnce(&mut BusSend),
) -> Result<(), String> {
    match source {
        super::super::mixer::RoutingStrip::Track(index) => {
            let id = TrackId(index as u16);
            let mut send = *mixer
                .track(id)
                .map_err(|error| error.to_string())?
                .routing
                .sends
                .get(send_index)
                .ok_or_else(|| format!("track send index {send_index} is out of bounds"))?;
            update(&mut send);
            mixer
                .set_track_send(id, send_index, send)
                .map_err(|error| error.to_string())
        }
        super::super::mixer::RoutingStrip::Bus(id) => {
            let id = BusId(id);
            let mut send = *mixer
                .bus(id)
                .map_err(|error| error.to_string())?
                .routing
                .sends
                .get(send_index)
                .ok_or_else(|| format!("bus send index {send_index} is out of bounds"))?;
            update(&mut send);
            mixer
                .set_bus_send(id, send_index, send)
                .map_err(|error| error.to_string())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MixerHistoryMode {
    None,
    Immediate,
    Gesture,
}

fn mixer_message_history_mode(
    message: &MixerMessage,
    primary_mouse_pressed: bool,
) -> MixerHistoryMode {
    match message {
        MixerMessage::ResetMasterMeter
        | MixerMessage::ResetTrackMeter(_)
        | MixerMessage::ResetBusMeter(_)
        | MixerMessage::InstrumentViewportScrolled(_)
        | MixerMessage::BusViewportScrolled(_)
        | MixerMessage::OpenTrackColorPicker
        | MixerMessage::ToggleProcessorBrowser(_)
        | MixerMessage::CloseProcessorBrowser
        | MixerMessage::ProcessorBrowserSearchChanged(_)
        | MixerMessage::SelectProcessorBrowserBackend(_)
        | MixerMessage::ToggleMixerEffectRack(_)
        | MixerMessage::SetProcessorSlotHovered(_)
        | MixerMessage::StartTrackEffectDrag { .. }
        | MixerMessage::DropTrackEffect { .. }
        | MixerMessage::TrackEffectDragMoved { .. }
        | MixerMessage::EffectRackCursorLeft(_)
        | MixerMessage::EffectRackViewportScrolled { .. }
        | MixerMessage::ToggleTrackInstrumentBrowser(_)
        | MixerMessage::CloseTrackInstrumentBrowser
        | MixerMessage::InstrumentBrowserSearchChanged(_)
        | MixerMessage::SelectInstrumentBrowserBackend(_)
        | MixerMessage::OpenEditor(_)
        | MixerMessage::PreviewTrackColor(_) => MixerHistoryMode::None,
        MixerMessage::SetMasterGain(_)
        | MixerMessage::SetMasterPan(_)
        | MixerMessage::SetTrackGain(_, _)
        | MixerMessage::SetTrackPan(_, _)
        | MixerMessage::SetBusGain(_, _)
        | MixerMessage::SetBusPan(_, _)
        | MixerMessage::SetSendGain(_, _, _) => {
            if primary_mouse_pressed {
                MixerHistoryMode::Gesture
            } else {
                MixerHistoryMode::Immediate
            }
        }
        MixerMessage::AddBus
        | MixerMessage::RemoveBus(_)
        | MixerMessage::SelectTrack(_)
        | MixerMessage::StartTrackRename(_)
        | MixerMessage::StartBusRename(_)
        | MixerMessage::TrackRenameInputChanged(_)
        | MixerMessage::CancelTrackRename
        | MixerMessage::CommitTrackRename
        | MixerMessage::SubmitTrackColor(_)
        | MixerMessage::ToggleTrackMute(_)
        | MixerMessage::ToggleTrackSolo(_)
        | MixerMessage::SetMainRoute(_, _)
        | MixerMessage::AddSend(_, _)
        | MixerMessage::SetSendDestination(_, _, _)
        | MixerMessage::ToggleSendEnabled(_, _)
        | MixerMessage::ToggleSendPreFader(_, _)
        | MixerMessage::RemoveSend(_, _)
        | MixerMessage::SelectProcessor(_, _)
        | MixerMessage::ToggleSlotBypass(_)
        | MixerMessage::MoveTrackEffect { .. }
        | MixerMessage::SelectTrackInstrument(_, _)
        | MixerMessage::ToggleBusMute(_)
        | MixerMessage::ToggleBusSolo(_) => MixerHistoryMode::Immediate,
    }
}

impl Lilypalooza {
    pub(in crate::app) fn editor_window_title(
        &self,
        strip_name: &str,
        slot: &SlotState,
        slot_index: usize,
    ) -> String {
        if slot_index == 0 {
            return strip_name.to_string();
        }

        slot.title(strip_name, slot_index)
    }

    fn open_editor_target(&mut self, target: EditorTarget) -> Task<Message> {
        if let Some(window_id) = self.processor_editor_windows.focus_existing(target) {
            return if self.processor_editor_windows.pending_contains(window_id) {
                Task::none()
            } else if self.processor_editor_windows.window_visible(window_id) {
                self.handle_processor_editor_close_requested(window_id)
            } else {
                for error in self.processor_editor_windows.show_window(window_id) {
                    self.log_processor_editor_error("show", error);
                }
                window::gain_focus(window_id)
            };
        }

        let Some(playback) = self.playback.as_ref() else {
            return Task::none();
        };
        let Some(strip) = playback.mixer_state().strip_by_index(target.strip_index) else {
            return Task::none();
        };
        let Some(slot) = strip.slot(target.slot_index) else {
            return Task::none();
        };
        let Ok(Some(controller)) = playback.controller(lilypalooza_audio::SlotAddress {
            strip_index: target.strip_index,
            slot_index: target.slot_index,
        }) else {
            return Task::none();
        };
        let title = self.editor_window_title(&strip.name, slot, target.slot_index);
        let descriptor = controller.descriptor().editor;
        let session_result = controller.create_editor_session();

        self.open_editor(target, title, descriptor, session_result)
    }

    fn open_editor(
        &mut self,
        target: EditorTarget,
        title: String,
        descriptor: Option<lilypalooza_audio::EditorDescriptor>,
        session_result: Result<
            Option<Box<dyn lilypalooza_audio::EditorSession>>,
            lilypalooza_audio::EditorError,
        >,
    ) -> Task<Message> {
        let Some(descriptor) = descriptor else {
            return Task::none();
        };
        let Ok(Some(session)) = session_result else {
            return Task::none();
        };

        let (window_id, open_task) =
            window::open(processor_editor_window_settings(descriptor, None));
        self.processor_editor_windows.begin_open(
            target,
            title,
            descriptor.resizable,
            session,
            window_id,
        );
        open_task.map(|_| Message::Noop)
    }

    pub(in crate::app) fn handle_processor_editor_frame_command(
        &mut self,
        target: EditorTarget,
        command: EditorFrameCommand,
    ) {
        match command {
            EditorFrameCommand::PreviousPreset => self.step_processor_preset(target, -1),
            EditorFrameCommand::NextPreset => self.step_processor_preset(target, 1),
            EditorFrameCommand::LoadPreset(id) => self.load_processor_preset_for_target(target, id),
            EditorFrameCommand::RenamePreset { id, name } => {
                self.rename_processor_preset_for_target(target, id, name);
            }
            EditorFrameCommand::DeletePreset(id) => {
                self.delete_processor_preset_for_target(target, id);
            }
            EditorFrameCommand::SavePreset => self.save_processor_preset_from_target(target),
            EditorFrameCommand::TogglePresetBrowser => {
                self.expanded_processor_preset_browser =
                    (self.expanded_processor_preset_browser != Some(target)).then_some(target);
                let selected_id = self
                    .processor_editor_windows
                    .preset_state(target)
                    .and_then(|state| state.selected_id);
                self.refresh_editor_preset_state(target, selected_id);
            }
        }
    }

    fn processor_kind_for_target(&self, target: EditorTarget) -> Option<ProcessorKind> {
        self.playback
            .as_ref()
            .and_then(|playback| {
                playback
                    .mixer_state()
                    .strip_by_index(target.strip_index)
                    .and_then(|strip| strip.slot(target.slot_index))
            })
            .map(|slot| slot.kind.clone())
    }

    fn refresh_editor_preset_state(&mut self, target: EditorTarget, selected_id: Option<String>) {
        let Some(kind) = self.processor_kind_for_target(target) else {
            self.processor_editor_windows.set_preset_state(target, None);
            return;
        };
        let items = self
            .processor_presets
            .presets_for(&kind)
            .into_iter()
            .map(|preset| EditorPresetItem {
                id: preset.id.clone(),
                name: preset.name.clone(),
                origin: EditorPresetOrigin::User,
            })
            .collect::<Vec<_>>();
        let current_name = selected_id
            .as_deref()
            .and_then(|id| items.iter().find(|item| item.id == id))
            .map(|item| item.name.clone())
            .unwrap_or_else(|| "Preset".to_string());
        let expanded = self.expanded_processor_preset_browser == Some(target);
        self.processor_editor_windows.set_preset_state(
            target,
            Some(EditorPresetState {
                current_name,
                selected_id,
                expanded,
                items,
            }),
        );
    }

    fn save_processor_preset_from_target(&mut self, target: EditorTarget) {
        let Some(playback) = self.playback.as_ref() else {
            return;
        };
        let Some(kind) = self.processor_kind_for_target(target) else {
            return;
        };
        let Ok(Some(controller)) = playback.controller(lilypalooza_audio::SlotAddress {
            strip_index: target.strip_index,
            slot_index: target.slot_index,
        }) else {
            return;
        };
        let Ok(state) = controller.save_state() else {
            return;
        };
        let name = format!(
            "User Preset {}",
            self.processor_presets.presets_for(&kind).len() + 1
        );
        let id = self.processor_presets.save_user_preset(name, kind, state);
        self.refresh_editor_preset_state(target, Some(id));
        self.persist_settings();
    }

    fn rename_processor_preset_for_target(
        &mut self,
        target: EditorTarget,
        id: String,
        name: String,
    ) {
        let Some(kind) = self.processor_kind_for_target(target) else {
            return;
        };
        if self.processor_presets.rename_user_preset(&kind, &id, name) {
            self.refresh_editor_preset_state(target, Some(id));
            self.persist_settings();
        }
    }

    fn delete_processor_preset_for_target(&mut self, target: EditorTarget, id: String) {
        let Some(kind) = self.processor_kind_for_target(target) else {
            return;
        };
        if self.processor_presets.delete_user_preset(&kind, &id) {
            let selected_id = self
                .processor_editor_windows
                .preset_state(target)
                .and_then(|state| state.selected_id)
                .filter(|selected| selected != &id);
            self.refresh_editor_preset_state(target, selected_id);
            self.persist_settings();
        }
    }

    fn load_processor_preset_for_target(&mut self, target: EditorTarget, id: String) {
        let Some(kind) = self.processor_kind_for_target(target) else {
            return;
        };
        let Some(state) = self.processor_presets.state_for(&kind, &id).cloned() else {
            return;
        };
        let Some(playback) = self.playback.as_mut() else {
            return;
        };
        let Ok(Some(controller)) = playback.controller(lilypalooza_audio::SlotAddress {
            strip_index: target.strip_index,
            slot_index: target.slot_index,
        }) else {
            return;
        };
        if controller.load_state(&state).is_err() {
            return;
        }
        if target.slot_index == 0 && target.strip_index > 0 {
            let track_id = TrackId((target.strip_index - 1) as u16);
            let next_slot = playback
                .mixer_state()
                .track(track_id)
                .ok()
                .and_then(|track| track.instrument_slot())
                .cloned()
                .map(|mut slot| {
                    slot.state = state;
                    slot
                });
            if let Some(slot) = next_slot
                && playback
                    .mixer()
                    .set_track_instrument(track_id, slot)
                    .is_ok()
            {
                self.project_mixer_state = playback.mixer_state().clone();
            }
        }
        self.refresh_editor_preset_state(target, Some(id));
    }

    fn step_processor_preset(&mut self, target: EditorTarget, direction: isize) {
        let Some(kind) = self.processor_kind_for_target(target) else {
            return;
        };
        let presets = self.processor_presets.presets_for(&kind);
        if presets.is_empty() {
            return;
        }
        let current_id = self
            .processor_editor_windows
            .preset_state(target)
            .and_then(|state| state.selected_id);
        let next_index = current_id
            .as_deref()
            .and_then(|id| presets.iter().position(|preset| preset.id == id))
            .map_or_else(
                || {
                    if direction >= 0 { 0 } else { presets.len() - 1 }
                },
                |current_index| {
                    (current_index as isize + direction).rem_euclid(presets.len() as isize) as usize
                },
            );
        let id = presets[next_index].id.clone();
        self.load_processor_preset_for_target(target, id);
    }
}

fn default_track_instrument_slot(
    mixer: &lilypalooza_audio::MixerHandle<'_>,
    track_id: TrackId,
    processor_id: &str,
    backend: super::super::mixer::ProcessorBrowserBackend,
) -> SlotState {
    if backend == super::super::mixer::ProcessorBrowserBackend::BuiltIn
        && processor_id == BUILTIN_SOUNDFONT_ID
    {
        let current_state = mixer
            .track(track_id)
            .ok()
            .and_then(|track| track.instrument_slot())
            .and_then(|slot| {
                slot.decode_built_in(BUILTIN_SOUNDFONT_ID, soundfont_synth::decode_state)
                    .ok()
                    .flatten()
            });
        let soundfont_id = current_state
            .as_ref()
            .map(|state| state.soundfont_id.clone())
            .or_else(|| {
                mixer
                    .soundfonts()
                    .first()
                    .map(|soundfont| soundfont.id.clone())
            })
            .unwrap_or_else(|| SoundfontProcessorState::default().soundfont_id);
        let bank = current_state.as_ref().map_or(0, |state| state.bank);
        let program = current_state.as_ref().map_or(0, |state| state.program);

        return SlotState::built_in(
            BUILTIN_SOUNDFONT_ID,
            soundfont_synth::encode_state(&SoundfontProcessorState {
                soundfont_id,
                bank,
                program,
                ..SoundfontProcessorState::default()
            }),
        );
    }

    processor_slot(processor_id, backend)
}

fn processor_slot(
    processor_id: &str,
    backend: super::super::mixer::ProcessorBrowserBackend,
) -> SlotState {
    match backend {
        super::super::mixer::ProcessorBrowserBackend::BuiltIn => {
            SlotState::built_in(processor_id, lilypalooza_audio::ProcessorState::default())
        }
        super::super::mixer::ProcessorBrowserBackend::Clap
        | super::super::mixer::ProcessorBrowserBackend::Vst3 => SlotState::new(
            ProcessorKind::Plugin {
                plugin_id: processor_id.to_string(),
            },
            lilypalooza_audio::ProcessorState::default(),
        ),
    }
}

fn assign_effect_instance_label_index(
    effects: &[SlotState],
    target_effect_index: usize,
    processor_name: &str,
    slot: &mut SlotState,
) {
    if let Some(existing) = effects.get(target_effect_index)
        && effect_slot_name(existing).as_deref() == Some(processor_name)
        && existing.instance_label_index > 0
    {
        slot.instance_label_index = existing.instance_label_index;
        return;
    }

    let mut used = std::collections::BTreeSet::new();
    for (effect_index, effect) in effects.iter().enumerate() {
        if effect_index != target_effect_index
            && effect_slot_name(effect).as_deref() == Some(processor_name)
        {
            used.insert(effect.instance_label_index);
        }
    }

    slot.instance_label_index = (1..)
        .find(|index| !used.contains(index))
        .expect("infinite label index range should have a free value");
}

fn effect_slot_name(slot: &SlotState) -> Option<String> {
    lilypalooza_audio::instrument::registry::resolve(&slot.kind)
        .map(|entry| entry.name.into_owned())
}

fn sync_piano_roll_mix_from_mixer_state(
    piano_roll: &mut super::super::piano_roll::PianoRollState,
    mixer: &lilypalooza_audio::MixerState,
) -> Result<(), String> {
    for (track_id, track) in mixer.tracks_with_ids() {
        let index = track_id.index();
        piano_roll
            .set_track_muted(index, track.state.muted)
            .ok_or_else(|| format!("track {index} is missing"))?;
        piano_roll
            .set_track_soloed(index, track.state.soloed)
            .ok_or_else(|| format!("track {index} is missing"))?;
    }
    piano_roll.set_global_solo_active(
        mixer.tracks().iter().any(|track| track.state.soloed)
            || mixer.buses().iter().any(|bus| bus.state.soloed),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{Lilypalooza, MixerHistoryMode, mixer_message_history_mode};
    use crate::app::RenameTarget;
    use crate::app::messages::{MixerMessage, PromptMessage};
    use crate::app::processor_editor_windows::EditorTarget;
    use crate::state::ProjectState;
    use iced::Size;
    use lilypalooza_audio::{
        AudioEngine, AudioEngineOptions, BusId, EditorError, EditorParent, EditorSession,
        EditorSize, MixerState, SlotState, TrackId,
    };
    use lilypalooza_builtins::soundfont_synth::{self, SoundfontProcessorState};

    struct FakeEditorSession;

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

        fn resize(&mut self, _size: EditorSize) -> Result<(), EditorError> {
            Ok(())
        }
    }

    struct RecordingEditorSession {
        visible: Rc<RefCell<Vec<bool>>>,
        detached: Rc<RefCell<usize>>,
    }

    impl EditorSession for RecordingEditorSession {
        fn attach(&mut self, _parent: EditorParent) -> Result<(), EditorError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), EditorError> {
            *self.detached.borrow_mut() += 1;
            Ok(())
        }

        fn set_visible(&mut self, visible: bool) -> Result<(), EditorError> {
            self.visible.borrow_mut().push(visible);
            Ok(())
        }

        fn resize(&mut self, _size: EditorSize) -> Result<(), EditorError> {
            Ok(())
        }
    }

    fn test_app() -> Lilypalooza {
        let (app, _task) = super::super::super::new_with_default_test_state();
        app
    }

    fn fake_editor_parent() -> EditorParent {
        EditorParent {
            window: iced::window::raw_window_handle::RawWindowHandle::AppKit(
                iced::window::raw_window_handle::AppKitWindowHandle::new(std::ptr::NonNull::<
                    std::ffi::c_void,
                >::dangling(
                )),
            ),
            display: None,
        }
    }

    fn attach_recording_editor(
        app: &mut Lilypalooza,
        target: EditorTarget,
        detached: Rc<RefCell<usize>>,
    ) {
        let window_id = iced::window::Id::unique();
        app.processor_editor_windows.begin_open(
            target,
            "Editor".to_string(),
            true,
            Box::new(RecordingEditorSession {
                visible: Rc::new(RefCell::new(Vec::new())),
                detached,
            }),
            window_id,
        );
        app.processor_editor_windows
            .attach(window_id, None, fake_editor_parent())
            .expect("attach should succeed");
    }

    #[test]
    fn mixer_drag_value_changes_use_gesture_history() {
        assert_eq!(
            mixer_message_history_mode(&MixerMessage::SetTrackGain(0, -3.0), true),
            MixerHistoryMode::Gesture
        );
        assert_eq!(
            mixer_message_history_mode(&MixerMessage::SetTrackPan(0, 0.25), true),
            MixerHistoryMode::Gesture
        );
    }

    #[test]
    fn mixer_discrete_value_changes_use_immediate_history() {
        assert_eq!(
            mixer_message_history_mode(&MixerMessage::SetTrackGain(0, -3.0), false),
            MixerHistoryMode::Immediate
        );
        assert_eq!(
            mixer_message_history_mode(
                &MixerMessage::SelectTrackInstrument(0, crate::app::mixer::InstrumentChoice::None,),
                false
            ),
            MixerHistoryMode::Immediate
        );
        assert_eq!(
            mixer_message_history_mode(&MixerMessage::ToggleTrackInstrumentBrowser(0), false),
            MixerHistoryMode::None
        );
    }

    #[test]
    fn mixer_meter_resets_do_not_record_history() {
        assert_eq!(
            mixer_message_history_mode(&MixerMessage::ResetTrackMeter(0), false),
            MixerHistoryMode::None
        );
    }

    #[test]
    fn editor_window_title_uses_track_name_only_for_instrument_slot() {
        lilypalooza_builtins::register_all();
        let app = test_app();
        let slot = lilypalooza_audio::SlotState::built_in(
            lilypalooza_audio::BUILTIN_SOUNDFONT_ID,
            lilypalooza_audio::ProcessorState::default(),
        );

        assert_eq!(app.editor_window_title("Violin", &slot, 0), "Violin");
    }

    #[test]
    fn toggle_mixer_effect_rack_opens_and_closes_track_panel() {
        let mut app = test_app();

        let _ = app.handle_mixer_message(MixerMessage::ToggleMixerEffectRack(3));
        assert_eq!(app.open_mixer_effect_rack_tracks, vec![3]);

        let _ = app.handle_mixer_message(MixerMessage::ToggleMixerEffectRack(5));
        assert_eq!(app.open_mixer_effect_rack_tracks, vec![3, 5]);

        let _ = app.handle_mixer_message(MixerMessage::ToggleMixerEffectRack(3));
        assert_eq!(app.open_mixer_effect_rack_tracks, vec![5]);
    }

    #[test]
    fn track_rename_commits_on_focus_loss() {
        let mut app = test_app();
        app.renaming_target = Some(RenameTarget::Track(0));
        app.track_rename_was_focused = true;
        app.track_rename_value = "Lead".into();

        let _ = app.handle_track_rename_focus_changed(false);

        assert_eq!(app.track_name_override(0), Some("Lead"));
        assert!(app.renaming_target.is_none());
        assert!(app.track_rename_value.is_empty());
    }

    #[test]
    fn remove_bus_message_removes_bus() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );

        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let bus_id = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .buses()
            .first()
            .and_then(|bus| bus.bus_id)
            .expect("bus should be added");

        let _ = app.handle_mixer_message(MixerMessage::RemoveBus(bus_id.0));

        assert!(
            app.playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state()
                .bus(BusId(bus_id.0))
                .is_ok()
        );
        assert_eq!(
            app.error_prompt.as_ref().map(|prompt| prompt.title()),
            Some("Remove Bus 1?")
        );

        let _ = app.handle_prompt_message(PromptMessage::Acknowledge);

        assert!(
            app.playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state()
                .bus(BusId(bus_id.0))
                .is_err()
        );
    }

    #[test]
    fn canceling_remove_bus_prompt_keeps_bus() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );

        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let bus_id = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .buses()
            .first()
            .and_then(|bus| bus.bus_id)
            .expect("bus should be added");

        let _ = app.handle_mixer_message(MixerMessage::RemoveBus(bus_id.0));
        let _ = app.handle_prompt_message(PromptMessage::Cancel);

        assert!(app.error_prompt.is_none());
        assert!(
            app.playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state()
                .bus(BusId(bus_id.0))
                .is_ok()
        );
    }

    #[test]
    fn adding_bus_keeps_open_processor_editor_session() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        let detached = Rc::new(RefCell::new(0));
        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        attach_recording_editor(&mut app, target, Rc::clone(&detached));

        let _ = app.handle_mixer_message(MixerMessage::AddBus);

        assert_eq!(*detached.borrow(), 0);
        assert!(app.processor_editor_windows.contains_window(target));
    }

    #[test]
    fn removing_bus_detaches_only_removed_bus_and_reindexes_later_bus_editors() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let (first_bus_id, track_count) = {
            let mixer = app
                .playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state();
            (
                mixer
                    .buses()
                    .first()
                    .and_then(|bus| bus.bus_id)
                    .expect("first bus should exist")
                    .0,
                mixer.track_count(),
            )
        };
        let track_target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let removed_bus_target = EditorTarget {
            strip_index: 1 + track_count,
            slot_index: 1,
        };
        let later_bus_target = EditorTarget {
            strip_index: 1 + track_count + 1,
            slot_index: 1,
        };
        let reindexed_later_bus_target = EditorTarget {
            strip_index: 1 + track_count,
            slot_index: 1,
        };
        let track_detached = Rc::new(RefCell::new(0));
        let removed_bus_detached = Rc::new(RefCell::new(0));
        let later_bus_detached = Rc::new(RefCell::new(0));
        attach_recording_editor(&mut app, track_target, Rc::clone(&track_detached));
        attach_recording_editor(
            &mut app,
            removed_bus_target,
            Rc::clone(&removed_bus_detached),
        );
        attach_recording_editor(&mut app, later_bus_target, Rc::clone(&later_bus_detached));

        let _ = app.remove_bus_confirmed(first_bus_id);

        assert_eq!(*track_detached.borrow(), 0);
        assert_eq!(*removed_bus_detached.borrow(), 1);
        assert_eq!(*later_bus_detached.borrow(), 0);
        assert!(app.processor_editor_windows.contains_window(track_target));
        assert!(
            app.processor_editor_windows
                .contains_window(reindexed_later_bus_target)
        );
        assert!(
            !app.processor_editor_windows
                .contains_window(later_bus_target)
        );
    }

    #[test]
    fn track_instrument_without_editor_does_not_open_processor_window() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );

        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let _ = app.handle_mixer_message(MixerMessage::OpenEditor(target));

        assert!(!app.processor_editor_windows.contains_window(target));
    }

    #[test]
    fn instrument_browser_toggle_opens_and_closes_same_track() {
        let mut app = test_app();

        let _ = app.handle_mixer_message(MixerMessage::ToggleTrackInstrumentBrowser(3));

        assert_eq!(app.open_instrument_browser_track, Some(3));
        assert_eq!(
            app.open_processor_browser_target,
            Some(EditorTarget {
                strip_index: 4,
                slot_index: 0,
            })
        );
        assert_eq!(
            app.instrument_browser_backend,
            crate::app::mixer::InstrumentBrowserBackend::BuiltIn
        );
        assert!(app.instrument_browser_search.is_empty());

        let _ = app.handle_mixer_message(MixerMessage::ToggleTrackInstrumentBrowser(3));

        assert_eq!(app.open_instrument_browser_track, None);
        assert_eq!(app.open_processor_browser_target, None);
    }

    #[test]
    fn processor_browser_opens_for_master_effect_without_track_underflow() {
        let mut app = test_app();
        let target = EditorTarget {
            strip_index: 0,
            slot_index: 1,
        };

        let _ = app.handle_mixer_message(MixerMessage::ToggleProcessorBrowser(target));

        assert_eq!(app.open_processor_browser_target, Some(target));
        assert_eq!(app.open_instrument_browser_track, None);
    }

    #[test]
    fn processor_browser_reopening_keeps_selected_backend_for_session() {
        let mut app = test_app();
        let target = EditorTarget {
            strip_index: 0,
            slot_index: 1,
        };

        let _ = app.handle_mixer_message(MixerMessage::ToggleProcessorBrowser(target));
        let _ = app.handle_mixer_message(MixerMessage::SelectProcessorBrowserBackend(
            crate::app::mixer::ProcessorBrowserBackend::Clap,
        ));
        let _ = app.handle_mixer_message(MixerMessage::CloseProcessorBrowser);
        let _ = app.handle_mixer_message(MixerMessage::ToggleProcessorBrowser(target));

        assert_eq!(
            app.instrument_browser_backend,
            crate::app::mixer::ProcessorBrowserBackend::Clap
        );
    }

    #[test]
    fn selecting_track_instrument_closes_open_browser() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        app.open_processor_browser_target = Some(EditorTarget {
            strip_index: 1,
            slot_index: 0,
        });
        app.open_instrument_browser_track = Some(0);
        app.instrument_browser_search = "piano".into();

        let _ = app.handle_mixer_message(MixerMessage::SelectTrackInstrument(
            0,
            crate::app::mixer::InstrumentChoice::None,
        ));

        assert_eq!(app.open_instrument_browser_track, None);
        assert_eq!(app.open_processor_browser_target, None);
        assert!(app.instrument_browser_search.is_empty());
    }

    #[test]
    fn selecting_track_effect_adds_effect_slot() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );

        let _ = app.handle_mixer_message(MixerMessage::SelectProcessor(
            EditorTarget {
                strip_index: 1,
                slot_index: 1,
            },
            crate::app::mixer::ProcessorChoice::Processor {
                processor_id: lilypalooza_audio::BUILTIN_GAIN_ID.to_string(),
                name: "Gain".to_string(),
                backend: crate::app::mixer::ProcessorBrowserBackend::BuiltIn,
            },
        ));

        let track = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .track(TrackId(0))
            .expect("track should exist");
        assert_eq!(track.effect_count(), 1);
        assert!(matches!(
            &track.effect(0).expect("effect slot should exist").kind,
            lilypalooza_audio::ProcessorKind::BuiltIn { processor_id }
                if processor_id == lilypalooza_audio::BUILTIN_GAIN_ID
        ));
    }

    #[test]
    fn selecting_track_effect_uses_lowest_free_duplicate_label_index() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let mut first = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState::default(),
        );
        first.instance_label_index = 1;
        let mut third = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState::default(),
        );
        third.instance_label_index = 3;
        playback
            .mixer()
            .set_track_effects(TrackId(0), vec![first, third])
            .expect("effects should be installed");
        app.playback = Some(playback);

        let _ = app.handle_mixer_message(MixerMessage::SelectProcessor(
            EditorTarget {
                strip_index: 1,
                slot_index: 3,
            },
            crate::app::mixer::ProcessorChoice::Processor {
                processor_id: lilypalooza_audio::BUILTIN_GAIN_ID.to_string(),
                name: "Gain".to_string(),
                backend: crate::app::mixer::ProcessorBrowserBackend::BuiltIn,
            },
        ));

        let track = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .track(TrackId(0))
            .expect("track should exist");

        assert_eq!(
            track
                .effect(2)
                .expect("new effect should exist")
                .instance_label_index,
            2
        );
    }

    #[test]
    fn selecting_master_effect_adds_effect_slot() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );

        let _ = app.handle_mixer_message(MixerMessage::SelectProcessor(
            EditorTarget {
                strip_index: 0,
                slot_index: 1,
            },
            crate::app::mixer::ProcessorChoice::Processor {
                processor_id: lilypalooza_audio::BUILTIN_GAIN_ID.to_string(),
                name: "Gain".to_string(),
                backend: crate::app::mixer::ProcessorBrowserBackend::BuiltIn,
            },
        ));

        assert_eq!(
            app.playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state()
                .master()
                .effect_count(),
            1
        );
    }

    #[test]
    fn selecting_bus_effect_adds_effect_slot() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let bus_strip_index = {
            let mixer = app
                .playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state();
            1 + mixer.track_count()
        };

        let _ = app.handle_mixer_message(MixerMessage::SelectProcessor(
            EditorTarget {
                strip_index: bus_strip_index,
                slot_index: 1,
            },
            crate::app::mixer::ProcessorChoice::Processor {
                processor_id: lilypalooza_audio::BUILTIN_GAIN_ID.to_string(),
                name: "Gain".to_string(),
                backend: crate::app::mixer::ProcessorBrowserBackend::BuiltIn,
            },
        ));

        assert_eq!(
            app.playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state()
                .buses()
                .first()
                .expect("bus should exist")
                .effect_count(),
            1
        );
    }

    #[test]
    fn toggling_effect_slot_bypass_updates_slot_state() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        playback
            .mixer()
            .set_track_effects(
                TrackId(0),
                vec![SlotState::built_in(
                    lilypalooza_audio::BUILTIN_GAIN_ID,
                    lilypalooza_audio::ProcessorState::default(),
                )],
            )
            .expect("effect should be installed");
        app.playback = Some(playback);

        let _ = app.handle_mixer_message(MixerMessage::ToggleSlotBypass(EditorTarget {
            strip_index: 1,
            slot_index: 1,
        }));

        assert!(
            app.playback
                .as_ref()
                .expect("playback should exist")
                .mixer_state()
                .track(TrackId(0))
                .expect("track should exist")
                .effect(0)
                .expect("effect should exist")
                .bypassed
        );
    }

    #[test]
    fn moving_track_effect_reorders_effect_slots() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let first = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState(vec![1]),
        );
        let second = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState(vec![2]),
        );
        playback
            .mixer()
            .set_track_effects(TrackId(0), vec![first.clone(), second.clone()])
            .expect("effects should be installed");
        app.playback = Some(playback);

        let _ = app.handle_mixer_message(MixerMessage::MoveTrackEffect {
            strip_index: 1,
            from_effect_index: 0,
            to_effect_index: 1,
        });

        let track = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .track(TrackId(0))
            .expect("track should exist");
        assert_eq!(track.effect(0), Some(&second));
        assert_eq!(track.effect(1), Some(&first));
    }

    #[test]
    fn dropping_dragged_track_effect_reorders_effect_slots() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let first = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState(vec![1]),
        );
        let second = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState(vec![2]),
        );
        playback
            .mixer()
            .set_track_effects(TrackId(0), vec![first.clone(), second.clone()])
            .expect("effects should be installed");
        app.playback = Some(playback);

        let _ = app.handle_mixer_message(MixerMessage::StartTrackEffectDrag {
            strip_index: 1,
            effect_index: 0,
        });
        let _ = app.handle_mixer_message(MixerMessage::DropTrackEffect {
            strip_index: 1,
            effect_index: 1,
        });

        let track = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .track(TrackId(0))
            .expect("track should exist");
        assert_eq!(track.effect(0), Some(&second));
        assert_eq!(track.effect(1), Some(&first));
        assert_eq!(app.effect_drag_source, None);
    }

    #[test]
    fn rack_hover_press_drag_release_reorders_effect_slots() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let first = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState(vec![1]),
        );
        let second = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState(vec![2]),
        );
        let third = SlotState::built_in(
            lilypalooza_audio::BUILTIN_GAIN_ID,
            lilypalooza_audio::ProcessorState(vec![3]),
        );
        playback
            .mixer()
            .set_track_effects(
                TrackId(0),
                vec![first.clone(), second.clone(), third.clone()],
            )
            .expect("effects should be installed");
        app.playback = Some(playback);
        let row_height = crate::app::mixer::EFFECT_RACK_HEIGHT / 7.0;

        let _ = app.handle_mixer_message(MixerMessage::TrackEffectDragMoved {
            strip_index: 1,
            y: row_height * 0.5,
        });
        let _ = app.handle_primary_mouse_pressed(true);
        let _ = app.handle_mixer_message(MixerMessage::TrackEffectDragMoved {
            strip_index: 1,
            y: row_height * 2.5,
        });
        let _ = app.handle_primary_mouse_pressed(false);

        let track = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .track(TrackId(0))
            .expect("track should exist");
        assert_eq!(track.effect(0), Some(&second));
        assert_eq!(track.effect(1), Some(&third));
        assert_eq!(track.effect(2), Some(&first));
    }

    #[test]
    fn processor_slot_hover_can_start_effect_drag() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        playback
            .mixer()
            .set_track_effects(
                TrackId(0),
                vec![SlotState::built_in(
                    lilypalooza_audio::BUILTIN_GAIN_ID,
                    lilypalooza_audio::ProcessorState(vec![1]),
                )],
            )
            .expect("effects should be installed");
        app.playback = Some(playback);

        let target = EditorTarget {
            strip_index: 1,
            slot_index: 1,
        };
        let _ = app.handle_mixer_message(MixerMessage::SetProcessorSlotHovered(Some((
            target,
            crate::app::mixer::ProcessorSlotSegment::Editor,
        ))));
        let _ = app.handle_primary_mouse_pressed(true);

        assert_eq!(app.effect_drag_source, Some((1, 0)));
        assert_eq!(app.effect_drag_target, Some((1, 0)));
    }

    #[test]
    fn rack_drag_target_uses_scroll_offset() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let effects: Vec<_> = (1..=4)
            .map(|value| {
                SlotState::built_in(
                    lilypalooza_audio::BUILTIN_GAIN_ID,
                    lilypalooza_audio::ProcessorState(vec![value]),
                )
            })
            .collect();
        playback
            .mixer()
            .set_track_effects(TrackId(0), effects.clone())
            .expect("effects should be installed");
        app.playback = Some(playback);
        let row_height = crate::app::mixer::EFFECT_RACK_ROW_HEIGHT;
        app.effect_rack_scroll_y.insert(1, row_height * 2.0);

        let _ = app.handle_mixer_message(MixerMessage::StartTrackEffectDrag {
            strip_index: 1,
            effect_index: 0,
        });
        let _ = app.handle_mixer_message(MixerMessage::TrackEffectDragMoved {
            strip_index: 1,
            y: row_height * 1.5,
        });
        let _ = app.handle_primary_mouse_pressed(false);

        let track = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .track(TrackId(0))
            .expect("track should exist");
        assert_eq!(track.effect(0), Some(&effects[1]));
        assert_eq!(track.effect(1), Some(&effects[2]));
        assert_eq!(track.effect(2), Some(&effects[3]));
        assert_eq!(track.effect(3), Some(&effects[0]));
    }

    #[test]
    fn rack_drag_near_bottom_enables_push_to_scroll_until_release() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        playback
            .mixer()
            .set_track_effects(
                TrackId(0),
                vec![SlotState::built_in(
                    lilypalooza_audio::BUILTIN_GAIN_ID,
                    lilypalooza_audio::ProcessorState(vec![1]),
                )],
            )
            .expect("effects should be installed");
        app.playback = Some(playback);
        app.effect_rack_viewport_height
            .insert(1, crate::app::mixer::EFFECT_RACK_HEIGHT);

        let _ = app.handle_mixer_message(MixerMessage::StartTrackEffectDrag {
            strip_index: 1,
            effect_index: 0,
        });
        let _ = app.handle_mixer_message(MixerMessage::TrackEffectDragMoved {
            strip_index: 1,
            y: crate::app::mixer::EFFECT_RACK_HEIGHT - 1.0,
        });

        assert_eq!(app.effect_rack_autoscroll_direction, 1);

        let _ = app.handle_primary_mouse_pressed(false);

        assert_eq!(app.effect_rack_autoscroll_direction, 0);
    }

    #[test]
    fn rack_cursor_exit_clears_hovered_effect_before_mouse_press() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        playback
            .mixer()
            .set_track_effects(
                TrackId(0),
                vec![SlotState::built_in(
                    lilypalooza_audio::BUILTIN_GAIN_ID,
                    lilypalooza_audio::ProcessorState(vec![1]),
                )],
            )
            .expect("effects should be installed");
        app.playback = Some(playback);

        let _ = app.handle_mixer_message(MixerMessage::TrackEffectDragMoved {
            strip_index: 1,
            y: crate::app::mixer::EFFECT_RACK_ROW_HEIGHT * 0.5,
        });
        let _ = app.handle_mixer_message(MixerMessage::EffectRackCursorLeft(1));
        let _ = app.handle_primary_mouse_pressed(true);

        assert_eq!(app.effect_rack_hovered_effect, None);
        assert_eq!(app.effect_drag_source, None);
    }

    #[test]
    fn selecting_track_instrument_opens_editor_when_available() {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        playback
            .mixer()
            .set_soundfont(lilypalooza_audio::SoundfontResource {
                id: "default".to_string(),
                name: "FluidR3".to_string(),
                path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/soundfonts/lilypalooza-test.sf2")
                    .canonicalize()
                    .expect("test SoundFont should exist"),
            })
            .expect("test SoundFont should load");
        app.playback = Some(playback);

        let _ = app.handle_mixer_message(MixerMessage::SelectTrackInstrument(
            0,
            crate::app::mixer::InstrumentChoice::Processor {
                processor_id: lilypalooza_audio::BUILTIN_SOUNDFONT_ID.to_string(),
                name: "SF-01".to_string(),
                backend: crate::app::mixer::InstrumentBrowserBackend::BuiltIn,
            },
        ));

        assert!(
            app.processor_editor_windows
                .focus_existing(EditorTarget {
                    strip_index: 1,
                    slot_index: 0,
                })
                .is_some()
        );
    }

    fn app_with_soundfont_track() -> (Lilypalooza, EditorTarget) {
        lilypalooza_builtins::register_all();
        let mut app = test_app();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        playback
            .mixer()
            .set_soundfont(lilypalooza_audio::SoundfontResource {
                id: "default".to_string(),
                name: "FluidR3".to_string(),
                path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/soundfonts/lilypalooza-test.sf2")
                    .canonicalize()
                    .expect("test SoundFont should exist"),
            })
            .expect("test SoundFont should load");
        app.playback = Some(playback);
        let _ = app.handle_mixer_message(MixerMessage::SelectTrackInstrument(
            0,
            crate::app::mixer::InstrumentChoice::Processor {
                processor_id: lilypalooza_audio::BUILTIN_SOUNDFONT_ID.to_string(),
                name: "SF-01".to_string(),
                backend: crate::app::mixer::InstrumentBrowserBackend::BuiltIn,
            },
        ));
        let temp = tempfile::tempdir().expect("temp project dir should exist");
        app.project_root = Some(temp.path().to_path_buf());
        (
            app,
            EditorTarget {
                strip_index: 1,
                slot_index: 0,
            },
        )
    }

    #[test]
    fn processor_frame_save_command_creates_user_preset_for_slot() {
        let (mut app, target) = app_with_soundfont_track();

        app.handle_processor_editor_frame_command(
            target,
            editor_host::EditorFrameCommand::SavePreset,
        );

        let kind = app.processor_kind_for_target(target).expect("slot kind");
        let presets = app.processor_presets.presets_for(&kind);
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "User Preset 1");
    }

    #[test]
    fn processor_frame_load_command_updates_slot_state() {
        let (mut app, target) = app_with_soundfont_track();
        let kind = app.processor_kind_for_target(target).expect("slot kind");
        let state = soundfont_synth::encode_state(&SoundfontProcessorState {
            program: 7,
            output_gain: 0.25,
            ..SoundfontProcessorState::default()
        });
        let id = app
            .processor_presets
            .save_user_preset("Muted Piano", kind, state.clone());

        app.handle_processor_editor_frame_command(
            target,
            editor_host::EditorFrameCommand::LoadPreset(id),
        );

        let slot_state = app
            .playback
            .as_ref()
            .expect("playback")
            .mixer_state()
            .strip_by_index(target.strip_index)
            .and_then(|strip| strip.slot(target.slot_index))
            .map(|slot| slot.state.clone());
        assert_eq!(slot_state, Some(state));
    }

    #[test]
    fn processor_frame_next_from_placeholder_loads_first_preset() {
        let (mut app, target) = app_with_soundfont_track();
        let kind = app.processor_kind_for_target(target).expect("slot kind");
        let first_state = soundfont_synth::encode_state(&SoundfontProcessorState {
            program: 1,
            ..SoundfontProcessorState::default()
        });
        let second_state = soundfont_synth::encode_state(&SoundfontProcessorState {
            program: 2,
            ..SoundfontProcessorState::default()
        });
        app.processor_presets
            .save_user_preset("First", kind.clone(), first_state.clone());
        app.processor_presets
            .save_user_preset("Second", kind, second_state);
        app.refresh_editor_preset_state(target, None);

        app.handle_processor_editor_frame_command(
            target,
            editor_host::EditorFrameCommand::NextPreset,
        );

        let slot_state = app
            .playback
            .as_ref()
            .expect("playback")
            .mixer_state()
            .strip_by_index(target.strip_index)
            .and_then(|strip| strip.slot(target.slot_index))
            .map(|slot| slot.state.clone());
        assert_eq!(slot_state, Some(first_state));
    }

    #[test]
    fn processor_frame_rename_command_updates_user_preset() {
        let (mut app, target) = app_with_soundfont_track();
        let kind = app.processor_kind_for_target(target).expect("slot kind");
        let id = app.processor_presets.save_user_preset(
            "Warm Piano",
            kind.clone(),
            lilypalooza_audio::ProcessorState(vec![]),
        );

        app.handle_processor_editor_frame_command(
            target,
            editor_host::EditorFrameCommand::RenamePreset {
                id: id.clone(),
                name: "Soft Piano".to_string(),
            },
        );

        assert_eq!(
            app.processor_presets.presets_for(&kind)[0].name,
            "Soft Piano"
        );
    }

    #[test]
    fn processor_frame_delete_command_removes_user_preset() {
        let (mut app, target) = app_with_soundfont_track();
        let kind = app.processor_kind_for_target(target).expect("slot kind");
        let id = app.processor_presets.save_user_preset(
            "Warm Piano",
            kind.clone(),
            lilypalooza_audio::ProcessorState(vec![]),
        );

        app.handle_processor_editor_frame_command(
            target,
            editor_host::EditorFrameCommand::DeletePreset(id),
        );

        assert!(app.processor_presets.presets_for(&kind).is_empty());
    }

    #[test]
    fn processor_frame_toggle_command_tracks_expanded_browser_target() {
        let (mut app, target) = app_with_soundfont_track();

        app.handle_processor_editor_frame_command(
            target,
            editor_host::EditorFrameCommand::TogglePresetBrowser,
        );

        assert_eq!(app.expanded_processor_preset_browser, Some(target));

        app.handle_processor_editor_frame_command(
            target,
            editor_host::EditorFrameCommand::TogglePresetBrowser,
        );

        assert_eq!(app.expanded_processor_preset_browser, None);
    }

    #[test]
    fn mixer_main_route_message_updates_track_and_bus_routes() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let bus_ids: Vec<_> = app
            .playback
            .as_ref()
            .expect("playback")
            .mixer_state()
            .buses()
            .iter()
            .filter_map(|bus| bus.bus_id.map(|id| id.0))
            .collect();

        let _ = app.handle_mixer_message(MixerMessage::SetMainRoute(
            crate::app::mixer::RoutingStrip::Track(0),
            lilypalooza_audio::TrackRoute::Bus(BusId(bus_ids[0])),
        ));
        let _ = app.handle_mixer_message(MixerMessage::SetMainRoute(
            crate::app::mixer::RoutingStrip::Bus(bus_ids[0]),
            lilypalooza_audio::TrackRoute::Bus(BusId(bus_ids[1])),
        ));
        let _ = app.handle_mixer_message(MixerMessage::SetMainRoute(
            crate::app::mixer::RoutingStrip::Bus(bus_ids[0]),
            lilypalooza_audio::TrackRoute::Bus(BusId(bus_ids[0])),
        ));

        let mixer = app.playback.as_ref().expect("playback").mixer_state();
        assert_eq!(
            mixer.track(TrackId(0)).expect("track").routing.main,
            lilypalooza_audio::TrackRoute::Bus(BusId(bus_ids[0]))
        );
        assert_eq!(
            mixer.bus(BusId(bus_ids[0])).expect("bus").routing.main,
            lilypalooza_audio::TrackRoute::Bus(BusId(bus_ids[1]))
        );
    }

    #[test]
    fn mixer_send_messages_update_destination_gain_enabled_and_prepost() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let bus_ids: Vec<_> = app
            .playback
            .as_ref()
            .expect("playback")
            .mixer_state()
            .buses()
            .iter()
            .filter_map(|bus| bus.bus_id.map(|id| id.0))
            .collect();
        let source = crate::app::mixer::RoutingStrip::Track(0);

        let _ = app.handle_mixer_message(MixerMessage::AddSend(source, bus_ids[0]));
        let _ = app.handle_mixer_message(MixerMessage::SetSendDestination(source, 0, bus_ids[1]));
        let _ = app.handle_mixer_message(MixerMessage::SetSendGain(source, 0, -7.5));
        let _ = app.handle_mixer_message(MixerMessage::ToggleSendEnabled(source, 0));
        let _ = app.handle_mixer_message(MixerMessage::ToggleSendPreFader(source, 0));

        let send = app
            .playback
            .as_ref()
            .expect("playback")
            .mixer_state()
            .track(TrackId(0))
            .expect("track")
            .routing
            .sends[0];
        assert_eq!(send.bus_id, BusId(bus_ids[1]));
        assert_eq!(send.gain_db, -7.5);
        assert!(!send.enabled);
        assert!(send.pre_fader);
    }

    #[test]
    fn removing_bus_from_app_clears_routes_and_sends() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let bus_id = app
            .playback
            .as_ref()
            .expect("playback")
            .mixer_state()
            .buses()
            .first()
            .and_then(|bus| bus.bus_id.map(|id| id.0))
            .expect("bus");

        let _ = app.handle_mixer_message(MixerMessage::SetMainRoute(
            crate::app::mixer::RoutingStrip::Track(0),
            lilypalooza_audio::TrackRoute::Bus(BusId(bus_id)),
        ));
        let _ = app.handle_mixer_message(MixerMessage::AddSend(
            crate::app::mixer::RoutingStrip::Track(0),
            bus_id,
        ));
        let _ = app.remove_bus_confirmed(bus_id);

        let track = app
            .playback
            .as_ref()
            .expect("playback")
            .mixer_state()
            .track(TrackId(0))
            .expect("track");
        assert_eq!(track.routing.main, lilypalooza_audio::TrackRoute::Master);
        assert!(track.routing.sends.is_empty());
    }

    #[test]
    fn mixer_changes_mark_project_dirty() {
        let mut app = test_app();
        let temp = tempfile::tempdir().expect("temp dir should exist");
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        app.apply_project_state(temp.path().to_path_buf(), ProjectState::default());
        assert!(!app.project_is_dirty());

        let _ = app.handle_mixer_message(MixerMessage::AddBus);
        let bus_id = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .buses()
            .first()
            .and_then(|bus| bus.bus_id.map(|id| id.0))
            .expect("bus should be added");
        let _ = app.handle_mixer_message(MixerMessage::SetBusGain(bus_id, -6.0));

        assert!(app.project_is_dirty());
    }

    #[test]
    fn unsaved_project_mixer_changes_prompt_on_close() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        app.saved_project_state = Some(app.current_project_state());

        let _ = app.handle_mixer_message(MixerMessage::AddBus);

        let _ = app.handle_window_close_requested(app.main_window_id);

        assert!(matches!(
            app.pending_editor_action,
            Some(crate::app::PendingEditorAction::ResolveDirtyProject {
                continuation: crate::app::EditorContinuation::ExitApp
            })
        ));
    }

    #[test]
    fn editor_window_close_request_hides_only_editor_window() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        app.saved_project_state = Some(app.current_project_state());
        let _ = app.handle_mixer_message(MixerMessage::AddBus);

        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let window_id = iced::window::Id::unique();
        app.processor_editor_windows.begin_open(
            target,
            "Track 1".to_string(),
            true,
            Box::new(FakeEditorSession),
            window_id,
        );
        app.processor_editor_windows
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

        let _ = app.handle_window_close_requested(window_id);

        assert!(app.processor_editor_windows.contains_window(target));
        assert!(app.pending_editor_action.is_none());
    }

    #[test]
    fn processor_editor_window_settings_use_session_reported_content_size() {
        let descriptor = lilypalooza_audio::EditorDescriptor {
            default_size: EditorSize {
                width: 720,
                height: 480,
            },
            min_size: Some(EditorSize {
                width: 320,
                height: 220,
            }),
            resizable: true,
        };

        let settings = super::processor_editor_window_settings(
            descriptor,
            Some(EditorSize {
                width: 936,
                height: 612,
            }),
        );

        assert_eq!(settings.size, Size::new(936.0, 612.0));
        assert_eq!(settings.min_size, Some(Size::new(320.0, 220.0)));
        assert!(settings.resizable);
        assert!(!settings.decorations);
    }

    #[test]
    fn processor_editor_window_settings_can_disable_resizing() {
        let descriptor = lilypalooza_audio::EditorDescriptor {
            default_size: EditorSize {
                width: 720,
                height: 480,
            },
            min_size: None,
            resizable: false,
        };

        let settings = super::processor_editor_window_settings(descriptor, None);

        assert!(!settings.resizable);
    }

    #[test]
    fn main_window_close_request_hides_editors_before_prompt() {
        let mut app = test_app();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        app.saved_project_state = Some(app.current_project_state());
        let _ = app.handle_mixer_message(MixerMessage::AddBus);

        let visible = Rc::new(RefCell::new(Vec::new()));
        let detached = Rc::new(RefCell::new(0));
        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let window_id = iced::window::Id::unique();
        app.processor_editor_windows.begin_open(
            target,
            "Track 1".to_string(),
            true,
            Box::new(RecordingEditorSession {
                visible: Rc::clone(&visible),
                detached: Rc::clone(&detached),
            }),
            window_id,
        );
        app.processor_editor_windows
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

        let _ = app.handle_window_close_requested(app.main_window_id);

        assert_eq!(*visible.borrow(), vec![false]);
        assert_eq!(*detached.borrow(), 0);
        assert!(app.processor_editor_windows.contains_window(target));
        assert!(matches!(
            app.pending_editor_action,
            Some(crate::app::PendingEditorAction::ResolveDirtyProject {
                continuation: crate::app::EditorContinuation::ExitApp
            })
        ));
    }

    #[test]
    fn exit_app_detaches_editor_sessions() {
        let mut app = test_app();
        let visible = Rc::new(RefCell::new(Vec::new()));
        let detached = Rc::new(RefCell::new(0));
        let target = EditorTarget {
            strip_index: 1,
            slot_index: 0,
        };
        let window_id = iced::window::Id::unique();
        app.processor_editor_windows.begin_open(
            target,
            "Track 1".to_string(),
            true,
            Box::new(RecordingEditorSession {
                visible,
                detached: Rc::clone(&detached),
            }),
            window_id,
        );
        app.processor_editor_windows
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

        let _ = app.exit_app();

        assert_eq!(*detached.borrow(), 1);
        assert!(!app.processor_editor_windows.contains_window(target));
    }
}
