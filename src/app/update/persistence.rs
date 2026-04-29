use super::*;
use crate::settings::WorkspaceLayoutSettings;

impl Lilypalooza {
    pub(in crate::app) fn current_project_state(&self) -> ProjectState {
        ProjectState {
            project_name: self.project_name.clone(),
            workspace_layout: WorkspaceLayoutSettings {
                root: self
                    .dock_layout
                    .as_ref()
                    .map(|layout| dock_node_to_settings(layout, &self.dock_groups)),
                folded_panes: self
                    .folded_panes
                    .iter()
                    .cloned()
                    .map(folded_pane_to_settings)
                    .collect(),
                piano_visible: !self.is_pane_folded(WorkspacePaneKind::PianoRoll),
            },
            score_view: settings::ScoreViewSettings {
                zoom: self.svg_zoom,
                page_brightness: self.svg_page_brightness,
            },
            piano_roll_view: settings::PianoRollViewSettings {
                zoom_x: self.piano_roll.zoom_x,
                beat_subdivision: self.piano_roll.beat_subdivision,
            },
            main_score: self.current_score.as_ref().map(|score| {
                self.project_root
                    .as_ref()
                    .and_then(|project_root| {
                        state::main_score_relative_to(project_root, &score.path).ok()
                    })
                    .unwrap_or_else(|| score.path.clone())
            }),
            editor_tabs: self.editor.file_backed_tab_paths(),
            active_editor_tab: self.editor.active_file_backed_tab_path(),
            has_clean_untitled_editor_tab: self.editor.has_clean_untitled_tab(),
            track_name_overrides: self.track_name_overrides.clone(),
            track_color_overrides: self
                .track_color_overrides
                .iter()
                .copied()
                .map(|color: Option<iced::Color>| color.map(crate::track_colors::to_override))
                .collect(),
            metronome: self.metronome,
            mixer_state: self
                .playback
                .as_ref()
                .map(|playback| playback.mixer_state().clone())
                .unwrap_or_else(|| self.project_mixer_state.clone()),
            processor_presets: self.processor_presets.clone(),
        }
    }

    pub(in crate::app) fn project_is_dirty(&self) -> bool {
        self.saved_project_state
            .as_ref()
            .is_some_and(|saved| self.current_project_state() != *saved)
    }

    pub(in crate::app) fn attach_persistence_context_for_score(&mut self, score_path: &Path) {
        let next_project_root = state::find_project_root(score_path);
        if next_project_root == self.project_root {
            return;
        }

        match next_project_root {
            Some(project_root) => {
                let project_state = match state::load_project(&project_root) {
                    Ok(mut state) => {
                        migrate_workspace_layout(
                            &mut state.workspace_layout.root,
                            &state.workspace_layout.folded_panes,
                        );
                        state
                    }
                    Err(error) => {
                        self.show_prompt(
                            ErrorPrompt::new(
                                "Project Load Error",
                                error,
                                ErrorFatality::Recoverable,
                                PromptButtons::Ok,
                            ),
                            None,
                        );
                        ProjectState::default()
                    }
                };
                self.apply_project_state(project_root, project_state);
            }
            None => {
                let global_state = match state::load_global() {
                    Ok(mut state) => {
                        migrate_workspace_layout(
                            &mut state.workspace_layout.root,
                            &state.workspace_layout.folded_panes,
                        );
                        state
                    }
                    Err(error) => {
                        self.show_prompt(
                            ErrorPrompt::new(
                                "State Load Error",
                                error,
                                ErrorFatality::Recoverable,
                                PromptButtons::Ok,
                            ),
                            None,
                        );
                        GlobalState::default()
                    }
                };
                self.apply_global_state(global_state);
            }
        }
    }

    pub(in crate::app) fn apply_global_state(&mut self, state: GlobalState) {
        self.project_root = None;
        self.editor.set_project_root(None);
        self.sync_browser_file_watcher();
        self.project_name = None;
        self.track_name_overrides.clear();
        self.track_color_overrides.clear();
        self.selected_track_index = None;
        self.open_processor_browser_target = None;
        self.hovered_processor_slot = None;
        self.effect_rack_hovered_effect = None;
        self.effect_drag_source = None;
        self.effect_drag_target = None;
        self.effect_rack_autoscroll_direction = 0;
        self.effect_rack_drag_pointer_y = None;
        self.effect_rack_scroll_y.clear();
        self.effect_rack_viewport_height.clear();
        self.open_mixer_effect_rack_tracks.clear();
        self.open_instrument_browser_track = None;
        self.instrument_browser_search.clear();
        self.instrument_browser_backend = super::super::mixer::InstrumentBrowserBackend::BuiltIn;
        self.metronome = state::MetronomeState::default();
        self.project_mixer_state = lilypalooza_audio::MixerState::new();
        self.processor_presets = state.processor_presets;
        self.metronome_menu_open = false;
        self.cancel_track_rename();
        self.apply_metronome_state_to_playback();
        self.editor_recent_files = state.editor_recent_files;
        self.recent_projects = state.recent_projects;
        self.restore_editor_session(
            &state.editor_tabs,
            state.active_editor_tab.as_deref(),
            state.has_clean_untitled_editor_tab,
        );
        self.apply_workspace_state(
            state.workspace_layout,
            state.score_view,
            state.piano_roll_view,
        );
        self.saved_project_state = Some(self.current_project_state());
    }

    pub(in crate::app) fn apply_project_state(
        &mut self,
        project_root: PathBuf,
        state: ProjectState,
    ) {
        let project_name = state
            .project_name
            .clone()
            .or_else(|| Some(default_project_name(&project_root)));
        self.register_recent_project(&project_root);
        self.project_root = Some(project_root);
        self.editor.set_project_root(self.project_root.clone());
        self.sync_browser_file_watcher();
        self.project_name = project_name;
        self.track_name_overrides = state.track_name_overrides;
        self.track_color_overrides = state
            .track_color_overrides
            .into_iter()
            .map(|color| color.map(crate::track_colors::from_override))
            .collect();
        self.selected_track_index = None;
        self.open_processor_browser_target = None;
        self.hovered_processor_slot = None;
        self.effect_rack_hovered_effect = None;
        self.effect_drag_source = None;
        self.effect_drag_target = None;
        self.effect_rack_autoscroll_direction = 0;
        self.effect_rack_drag_pointer_y = None;
        self.effect_rack_scroll_y.clear();
        self.effect_rack_viewport_height.clear();
        self.open_mixer_effect_rack_tracks.clear();
        self.open_instrument_browser_track = None;
        self.instrument_browser_search.clear();
        self.instrument_browser_backend = super::super::mixer::InstrumentBrowserBackend::BuiltIn;
        self.metronome = state.metronome;
        self.project_mixer_state = state.mixer_state;
        self.processor_presets = state.processor_presets;
        self.metronome_menu_open = false;
        self.cancel_track_rename();
        self.apply_metronome_state_to_playback();
        if let Some(playback) = self.playback.as_mut() {
            let state = self.project_mixer_state.clone();
            if playback.mixer().replace_state(state.clone()).is_ok() {
                for (track_id, track) in state.tracks_with_ids() {
                    let index = track_id.index();
                    let _ = self.piano_roll.set_track_muted(index, track.state.muted);
                    let _ = self.piano_roll.set_track_soloed(index, track.state.soloed);
                }
                self.piano_roll.set_global_solo_active(
                    state.tracks().iter().any(|track| track.state.soloed)
                        || state.buses().iter().any(|bus| bus.state.soloed),
                );
            }
        }
        self.restore_editor_session(
            &state.editor_tabs,
            state.active_editor_tab.as_deref(),
            state.has_clean_untitled_editor_tab,
        );
        self.apply_workspace_state(
            state.workspace_layout,
            state.score_view,
            state.piano_roll_view,
        );
        self.saved_project_state = Some(self.current_project_state());
    }

    pub(in crate::app) fn apply_workspace_state(
        &mut self,
        workspace_layout: WorkspaceLayoutSettings,
        score_view: settings::ScoreViewSettings,
        piano_roll_view: settings::PianoRollViewSettings,
    ) {
        let WorkspaceLayoutSettings {
            root,
            folded_panes,
            piano_visible,
        } = workspace_layout;
        let (dock_layout, dock_groups, next_dock_group_id, workspace_panes) =
            build_dock_runtime(root.as_ref());
        self.workspace_panes = workspace_panes;
        self.dock_layout = dock_layout;
        self.dock_groups = dock_groups;
        self.next_dock_group_id = next_dock_group_id;
        self.folded_panes = folded_panes
            .into_iter()
            .map(folded_pane_from_settings)
            .collect();
        if self.folded_panes.is_empty() && !piano_visible {
            self.folded_panes.push(FoldedPaneState {
                pane: WorkspacePaneKind::PianoRoll,
                restore: FoldedPaneRestore::Tab {
                    anchor: WorkspacePaneKind::Score,
                },
            });
        }

        self.piano_roll.visible = !self.is_pane_folded(WorkspacePaneKind::PianoRoll);
        self.piano_roll
            .apply_view_settings(piano_roll_view.zoom_x, piano_roll_view.beat_subdivision);
        self.svg_zoom = score_view.zoom.clamp(MIN_SVG_ZOOM, MAX_SVG_ZOOM);
        self.svg_page_brightness = score_view
            .page_brightness
            .clamp(MIN_SVG_PAGE_BRIGHTNESS, MAX_SVG_PAGE_BRIGHTNESS);
        self.focused_workspace_pane = self
            .dock_layout
            .as_ref()
            .and_then(|layout| first_active_workspace_pane(layout, &self.dock_groups))
            .or_else(|| self.dock_groups.values().next().map(|group| group.active));
        self.hovered_workspace_pane = None;
        self.pressed_workspace_pane = None;
        self.workspace_drag_origin = None;
        self.dragged_workspace_pane = None;
        self.dock_drop_target = None;
        self.open_header_overflow_menu = None;
        self.open_editor_menu_section = None;
        self.open_editor_file_menu_section = None;
        self.hovered_editor_file_menu_section = None;
        self.open_project_menu = false;
        self.open_project_menu_section = None;
        self.open_project_recent = false;
        self.sync_editor_widget_focus();
    }

    pub(in crate::app) fn register_editor_recent_file(&mut self, path: &Path) {
        let path = state::normalize_path(path);
        self.editor_recent_files
            .retain(|existing| existing != &path);
        self.editor_recent_files.insert(0, path);
        self.editor_recent_files
            .truncate(self.editor_recent_files_limit.max(1));
        self.persist_settings();
    }

    pub(in crate::app) fn register_recent_project(&mut self, project_root: &Path) {
        let project_root = state::normalize_path(project_root);
        self.recent_projects
            .retain(|existing| existing != &project_root);
        self.recent_projects.insert(0, project_root);
        self.recent_projects.truncate(7);
    }

    pub(in crate::app) fn persist_settings(&mut self) {
        if let Err(error) = self.try_persist_settings() {
            self.show_prompt(
                ErrorPrompt::new(
                    "Persistence Error",
                    error,
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
        }
    }

    pub(in crate::app) fn try_persist_settings(&self) -> Result<(), String> {
        let settings_path = settings::path().ok();
        let settings_file_open = settings_path
            .as_deref()
            .is_some_and(|path| self.editor.find_tab_by_path(path).is_some());

        if !settings_file_open {
            settings::save(&settings::AppSettings {
                editor_view: self.editor.view_settings(),
                editor_theme: self.editor.theme_settings(),
                editor_recent_files_limit: self.editor_recent_files_limit,
                playback: self.playback_settings.clone(),
                shortcuts: self.shortcut_settings.clone(),
            })?;
        }

        if let Some(project_root) = self.project_root.as_ref() {
            let project_state = self.current_project_state();
            state::save_project(project_root, &project_state)?;

            let mut global_state = state::load_global()?;
            global_state.editor_recent_files = self.editor_recent_files.clone();
            global_state.recent_projects = self.recent_projects.clone();
            state::save_global(&global_state)?;
            return Ok(());
        }

        state::save_global(&GlobalState {
            workspace_layout: WorkspaceLayoutSettings {
                root: self
                    .dock_layout
                    .as_ref()
                    .map(|layout| dock_node_to_settings(layout, &self.dock_groups)),
                folded_panes: self
                    .folded_panes
                    .iter()
                    .cloned()
                    .map(folded_pane_to_settings)
                    .collect(),
                piano_visible: !self.is_pane_folded(WorkspacePaneKind::PianoRoll),
            },
            score_view: settings::ScoreViewSettings {
                zoom: self.svg_zoom,
                page_brightness: self.svg_page_brightness,
            },
            piano_roll_view: settings::PianoRollViewSettings {
                zoom_x: self.piano_roll.zoom_x,
                beat_subdivision: self.piano_roll.beat_subdivision,
            },
            processor_presets: self.processor_presets.clone(),
            main_score: self.current_score.as_ref().map(|score| score.path.clone()),
            editor_tabs: self.editor.file_backed_tab_paths(),
            active_editor_tab: self.editor.active_file_backed_tab_path(),
            has_clean_untitled_editor_tab: self.editor.has_clean_untitled_tab(),
            editor_recent_files: self.editor_recent_files.clone(),
            recent_projects: self.recent_projects.clone(),
        })
    }

    pub(in crate::app) fn save_project_to_root(&mut self, project_root: PathBuf) -> Task<Message> {
        self.open_project_menu = false;
        self.open_project_menu_section = None;
        self.open_project_recent = false;
        self.register_recent_project(&project_root);
        self.project_root = Some(project_root.clone());
        self.editor.set_project_root(Some(project_root.clone()));
        if self.project_name.is_none() {
            self.project_name = Some(default_project_name(&project_root));
        }
        self.persist_settings();
        self.saved_project_state = Some(self.current_project_state());
        self.logger.push(format!(
            "Saved project {}",
            state::project_file_path(&project_root).display()
        ));
        Task::none()
    }

    pub(in crate::app) fn load_project_from_root(
        &mut self,
        project_root: PathBuf,
    ) -> Task<Message> {
        if self.pending_editor_action.is_none() && self.editor.has_dirty_tabs() {
            return self.begin_pending_editor_action(
                self.editor.tabs_requiring_resolution(),
                EditorContinuation::LoadProject(project_root),
            );
        }
        if self.pending_editor_action.is_none()
            && self.project_root.as_ref() != Some(&project_root)
            && self.project_is_dirty()
        {
            return self
                .begin_pending_project_action(EditorContinuation::LoadProject(project_root));
        }

        self.open_project_menu = false;
        self.open_project_menu_section = None;
        self.open_project_recent = false;
        if !state::project_file_path(&project_root).is_file() {
            self.show_prompt(
                ErrorPrompt::new(
                    "Project Load Error",
                    format!(
                        "No project file found at {}",
                        state::project_file_path(&project_root).display()
                    ),
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
            return Task::none();
        }

        let project_state = match state::load_project(&project_root) {
            Ok(mut state) => {
                migrate_workspace_layout(
                    &mut state.workspace_layout.root,
                    &state.workspace_layout.folded_panes,
                );
                state
            }
            Err(error) => {
                self.show_prompt(
                    ErrorPrompt::new(
                        "Project Load Error",
                        error,
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
                return Task::none();
            }
        };

        let main_score_path = project_state
            .main_score
            .as_ref()
            .map(|path| project_root.join(path));
        self.apply_project_state(project_root, project_state);

        if let Some(path) = main_score_path {
            match selected_score_from_path(path) {
                Ok(selected_score) => self.activate_score(selected_score),
                Err(error) => {
                    self.show_prompt(
                        ErrorPrompt::new(
                            "Project Load Error",
                            error,
                            ErrorFatality::Recoverable,
                            PromptButtons::Ok,
                        ),
                        None,
                    );
                    Task::none()
                }
            }
        } else {
            self.unload_current_score();
            self.persist_settings();
            Task::none()
        }
    }

    pub(in crate::app) fn restore_runtime_view_state(
        &self,
        pane: WorkspacePaneKind,
    ) -> Task<Message> {
        if self.group_for_pane(pane).is_none() {
            return Task::none();
        }

        match pane {
            WorkspacePaneKind::PianoRoll => self.restore_piano_roll_scroll(),
            WorkspacePaneKind::Score => self.restore_score_scroll(),
            WorkspacePaneKind::Mixer | WorkspacePaneKind::Editor | WorkspacePaneKind::Logger => {
                Task::none()
            }
        }
    }

    pub(in crate::app) fn restore_score_scroll(&self) -> Task<Message> {
        iced::widget::operation::scroll_to(
            super::SCORE_SCROLLABLE_ID,
            iced::widget::operation::AbsoluteOffset {
                x: Some(self.svg_scroll_x),
                y: Some(self.svg_scroll_y),
            },
        )
    }

    pub(in crate::app) fn restore_editor_session(
        &mut self,
        paths: &[PathBuf],
        active_path: Option<&Path>,
        has_clean_untitled: bool,
    ) {
        let (_tasks, warnings) =
            self.editor
                .restore_file_tabs(paths, active_path, has_clean_untitled);
        self.sync_editor_file_watcher();
        self.editor_font_metrics_refresh_pending = !self.editor.tab_ids().is_empty();
        for warning in warnings {
            self.logger.push(warning);
        }
    }
}
