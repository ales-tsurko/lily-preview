use super::*;
use crate::app::editor::EditorTabFileState;

impl Lilypalooza {
    #[cfg_attr(test, allow(dead_code))]
    pub(in crate::app) fn start_plugin_scan(&mut self) {
        self.start_plugin_scan_with_validator(plugin_validator_path());
    }

    pub(in crate::app) fn start_plugin_scan_with_validator(&mut self, validator: PathBuf) {
        if let Err(error) = ensure_plugin_validator_available(&validator) {
            self.logger.push(error);
            return;
        }

        self.logger.push(format!(
            "Scanning plugins from {} path(s)",
            self.plugin_search_paths
                .iter()
                .filter(|path| path.enabled)
                .count()
        ));
        let cache = self.plugin_scan_cache.clone();
        self.plugin_scan
            .start(self.plugin_search_paths.clone(), cache, validator);
    }

    pub(in crate::app) fn poll_plugin_scan(&mut self) {
        for event in self.plugin_scan.drain_events() {
            match event {
                lilypalooza_plugin_scan::PluginScanEvent::Log(line) => self.logger.push(line),
                lilypalooza_plugin_scan::PluginScanEvent::ClapPlugins(plugins) => {
                    lilypalooza_clap::register_plugins(plugins);
                }
                lilypalooza_plugin_scan::PluginScanEvent::Finished { summary, cache } => {
                    let _ = summary;
                    self.plugin_scan_cache = cache;
                    if let Err(error) = self.plugin_scan_cache.save_to(&plugin_scan_cache_path()) {
                        self.logger.push(error);
                    }
                }
            }
        }
    }

    pub(in crate::app) fn handle_startup_checked(
        &mut self,
        result: Result<lilypond::VersionCheck, String>,
    ) -> Task<Message> {
        match result {
            Ok(version_check) => {
                self.lilypond_status = LilypondStatus::Ready {
                    detected: version_check.detected,
                    min_required: version_check.min_required,
                };
                self.logger.push(format!(
                    "LilyPond ready: installed {}, minimum required {}",
                    version_check.detected, version_check.min_required
                ));
            }
            Err(error) => {
                self.lilypond_status = LilypondStatus::Unavailable;
                self.logger
                    .push(format!("LilyPond startup check failed: {error}"));
                self.show_prompt(
                    ErrorPrompt::new(
                        "LilyPond Startup Error",
                        error,
                        ErrorFatality::Critical,
                        PromptButtons::Ok,
                    ),
                    Some(PromptOkAction::ExitApp),
                );
            }
        }

        Task::none()
    }

    pub(in crate::app) fn handle_file_message(&mut self, message: FileMessage) -> Task<Message> {
        match message {
            FileMessage::RequestOpen => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter("LilyPond score", &["ly", "ily"])
                        .pick_file()
                        .await
                        .map(|file| file.path().to_path_buf())
                },
                |picked| Message::File(FileMessage::Picked(picked)),
            ),
            FileMessage::Picked(Some(path)) => {
                let next_project_root = state::find_project_root(&path);
                if next_project_root != self.project_root && self.editor.has_dirty_tabs() {
                    return self.begin_pending_editor_action(
                        self.editor.tabs_requiring_resolution(),
                        EditorContinuation::OpenScore(path),
                    );
                }
                self.continue_editor_continuation(EditorContinuation::OpenScore(path))
            }
            FileMessage::Picked(None) => Task::none(),
            FileMessage::RequestCreateProject => {
                self.open_project_menu = false;
                self.open_project_menu_section = None;
                self.open_project_recent = false;
                let suggested_directory = self
                    .project_root
                    .clone()
                    .or_else(|| {
                        self.current_score
                            .as_ref()
                            .and_then(|score| score.path.parent().map(Path::to_path_buf))
                    })
                    .unwrap_or_else(|| PathBuf::from("."));

                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_directory(&suggested_directory)
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
                    |picked| Message::File(FileMessage::CreateProjectPicked(picked)),
                )
            }
            FileMessage::RequestSaveProject => {
                self.open_project_menu = false;
                self.open_project_menu_section = None;
                self.open_project_recent = false;
                if let Some(project_root) = self.project_root.clone() {
                    self.save_project_to_root(project_root)
                } else {
                    update(self, Message::File(FileMessage::RequestCreateProject))
                }
            }
            FileMessage::RequestLoadProject => {
                self.open_project_menu = false;
                self.open_project_menu_section = None;
                self.open_project_recent = false;
                let suggested_directory = self
                    .project_root
                    .clone()
                    .or_else(|| {
                        self.current_score
                            .as_ref()
                            .and_then(|score| score.path.parent().map(Path::to_path_buf))
                    })
                    .unwrap_or_else(|| PathBuf::from("."));

                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_directory(&suggested_directory)
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
                    |picked| Message::File(FileMessage::LoadProjectPicked(picked)),
                )
            }
            FileMessage::CreateProjectPicked(Some(project_root)) => {
                let save_task = self.save_project_to_root(project_root);
                if matches!(
                    self.pending_editor_action,
                    Some(PendingEditorAction::ResolveDirtyProject { .. })
                ) {
                    let advance_task = self.advance_pending_editor_action();
                    Task::batch([save_task, advance_task])
                } else {
                    save_task
                }
            }
            FileMessage::CreateProjectPicked(None) => Task::none(),
            FileMessage::LoadProjectPicked(Some(project_root)) => {
                self.load_project_from_root(project_root)
            }
            FileMessage::LoadProjectPicked(None) => Task::none(),
            FileMessage::OpenRecentProject(project_root) => {
                self.load_project_from_root(project_root)
            }
        }
    }

    pub(in crate::app) fn handle_tick(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();

        if self.playback.is_some() && self.piano_roll.playback_is_playing() {
            self.refresh_playback_position();
        }

        tasks.push(self.tick_editor_tabbar_autoscroll());
        tasks.push(self.tick_effect_rack_autoscroll());

        if let Some(tab_id) = self.pending_reveal_editor_tab {
            if self.editor.tab_ids().contains(&tab_id) {
                if self.editor_tab_reveal_target_x(tab_id).is_some() {
                    tasks.push(self.reveal_editor_tab(tab_id));
                } else {
                    self.pending_reveal_editor_tab = None;
                }
            } else {
                self.pending_reveal_editor_tab = None;
            }
        }

        if self.editor_font_metrics_refresh_pending {
            self.editor.refresh_font_metrics();
            self.editor_font_metrics_refresh_pending = false;
            if let Some(tab_id) = self.editor.active_tab_id() {
                tasks.push(
                    self.map_editor_widget_task(tab_id, self.editor.sync_tab_scroll_state(tab_id)),
                );
            }
        }

        self.poll_editor_file_watcher(&mut tasks);
        self.poll_browser_file_watcher(&mut tasks);
        self.poll_plugin_scan();

        if self.editor_tick_active()
            && let Some(tab_id) = self.editor.active_tab_id()
        {
            let task = self.editor.update(tab_id, &iced_code_editor::Message::Tick);
            tasks.push(self.map_editor_widget_task(tab_id, task));
        }

        if self.spinner_active() {
            self.spinner_step = self.spinner_step.wrapping_add(1);
        }

        if self.spinner_active() {
            self.poll_score_watcher();
            tasks.push(self.poll_compile_logs());
            self.start_compile_if_queued();
            tasks.push(self.apply_initial_piano_roll_center_if_needed());
        }

        if self
            .score_zoom_last_interaction
            .is_some_and(|instant| instant.elapsed() >= SCORE_ZOOM_PREVIEW_SETTLE_DELAY)
        {
            self.score_zoom_last_interaction = None;
        }

        if self.score_zoom_last_interaction.is_none() && self.score_zoom_persist_pending {
            self.score_zoom_persist_pending = false;
            self.persist_settings();
        }

        Task::batch(tasks)
    }

    pub(in crate::app) fn handle_frame(&mut self, _now: std::time::Instant) -> Task<Message> {
        if let Some(message) = self.pending_mixer_message_after_editor_detach.take() {
            return self.handle_mixer_message(message);
        }

        if self.playback.is_some() {
            self.refresh_playback_position();
        }
        for error in self
            .processor_editor_windows
            .apply_requested_content_resizes()
        {
            self.log_processor_editor_error("resize", error);
        }
        for (target, command) in self.processor_editor_windows.drain_frame_commands() {
            self.handle_processor_editor_frame_command(target, command);
        }
        let close_requests = self.processor_editor_windows.close_requested_windows();
        Task::batch(
            close_requests
                .into_iter()
                .map(|window_id| self.handle_processor_editor_close_requested(window_id)),
        )
    }

    pub(in crate::app) fn activate_score(
        &mut self,
        selected_score: SelectedScore,
    ) -> Task<Message> {
        let watched_path = selected_score.path.clone();
        let editor_task = match self.editor.load_file(&watched_path) {
            Ok((tab_id, task, _)) => {
                self.register_editor_recent_file(&watched_path);
                self.pending_reveal_editor_tab = Some(tab_id);
                let sync_task = self.editor.sync_tab_scroll_state(tab_id);
                self.map_editor_widget_task(tab_id, Task::batch([task, sync_task]))
            }
            Err(error) => {
                self.show_prompt(
                    ErrorPrompt::new(
                        "Editor Load Error",
                        error,
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
                Task::none()
            }
        };
        self.logger.push(format!(
            "Opened score file {}",
            selected_score.path.display()
        ));
        self.rendered_score = None;
        self.score_cursor_maps = None;
        self.score_cursor_overlay = None;
        self.compile_outputs_loading = false;
        self.piano_roll.clear_files();
        self.unload_playback_file();
        self.current_score = Some(selected_score);
        self.persist_settings();
        if self.project_root.is_none() {
            self.saved_project_state = Some(self.current_project_state());
        }
        self.sync_editor_file_watcher();
        self.restart_score_watcher(&watched_path);
        self.queue_compile("Score loaded, compiling SVG and MIDI");
        self.start_compile_if_queued();
        editor_task
    }

    pub(in crate::app) fn restart_score_watcher(&mut self, path: &Path) {
        match crate::score_watcher::ScoreWatcher::start(path) {
            Ok(watcher) => {
                self.logger.push(format!("Watching {}", path.display()));
                self.score_watcher = Some(watcher);
            }
            Err(error) => {
                self.score_watcher = None;
                self.show_prompt(
                    ErrorPrompt::new(
                        "File Watcher Error",
                        format!("Failed to watch score file changes: {error}"),
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
            }
        }
    }

    pub(in crate::app) fn poll_score_watcher(&mut self) {
        let Some(score_watcher) = &self.score_watcher else {
            return;
        };

        let watched_path = score_watcher.watched_path().to_path_buf();
        let mut should_recompile = false;
        let mut disconnected = false;

        loop {
            match score_watcher.try_recv() {
                Ok(Ok(event)) => {
                    if is_relevant_score_change(&event, &watched_path) {
                        should_recompile = true;
                    }
                }
                Ok(Err(error)) => {
                    self.logger.push(format!("[watcher:error] {error}"));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if disconnected {
            self.score_watcher = None;
            self.show_prompt(
                ErrorPrompt::new(
                    "File Watcher Error",
                    "Score file watcher disconnected",
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
        }

        if should_recompile {
            self.queue_compile("Score changed, recompiling");
        }
    }

    pub(in crate::app) fn sync_editor_file_watcher(&mut self) {
        let paths = self.editor.file_backed_tab_paths();

        if paths.is_empty() {
            self.editor_file_watcher = None;
            return;
        }

        if self.editor_file_watcher.is_none() {
            match crate::editor_file_watcher::EditorFileWatcher::start() {
                Ok(watcher) => self.editor_file_watcher = Some(watcher),
                Err(error) => {
                    self.show_prompt(
                        ErrorPrompt::new(
                            "File Watcher Error",
                            format!("Failed to watch editor file changes: {error}"),
                            ErrorFatality::Recoverable,
                            PromptButtons::Ok,
                        ),
                        None,
                    );
                    return;
                }
            }
        }

        let sync_result = if let Some(watcher) = &mut self.editor_file_watcher {
            watcher.sync_paths(&paths)
        } else {
            Ok(())
        };

        if let Err(error) = sync_result {
            self.editor_file_watcher = None;
            self.show_prompt(
                ErrorPrompt::new(
                    "File Watcher Error",
                    format!("Failed to update editor file watches: {error}"),
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
        }
    }

    pub(in crate::app) fn sync_browser_file_watcher(&mut self) {
        if !self.editor.file_browser_expanded() {
            self.browser_file_watcher = None;
            return;
        }

        let root = self.editor.file_browser_root().to_path_buf();
        if self
            .browser_file_watcher
            .as_ref()
            .is_some_and(|watcher| watcher.watched_root() == root)
        {
            return;
        }

        match crate::browser_file_watcher::BrowserFileWatcher::start(&root) {
            Ok(watcher) => self.browser_file_watcher = Some(watcher),
            Err(error) => {
                self.browser_file_watcher = None;
                self.show_prompt(
                    ErrorPrompt::new(
                        "File Browser Error",
                        format!("Failed to watch browser directory changes: {error}"),
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
            }
        }
    }

    pub(in crate::app) fn poll_editor_file_watcher(&mut self, tasks: &mut Vec<Task<Message>>) {
        if self.editor_file_watcher.is_none() {
            return;
        }

        let watched_tabs = self.editor.file_backed_tabs();
        let mut affected_tabs = Vec::new();
        let mut disconnected = false;

        while let Some(watcher) = &self.editor_file_watcher {
            let recv_result = watcher.try_recv();
            match recv_result {
                Ok(Ok(event)) => {
                    for (tab_id, path) in &watched_tabs {
                        if is_relevant_editor_file_change(&event, path)
                            && !affected_tabs.contains(tab_id)
                        {
                            affected_tabs.push(*tab_id);
                        }
                    }
                }
                Ok(Err(error)) => {
                    self.logger.push(format!("[editor-watcher:error] {error}"));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if disconnected {
            self.editor_file_watcher = None;
            self.show_prompt(
                ErrorPrompt::new(
                    "File Watcher Error",
                    "Editor file watcher disconnected",
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
            return;
        }

        for tab_id in affected_tabs {
            if let Some(task) = self.reconcile_editor_tab_file(tab_id) {
                tasks.push(task);
            }
        }
    }

    pub(in crate::app) fn poll_browser_file_watcher(&mut self, tasks: &mut Vec<Task<Message>>) {
        let Some(watcher) = &self.browser_file_watcher else {
            return;
        };

        let watched_root = watcher.watched_root().to_path_buf();
        let mut changed = false;
        let mut disconnected = false;

        while let Some(watcher) = &self.browser_file_watcher {
            match watcher.try_recv() {
                Ok(Ok(event)) => {
                    if is_relevant_browser_file_change(&event, &watched_root) {
                        changed = true;
                    }
                }
                Ok(Err(_error)) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if disconnected {
            self.browser_file_watcher = None;
            self.show_prompt(
                ErrorPrompt::new(
                    "File Browser Error",
                    "Browser directory watcher disconnected",
                    ErrorFatality::Recoverable,
                    PromptButtons::Ok,
                ),
                None,
            );
            return;
        }

        if changed {
            tasks.push(self.refresh_browser_after_fs_change());
        }
    }

    fn reconcile_editor_tab_file(&mut self, tab_id: u64) -> Option<Task<Message>> {
        let path = self.editor.tab_path(tab_id)?.to_path_buf();

        if !path.exists() {
            if self
                .editor
                .set_tab_file_state(tab_id, EditorTabFileState::MissingOnDisk)
            {
                self.logger
                    .push(format!("Editor file missing on disk: {}", path.display()));
                if self.error_prompt.is_none() {
                    self.show_prompt(
                        ErrorPrompt::new(
                            "File Missing on Disk",
                            format!(
                                "{} was removed or moved outside Lilypalooza. The tab stays open and you can save to recreate it.",
                                path.display()
                            ),
                            ErrorFatality::Recoverable,
                            PromptButtons::Ok,
                        )
                        .with_ok_label("Keep Editing"),
                        None,
                    );
                }
                self.persist_settings();
            }
            return None;
        }

        let disk_content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                self.logger.push(format!(
                    "[editor-watcher:error] Failed to read {}: {error}",
                    path.display()
                ));
                return None;
            }
        };

        if self.editor.tab_saved_content(tab_id) == Some(disk_content.as_str()) {
            if self
                .editor
                .set_tab_file_state(tab_id, EditorTabFileState::Ok)
            {
                self.persist_settings();
            }
            return None;
        }

        if self.editor.tab_is_modified(tab_id) {
            if self
                .editor
                .set_tab_file_state(tab_id, EditorTabFileState::ChangedOnDisk)
            {
                self.logger.push(format!(
                    "Editor file changed on disk while modified: {}",
                    path.display()
                ));
                if self.error_prompt.is_none() {
                    self.show_prompt(
                        ErrorPrompt::new(
                            "File Changed on Disk",
                            format!(
                                "{} changed on disk while this tab has unsaved changes.",
                                path.display()
                            ),
                            ErrorFatality::Recoverable,
                            PromptButtons::OkCancel,
                        )
                        .with_ok_label("Reload")
                        .with_cancel_label("Keep Editor Version"),
                        Some(PromptOkAction::ReloadEditorTab(tab_id)),
                    );
                }
                self.persist_settings();
            }
            return None;
        }

        match self.editor.reload_tab_from_disk(tab_id) {
            Ok(task) => {
                self.logger
                    .push(format!("Reloaded editor file {}", path.display()));
                self.persist_settings();
                Some(self.map_editor_widget_task(tab_id, task))
            }
            Err(error) => {
                self.show_prompt(
                    ErrorPrompt::new(
                        "Editor Reload Error",
                        error,
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
                None
            }
        }
    }

    pub(in crate::app) fn queue_compile(&mut self, message: &str) {
        if !self.compile_requested {
            self.logger.push(message.to_string());
        }
        self.compile_requested = true;
    }

    pub(in crate::app) fn start_compile_if_queued(&mut self) {
        if !self.compile_requested || self.compile_session.is_some() || self.compile_outputs_loading
        {
            return;
        }

        let Some(selected_score) = self
            .current_score
            .as_ref()
            .map(|score| (score.path.clone(), score.file_name.clone()))
        else {
            self.compile_requested = false;
            return;
        };

        let output_prefix = match self.compile_output_prefix(&selected_score.1) {
            Ok(path) => path,
            Err(error) => {
                self.compile_requested = false;
                self.show_prompt(
                    ErrorPrompt::new(
                        "Build Directory Error",
                        error,
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
                return;
            }
        };

        let mut request = lilypond::CompileRequest::new(selected_score.0.clone());
        request.args = lilypond_compile_args(&output_prefix);
        request.working_dir = selected_score.0.parent().map(std::path::Path::to_path_buf);

        self.logger.push("Starting LilyPond compile".to_string());
        self.compile_generation = self.compile_generation.wrapping_add(1);

        match lilypond::spawn_compile(request) {
            Ok(session) => {
                self.compile_session = Some(session);
                self.compile_requested = false;
            }
            Err(error) => {
                self.compile_requested = false;
                self.show_prompt(
                    ErrorPrompt::new(
                        "LilyPond Compile Error",
                        error.to_string(),
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
            }
        }
    }

    pub(in crate::app) fn compile_output_prefix(
        &mut self,
        selected_file_name: &str,
    ) -> Result<std::path::PathBuf, String> {
        if self.build_dir.is_none() {
            let build_dir = tempfile::Builder::new()
                .prefix("lilypalooza-build-")
                .tempdir()
                .map_err(|error| format!("Failed to create temporary build directory: {error}"))?;
            self.logger.push(format!(
                "Using temporary build dir {}",
                build_dir.path().display()
            ));
            self.build_dir = Some(build_dir);
        }

        let file_stem = selected_score_stem(selected_file_name)?;
        let build_dir = self
            .build_dir
            .as_ref()
            .ok_or_else(|| "Temporary build directory is not available".to_string())?;

        Ok(build_dir.path().join(file_stem))
    }

    pub(in crate::app) fn poll_compile_logs(&mut self) -> Task<Message> {
        let Some(session) = self.compile_session.take() else {
            return Task::none();
        };

        let mut keep_session = true;
        let mut compile_finished_successfully = false;
        let mut log_lines = Vec::new();

        loop {
            match session.try_recv() {
                Ok(event) => match event {
                    lilypond::CompileEvent::Log { stream, line } => {
                        let prefix = match stream {
                            lilypond::LogStream::Stdout => "lilypond:stdout",
                            lilypond::LogStream::Stderr => "lilypond:stderr",
                        };
                        log_lines.push(format!("[{prefix}] {line}"));
                    }
                    lilypond::CompileEvent::ProcessError(message) => {
                        log_lines.push(format!("[lilypond:process-error] {message}"));
                    }
                    lilypond::CompileEvent::Finished { success, exit_code } => {
                        if success {
                            log_lines.push(format!(
                                "LilyPond compile finished successfully (exit code {})",
                                exit_code.unwrap_or(0)
                            ));
                            compile_finished_successfully = true;
                        } else {
                            log_lines.push(format!(
                                "LilyPond compile failed (exit code {:?})",
                                exit_code
                            ));
                        }
                        keep_session = false;
                        break;
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    keep_session = false;
                    break;
                }
            }
        }

        self.logger.extend(log_lines);

        if keep_session {
            self.compile_session = Some(session);
            return Task::none();
        }

        if !compile_finished_successfully {
            return Task::none();
        }

        let Some(selected_score) = self.current_score.as_ref() else {
            return Task::none();
        };
        let Some(build_dir) = self.build_dir.as_ref() else {
            return Task::none();
        };

        self.compile_outputs_loading = true;
        let generation = self.compile_generation;
        let build_dir = build_dir.path().to_path_buf();
        let score_path = selected_score.path.clone();
        let file_name = selected_score.file_name.clone();

        Task::perform(
            async move {
                super::messages::CompileOutputsReady {
                    generation,
                    result: load_compile_outputs(build_dir, score_path, file_name),
                }
            },
            Message::CompileOutputsReady,
        )
    }

    pub(in crate::app) fn handle_compile_outputs_ready(
        &mut self,
        ready: super::messages::CompileOutputsReady,
    ) -> Task<Message> {
        self.compile_outputs_loading = false;

        let current_score_path = self
            .current_score
            .as_ref()
            .map(|score| score.path.as_path());
        let matches_current_score = ready
            .result
            .as_ref()
            .ok()
            .is_none_or(|outputs| current_score_path == Some(outputs.score_path.as_path()));
        if ready.generation != self.compile_generation || !matches_current_score {
            self.start_compile_if_queued();
            return Task::none();
        }

        match ready.result {
            Ok(outputs) => {
                let previous_page = self
                    .rendered_score
                    .as_ref()
                    .map(|rendered_score| rendered_score.current_page)
                    .unwrap_or(0);
                let page_count = outputs.rendered_pages.len();
                self.rendered_score = Some(RenderedScore {
                    pages: outputs
                        .rendered_pages
                        .into_iter()
                        .map(|page| RenderedPage {
                            handle: svg::Handle::from_memory(page.svg_bytes.clone()),
                            svg_bytes: Bytes::from(page.svg_bytes),
                            display_size: page.display_size,
                            coord_size: page.coord_size,
                            note_anchors: page.note_anchors,
                            system_bands: page.system_bands,
                        })
                        .collect(),
                    current_page: previous_page.min(page_count.saturating_sub(1)),
                });
                self.score_zoom_preview = None;
                self.score_zoom_preview_pending = None;
                self.score_zoom_last_interaction = None;
                self.score_zoom_persist_pending = false;
                self.logger.push(format!("Loaded {page_count} SVG page(s)"));

                if outputs.midi_files.is_empty() {
                    self.logger.push("No MIDI output found");
                } else {
                    self.logger
                        .push(format!("Loaded {} MIDI file(s)", outputs.midi_files.len()));
                }
                self.piano_roll.replace_files(outputs.midi_files);
                self.sync_playback_file();

                self.score_cursor_maps = outputs.score_cursor_maps;
                if self
                    .score_cursor_maps
                    .as_ref()
                    .is_some_and(ScoreCursorMaps::is_empty)
                    && outputs.point_and_click_disabled
                {
                    self.logger.push(
                        "Score cursor unavailable because point-and-click is disabled in the score",
                    );
                }
                if let Some(maps) = &self.score_cursor_maps
                    && let Some(current_file) = self.piano_roll.current_file()
                    && outputs.score_has_repeats
                {
                    let tick_tolerance = u64::from(current_file.data.ppq);
                    if let Some(map_max_tick) = maps.max_tick_for_midi(&current_file.path)
                        && current_file.data.total_ticks <= map_max_tick + tick_tolerance
                    {
                        self.logger.push(
                            "Score has repeats but MIDI appears non-unfolded, cursor follows MIDI timeline only",
                        );
                    }
                }
                self.refresh_playback_position();
            }
            Err(error) => {
                self.rendered_score = None;
                self.score_cursor_maps = None;
                self.score_cursor_overlay = None;
                self.piano_roll.clear_files();
                self.unload_playback_file();
                self.score_zoom_preview = None;
                self.score_zoom_preview_pending = None;
                self.score_zoom_last_interaction = None;
                self.score_zoom_persist_pending = false;
                self.show_prompt(
                    ErrorPrompt::new(
                        "Compile Output Error",
                        error,
                        ErrorFatality::Recoverable,
                        PromptButtons::Ok,
                    ),
                    None,
                );
            }
        }

        self.start_compile_if_queued();
        Task::none()
    }

    pub(in crate::app) fn unload_current_score(&mut self) {
        self.current_score = None;
        self.rendered_score = None;
        self.score_cursor_maps = None;
        self.score_cursor_overlay = None;
        self.compile_outputs_loading = false;
        self.score_watcher = None;
        self.piano_roll.clear_files();
        self.unload_playback_file();
    }
}

fn lilypond_compile_args(output_prefix: &Path) -> Vec<String> {
    vec![
        "--svg".to_string(),
        "-dmidi-extension=midi".to_string(),
        "-dinclude-settings=event-listener.ly".to_string(),
        "-dpoint-and-click=note-event".to_string(),
        "-o".to_string(),
        output_prefix.to_string_lossy().to_string(),
    ]
}

fn load_compile_outputs(
    build_dir: PathBuf,
    score_path: PathBuf,
    selected_file_name: String,
) -> Result<super::LoadedCompileOutputs, String> {
    let score_stem = selected_score_stem(&selected_file_name)?;
    let rendered_pages = collect_loaded_rendered_pages(&build_dir, score_stem)?;
    if rendered_pages.is_empty() {
        return Err("LilyPond finished without SVG output".to_string());
    }

    let midi_files = midi::collect_midi_roll_files(&build_dir, score_stem)?;
    let point_and_click_disabled =
        score_cursor::score_disables_point_and_click(&score_path).unwrap_or_default();
    let score_has_repeats = score_cursor::score_contains_repeats(&score_path).unwrap_or_default();

    let all_anchors: Vec<_> = rendered_pages
        .iter()
        .flat_map(|page| page.note_anchors.iter().cloned())
        .collect();
    let score_cursor_maps =
        score_cursor::build_score_cursor_maps(&build_dir, score_stem, &all_anchors, &midi_files)
            .ok();

    Ok(super::LoadedCompileOutputs {
        score_path,
        rendered_pages,
        midi_files,
        score_cursor_maps,
        point_and_click_disabled,
        score_has_repeats,
    })
}

fn collect_loaded_rendered_pages(
    build_dir: &Path,
    score_stem: &str,
) -> Result<Vec<super::LoadedRenderedPage>, String> {
    let entries = fs::read_dir(build_dir).map_err(|error| {
        format!(
            "Failed to read build directory {}: {error}",
            build_dir.display()
        )
    })?;

    let mut pages = Vec::new();

    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Failed to read build artifact entry: {error}"))?;
        let path = entry.path();

        if !is_svg_file(&path) {
            continue;
        }

        let Some(file_stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(page_index) = svg_page_index(file_stem, score_stem) else {
            continue;
        };

        let display_size = read_svg_size(&path).unwrap_or(SvgSize {
            width: 1200.0,
            height: 1700.0,
        });
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("Failed to read SVG {}: {error}", path.display()))?;
        let coord_size =
            super::parse_svg_coordinate_size_from_source(&source).unwrap_or(display_size);

        pages.push((page_index, path, display_size, coord_size));
    }

    pages.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let mut rendered_pages = Vec::with_capacity(pages.len());

    for (index, path, display_size, coord_size) in pages {
        let bytes = fs::read(&path)
            .map_err(|error| format!("Failed to read SVG {}: {error}", path.display()))?;
        let source = String::from_utf8_lossy(&bytes);
        let page_index = index.saturating_sub(1) as usize;
        let note_anchors = score_cursor::parse_svg_note_anchors(&source, page_index);
        let system_bands = score_cursor::parse_svg_system_bands(&source);

        rendered_pages.push(super::LoadedRenderedPage {
            svg_bytes: bytes,
            display_size,
            coord_size,
            note_anchors,
            system_bands,
        });
    }

    Ok(rendered_pages)
}

#[cfg(test)]
mod tests {
    use super::lilypond_compile_args;
    use std::path::Path;

    #[test]
    fn lilypond_compile_args_keep_interactive_svg_output() {
        let args = lilypond_compile_args(Path::new("/tmp/out"));

        assert_eq!(args[0], "--svg");
        assert!(!args.iter().any(|arg| arg == "-dbackend=cairo"));
        assert!(args.iter().any(|arg| arg == "-dmidi-extension=midi"));
        assert!(args.iter().any(|arg| arg == "-dpoint-and-click=note-event"));
        assert_eq!(args[args.len() - 2], "-o");
        assert_eq!(args[args.len() - 1], "/tmp/out");
    }
}
