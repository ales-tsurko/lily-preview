use super::*;
use crate::app::editor::EditorTabFileState;
use crate::app::messages::ShortcutsMessage;
use crate::app::piano_roll::{adjacent_subdivision_tick, roll_scroll_id};
use crate::error_prompt::{PromptButtons, PromptSelectedButton};

impl Lilypalooza {
    fn dispatch_active_editor_widget_message(
        &mut self,
        message: iced_code_editor::Message,
    ) -> Task<Message> {
        update(
            self,
            Message::Editor(EditorMessage::ActiveWidgetMessage(message)),
        )
    }

    pub(in crate::app) fn sync_editor_widget_focus(&mut self) {
        if self.focused_workspace_pane == Some(WorkspacePaneKind::Editor)
            && !self.editor_file_browser_focused
        {
            self.editor.request_focus();
        } else {
            self.editor.lose_focus();
        }
    }

    pub(in crate::app) fn focus_editor_file_browser(&mut self) {
        self.focused_workspace_pane = Some(WorkspacePaneKind::Editor);
        self.editor_file_browser_focused = true;
        self.editor.lose_focus();
    }

    pub(in crate::app) fn focus_editor_text_area(&mut self) {
        self.editor_file_browser_focused = false;
        self.sync_editor_widget_focus();
    }

    pub(in crate::app) fn handle_key_pressed(&mut self, key_press: KeyPress) -> Task<Message> {
        if self.error_prompt.is_some() {
            return self.handle_prompt_key_pressed(key_press);
        }

        if self.open_shortcuts_dialog {
            match key_press.key {
                keyboard::Key::Named(keyboard::key::Named::Escape) => {
                    return update(self, Message::Shortcuts(ShortcutsMessage::CloseDialog));
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                    return update(self, Message::Shortcuts(ShortcutsMessage::SelectNext));
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                    return update(self, Message::Shortcuts(ShortcutsMessage::SelectPrevious));
                }
                keyboard::Key::Named(keyboard::key::Named::Enter) => {
                    return update(self, Message::Shortcuts(ShortcutsMessage::ActivateSelected));
                }
                _ => {}
            }

            return Task::none();
        }

        if self.renaming_editor_tab.is_some()
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Escape)
            )
        {
            return update(self, Message::Editor(EditorMessage::CancelRename));
        }
        if self.browser_inline_edit.is_some()
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Escape)
            )
        {
            return update(
                self,
                Message::Editor(EditorMessage::CancelFileBrowserInlineEdit),
            );
        }
        if self.browser_inline_edit.is_some()
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Enter)
            )
        {
            return update(
                self,
                Message::Editor(EditorMessage::CommitFileBrowserInlineEdit),
            );
        }
        if self.renaming_editor_tab.is_some()
            && matches!(key_press.status, iced::event::Status::Captured)
        {
            return Task::none();
        }
        if (self.renaming_target.is_some() || self.track_color_picker_target.is_some())
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Escape)
            )
        {
            return match self.renaming_origin {
                Some(WorkspacePaneKind::PianoRoll) => update(
                    self,
                    Message::PianoRoll(PianoRollMessage::CancelTrackRename),
                ),
                Some(WorkspacePaneKind::Mixer) => {
                    update(self, Message::Mixer(MixerMessage::CancelTrackRename))
                }
                _ => {
                    self.cancel_track_rename();
                    Task::none()
                }
            };
        }
        if self.browser_inline_edit.is_some()
            && matches!(key_press.status, iced::event::Status::Captured)
        {
            return Task::none();
        }
        if self.metronome_menu_open
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Escape)
            )
        {
            return update(
                self,
                Message::PianoRoll(PianoRollMessage::TransportCloseMetronomeMenu),
            );
        }
        if self.open_processor_browser_target.is_some()
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Escape)
            )
        {
            return update(self, Message::Mixer(MixerMessage::CloseProcessorBrowser));
        }
        if self.renaming_target.is_some()
            && matches!(key_press.status, iced::event::Status::Captured)
        {
            return Task::none();
        }
        if self.editor_file_browser_focused
            && matches!(key_press.status, iced::event::Status::Captured)
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Enter)
            )
        {
            return Task::none();
        }

        if self.focused_workspace_pane() == Some(WorkspacePaneKind::Editor)
            && matches!(
                key_press.key,
                keyboard::Key::Named(keyboard::key::Named::Escape)
            )
        {
            return Task::batch([
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::CloseSearch),
                self.dispatch_active_editor_widget_message(
                    iced_code_editor::Message::CloseGotoLine,
                ),
            ]);
        }

        let shortcut_input =
            ShortcutInput::new(&key_press.key, key_press.physical_key, key_press.modifiers);

        if let Some(action) = shortcuts::resolve_global(&self.shortcut_settings, shortcut_input) {
            if action == ShortcutAction::ToggleMetronome
                && self.focused_workspace_pane() == Some(WorkspacePaneKind::Editor)
            {
                return Task::none();
            }
            return self.handle_shortcut_action(action);
        }

        if let Some(action) = shortcuts::resolve_navigation(&self.shortcut_settings, shortcut_input)
        {
            return self.handle_shortcut_action(action);
        }

        if self.editor_file_browser_focused {
            match key_press.key {
                keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                    return self.handle_editor_file_browser_move(-1);
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                    return self.handle_editor_file_browser_move(1);
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => {
                    return self.handle_editor_file_browser_column(false);
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowRight) => {
                    return self.handle_editor_file_browser_column(true);
                }
                _ => {}
            }
        }

        let Some(focused_pane) = self.focused_workspace_pane() else {
            return Task::none();
        };

        if focused_pane == WorkspacePaneKind::Editor {
            if self.editor_file_browser_focused
                && let Some(action) =
                    shortcuts::resolve_editor_browser(&self.shortcut_settings, shortcut_input)
            {
                return self.handle_shortcut_action(action);
            }
            if let Some(action) =
                shortcuts::resolve_contextual(&self.shortcut_settings, focused_pane, shortcut_input)
            {
                return self.handle_shortcut_action(action);
            }
        } else if (key_press.modifiers.command() || key_press.modifiers.control())
            && let Some(action) =
                shortcuts::resolve_contextual(&self.shortcut_settings, focused_pane, shortcut_input)
        {
            return self.handle_shortcut_action(action);
        }

        if matches!(key_press.status, iced::event::Status::Captured) {
            return Task::none();
        }

        if let Some(action) =
            shortcuts::resolve_contextual(&self.shortcut_settings, focused_pane, shortcut_input)
        {
            return self.handle_shortcut_action(action);
        }

        Task::none()
    }

    fn handle_prompt_key_pressed(&mut self, key_press: KeyPress) -> Task<Message> {
        match key_press.key {
            keyboard::Key::Named(keyboard::key::Named::Enter) => {
                update(self, Message::Prompt(self.selected_prompt_message()))
            }
            keyboard::Key::Named(keyboard::key::Named::Tab)
            | keyboard::Key::Named(keyboard::key::Named::ArrowRight) => {
                self.cycle_prompt_selection(if key_press.modifiers.shift() { -1 } else { 1 });
                Task::none()
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => {
                self.cycle_prompt_selection(-1);
                Task::none()
            }
            keyboard::Key::Named(keyboard::key::Named::Escape) => {
                if self.prompt_has_button(PromptSelectedButton::Cancel) {
                    update(self, Message::Prompt(PromptMessage::Cancel))
                } else {
                    Task::none()
                }
            }
            _ => Task::none(),
        }
    }

    fn selected_prompt_message(&self) -> PromptMessage {
        match self.prompt_selected_button {
            PromptSelectedButton::Ok => PromptMessage::Acknowledge,
            PromptSelectedButton::Discard => PromptMessage::Discard,
            PromptSelectedButton::Cancel => PromptMessage::Cancel,
        }
    }

    fn prompt_has_button(&self, button: PromptSelectedButton) -> bool {
        let Some(prompt) = &self.error_prompt else {
            return false;
        };
        prompt_buttons(prompt.buttons()).contains(&button)
    }

    fn cycle_prompt_selection(&mut self, direction: i8) {
        let Some(prompt) = &self.error_prompt else {
            return;
        };
        let buttons = prompt_buttons(prompt.buttons());
        let Some(current_index) = buttons
            .iter()
            .position(|candidate| *candidate == self.prompt_selected_button)
        else {
            self.prompt_selected_button = buttons[0];
            return;
        };
        let next_index = if direction < 0 {
            current_index.checked_sub(1).unwrap_or(buttons.len() - 1)
        } else {
            (current_index + 1) % buttons.len()
        };
        self.prompt_selected_button = buttons[next_index];
    }

    pub(in crate::app) fn handle_shortcuts_message(
        &mut self,
        message: ShortcutsMessage,
    ) -> Task<Message> {
        match message {
            ShortcutsMessage::OpenDialog => {
                self.open_project_menu = false;
                self.open_project_menu_section = None;
                self.open_project_recent = false;
                self.open_header_overflow_menu = None;
                self.open_editor_menu_section = None;
                self.open_editor_file_menu_section = None;
                self.hovered_editor_file_menu_section = None;
                self.open_shortcuts_dialog = true;
                self.editor.lose_focus();
                self.shortcuts_search_query.clear();
                self.shortcuts_selected_action = shortcuts::filtered_action_metadata("")
                    .first()
                    .map(|metadata| metadata.id);
                Task::batch([
                    iced::widget::operation::focus(self.shortcuts_search_input_id.clone()),
                    self.reveal_selected_shortcut_action(),
                ])
            }
            ShortcutsMessage::CloseDialog => {
                self.open_shortcuts_dialog = false;
                self.shortcuts_search_query.clear();
                self.shortcuts_selected_action = None;
                self.sync_editor_widget_focus();
                Task::none()
            }
            ShortcutsMessage::SearchChanged(value) => {
                if self.shortcut_modifier_active() {
                    return Task::none();
                }
                self.shortcuts_search_query = value;
                self.reconcile_shortcut_palette_selection()
            }
            ShortcutsMessage::SelectNext => self.move_shortcut_palette_selection(1),
            ShortcutsMessage::SelectPrevious => self.move_shortcut_palette_selection(-1),
            ShortcutsMessage::ActivateSelected => {
                let Some(action_id) = self.shortcuts_selected_action else {
                    return Task::none();
                };
                self.handle_shortcuts_message(ShortcutsMessage::ActivateAction(action_id))
            }
            ShortcutsMessage::ActivateAction(action_id) => {
                self.open_shortcuts_dialog = false;
                self.shortcuts_search_query.clear();
                self.shortcuts_selected_action = None;
                self.sync_editor_widget_focus();
                self.handle_shortcut_action(shortcuts::action_for_id(action_id))
            }
        }
    }

    fn shortcut_palette_actions(&self) -> Vec<crate::shortcuts::ShortcutActionMetadata> {
        shortcuts::filtered_action_metadata(&self.shortcuts_search_query)
    }

    fn reconcile_shortcut_palette_selection(&mut self) -> Task<Message> {
        let actions = self.shortcut_palette_actions();
        self.shortcuts_selected_action = if actions
            .iter()
            .any(|metadata| Some(metadata.id) == self.shortcuts_selected_action)
        {
            self.shortcuts_selected_action
        } else {
            actions.first().map(|metadata| metadata.id)
        };
        self.reveal_selected_shortcut_action()
    }

    fn move_shortcut_palette_selection(&mut self, delta: i32) -> Task<Message> {
        let actions = self.shortcut_palette_actions();
        if actions.is_empty() {
            self.shortcuts_selected_action = None;
            return Task::none();
        }

        let current_index = self
            .shortcuts_selected_action
            .and_then(|selected| actions.iter().position(|metadata| metadata.id == selected))
            .unwrap_or(0);
        let next_index = (current_index as i32 + delta).clamp(0, actions.len() as i32 - 1);
        self.shortcuts_selected_action = Some(actions[next_index as usize].id);
        self.reveal_selected_shortcut_action()
    }

    fn reveal_selected_shortcut_action(&self) -> Task<Message> {
        let Some(selected) = self.shortcuts_selected_action else {
            return Task::none();
        };
        let Some(index) = self
            .shortcut_palette_actions()
            .iter()
            .position(|metadata| metadata.id == selected)
        else {
            return Task::none();
        };

        iced::widget::operation::scroll_to(
            super::SHORTCUTS_SCROLLABLE_ID,
            iced::widget::operation::AbsoluteOffset {
                x: 0.0,
                y: index as f32 * super::SHORTCUTS_ACTION_ROW_HEIGHT,
            },
        )
    }

    pub(in crate::app) fn handle_shortcut_action(
        &mut self,
        action: ShortcutAction,
    ) -> Task<Message> {
        match action {
            ShortcutAction::QuitApp => self.handle_window_close_requested(self.main_window_id),
            ShortcutAction::OpenActions => {
                update(self, Message::Shortcuts(ShortcutsMessage::OpenDialog))
            }
            ShortcutAction::OpenSettingsFile => self.open_settings_file_in_editor(),
            ShortcutAction::NewEditor => update(self, Message::Editor(EditorMessage::NewRequested)),
            ShortcutAction::OpenEditorFile => {
                update(self, Message::Editor(EditorMessage::OpenRequested))
            }
            ShortcutAction::ToggleFileBrowser => {
                update(self, Message::Editor(EditorMessage::ToggleFileBrowser))
            }
            ShortcutAction::FileBrowserUndo => self.undo_browser_operation(),
            ShortcutAction::FileBrowserRedo => self.redo_browser_operation(),
            ShortcutAction::FileBrowserCut => update(
                self,
                Message::Editor(EditorMessage::FileBrowserCutRequested),
            ),
            ShortcutAction::FileBrowserCopy => update(
                self,
                Message::Editor(EditorMessage::FileBrowserCopyRequested),
            ),
            ShortcutAction::FileBrowserPaste => update(
                self,
                Message::Editor(EditorMessage::FileBrowserPasteRequested),
            ),
            ShortcutAction::FileBrowserRename => update(
                self,
                Message::Editor(EditorMessage::FileBrowserRenameRequested),
            ),
            ShortcutAction::FileBrowserDelete => update(
                self,
                Message::Editor(EditorMessage::FileBrowserTrashRequested),
            ),
            ShortcutAction::SaveEditor => {
                update(self, Message::Editor(EditorMessage::SaveRequested))
            }
            ShortcutAction::EditorUndo => {
                if self.focused_workspace_pane == Some(WorkspacePaneKind::Mixer) {
                    self.undo_mixer_operation()
                } else {
                    self.dispatch_active_editor_widget_message(iced_code_editor::Message::Undo)
                }
            }
            ShortcutAction::EditorRedo => {
                if self.focused_workspace_pane == Some(WorkspacePaneKind::Mixer) {
                    self.redo_mixer_operation()
                } else {
                    self.dispatch_active_editor_widget_message(iced_code_editor::Message::Redo)
                }
            }
            ShortcutAction::EditorCopy => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::Copy)
            }
            ShortcutAction::EditorPaste => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::Paste(String::new()),
            ),
            ShortcutAction::EditorOpenSearch => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::OpenSearch)
            }
            ShortcutAction::EditorOpenSearchReplace => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::OpenSearchReplace,
            ),
            ShortcutAction::EditorOpenGotoLine => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::OpenGotoLine)
            }
            ShortcutAction::EditorTriggerCompletion => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::TriggerCompletion,
            ),
            ShortcutAction::EditorFindNext => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::FindNext)
            }
            ShortcutAction::EditorFindPrevious => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::FindPrevious)
            }
            ShortcutAction::EditorWordLeft => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::WordArrowKey(
                    iced_code_editor::ArrowDirection::Left,
                    false,
                ))
            }
            ShortcutAction::EditorWordRight => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::WordArrowKey(
                    iced_code_editor::ArrowDirection::Right,
                    false,
                ))
            }
            ShortcutAction::EditorWordLeftSelect => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::WordArrowKey(
                    iced_code_editor::ArrowDirection::Left,
                    true,
                ))
            }
            ShortcutAction::EditorWordRightSelect => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::WordArrowKey(
                    iced_code_editor::ArrowDirection::Right,
                    true,
                ))
            }
            ShortcutAction::EditorDeleteWordBackward => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::DeleteWordBackward,
            ),
            ShortcutAction::EditorDeleteWordForward => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::DeleteWordForward,
            ),
            ShortcutAction::EditorDeleteToLineStart => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::DeleteToLineStart,
            ),
            ShortcutAction::EditorDeleteToLineEnd => self
                .dispatch_active_editor_widget_message(iced_code_editor::Message::DeleteToLineEnd),
            ShortcutAction::EditorLineStart => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::Home(false))
            }
            ShortcutAction::EditorLineEnd => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::End(false))
            }
            ShortcutAction::EditorLineStartSelect => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::Home(true))
            }
            ShortcutAction::EditorLineEndSelect => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::End(true))
            }
            ShortcutAction::EditorDocumentStart => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::DocumentHome(false),
            ),
            ShortcutAction::EditorDocumentEnd => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::DocumentEnd(false),
            ),
            ShortcutAction::EditorDocumentStartSelect => self
                .dispatch_active_editor_widget_message(iced_code_editor::Message::DocumentHome(
                    true,
                )),
            ShortcutAction::EditorDocumentEndSelect => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::DocumentEnd(true),
            ),
            ShortcutAction::EditorDeleteSelection => self
                .dispatch_active_editor_widget_message(iced_code_editor::Message::DeleteSelection),
            ShortcutAction::EditorSelectAll => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::SelectAll)
            }
            ShortcutAction::EditorInsertLineBelow => self
                .dispatch_active_editor_widget_message(iced_code_editor::Message::InsertLineBelow),
            ShortcutAction::EditorInsertLineAbove => self
                .dispatch_active_editor_widget_message(iced_code_editor::Message::InsertLineAbove),
            ShortcutAction::EditorDeleteLine => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::DeleteLine)
            }
            ShortcutAction::EditorMoveLineUp => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::MoveLineUp)
            }
            ShortcutAction::EditorMoveLineDown => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::MoveLineDown)
            }
            ShortcutAction::EditorCopyLineUp => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::CopyLineUp)
            }
            ShortcutAction::EditorCopyLineDown => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::CopyLineDown)
            }
            ShortcutAction::EditorJoinLines => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::JoinLines)
            }
            ShortcutAction::EditorIndent => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::Tab)
            }
            ShortcutAction::EditorOutdent => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::ShiftTab)
            }
            ShortcutAction::EditorToggleLineComment => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::ToggleLineComment,
            ),
            ShortcutAction::EditorToggleBlockComment => self.dispatch_active_editor_widget_message(
                iced_code_editor::Message::ToggleBlockComment,
            ),
            ShortcutAction::EditorSelectLine => {
                self.dispatch_active_editor_widget_message(iced_code_editor::Message::SelectLine)
            }
            ShortcutAction::EditorJumpToMatchingBracket => self
                .dispatch_active_editor_widget_message(
                    iced_code_editor::Message::JumpToMatchingBracket,
                ),
            ShortcutAction::CloseEditorTab => {
                let Some(tab_id) = self.editor.active_tab_id() else {
                    return Task::none();
                };
                update(
                    self,
                    Message::Editor(EditorMessage::CloseTabRequested(tab_id)),
                )
            }
            ShortcutAction::ToggleWorkspacePane(pane) => {
                update(self, Message::Pane(PaneMessage::ToggleWorkspacePane(pane)))
            }
            ShortcutAction::SwitchWorkspaceTabPrevious => {
                self.switch_focused_workspace_tab(TabDirection::Previous);
                Task::none()
            }
            ShortcutAction::SwitchWorkspaceTabNext => {
                self.switch_focused_workspace_tab(TabDirection::Next);
                Task::none()
            }
            ShortcutAction::SwitchEditorTabPrevious => {
                let Some(tab_id) = self.editor.activate_adjacent_tab(false) else {
                    return Task::none();
                };
                self.set_focused_workspace_pane(WorkspacePaneKind::Editor);
                self.focus_editor_text_area();
                self.pending_reveal_editor_tab = Some(tab_id);
                self.map_editor_widget_task(tab_id, self.editor.sync_tab_scroll_state(tab_id))
            }
            ShortcutAction::SwitchEditorTabNext => {
                let Some(tab_id) = self.editor.activate_adjacent_tab(true) else {
                    return Task::none();
                };
                self.set_focused_workspace_pane(WorkspacePaneKind::Editor);
                self.focus_editor_text_area();
                self.pending_reveal_editor_tab = Some(tab_id);
                self.map_editor_widget_task(tab_id, self.editor.sync_tab_scroll_state(tab_id))
            }
            ShortcutAction::FocusWorkspacePanePrevious => {
                self.cycle_workspace_pane_focus(PaneCycleDirection::Previous);
                Task::none()
            }
            ShortcutAction::FocusWorkspacePaneNext => {
                self.cycle_workspace_pane_focus(PaneCycleDirection::Next);
                Task::none()
            }
            ShortcutAction::ScoreZoomIn => update(self, Message::Viewer(ViewerMessage::ZoomIn)),
            ShortcutAction::ScoreZoomOut => update(self, Message::Viewer(ViewerMessage::ZoomOut)),
            ShortcutAction::ScoreZoomReset => {
                update(self, Message::Viewer(ViewerMessage::ResetZoom))
            }
            ShortcutAction::EditorZoomIn => {
                self.editor.zoom_in();
                self.persist_settings();
                Task::none()
            }
            ShortcutAction::EditorZoomOut => {
                self.editor.zoom_out();
                self.persist_settings();
                Task::none()
            }
            ShortcutAction::EditorZoomReset => {
                self.editor.reset_zoom();
                self.persist_settings();
                Task::none()
            }
            ShortcutAction::PianoRollZoomIn => {
                update(self, Message::PianoRoll(PianoRollMessage::ZoomIn))
            }
            ShortcutAction::PianoRollZoomOut => {
                update(self, Message::PianoRoll(PianoRollMessage::ZoomOut))
            }
            ShortcutAction::PianoRollZoomReset => {
                update(self, Message::PianoRoll(PianoRollMessage::ResetZoom))
            }
            ShortcutAction::PianoRollCursorSubdivisionPrevious => {
                if self.piano_roll.playback_is_playing() {
                    return Task::none();
                }
                let Some(file) = self.piano_roll.current_file() else {
                    return Task::none();
                };
                let tick = adjacent_subdivision_tick(
                    &file.data,
                    self.piano_roll.beat_subdivision,
                    self.piano_roll.playback_tick(),
                    false,
                );
                update(
                    self,
                    Message::PianoRoll(PianoRollMessage::SetCursorTicks(tick)),
                )
            }
            ShortcutAction::PianoRollCursorSubdivisionNext => {
                if self.piano_roll.playback_is_playing() {
                    return Task::none();
                }
                let Some(file) = self.piano_roll.current_file() else {
                    return Task::none();
                };
                let tick = adjacent_subdivision_tick(
                    &file.data,
                    self.piano_roll.beat_subdivision,
                    self.piano_roll.playback_tick(),
                    true,
                );
                update(
                    self,
                    Message::PianoRoll(PianoRollMessage::SetCursorTicks(tick)),
                )
            }
            ShortcutAction::PianoRollScrollUp => iced::widget::operation::scroll_by(
                roll_scroll_id(),
                iced::widget::operation::AbsoluteOffset {
                    x: 0.0,
                    y: -KEYBOARD_SCROLL_STEP,
                },
            ),
            ShortcutAction::PianoRollScrollDown => iced::widget::operation::scroll_by(
                roll_scroll_id(),
                iced::widget::operation::AbsoluteOffset {
                    x: 0.0,
                    y: KEYBOARD_SCROLL_STEP,
                },
            ),
            ShortcutAction::TransportPlayPause => update(
                self,
                Message::PianoRoll(PianoRollMessage::TransportPlayPause),
            ),
            ShortcutAction::TransportRewind => {
                update(self, Message::PianoRoll(PianoRollMessage::TransportRewind))
            }
            ShortcutAction::ToggleMetronome => {
                if self.focused_workspace_pane() == Some(WorkspacePaneKind::Editor) {
                    Task::none()
                } else {
                    update(
                        self,
                        Message::PianoRoll(PianoRollMessage::TransportToggleMetronome),
                    )
                }
            }
            ShortcutAction::ScoreScrollUp => update(self, Message::Viewer(ViewerMessage::ScrollUp)),
            ShortcutAction::ScoreScrollDown => {
                update(self, Message::Viewer(ViewerMessage::ScrollDown))
            }
            ShortcutAction::ScorePrevPage => update(self, Message::Viewer(ViewerMessage::PrevPage)),
            ShortcutAction::ScoreNextPage => update(self, Message::Viewer(ViewerMessage::NextPage)),
        }
    }

    pub(in crate::app) fn set_focused_workspace_pane(&mut self, pane: WorkspacePaneKind) {
        if self.group_for_pane(pane).is_some() {
            if pane != WorkspacePaneKind::Editor {
                self.editor_file_browser_focused = false;
            }
            self.focused_workspace_pane = Some(pane);
            self.sync_editor_widget_focus();
        }
    }

    pub(in crate::app) fn normalize_focused_workspace_pane(&mut self) {
        if self
            .focused_workspace_pane
            .is_some_and(|pane| self.group_for_pane(pane).is_some())
        {
            return;
        }

        self.focused_workspace_pane = self
            .dock_layout
            .as_ref()
            .and_then(|layout| first_active_workspace_pane(layout, &self.dock_groups))
            .or_else(|| self.dock_groups.values().next().map(|group| group.active));
        self.sync_editor_widget_focus();
    }

    pub(in crate::app) fn handle_logger_message(
        &mut self,
        message: LoggerMessage,
    ) -> Task<Message> {
        match message {
            LoggerMessage::RequestClear => {
                self.set_focused_workspace_pane(WorkspacePaneKind::Logger);
                if !self.logger.is_empty() {
                    self.show_prompt(
                        ErrorPrompt::new(
                            "Clear Logger",
                            "Do you want to clear all log messages?",
                            ErrorFatality::Recoverable,
                            PromptButtons::OkCancel,
                        ),
                        Some(PromptOkAction::ClearLogs),
                    );
                }
                Task::none()
            }
            LoggerMessage::TextAction(action) => {
                self.set_focused_workspace_pane(WorkspacePaneKind::Logger);
                self.logger.handle_editor_action(action);
                Task::none()
            }
        }
    }

    pub(in crate::app) fn handle_prompt_message(
        &mut self,
        message: PromptMessage,
    ) -> Task<Message> {
        if self.pending_editor_action.is_some() {
            return match message {
                PromptMessage::Acknowledge => self.handle_pending_prompt_save(),
                PromptMessage::Discard => self.handle_pending_prompt_discard(),
                PromptMessage::Cancel => {
                    self.error_prompt = None;
                    self.prompt_selected_button = PromptSelectedButton::Ok;
                    self.pending_editor_action = None;
                    self.pending_editor_save_as_tab = None;
                    Task::none()
                }
            };
        }

        match message {
            PromptMessage::Acknowledge => {
                if self.error_prompt.take().is_some() {
                    self.prompt_selected_button = PromptSelectedButton::Ok;
                    match self.prompt_ok_action.take() {
                        Some(PromptOkAction::ExitApp) => self.exit_app(),
                        Some(PromptOkAction::ClearLogs) => {
                            self.logger.clear();
                            Task::none()
                        }
                        Some(PromptOkAction::ReloadEditorTab(tab_id)) => {
                            self.reload_editor_tab_from_disk(tab_id)
                        }
                        Some(PromptOkAction::RemoveBus(bus_id)) => {
                            self.remove_bus_confirmed(bus_id)
                        }
                        Some(PromptOkAction::DeleteBrowserPath(path)) => {
                            match self.delete_browser_path_with_history(&path) {
                                Ok(task) => task,
                                Err(error) => {
                                    self.show_prompt(
                                        ErrorPrompt::new(
                                            "File Browser Error",
                                            error,
                                            ErrorFatality::Recoverable,
                                            PromptButtons::Ok,
                                        ),
                                        None,
                                    );
                                    Task::none()
                                }
                            }
                        }
                        None => Task::none(),
                    }
                } else {
                    Task::none()
                }
            }
            PromptMessage::Discard => Task::none(),
            PromptMessage::Cancel => {
                self.error_prompt = None;
                self.prompt_ok_action = None;
                self.prompt_selected_button = PromptSelectedButton::Ok;
                Task::none()
            }
        }
    }

    pub(in crate::app) fn handle_window_close_requested(
        &mut self,
        window_id: window::Id,
    ) -> Task<Message> {
        if window_id != self.main_window_id {
            return self.handle_processor_editor_close_requested(window_id);
        }
        let hide_editors = self.hide_all_editor_windows();
        let dirty_tabs = self.editor.tabs_requiring_resolution();
        if dirty_tabs.is_empty() {
            if self.project_is_dirty() {
                return Task::batch([
                    hide_editors,
                    self.begin_pending_project_action(EditorContinuation::ExitApp),
                ]);
            }
            return Task::batch([hide_editors, self.exit_app()]);
        }

        self.pending_editor_action = Some(PendingEditorAction::ResolveDirtyTabs {
            dirty_tab_ids: dirty_tabs,
            continuation: EditorContinuation::ExitApp,
        });
        self.show_current_pending_editor_prompt();
        hide_editors
    }

    pub(in crate::app) fn handle_window_resized(
        &mut self,
        window_id: window::Id,
        size: Size,
    ) -> Task<Message> {
        if window_id != self.main_window_id {
            return Task::none();
        }

        self.window_width = size.width.max(1.0);
        self.window_height = size.height.max(1.0);
        self.sync_editor_viewport_from_layout();

        Task::none()
    }

    pub(in crate::app) fn handle_modifiers_changed(
        &mut self,
        modifiers: iced::keyboard::Modifiers,
    ) -> Task<Message> {
        self.keyboard_modifiers = modifiers;
        Task::none()
    }

    pub(in crate::app) fn show_prompt(
        &mut self,
        prompt: ErrorPrompt,
        ok_action: Option<PromptOkAction>,
    ) {
        self.error_prompt = Some(prompt);
        self.prompt_ok_action = ok_action;
        self.prompt_selected_button = PromptSelectedButton::Ok;
    }

    pub(in crate::app) fn begin_pending_editor_action(
        &mut self,
        dirty_tab_ids: Vec<u64>,
        continuation: EditorContinuation,
    ) -> Task<Message> {
        if dirty_tab_ids.is_empty() {
            return self.continue_editor_continuation(continuation);
        }

        self.pending_editor_action = Some(PendingEditorAction::ResolveDirtyTabs {
            dirty_tab_ids,
            continuation,
        });
        self.show_current_pending_editor_prompt();
        Task::none()
    }

    pub(in crate::app) fn begin_pending_project_action(
        &mut self,
        continuation: EditorContinuation,
    ) -> Task<Message> {
        if !self.project_is_dirty() {
            return self.continue_editor_continuation(continuation);
        }

        self.pending_editor_action =
            Some(PendingEditorAction::ResolveDirtyProject { continuation });
        self.show_current_pending_editor_prompt();
        Task::none()
    }

    pub(in crate::app) fn show_current_pending_editor_prompt(&mut self) {
        let Some(action) = self.pending_editor_action.as_ref() else {
            return;
        };

        let prompt = match action {
            PendingEditorAction::ResolveDirtyTabs { dirty_tab_ids, .. } => {
                let Some(&tab_id) = dirty_tab_ids.first() else {
                    return;
                };
                let title = self.editor.tab_title(tab_id);
                if self.editor.tab_file_state(tab_id) == Some(EditorTabFileState::MissingOnDisk) {
                    ErrorPrompt::new(
                        format!("Save {title}?"),
                        format!(
                            "{title} is missing on disk. Save it before continuing to recreate the file?"
                        ),
                        ErrorFatality::Recoverable,
                        PromptButtons::SaveDiscardCancel,
                    )
                    .with_discard_label("Close Without Saving")
                } else {
                    ErrorPrompt::new(
                        format!("Close {title}?"),
                        format!("Save changes to {title} before continuing?"),
                        ErrorFatality::Recoverable,
                        PromptButtons::SaveDiscardCancel,
                    )
                }
            }
            PendingEditorAction::ResolveDirtyProject { .. } => ErrorPrompt::new(
                "Save project changes?",
                "Project state changed. Save the project before continuing?",
                ErrorFatality::Recoverable,
                PromptButtons::SaveDiscardCancel,
            ),
        };
        self.error_prompt = Some(prompt);
        self.prompt_ok_action = None;
        self.prompt_selected_button = PromptSelectedButton::Ok;
    }

    pub(in crate::app) fn handle_pending_prompt_save(&mut self) -> Task<Message> {
        self.error_prompt = None;
        self.prompt_selected_button = PromptSelectedButton::Ok;
        match self.pending_editor_action.as_ref() {
            Some(PendingEditorAction::ResolveDirtyTabs { dirty_tab_ids, .. }) => {
                let Some(&tab_id) = dirty_tab_ids.first() else {
                    return Task::none();
                };

                if self.editor.tab_path(tab_id).is_none() {
                    self.pending_editor_save_as_tab = Some(tab_id);
                    let suggested_name = self.editor.suggested_rename_name(tab_id);
                    return Task::perform(
                        async move {
                            rfd::AsyncFileDialog::new()
                                .set_file_name(&suggested_name)
                                .save_file()
                                .await
                                .map(|file| file.path().to_path_buf())
                        },
                        |picked| Message::Editor(EditorMessage::SaveAsPicked(picked)),
                    );
                }

                let save_task = self.save_editor_tab(tab_id);
                let advance_task = self.advance_pending_editor_action();
                Task::batch([save_task, advance_task])
            }
            Some(PendingEditorAction::ResolveDirtyProject { .. }) => {
                match self.project_root.clone() {
                    Some(project_root) => {
                        let save_task = self.save_project_to_root(project_root);
                        let advance_task = self.advance_pending_editor_action();
                        Task::batch([save_task, advance_task])
                    }
                    None => update(self, Message::File(FileMessage::RequestCreateProject)),
                }
            }
            None => Task::none(),
        }
    }

    pub(in crate::app) fn handle_pending_prompt_discard(&mut self) -> Task<Message> {
        match self.pending_editor_action.take() {
            Some(PendingEditorAction::ResolveDirtyProject { continuation }) => {
                self.error_prompt = None;
                self.prompt_selected_button = PromptSelectedButton::Ok;
                self.continue_editor_continuation_without_project_dirty_check(continuation)
            }
            Some(action) => {
                self.pending_editor_action = Some(action);
                self.advance_pending_editor_action()
            }
            None => Task::none(),
        }
    }

    pub(in crate::app) fn advance_pending_editor_action(&mut self) -> Task<Message> {
        let Some(action) = self.pending_editor_action.as_mut() else {
            return Task::none();
        };

        match action {
            PendingEditorAction::ResolveDirtyTabs { dirty_tab_ids, .. } => {
                if !dirty_tab_ids.is_empty() {
                    dirty_tab_ids.remove(0);
                }

                if !dirty_tab_ids.is_empty() {
                    self.show_current_pending_editor_prompt();
                    return Task::none();
                }
            }
            PendingEditorAction::ResolveDirtyProject { .. } => {}
        }

        let continuation = match self.pending_editor_action.take() {
            Some(PendingEditorAction::ResolveDirtyTabs { continuation, .. })
            | Some(PendingEditorAction::ResolveDirtyProject { continuation }) => continuation,
            None => return Task::none(),
        };

        self.error_prompt = None;
        self.prompt_selected_button = PromptSelectedButton::Ok;
        self.continue_editor_continuation(continuation)
    }

    pub(in crate::app) fn continue_editor_continuation(
        &mut self,
        continuation: EditorContinuation,
    ) -> Task<Message> {
        match continuation {
            EditorContinuation::CloseTab(tab_id) => {
                self.editor.close_tab(tab_id);
                self.sync_editor_file_watcher();
                self.focus_editor_text_area();
                self.persist_settings();
                Task::none()
            }
            EditorContinuation::LoadProject(project_root) => {
                if self.project_root.as_ref() != Some(&project_root) && self.project_is_dirty() {
                    return self.begin_pending_project_action(EditorContinuation::LoadProject(
                        project_root,
                    ));
                }
                self.load_project_from_root(project_root)
            }
            EditorContinuation::OpenScore(path) => {
                let next_project_root = state::find_project_root(&path);
                if next_project_root != self.project_root && self.project_is_dirty() {
                    return self.begin_pending_project_action(EditorContinuation::OpenScore(path));
                }
                match selected_score_from_path(path) {
                    Ok(selected_score) => {
                        self.attach_persistence_context_for_score(&selected_score.path);
                        self.activate_score(selected_score)
                    }
                    Err(error) => {
                        self.show_prompt(
                            ErrorPrompt::new(
                                "Open File Error",
                                error,
                                ErrorFatality::Recoverable,
                                PromptButtons::Ok,
                            ),
                            None,
                        );
                        Task::none()
                    }
                }
            }
            EditorContinuation::ExitApp => {
                if self.project_is_dirty() {
                    return self.begin_pending_project_action(EditorContinuation::ExitApp);
                }
                self.exit_app()
            }
        }
    }

    fn continue_editor_continuation_without_project_dirty_check(
        &mut self,
        continuation: EditorContinuation,
    ) -> Task<Message> {
        match continuation {
            EditorContinuation::CloseTab(tab_id) => {
                self.editor.close_tab(tab_id);
                self.sync_editor_file_watcher();
                self.focus_editor_text_area();
                self.persist_settings();
                Task::none()
            }
            EditorContinuation::LoadProject(project_root) => {
                self.load_project_from_root(project_root)
            }
            EditorContinuation::OpenScore(path) => match selected_score_from_path(path) {
                Ok(selected_score) => {
                    self.attach_persistence_context_for_score(&selected_score.path);
                    self.activate_score(selected_score)
                }
                Err(error) => {
                    self.show_prompt(
                        ErrorPrompt::new(
                            "Open File Error",
                            error,
                            ErrorFatality::Recoverable,
                            PromptButtons::Ok,
                        ),
                        None,
                    );
                    Task::none()
                }
            },
            EditorContinuation::ExitApp => self.exit_app(),
        }
    }

    pub(in crate::app) fn exit_app(&mut self) -> Task<Message> {
        Task::batch([self.destroy_all_editor_windows(), iced::exit()])
    }

    fn open_settings_file_in_editor(&mut self) -> Task<Message> {
        let path = match settings::path() {
            Ok(path) => path,
            Err(error) => {
                self.show_prompt(
                    ErrorPrompt::new(
                        "Settings Error",
                        error,
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
                return Task::none();
            }
        };

        if !path.exists() {
            let settings = settings::AppSettings {
                editor_view: self.editor.view_settings(),
                editor_theme: self.editor.theme_settings(),
                editor_recent_files_limit: self.editor_recent_files_limit,
                playback: self.playback_settings.clone(),
                plugin_search_paths: self.plugin_search_paths.clone(),
                shortcuts: self.shortcut_settings.clone(),
            };

            if let Err(error) = settings::save(&settings) {
                self.show_prompt(
                    ErrorPrompt::new(
                        "Settings Error",
                        error,
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
                return Task::none();
            }
        }

        let _ = self.unfold_workspace_pane(WorkspacePaneKind::Editor);
        let task = self.open_editor_file_in_editor(&path);
        if let Some(tab_id) = self.editor.find_tab_by_path(&path) {
            self.editor.activate_tab(tab_id);
            self.pending_reveal_editor_tab = Some(tab_id);
        }
        self.set_focused_workspace_pane(WorkspacePaneKind::Editor);
        self.focus_editor_text_area();
        task
    }

    pub(in crate::app) fn is_settings_file_path(&self, path: &Path) -> bool {
        settings::path()
            .map(|settings_path| {
                state::normalize_path(&settings_path) == state::normalize_path(path)
            })
            .unwrap_or(false)
    }

    pub(in crate::app) fn reload_settings_from_disk(&mut self, path: &Path) -> Result<(), String> {
        let loaded = settings::load_from_path(path)?;
        let previous_playback = self.playback_settings.clone();
        self.editor.apply_view_settings(loaded.editor_view);
        self.editor.apply_theme_settings(loaded.editor_theme);
        self.editor_recent_files_limit = loaded.editor_recent_files_limit.max(1);
        self.editor_recent_files
            .truncate(self.editor_recent_files_limit);
        self.playback_settings = loaded.playback.clone();

        let restart_playback = loaded.playback.sample_rate != previous_playback.sample_rate
            || loaded.playback.device != previous_playback.device
            || loaded.playback.block_size != previous_playback.block_size
            || loaded.playback.chase_notes_on_seek != previous_playback.chase_notes_on_seek;
        if restart_playback {
            self.restart_playback_engine();
        } else if loaded.playback.soundfonts.is_empty() {
            self.soundfont_status = SoundfontStatus::NotSelected;
            self.unload_playback_file();
        } else {
            self.initialize_playback_soundfonts(loaded.playback.soundfonts);
        }
        self.shortcut_settings = loaded.shortcuts;
        self.sync_editor_viewport_from_layout();
        self.sync_editor_widget_focus();
        Ok(())
    }

    fn handle_editor_file_browser_move(&mut self, delta: i32) -> Task<Message> {
        if let Err(error) = self.editor.move_file_browser_selection(delta) {
            self.show_prompt(
                ErrorPrompt::new(
                    "File Browser Error",
                    error,
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
            return Task::none();
        }
        self.reveal_editor_file_browser_selection(
            self.editor
                .file_browser_has_preview_column(self.editor.file_browser_active_column_index()),
        )
    }

    fn handle_editor_file_browser_column(&mut self, right: bool) -> Task<Message> {
        if let Err(error) = self.editor.move_file_browser_column(right) {
            self.show_prompt(
                ErrorPrompt::new(
                    "File Browser Error",
                    error,
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
            return Task::none();
        }

        self.reveal_editor_file_browser_selection(right)
    }

    pub(in crate::app) fn reveal_editor_file_browser_selection(
        &self,
        reveal_preview: bool,
    ) -> Task<Message> {
        let horizontal = self.reveal_editor_file_browser_columns(reveal_preview);
        let column_index = self.editor.file_browser_active_column_index();
        let vertical = self
            .editor
            .file_browser_selected_index(column_index)
            .map(|selected_index| {
                let current_y = self
                    .editor_file_browser_column_scroll_y
                    .get(&column_index)
                    .copied()
                    .unwrap_or(0.0);
                let viewport_height = self
                    .editor_file_browser_column_viewport_height
                    .get(&column_index)
                    .copied()
                    .filter(|height| *height > 0.0)
                    .unwrap_or(super::EDITOR_FILE_BROWSER_HEIGHT);
                let row_top = selected_index as f32 * super::EDITOR_FILE_BROWSER_ENTRY_HEIGHT;
                let row_bottom = row_top + super::EDITOR_FILE_BROWSER_ENTRY_HEIGHT;
                let target_y = if row_top < current_y {
                    row_top
                } else if row_bottom > current_y + viewport_height {
                    (row_bottom - viewport_height).max(0.0)
                } else {
                    current_y
                };
                iced::widget::operation::scroll_to(
                    crate::app::editor_file_browser_column_scroll_id(column_index),
                    iced::widget::operation::AbsoluteOffset {
                        x: 0.0,
                        y: target_y,
                    },
                )
            })
            .unwrap_or_else(Task::none);

        Task::batch([horizontal, vertical])
    }

    pub(in crate::app) fn reveal_editor_file_browser_columns(
        &self,
        reveal_preview: bool,
    ) -> Task<Message> {
        let column_index = self.editor.file_browser_active_column_index();
        let current_x = self.editor_file_browser_scroll_x;
        let viewport_width = if self.editor_file_browser_viewport_width > 0.0 {
            self.editor_file_browser_viewport_width
        } else {
            super::EDITOR_FILE_BROWSER_COLUMN_WIDTH
        };
        let visible_column_count = self.editor.file_browser_columns().len();
        let reveal_column_index = if reveal_preview && visible_column_count > column_index + 1 {
            column_index + 1
        } else {
            column_index
        };
        let active_column_left = column_index as f32 * super::EDITOR_FILE_BROWSER_COLUMN_WIDTH;
        let reveal_column_right =
            (reveal_column_index + 1) as f32 * super::EDITOR_FILE_BROWSER_COLUMN_WIDTH;
        let target_x = if active_column_left < current_x {
            active_column_left
        } else if reveal_column_right > current_x + viewport_width {
            (reveal_column_right - viewport_width).max(0.0)
        } else {
            current_x
        };
        iced::widget::operation::scroll_to(
            super::EDITOR_FILE_BROWSER_SCROLL_ID,
            iced::widget::operation::AbsoluteOffset {
                x: target_x,
                y: 0.0,
            },
        )
    }
}

fn prompt_buttons(buttons: PromptButtons) -> &'static [PromptSelectedButton] {
    match buttons {
        PromptButtons::Ok => &[PromptSelectedButton::Ok],
        PromptButtons::OkCancel => &[PromptSelectedButton::Cancel, PromptSelectedButton::Ok],
        PromptButtons::SaveDiscardCancel => &[
            PromptSelectedButton::Cancel,
            PromptSelectedButton::Discard,
            PromptSelectedButton::Ok,
        ],
    }
}
