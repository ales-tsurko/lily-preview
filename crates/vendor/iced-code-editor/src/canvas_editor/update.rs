//! Message handling and update logic.

use iced::Task;
use iced::widget::operation::{focus, select_all};

use super::command::{
    Command, CompositeCommand, DeleteCharCommand, DeleteForwardCommand, InsertCharCommand,
    ReplaceRangeCommand, ReplaceTextCommand,
};
use super::{ArrowDirection, CURSOR_BLINK_INTERVAL, CodeEditor, ImePreedit, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Whitespace,
    Word,
    Punctuation,
}

impl CodeEditor {
    // =========================================================================
    // Helper Methods
    // =========================================================================

    /// Performs common cleanup operations after edit operations.
    ///
    /// This method should be called after any operation that modifies the buffer content.
    /// It resets the cursor blink animation, refreshes search matches if search is active,
    /// and invalidates all caches that depend on buffer content or layout:
    /// - `buffer_revision` is bumped to invalidate layout-derived caches
    /// - `visual_lines_cache` is cleared so wrapping is recalculated on next use
    /// - `content_cache` and `overlay_cache` are cleared to rebuild canvas geometry
    pub(crate) fn finish_edit_operation(&mut self) {
        self.reset_cursor_blink();
        self.preferred_column = self.cursor.1;
        self.refresh_search_matches_if_needed();
        // The exact revision value is not semantically meaningful; it only needs
        // to change on edits, so `wrapping_add` is sufficient and overflow-safe.
        self.buffer_revision = self.buffer_revision.wrapping_add(1);
        *self.visual_lines_cache.borrow_mut() = None;
        self.content_cache.clear();
        self.overlay_cache.clear();
        self.enqueue_lsp_change();
    }

    /// Performs common cleanup operations after navigation operations.
    ///
    /// This method should be called after cursor movement operations.
    /// It resets the cursor blink animation and invalidates only the overlay
    /// rendering cache. Cursor movement and selection changes do not modify the
    /// buffer content, so keeping the content cache intact avoids unnecessary
    /// re-rendering of syntax-highlighted text.
    fn finish_navigation_operation(&mut self) {
        self.reset_cursor_blink();
        self.overlay_cache.clear();
    }

    fn close_completion_for_interaction(&mut self) {
        self.close_completion(false);
        self.clear_completion_suppression_if_needed();
    }

    pub(crate) fn max_horizontal_scroll_offset(&self) -> f32 {
        if self.wrap_enabled {
            return 0.0;
        }

        let code_viewport_width = (self.viewport_width - self.gutter_width()).max(0.0);
        (self.max_content_width() - self.gutter_width() - code_viewport_width).max(0.0)
    }

    pub(crate) fn clamp_horizontal_scroll_offset(&mut self) -> bool {
        let clamped = self
            .horizontal_scroll_offset
            .clamp(0.0, self.max_horizontal_scroll_offset());

        if (self.horizontal_scroll_offset - clamped).abs() > 0.1 {
            self.horizontal_scroll_offset = clamped;
            self.content_cache.clear();
            self.overlay_cache.clear();
            true
        } else {
            false
        }
    }

    /// Starts command grouping with the given label if not already grouping.
    ///
    /// This is used for smart undo functionality, allowing multiple related
    /// operations to be undone as a single unit.
    ///
    /// # Arguments
    ///
    /// * `label` - A descriptive label for the group of commands
    fn ensure_grouping_started(&mut self, label: &str) {
        if !self.is_grouping {
            self.history.begin_group(label);
            self.is_grouping = true;
        }
    }

    /// Ends command grouping if currently active.
    ///
    /// This should be called when a series of related operations is complete,
    /// or when starting a new type of operation that shouldn't be grouped
    /// with previous operations.
    pub(crate) fn end_grouping_if_active(&mut self) {
        if self.is_grouping {
            self.history.end_group();
            self.is_grouping = false;
        }
    }

    /// Deletes the current selection and performs cleanup if a selection exists.
    ///
    /// # Returns
    ///
    /// `true` if a selection was deleted, `false` if no selection existed
    fn delete_selection_if_present(&mut self) -> bool {
        if let Some((start, end)) = self.get_selection_range()
            && start != end
        {
            self.delete_selection();
            self.finish_edit_operation();
            true
        } else {
            false
        }
    }

    fn apply_command(&mut self, mut command: Box<dyn Command>) {
        command.execute(&mut self.buffer, &mut self.cursor);
        self.history.push(command);
    }

    pub(crate) fn apply_replace_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        new_text: String,
        cursor_after: (usize, usize),
    ) {
        let command = ReplaceRangeCommand::new(
            &self.buffer,
            start,
            end,
            new_text,
            self.cursor,
            cursor_after,
        );
        self.apply_command(Box::new(command));
        self.clear_selection();
    }

    fn set_selection_after_move(&mut self, original_cursor: (usize, usize), shift_pressed: bool) {
        if shift_pressed {
            if self.selection_start.is_none() {
                self.selection_start = Some(original_cursor);
            }
            self.selection_end = Some(self.cursor);
        } else {
            self.clear_selection();
        }
    }

    fn line_indent(&self, line: usize) -> String {
        self.buffer
            .line(line)
            .chars()
            .take_while(|ch| matches!(ch, ' ' | '\t'))
            .collect()
    }

    fn first_non_whitespace_col(&self, line: usize) -> usize {
        self.buffer
            .line(line)
            .chars()
            .take_while(|ch| matches!(ch, ' ' | '\t'))
            .count()
    }

    fn trimmed_line_end_col(&self, line: usize) -> usize {
        self.buffer
            .line(line)
            .trim_end_matches([' ', '\t'])
            .chars()
            .count()
    }

    pub(crate) fn position_after_text(start: (usize, usize), text: &str) -> (usize, usize) {
        let lines: Vec<&str> = text.split('\n').collect();
        if lines.len() == 1 {
            (start.0, start.1 + text.chars().count())
        } else {
            (
                start.0 + lines.len() - 1,
                lines.last().map_or(0, |line| line.chars().count()),
            )
        }
    }

    fn word_class(ch: char) -> WordClass {
        if ch.is_whitespace() {
            WordClass::Whitespace
        } else if Self::is_word_char(ch) {
            WordClass::Word
        } else {
            WordClass::Punctuation
        }
    }

    fn previous_word_boundary(&self, position: (usize, usize)) -> (usize, usize) {
        let (mut line, mut col) = position;

        loop {
            if line >= self.buffer.line_count() {
                return position;
            }

            if col == 0 {
                if line == 0 {
                    return (0, 0);
                }
                line -= 1;
                col = self.buffer.line_len(line);
                continue;
            }

            let chars: Vec<char> = self.buffer.line(line).chars().collect();
            let mut idx = col.min(chars.len());

            while idx > 0 && chars[idx - 1].is_whitespace() {
                idx -= 1;
            }

            if idx == 0 {
                return (line, 0);
            }

            let class = Self::word_class(chars[idx - 1]);
            while idx > 0 && Self::word_class(chars[idx - 1]) == class {
                idx -= 1;
            }
            return (line, idx);
        }
    }

    fn next_word_boundary(&self, position: (usize, usize)) -> (usize, usize) {
        let (mut line, mut col) = position;

        loop {
            if line >= self.buffer.line_count() {
                return position;
            }

            let chars: Vec<char> = self.buffer.line(line).chars().collect();
            if col >= chars.len() {
                if line + 1 >= self.buffer.line_count() {
                    return (line, chars.len());
                }
                line += 1;
                col = 0;
                continue;
            }

            let class = Self::word_class(chars[col]);
            let mut idx = col;
            while idx < chars.len() && Self::word_class(chars[idx]) == class {
                idx += 1;
            }
            return (line, idx);
        }
    }

    fn is_between_empty_pair(&self) -> Option<(char, char)> {
        let (line, col) = self.cursor;
        let chars: Vec<char> = self.buffer.line(line).chars().collect();
        let prev = col.checked_sub(1).and_then(|idx| chars.get(idx)).copied()?;
        let next = chars.get(col).copied()?;
        if self.matching_closer(prev) == Some(next) {
            Some((prev, next))
        } else {
            None
        }
    }

    fn matching_closer(&self, ch: char) -> Option<char> {
        self.language_config()
            .auto_closing_pairs
            .iter()
            .find_map(|(open, close)| (*open == ch).then_some(*close))
    }

    fn is_closing_delimiter(&self, ch: char) -> bool {
        self.language_config()
            .auto_closing_pairs
            .iter()
            .any(|(_, close)| *close == ch)
    }

    fn should_autopair(&self, ch: char) -> bool {
        let Some(closer) = self.matching_closer(ch) else {
            return false;
        };

        let line = self.buffer.line(self.cursor.0);
        let next_char = line.chars().nth(self.cursor.1);

        match next_char {
            None => true,
            Some(next) if next.is_whitespace() => true,
            Some(next) if self.is_closing_delimiter(next) => true,
            Some(next) if next == closer && matches!(ch, '"' | '\'') => true,
            _ => false,
        }
    }

    fn insert_pair(&mut self, open: char, close: char) {
        self.ensure_grouping_started("Typing");
        let (line, col) = self.cursor;
        self.apply_replace_range(
            (line, col),
            (line, col),
            format!("{open}{close}"),
            (line, col + 1),
        );
        self.finish_edit_operation();
    }

    fn replace_selection_with_text(&mut self, text: String, cursor_after: (usize, usize)) {
        let (start, end) = self
            .get_selection_range()
            .unwrap_or((self.cursor, self.cursor));
        self.apply_replace_range(start, end, text, cursor_after);
        self.finish_edit_operation();
    }

    // =========================================================================
    // Text Input Handlers
    // =========================================================================

    /// Handles character input message operations.
    ///
    /// Inserts a character at the current cursor position and adds it to the
    /// undo history. Characters are grouped together for smart undo.
    /// Only processes input when the editor has active focus and is not locked.
    ///
    /// # Arguments
    ///
    /// * `ch` - The character to insert
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible (including
    /// horizontal scroll when wrap is disabled)
    fn handle_character_input_msg(&mut self, ch: char) -> Task<Message> {
        if !self.has_focus() {
            return Task::none();
        }

        self.last_key_character_input = Some(ch);
        self.ensure_grouping_started("Typing");

        if let Some((start, end)) = self.get_selection_range()
            && start != end
            && let Some(close) = self.matching_closer(ch)
        {
            let selected_text = self.get_selected_text().unwrap_or_default();
            let new_text = format!("{ch}{selected_text}{close}");
            let cursor_after = Self::position_after_text(start, &new_text);
            self.replace_selection_with_text(new_text, cursor_after);
            return self.scroll_to_cursor();
        }

        if let Some((start, end)) = self.get_selection_range()
            && start != end
        {
            let cursor_after = (start.0, start.1 + 1);
            self.replace_selection_with_text(ch.to_string(), cursor_after);
            return self.scroll_to_cursor();
        }

        if let Some(next_char) = self.buffer.line(self.cursor.0).chars().nth(self.cursor.1)
            && self.is_closing_delimiter(ch)
            && next_char == ch
        {
            self.cursor.1 += 1;
            self.finish_navigation_operation();
            return self.scroll_to_cursor();
        }

        if let Some(close) = self.matching_closer(ch)
            && self.should_autopair(ch)
        {
            self.insert_pair(ch, close);
            return self.scroll_to_cursor();
        }

        let (line, col) = self.cursor;
        self.apply_command(Box::new(InsertCharCommand::new(line, col, ch, self.cursor)));
        self.finish_edit_operation();
        self.clear_completion_suppression_if_needed();
        if self.should_auto_trigger_completion(ch) {
            self.trigger_completion(false);
        } else {
            self.close_completion(false);
        }

        self.scroll_to_cursor()
    }

    /// Handles Tab key press (inserts 4 spaces).
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible (including
    /// horizontal scroll when wrap is disabled)
    fn handle_tab(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        if let Some((start, end)) = self.get_selection_range()
            && start != end
        {
            let (start_line, end_line) = self.touched_line_range();
            let mut composite = CompositeCommand::new("Indent".to_string());
            for line in start_line..=end_line {
                composite.add(Box::new(ReplaceRangeCommand::new(
                    &self.buffer,
                    (line, 0),
                    (line, 0),
                    "    ".to_string(),
                    self.cursor,
                    self.cursor,
                )));
            }
            composite.execute(&mut self.buffer, &mut self.cursor);
            self.history.push(Box::new(composite));
            self.selection_start = Some((start.0, if start.1 == 0 { 0 } else { start.1 + 4 }));
            self.selection_end = Some((
                end.0,
                if end.0 > start.0 && end.1 == 0 {
                    0
                } else {
                    end.1 + 4
                },
            ));
        } else {
            let (line, col) = self.cursor;
            self.apply_replace_range(
                (line, col),
                (line, col),
                "    ".to_string(),
                (line, col + 4),
            );
        }

        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    fn handle_shift_tab(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        let original_selection = self.get_selection_range();
        let (start_line, end_line) = if original_selection.is_some_and(|(start, end)| start != end)
        {
            self.touched_line_range()
        } else {
            (self.cursor.0, self.cursor.0)
        };

        let mut edits = Vec::new();
        for line in start_line..=end_line {
            let indent: String = self
                .buffer
                .line(line)
                .chars()
                .take_while(|ch| *ch == ' ')
                .take(4)
                .collect();
            let remove_count = indent.chars().count();
            if remove_count > 0 {
                edits.push((line, remove_count));
            }
        }

        if edits.is_empty() {
            return Task::none();
        }

        let mut composite = CompositeCommand::new("Outdent".to_string());
        for (line, remove_count) in edits.iter().copied() {
            composite.add(Box::new(ReplaceRangeCommand::new(
                &self.buffer,
                (line, 0),
                (line, remove_count),
                String::new(),
                self.cursor,
                self.cursor,
            )));
        }
        composite.execute(&mut self.buffer, &mut self.cursor);
        self.history.push(Box::new(composite));

        if let Some((start, end)) = original_selection
            && start != end
        {
            let start_removed = edits
                .iter()
                .find(|(line, _)| *line == start.0)
                .map_or(0, |(_, count)| *count);
            let end_removed = edits
                .iter()
                .find(|(line, _)| *line == end.0)
                .map_or(0, |(_, count)| *count);
            self.selection_start = Some((start.0, start.1.saturating_sub(start_removed)));
            self.selection_end = Some((
                end.0,
                if end.0 > start.0 && end.1 == 0 {
                    0
                } else {
                    end.1.saturating_sub(end_removed)
                },
            ));
        } else if self.cursor.1 > 0 {
            let removed = edits
                .iter()
                .find(|(line, _)| *line == self.cursor.0)
                .map_or(0, |(_, count)| *count);
            self.cursor.1 = self.cursor.1.saturating_sub(removed);
        }

        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    fn touched_line_range(&self) -> (usize, usize) {
        if let Some((start, end)) = self.get_selection_range() {
            let mut end_line = end.0;
            if end_line > start.0 && end.1 == 0 {
                end_line = end_line.saturating_sub(1);
            }
            (start.0, end_line.max(start.0))
        } else {
            (self.cursor.0, self.cursor.0)
        }
    }

    fn full_document_end(&self) -> (usize, usize) {
        let last_line = self.buffer.line_count().saturating_sub(1);
        (last_line, self.buffer.line_len(last_line))
    }

    fn document_lines(&self) -> Vec<String> {
        (0..self.buffer.line_count())
            .map(|line| self.buffer.line(line).to_string())
            .collect()
    }

    fn replace_entire_document(
        &mut self,
        lines: Vec<String>,
        cursor_after: (usize, usize),
        selection_after: Option<((usize, usize), (usize, usize))>,
        group_name: &str,
    ) -> Task<Message> {
        let new_text = if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n")
        };

        if new_text == self.buffer.to_string() {
            return Task::none();
        }

        self.end_grouping_if_active();
        self.ensure_grouping_started(group_name);
        self.apply_replace_range((0, 0), self.full_document_end(), new_text, cursor_after);
        self.end_grouping_if_active();
        if let Some((start, end)) = selection_after {
            self.selection_start = Some(start);
            self.selection_end = Some(end);
            self.cursor = end;
        }
        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    fn line_comment_state(&self, line: &str, token: &str) -> Option<(usize, bool)> {
        let indent_len = line
            .chars()
            .take_while(|ch| matches!(ch, ' ' | '\t'))
            .count();
        let rest: String = line.chars().skip(indent_len).collect();
        if rest.starts_with(token) {
            let has_space = rest[token.chars().count()..].starts_with(' ');
            Some((indent_len, has_space))
        } else {
            None
        }
    }

    fn adjust_position_for_comment(
        pos: (usize, usize),
        line_index: usize,
        indent_len: usize,
        delta: isize,
    ) -> (usize, usize) {
        if pos.0 != line_index || pos.1 <= indent_len {
            return pos;
        }

        let next_col = if delta.is_negative() {
            pos.1.saturating_sub(delta.unsigned_abs())
        } else {
            pos.1.saturating_add(delta as usize)
        };
        (pos.0, next_col)
    }

    fn comment_token(&self) -> Option<&'static str> {
        self.language_config().line_comment
    }

    fn block_comment_tokens(&self) -> Option<(&'static str, &'static str)> {
        self.language_config().block_comment
    }

    fn line_text_with_trailing_newline(&self, start_line: usize, end_line: usize) -> String {
        let mut text = String::new();
        for line in start_line..=end_line {
            text.push_str(self.buffer.line(line));
            if line < end_line || end_line + 1 < self.buffer.line_count() {
                text.push('\n');
            }
        }
        text
    }

    fn word_range_at(&self, position: (usize, usize)) -> Option<((usize, usize), (usize, usize))> {
        let line = self.buffer.line(position.0);
        let start = Self::word_start_in_line(line, position.1);
        let end = Self::word_end_in_line(line, position.1);
        (start < end).then_some(((position.0, start), (position.0, end)))
    }

    fn join_requires_space(joined: &str, trimmed_next: &str) -> bool {
        let prev = joined.chars().next_back();
        let next = trimmed_next.chars().next();

        let Some(prev) = prev else {
            return false;
        };
        let Some(next) = next else {
            return false;
        };

        let next_is_tight = matches!(next, ')' | ']' | '}' | ',' | '.' | ';' | ':');
        let prev_is_tight = matches!(prev, '(' | '[' | '{');

        !next_is_tight && !prev_is_tight && !joined.ends_with(' ')
    }

    fn select_line_at(&mut self, line: usize) {
        let end = if line + 1 < self.buffer.line_count() {
            (line + 1, 0)
        } else {
            (line, self.buffer.line_len(line))
        };
        self.selection_start = Some((line, 0));
        self.selection_end = Some(end);
        self.cursor = end;
    }

    /// Handles Tab key press for focus navigation (when search dialog is not open).
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that may navigate focus to another editor
    fn handle_focus_navigation_tab(&mut self) -> Task<Message> {
        // Only handle focus navigation if search dialog is not open
        if !self.search_state.is_open {
            // Lose focus from current editor
            self.has_canvas_focus = false;
            self.show_cursor = false;

            // Return a task that could potentially focus another editor
            // This implements focus chain management by allowing the parent application
            // to handle focus navigation between multiple editors
            Task::none()
        } else {
            Task::none()
        }
    }

    /// Handles Shift+Tab key press for focus navigation (when search dialog is not open).
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that may navigate focus to another editor
    fn handle_focus_navigation_shift_tab(&mut self) -> Task<Message> {
        // Only handle focus navigation if search dialog is not open
        if !self.search_state.is_open {
            // Lose focus from current editor
            self.has_canvas_focus = false;
            self.show_cursor = false;

            // Return a task that could potentially focus another editor
            // This implements focus chain management by allowing the parent application
            // to handle focus navigation between multiple editors
            Task::none()
        } else {
            Task::none()
        }
    }

    /// Handles Enter key press (inserts newline).
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_enter(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        let (line, col) = self
            .get_selection_range()
            .map_or(self.cursor, |(start, _)| start);
        let line_content = self.buffer.line(line);
        let current_indent = self.line_indent(line);
        let previous_char = col
            .checked_sub(1)
            .and_then(|idx| line_content.chars().nth(idx));
        let next_char = line_content.chars().nth(col);
        let selection_end = self
            .get_selection_range()
            .map_or((line, col), |(_, end)| end);

        let extra_indent = if matches!(previous_char, Some('(' | '[' | '{')) {
            "    "
        } else {
            ""
        };

        if matches!(
            (previous_char, next_char),
            (Some('('), Some(')')) | (Some('['), Some(']')) | (Some('{'), Some('}'))
        ) {
            let new_text = format!("\n{current_indent}    \n{current_indent}");
            self.apply_replace_range(
                (line, col),
                selection_end,
                new_text,
                (line + 1, current_indent.chars().count() + 4),
            );
        } else {
            let new_text = format!("\n{current_indent}{extra_indent}");
            self.apply_replace_range(
                (line, col),
                selection_end,
                new_text,
                (
                    line + 1,
                    current_indent.chars().count() + extra_indent.chars().count(),
                ),
            );
        }

        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    // =========================================================================
    // Deletion Handlers
    // =========================================================================

    /// Handles Backspace key press.
    ///
    /// If there's a selection, deletes the selection. Otherwise, deletes the
    /// character before the cursor.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible if selection was deleted
    fn handle_backspace(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        if self.delete_selection_if_present() {
            return self.scroll_to_cursor();
        }

        if self.is_between_empty_pair().is_some() {
            let (line, col) = self.cursor;
            self.apply_replace_range(
                (line, col - 1),
                (line, col + 1),
                String::new(),
                (line, col - 1),
            );
            self.finish_edit_operation();
            return self.scroll_to_cursor();
        }

        let (line, col) = self.cursor;
        self.apply_command(Box::new(DeleteCharCommand::new(
            &self.buffer,
            line,
            col,
            self.cursor,
        )));

        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    /// Handles Delete key press.
    ///
    /// If there's a selection, deletes the selection. Otherwise, deletes the
    /// character after the cursor.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible if selection was deleted
    fn handle_delete(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        if self.delete_selection_if_present() {
            return self.scroll_to_cursor();
        }

        let (line, col) = self.cursor;
        self.apply_command(Box::new(DeleteForwardCommand::new(
            &self.buffer,
            line,
            col,
            self.cursor,
        )));

        self.finish_edit_operation();
        Task::none()
    }

    fn handle_delete_word_backward(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        if self.delete_selection_if_present() {
            return self.scroll_to_cursor();
        }

        let start = self.previous_word_boundary(self.cursor);
        if start == self.cursor {
            return Task::none();
        }

        self.apply_replace_range(start, self.cursor, String::new(), start);
        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    fn handle_delete_word_forward(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        if self.delete_selection_if_present() {
            return self.scroll_to_cursor();
        }

        let end = self.next_word_boundary(self.cursor);
        if end == self.cursor {
            return Task::none();
        }

        self.apply_replace_range(self.cursor, end, String::new(), self.cursor);
        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    fn handle_delete_to_line_start(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        if self.delete_selection_if_present() {
            return self.scroll_to_cursor();
        }

        let start = (self.cursor.0, 0);
        if start == self.cursor {
            return Task::none();
        }

        self.apply_replace_range(start, self.cursor, String::new(), start);
        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    fn handle_delete_to_line_end(&mut self) -> Task<Message> {
        self.end_grouping_if_active();

        if self.delete_selection_if_present() {
            return self.scroll_to_cursor();
        }

        let end = (self.cursor.0, self.buffer.line_len(self.cursor.0));
        if end == self.cursor {
            return Task::none();
        }

        self.apply_replace_range(self.cursor, end, String::new(), self.cursor);
        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    /// Handles explicit selection deletion (Shift+Delete).
    ///
    /// Deletes the selected text if a selection exists.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_delete_selection(&mut self) -> Task<Message> {
        // End grouping on delete selection
        self.end_grouping_if_active();

        if self.selection_start.is_some() && self.selection_end.is_some() {
            self.delete_selection();
            self.finish_edit_operation();
            self.scroll_to_cursor()
        } else {
            Task::none()
        }
    }

    // =========================================================================
    // Navigation Handlers
    // =========================================================================

    /// Handles arrow key navigation.
    ///
    /// # Arguments
    ///
    /// * `direction` - The direction of movement
    /// * `shift_pressed` - Whether Shift is held (for selection)
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_arrow_key(
        &mut self,
        direction: ArrowDirection,
        shift_pressed: bool,
    ) -> Task<Message> {
        self.end_grouping_if_active();
        let original_cursor = self.cursor;
        self.move_cursor(direction);
        self.set_selection_after_move(original_cursor, shift_pressed);
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    fn handle_word_arrow_key(
        &mut self,
        direction: ArrowDirection,
        shift_pressed: bool,
    ) -> Task<Message> {
        self.end_grouping_if_active();
        let original_cursor = self.cursor;

        self.cursor = match direction {
            ArrowDirection::Left => self.previous_word_boundary(self.cursor),
            ArrowDirection::Right => self.next_word_boundary(self.cursor),
            ArrowDirection::Up | ArrowDirection::Down => self.cursor,
        };

        self.sync_preferred_column();
        self.set_selection_after_move(original_cursor, shift_pressed);
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    /// Handles Home key press.
    ///
    /// Moves the cursor to the start of the current line.
    ///
    /// # Arguments
    ///
    /// * `shift_pressed` - Whether Shift is held (for selection)
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible (including
    /// horizontal scroll back to x=0 when wrap is disabled)
    fn handle_home(&mut self, shift_pressed: bool) -> Task<Message> {
        let original_cursor = self.cursor;
        let first_non_ws = self.first_non_whitespace_col(self.cursor.0);
        self.cursor.1 = if self.cursor.1 == first_non_ws {
            0
        } else {
            first_non_ws
        };
        self.sync_preferred_column();
        self.set_selection_after_move(original_cursor, shift_pressed);
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    /// Handles End key press.
    ///
    /// Moves the cursor to the end of the current line.
    ///
    /// # Arguments
    ///
    /// * `shift_pressed` - Whether Shift is held (for selection)
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible (including
    /// horizontal scroll to end of line when wrap is disabled)
    fn handle_end(&mut self, shift_pressed: bool) -> Task<Message> {
        let line = self.cursor.0;
        let line_len = self.buffer.line_len(line);
        let trimmed_end = self.trimmed_line_end_col(line);
        let original_cursor = self.cursor;
        self.cursor.1 = if self.cursor.1 == trimmed_end {
            line_len
        } else {
            trimmed_end
        };
        self.sync_preferred_column();
        self.set_selection_after_move(original_cursor, shift_pressed);
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    /// Handles Ctrl+Home key press.
    ///
    /// Moves the cursor to the beginning of the document.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_document_home(&mut self, shift_pressed: bool) -> Task<Message> {
        let original_cursor = self.cursor;
        self.cursor = (0, 0);
        self.sync_preferred_column();
        self.set_selection_after_move(original_cursor, shift_pressed);
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    /// Handles Ctrl+End key press.
    ///
    /// Moves the cursor to the end of the document.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_document_end(&mut self, shift_pressed: bool) -> Task<Message> {
        let original_cursor = self.cursor;
        let last_line = self.buffer.line_count().saturating_sub(1);
        let last_col = self.buffer.line_len(last_line);
        self.cursor = (last_line, last_col);
        self.sync_preferred_column();
        self.set_selection_after_move(original_cursor, shift_pressed);
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    /// Handles Page Up key press.
    ///
    /// Scrolls the view up by one page.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_page_up(&mut self) -> Task<Message> {
        self.page_up();
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    /// Handles Page Down key press.
    ///
    /// Scrolls the view down by one page.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_page_down(&mut self) -> Task<Message> {
        self.page_down();
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    /// Handles direct navigation to an explicit logical position.
    ///
    /// # Arguments
    ///
    /// * `line` - Target line index (0-based)
    /// * `col` - Target column index (0-based)
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to keep the cursor visible
    fn handle_goto_position(&mut self, line: usize, col: usize) -> Task<Message> {
        // End grouping on navigation command
        self.end_grouping_if_active();
        self.set_cursor(line, col)
    }

    fn parse_goto_line_query(query: &str) -> Option<(usize, usize)> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return None;
        }

        let mut parts = trimmed.split([':', ',']);
        let line = parts.next()?.trim().parse::<usize>().ok()?;
        let col = parts
            .next()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(1);
        Some((line.saturating_sub(1), col.saturating_sub(1)))
    }

    fn handle_open_goto_line_msg(&mut self) -> Task<Message> {
        self.search_state.close();
        self.goto_line_state.open();
        self.goto_line_state.query = format!("{}", self.cursor.0 + 1);
        Task::batch([
            focus(self.goto_line_state.input_id.clone()),
            select_all(self.goto_line_state.input_id.clone()),
        ])
    }

    fn handle_close_goto_line_msg(&mut self) -> Task<Message> {
        self.goto_line_state.close();
        Task::none()
    }

    fn handle_goto_line_query_changed_msg(&mut self, query: &str) -> Task<Message> {
        self.goto_line_state.query = query.to_string();
        Task::none()
    }

    fn handle_submit_goto_line_msg(&mut self) -> Task<Message> {
        let Some((line, col)) = Self::parse_goto_line_query(&self.goto_line_state.query) else {
            return Task::none();
        };
        self.goto_line_state.close();
        self.handle_goto_position(line, col)
    }

    fn handle_insert_line_below(&mut self) -> Task<Message> {
        let mut lines = self.document_lines();
        let current_line = self.cursor.0.min(lines.len().saturating_sub(1));
        let indent = self.line_indent(current_line);
        let insert_index = current_line + 1;
        lines.insert(insert_index, indent.clone());
        self.replace_entire_document(
            lines,
            (insert_index, indent.chars().count()),
            None,
            "Insert Line",
        )
    }

    fn handle_insert_line_above(&mut self) -> Task<Message> {
        let mut lines = self.document_lines();
        let current_line = self.cursor.0.min(lines.len().saturating_sub(1));
        let indent = self.line_indent(current_line);
        lines.insert(current_line, indent.clone());
        self.replace_entire_document(
            lines,
            (current_line, indent.chars().count()),
            None,
            "Insert Line",
        )
    }

    fn handle_delete_line(&mut self) -> Task<Message> {
        let (start_line, end_line) = self.touched_line_range();
        let mut lines = self.document_lines();
        if lines.is_empty() {
            return Task::none();
        }
        lines.drain(start_line..=end_line.min(lines.len().saturating_sub(1)));
        if lines.is_empty() {
            lines.push(String::new());
        }
        let next_line = start_line.min(lines.len().saturating_sub(1));
        let next_col = self.cursor.1.min(lines[next_line].chars().count());
        self.replace_entire_document(lines, (next_line, next_col), None, "Delete Line")
    }

    fn handle_move_line(&mut self, down: bool) -> Task<Message> {
        let (start_line, end_line) = self.touched_line_range();
        let mut lines = self.document_lines();
        let last_line = lines.len().saturating_sub(1);
        if (!down && start_line == 0) || (down && end_line >= last_line) {
            return Task::none();
        }

        let block: Vec<String> = lines.drain(start_line..=end_line).collect();
        let insert_at = if down { start_line + 1 } else { start_line - 1 };
        for (offset, line) in block.into_iter().enumerate() {
            lines.insert(insert_at + offset, line);
        }

        let line_delta = if down { 1isize } else { -1isize };
        let shift_line = |line: usize| -> usize {
            if line_delta.is_negative() {
                line.saturating_sub(line_delta.unsigned_abs())
            } else {
                line.saturating_add(line_delta as usize)
            }
        };
        let cursor_after = (shift_line(self.cursor.0), self.cursor.1);
        let selection_after = self
            .get_selection_range()
            .map(|(start, end)| ((shift_line(start.0), start.1), (shift_line(end.0), end.1)));

        self.replace_entire_document(lines, cursor_after, selection_after, "Move Line")
    }

    fn handle_copy_line(&mut self, down: bool) -> Task<Message> {
        if let Some((start, end)) = self.get_selection_range()
            && start != end
        {
            let selected_text = self.get_selected_text().unwrap_or_default();
            let insert_at = if down { end } else { start };
            let cursor_after = if down {
                Self::position_after_text(insert_at, &selected_text)
            } else {
                Self::position_after_text(start, &selected_text)
            };
            self.apply_replace_range(insert_at, insert_at, selected_text.clone(), cursor_after);
            if down {
                self.selection_start = Some(end);
                self.selection_end = Some(cursor_after);
            } else {
                self.selection_start = Some(start);
                self.selection_end = Some(cursor_after);
            }
            self.finish_edit_operation();
            return self.scroll_to_cursor();
        }

        let (start_line, end_line) = self.touched_line_range();
        let mut lines = self.document_lines();
        let block: Vec<String> = lines[start_line..=end_line].to_vec();
        let line_count = end_line - start_line + 1;
        let insert_at = if down { end_line + 1 } else { start_line };
        for (offset, line) in block.into_iter().enumerate() {
            lines.insert(insert_at + offset, line);
        }

        let line_delta = if down { line_count } else { 0 };
        let cursor_after = (self.cursor.0 + line_delta, self.cursor.1);
        let selection_after = self
            .get_selection_range()
            .map(|(start, end)| ((start.0 + line_delta, start.1), (end.0 + line_delta, end.1)));

        self.replace_entire_document(lines, cursor_after, selection_after, "Copy Line")
    }

    fn handle_join_lines(&mut self) -> Task<Message> {
        let (start_line, mut end_line) = self.touched_line_range();
        let mut lines = self.document_lines();
        if start_line >= lines.len() {
            return Task::none();
        }

        if start_line == end_line {
            if end_line + 1 >= lines.len() {
                return Task::none();
            }
            end_line += 1;
        }

        let mut joined = lines[start_line].trim_end().to_string();
        for next_line in &lines[(start_line + 1)..=end_line] {
            let trimmed = next_line.trim_start();
            if !joined.is_empty()
                && !trimmed.is_empty()
                && Self::join_requires_space(&joined, trimmed)
            {
                joined.push(' ');
            }
            joined.push_str(trimmed);
        }

        lines.splice(start_line..=end_line, [joined.clone()]);
        self.replace_entire_document(
            lines,
            (start_line, joined.chars().count()),
            None,
            "Join Lines",
        )
    }

    fn handle_toggle_line_comment(&mut self) -> Task<Message> {
        let Some(token) = self.comment_token() else {
            return Task::none();
        };
        let (start_line, end_line) = self.touched_line_range();
        let mut lines = self.document_lines();
        let non_empty_lines = (start_line..=end_line)
            .filter(|line| !lines[*line].trim().is_empty())
            .collect::<Vec<_>>();
        let uncomment = !non_empty_lines.is_empty()
            && non_empty_lines
                .iter()
                .all(|line| self.line_comment_state(&lines[*line], token).is_some());

        let original_selection = self.get_selection_range();
        let mut cursor_after = self.cursor;
        let mut selection_after = original_selection;

        for (line_index, line_ref) in lines
            .iter_mut()
            .enumerate()
            .take(end_line + 1)
            .skip(start_line)
        {
            let line = line_ref.clone();
            let indent_len = line
                .chars()
                .take_while(|ch| matches!(ch, ' ' | '\t'))
                .count();
            if uncomment {
                if let Some((_, had_space)) = self.line_comment_state(&line, token) {
                    let remove_len = token.chars().count() + usize::from(had_space);
                    let mut chars = line.chars().collect::<Vec<_>>();
                    chars.drain(indent_len..indent_len + remove_len);
                    *line_ref = chars.into_iter().collect();
                    cursor_after = Self::adjust_position_for_comment(
                        cursor_after,
                        line_index,
                        indent_len,
                        -(remove_len as isize),
                    );
                    selection_after = selection_after.map(|(start, end)| {
                        (
                            Self::adjust_position_for_comment(
                                start,
                                line_index,
                                indent_len,
                                -(remove_len as isize),
                            ),
                            Self::adjust_position_for_comment(
                                end,
                                line_index,
                                indent_len,
                                -(remove_len as isize),
                            ),
                        )
                    });
                }
            } else {
                let insert_text = if line.trim().is_empty() {
                    token.to_string()
                } else {
                    format!("{token} ")
                };
                let mut chars = line.chars().collect::<Vec<_>>();
                for (offset, ch) in insert_text.chars().enumerate() {
                    chars.insert(indent_len + offset, ch);
                }
                *line_ref = chars.into_iter().collect();
                let delta = insert_text.chars().count() as isize;
                cursor_after =
                    Self::adjust_position_for_comment(cursor_after, line_index, indent_len, delta);
                selection_after = selection_after.map(|(start, end)| {
                    (
                        Self::adjust_position_for_comment(start, line_index, indent_len, delta),
                        Self::adjust_position_for_comment(end, line_index, indent_len, delta),
                    )
                });
            }
        }

        self.replace_entire_document(lines, cursor_after, selection_after, "Toggle Line Comment")
    }

    fn handle_toggle_block_comment(&mut self) -> Task<Message> {
        let Some((open, close)) = self.block_comment_tokens() else {
            return Task::none();
        };

        let Some((start, end)) = self.get_selection_range() else {
            let cursor_after = Self::position_after_text(self.cursor, open);
            self.apply_replace_range(
                self.cursor,
                self.cursor,
                format!("{open}{close}"),
                cursor_after,
            );
            self.finish_edit_operation();
            return self.scroll_to_cursor();
        };

        let selected_text = self
            .get_selected_text()
            .unwrap_or_else(|| self.line_text_with_trailing_newline(start.0, end.0));

        if selected_text.starts_with(open) && selected_text.ends_with(close) {
            let inner = selected_text[open.len()..selected_text.len() - close.len()].to_string();
            let cursor_after = Self::position_after_text(start, &inner);
            self.apply_replace_range(start, end, inner, cursor_after);
            self.selection_start = Some(start);
            self.selection_end = Some(cursor_after);
            self.finish_edit_operation();
            return self.scroll_to_cursor();
        }

        let new_text = format!("{open}{selected_text}{close}");
        let selection_start = Self::position_after_text(start, open);
        let selection_end = Self::position_after_text(selection_start, &selected_text);
        self.apply_replace_range(start, end, new_text, selection_end);
        self.selection_start = Some(selection_start);
        self.selection_end = Some(selection_end);
        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    fn handle_select_line(&mut self) -> Task<Message> {
        self.select_line_at(self.cursor.0);
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    fn handle_select_all(&mut self) -> Task<Message> {
        let end = self.full_document_end();
        self.selection_start = Some((0, 0));
        self.selection_end = Some(end);
        self.cursor = end;
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    fn handle_jump_to_matching_bracket(&mut self) -> Task<Message> {
        self.end_grouping_if_active();
        if let Some((first, second)) = self.matching_bracket_pair() {
            self.cursor = if self.cursor == first { second } else { first };
        } else if let Some((nearest, _)) = self.nearest_bracket_pair(self.cursor) {
            self.cursor = nearest;
        } else {
            return Task::none();
        }
        self.clear_selection();
        self.finish_navigation_operation();
        self.scroll_to_cursor()
    }

    // =========================================================================
    // Mouse and Selection Handlers
    // =========================================================================

    /// Handles mouse click operations.
    ///
    /// Sets focus, ends command grouping, positions cursor, starts selection tracking.
    ///
    /// # Arguments
    ///
    /// * `point` - The click position
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that keeps the cursor centered when center-cursor mode is enabled.
    fn handle_mouse_click_msg(&mut self, point: iced::Point) -> Task<Message> {
        // Capture focus when clicked using the new focus method
        self.request_focus();

        // Set internal canvas focus state
        self.has_canvas_focus = true;
        self.focus_locked = false;

        // End grouping on mouse click
        self.end_grouping_if_active();

        let previous_cursor = self.cursor;
        let previous_selection_start = self.selection_start;
        self.handle_mouse_click(point);
        let clicked_cursor = self.cursor;
        self.reset_cursor_blink();
        if self
            .modifiers
            .get()
            .contains(iced::keyboard::Modifiers::SHIFT)
        {
            self.selection_start = Some(previous_selection_start.unwrap_or(previous_cursor));
            self.selection_end = Some(clicked_cursor);
            self.is_dragging = false;
        } else {
            self.selection_start = Some(clicked_cursor);
            self.selection_end = Some(clicked_cursor);
            self.is_dragging = true;
        }

        // Show cursor when focused
        self.show_cursor = true;

        if !self.center_cursor || clicked_cursor == previous_cursor {
            Task::none()
        } else {
            self.pending_center_cursor_scroll = Some(clicked_cursor);
            self.scroll_clicked_cursor_to_center(clicked_cursor)
        }
    }

    fn scroll_clicked_cursor_to_center(&self, cursor: (usize, usize)) -> Task<Message> {
        iced::Task::batch([
            iced::widget::operation::scroll_to(
                self.scrollable_id.clone(),
                iced::widget::scrollable::AbsoluteOffset {
                    x: 0.0,
                    y: self.target_vertical_scroll_offset_for_line(cursor.0, cursor.1),
                },
            ),
            iced::widget::operation::scroll_to(
                self.horizontal_scrollable_id.clone(),
                iced::widget::scrollable::AbsoluteOffset {
                    x: self.horizontal_scroll_offset,
                    y: 0.0,
                },
            ),
        ])
    }

    fn handle_mouse_double_click_msg(&mut self, point: iced::Point) -> Task<Message> {
        self.request_focus();
        self.has_canvas_focus = true;
        self.focus_locked = false;
        self.end_grouping_if_active();

        let Some(cursor) = self.calculate_cursor_from_point(point) else {
            return Task::none();
        };

        let word_range = self.word_range_at(cursor);

        self.cursor = cursor;
        self.is_dragging = false;
        self.show_cursor = true;
        self.reset_cursor_blink();
        if let Some((start, end)) = word_range {
            self.selection_start = Some(start);
            self.selection_end = Some(end);
            self.cursor = end;
        } else {
            self.clear_selection();
        }

        self.overlay_cache.clear();
        Task::none()
    }

    fn handle_mouse_triple_click_msg(&mut self, point: iced::Point) -> Task<Message> {
        self.request_focus();
        self.has_canvas_focus = true;
        self.focus_locked = false;
        self.end_grouping_if_active();

        let Some(cursor) = self.calculate_cursor_from_point(point) else {
            return Task::none();
        };

        self.cursor = cursor;
        self.is_dragging = false;
        self.show_cursor = true;
        self.reset_cursor_blink();
        self.select_line_at(cursor.0);
        self.overlay_cache.clear();
        Task::none()
    }

    /// Handles mouse drag operations for selection.
    ///
    /// # Arguments
    ///
    /// * `point` - The drag position
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none() as no scrolling is needed)
    fn handle_mouse_drag_msg(&mut self, point: iced::Point) -> Task<Message> {
        if self.is_dragging {
            let before_cursor = self.cursor;
            let before_selection_end = self.selection_end;
            self.handle_mouse_drag(point);
            if self.cursor != before_cursor || self.selection_end != before_selection_end {
                // Mouse move events can be very frequent. Only invalidate the
                // overlay cache if the drag actually changed selection/cursor.
                self.overlay_cache.clear();
            }
        }
        Task::none()
    }

    /// Handles mouse release operations.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none() as no scrolling is needed)
    fn handle_mouse_release_msg(&mut self) -> Task<Message> {
        self.is_dragging = false;
        if self.selection_start == self.selection_end {
            self.clear_selection();
        }
        Task::none()
    }

    // =========================================================================
    // Clipboard Handlers
    // =========================================================================

    /// Handles paste operations.
    ///
    /// If the provided text is empty, reads from clipboard. Otherwise pastes
    /// the provided text at the cursor position.
    ///
    /// # Arguments
    ///
    /// * `text` - The text to paste (empty string triggers clipboard read)
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that may read clipboard or scroll to cursor
    fn handle_paste_msg(&mut self, text: &str) -> Task<Message> {
        // End grouping on paste
        self.end_grouping_if_active();

        // If text is empty, we need to read from clipboard
        if text.is_empty() {
            // Return a task that reads clipboard and chains to paste
            iced::clipboard::read()
                .and_then(|clipboard_text| Task::done(Message::Paste(clipboard_text)))
        } else {
            // We have the text, paste it
            self.paste_text(text);
            self.finish_edit_operation();
            self.scroll_to_cursor()
        }
    }

    // =========================================================================
    // History (Undo/Redo) Handlers
    // =========================================================================

    /// Handles undo operations.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to cursor if undo succeeded
    fn handle_undo_msg(&mut self) -> Task<Message> {
        // End any current grouping before undoing
        self.end_grouping_if_active();

        if self.history.undo(&mut self.buffer, &mut self.cursor) {
            self.clear_selection();
            self.finish_edit_operation();
            self.scroll_to_cursor()
        } else {
            Task::none()
        }
    }

    /// Handles redo operations.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to cursor if redo succeeded
    fn handle_redo_msg(&mut self) -> Task<Message> {
        if self.history.redo(&mut self.buffer, &mut self.cursor) {
            self.clear_selection();
            self.finish_edit_operation();
            self.scroll_to_cursor()
        } else {
            Task::none()
        }
    }

    // =========================================================================
    // Search and Replace Handlers
    // =========================================================================

    /// Handles opening the search dialog.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that focuses and selects all in the search input
    fn handle_open_search_msg(&mut self) -> Task<Message> {
        self.goto_line_state.close();
        self.search_state.open_search();
        self.overlay_cache.clear();

        // Focus the search input and select all text if any
        Task::batch([
            focus(self.search_state.search_input_id.clone()),
            select_all(self.search_state.search_input_id.clone()),
        ])
    }

    /// Handles opening the search and replace dialog.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that focuses and selects all in the search input
    fn handle_open_search_replace_msg(&mut self) -> Task<Message> {
        self.goto_line_state.close();
        self.search_state.open_replace();
        self.overlay_cache.clear();

        // Focus the search input and select all text if any
        Task::batch([
            focus(self.search_state.search_input_id.clone()),
            select_all(self.search_state.search_input_id.clone()),
        ])
    }

    /// Handles closing the search dialog.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_close_search_msg(&mut self) -> Task<Message> {
        self.search_state.close();
        self.overlay_cache.clear();
        Task::none()
    }

    /// Handles search query text changes.
    ///
    /// # Arguments
    ///
    /// * `query` - The new search query
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to first match if any
    fn handle_search_query_changed_msg(&mut self, query: &str) -> Task<Message> {
        self.search_state.set_query(query.to_string(), &self.buffer);
        self.overlay_cache.clear();

        // Move cursor to first match if any
        if let Some(match_pos) = self.search_state.current_match() {
            self.cursor = (match_pos.line, match_pos.col);
            self.clear_selection();
            return self.scroll_to_cursor();
        }
        Task::none()
    }

    /// Handles replace query text changes.
    ///
    /// # Arguments
    ///
    /// * `replace_text` - The new replacement text
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_replace_query_changed_msg(&mut self, replace_text: &str) -> Task<Message> {
        self.search_state.set_replace_with(replace_text.to_string());
        Task::none()
    }

    /// Handles toggling case-sensitive search.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to first match if any
    fn handle_toggle_case_sensitive_msg(&mut self) -> Task<Message> {
        self.search_state.toggle_case_sensitive(&self.buffer);
        self.overlay_cache.clear();

        // Move cursor to first match if any
        if let Some(match_pos) = self.search_state.current_match() {
            self.cursor = (match_pos.line, match_pos.col);
            self.clear_selection();
            return self.scroll_to_cursor();
        }
        Task::none()
    }

    /// Handles finding the next match.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to the next match if any
    fn handle_find_next_msg(&mut self) -> Task<Message> {
        if !self.search_state.matches.is_empty() {
            self.search_state.next_match();
            if let Some(match_pos) = self.search_state.current_match() {
                self.cursor = (match_pos.line, match_pos.col);
                self.clear_selection();
                self.overlay_cache.clear();
                return self.scroll_to_cursor();
            }
        }
        Task::none()
    }

    /// Handles finding the previous match.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to the previous match if any
    fn handle_find_previous_msg(&mut self) -> Task<Message> {
        if !self.search_state.matches.is_empty() {
            self.search_state.previous_match();
            if let Some(match_pos) = self.search_state.current_match() {
                self.cursor = (match_pos.line, match_pos.col);
                self.clear_selection();
                self.overlay_cache.clear();
                return self.scroll_to_cursor();
            }
        }
        Task::none()
    }

    /// Handles replacing the current match and moving to the next.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to the next match if any
    fn handle_replace_next_msg(&mut self) -> Task<Message> {
        // Replace current match and move to next
        if let Some(match_pos) = self.search_state.current_match() {
            let query_len = self.search_state.query.chars().count();
            let replace_text = self.search_state.replace_with.clone();

            // Create and execute replace command
            let mut cmd = ReplaceTextCommand::new(
                &self.buffer,
                (match_pos.line, match_pos.col),
                query_len,
                replace_text,
                self.cursor,
            );
            cmd.execute(&mut self.buffer, &mut self.cursor);
            self.history.push(Box::new(cmd));

            // Update matches after replacement
            self.search_state.update_matches(&self.buffer);

            // Move to next match if available
            if !self.search_state.matches.is_empty()
                && let Some(next_match) = self.search_state.current_match()
            {
                self.cursor = (next_match.line, next_match.col);
            }

            self.clear_selection();
            self.finish_edit_operation();
            return self.scroll_to_cursor();
        }
        Task::none()
    }

    /// Handles replacing all matches.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to cursor after replacement
    fn handle_replace_all_msg(&mut self) -> Task<Message> {
        // Perform a fresh search to find ALL matches (ignoring the display limit)
        let all_matches = super::search::find_matches(
            &self.buffer,
            &self.search_state.query,
            self.search_state.case_sensitive,
            None, // No limit for Replace All
        );

        if !all_matches.is_empty() {
            let query_len = self.search_state.query.chars().count();
            let replace_text = self.search_state.replace_with.clone();

            // Create composite command for undo
            let mut composite = CompositeCommand::new("Replace All".to_string());

            // Process matches in reverse order (to preserve positions)
            for match_pos in all_matches.iter().rev() {
                let cmd = ReplaceTextCommand::new(
                    &self.buffer,
                    (match_pos.line, match_pos.col),
                    query_len,
                    replace_text.clone(),
                    self.cursor,
                );
                composite.add(Box::new(cmd));
            }

            // Execute all replacements
            composite.execute(&mut self.buffer, &mut self.cursor);
            self.history.push(Box::new(composite));

            // Update matches (should be empty now)
            self.search_state.update_matches(&self.buffer);
            self.clear_selection();
            self.finish_edit_operation();
            self.scroll_to_cursor()
        } else {
            Task::none()
        }
    }

    /// Handles Tab key in search dialog (cycle forward).
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that focuses the next field
    fn handle_search_dialog_tab_msg(&mut self) -> Task<Message> {
        // Cycle focus forward (Search → Replace → Search)
        self.search_state.focus_next_field();

        // Focus the appropriate input based on new focused_field
        match self.search_state.focused_field {
            crate::canvas_editor::search::SearchFocusedField::Search => {
                focus(self.search_state.search_input_id.clone())
            }
            crate::canvas_editor::search::SearchFocusedField::Replace => {
                focus(self.search_state.replace_input_id.clone())
            }
        }
    }

    /// Handles Shift+Tab key in search dialog (cycle backward).
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that focuses the previous field
    fn handle_search_dialog_shift_tab_msg(&mut self) -> Task<Message> {
        // Cycle focus backward (Replace → Search → Replace)
        self.search_state.focus_previous_field();

        // Focus the appropriate input based on new focused_field
        match self.search_state.focused_field {
            crate::canvas_editor::search::SearchFocusedField::Search => {
                focus(self.search_state.search_input_id.clone())
            }
            crate::canvas_editor::search::SearchFocusedField::Replace => {
                focus(self.search_state.replace_input_id.clone())
            }
        }
    }

    // =========================================================================
    // Focus and IME Handlers
    // =========================================================================

    /// Handles canvas focus gained event.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_canvas_focus_gained_msg(&mut self) -> Task<Message> {
        self.has_canvas_focus = true;
        self.focus_locked = false; // Unlock focus when gained
        self.show_cursor = true;
        self.reset_cursor_blink();
        self.overlay_cache.clear();
        Task::none()
    }

    /// Handles canvas focus lost event.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_canvas_focus_lost_msg(&mut self) -> Task<Message> {
        self.has_canvas_focus = false;
        self.focus_locked = true; // Lock focus when lost to prevent focus stealing
        self.show_cursor = false;
        self.ime_preedit = None;
        self.overlay_cache.clear();
        Task::none()
    }

    /// Handles IME opened event.
    ///
    /// Clears current preedit content to accept new input.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_ime_opened_msg(&mut self) -> Task<Message> {
        self.ime_preedit = None;
        self.last_key_character_input = None;
        self.overlay_cache.clear();
        Task::none()
    }

    /// Handles IME preedit event.
    ///
    /// Updates the preedit text and selection while the user is composing.
    ///
    /// # Arguments
    ///
    /// * `content` - The preedit text content
    /// * `selection` - The selection range within the preedit text
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_ime_preedit_msg(
        &mut self,
        content: &str,
        selection: &Option<std::ops::Range<usize>>,
    ) -> Task<Message> {
        if content.is_empty() {
            self.ime_preedit = None;
        } else {
            self.last_key_character_input = None;
            self.ime_preedit = Some(ImePreedit {
                content: content.to_string(),
                selection: selection.clone(),
            });
        }

        self.overlay_cache.clear();
        Task::none()
    }

    /// Handles IME commit event.
    ///
    /// Inserts the committed text at the cursor position.
    ///
    /// # Arguments
    ///
    /// * `text` - The committed text
    ///
    /// # Returns
    ///
    /// A `Task<Message>` that scrolls to cursor after insertion
    fn handle_ime_commit_msg(&mut self, text: &str) -> Task<Message> {
        let had_preedit = self.ime_preedit.is_some();
        self.ime_preedit = None;

        if text.is_empty() {
            self.overlay_cache.clear();
            return Task::none();
        }

        if !had_preedit {
            let mut chars = text.chars();
            if let Some(ch) = chars.next()
                && chars.next().is_none()
                && self.last_key_character_input == Some(ch)
            {
                self.last_key_character_input = None;
                self.overlay_cache.clear();
                return Task::none();
            }
        }
        self.last_key_character_input = None;
        self.ensure_grouping_started("Typing");

        self.paste_text(text);
        self.finish_edit_operation();
        self.scroll_to_cursor()
    }

    /// Handles IME closed event.
    ///
    /// Clears preedit state to return to normal input mode.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_ime_closed_msg(&mut self) -> Task<Message> {
        self.ime_preedit = None;
        self.last_key_character_input = None;
        self.overlay_cache.clear();
        Task::none()
    }

    // =========================================================================
    // Complex Standalone Handlers
    // =========================================================================

    /// Handles cursor blink tick event.
    ///
    /// Updates cursor visibility for blinking animation.
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_tick_msg(&mut self) -> Task<Message> {
        // Handle cursor blinking only if editor has focus
        if self.has_focus() && self.last_blink.elapsed() >= CURSOR_BLINK_INTERVAL {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = super::Instant::now();
            self.overlay_cache.clear();
        }

        // Hide cursor if editor doesn't have focus
        if !self.has_focus() {
            self.show_cursor = false;
        }

        Task::none()
    }

    /// Handles viewport scrolled event.
    ///
    /// Manages the virtual scrolling cache window to optimize rendering
    /// for large files. Only clears the cache when scrolling crosses the
    /// cached window boundary or when viewport dimensions change.
    ///
    /// # Arguments
    ///
    /// * `viewport` - The viewport information after scrolling
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently Task::none())
    fn handle_scrolled_msg(
        &mut self,
        viewport: iced::widget::scrollable::Viewport,
    ) -> Task<Message> {
        let new_scroll = viewport.absolute_offset().y;
        let new_height = viewport.bounds().height;
        let new_width = viewport.bounds().width;
        self.handle_vertical_scroll(new_scroll, new_width, new_height)
    }

    fn handle_vertical_scroll(
        &mut self,
        new_scroll: f32,
        new_width: f32,
        new_height: f32,
    ) -> Task<Message> {
        // Virtual-scrolling cache window:
        // Instead of clearing the canvas cache for every small scroll,
        // we maintain a larger "render window" of visual lines around
        // the visible range. We only clear the cache and re-window
        // when the scroll crosses the window boundary or the viewport
        // size changes significantly. This prevents frequent re-highlighting
        // and layout recomputation for very large files while ensuring
        // the first scroll renders correctly without requiring a click.
        let scroll_changed = (self.viewport_scroll - new_scroll).abs() > 0.1;
        let content_scroll = (new_scroll
            - if self.center_cursor {
                ((new_height - self.line_height) * 0.5).max(0.0)
            } else {
                0.0
            })
        .max(0.0);
        let visible_lines_count = (new_height / self.line_height).ceil() as usize + 2;
        let first_visible_line = (content_scroll / self.line_height).floor() as usize;
        let last_visible_line = first_visible_line + visible_lines_count;
        let margin = visible_lines_count * crate::canvas_editor::CACHE_WINDOW_MARGIN_MULTIPLIER;
        let window_start = first_visible_line.saturating_sub(margin);
        let window_end = last_visible_line + margin;
        // Decide whether we need to re-window the cache.
        // Special-case top-of-file: when window_start == 0, allow small forward scrolls
        // without forcing a rewindow, to avoid thrashing when the visible range is near 0.
        let need_rewindow = if self.cache_window_end_line > self.cache_window_start_line {
            let lower_boundary_trigger = self.cache_window_start_line > 0
                && first_visible_line
                    < self
                        .cache_window_start_line
                        .saturating_add(visible_lines_count / 2);
            let upper_boundary_trigger = last_visible_line
                > self
                    .cache_window_end_line
                    .saturating_sub(visible_lines_count / 2);
            lower_boundary_trigger || upper_boundary_trigger
        } else {
            true
        };
        // Clear cache when viewport dimensions change significantly
        // to ensure proper redraw (e.g., window resize)
        if (self.viewport_height - new_height).abs() > 1.0
            || (self.viewport_width - new_width).abs() > 1.0
            || (scroll_changed && need_rewindow)
        {
            self.cache_window_start_line = window_start;
            self.cache_window_end_line = window_end;
            self.last_first_visible_line = first_visible_line;
            self.content_cache.clear();
            self.overlay_cache.clear();
        }
        self.viewport_scroll = new_scroll;
        self.viewport_height = new_height;
        self.viewport_width = new_width;

        if self.center_cursor && scroll_changed {
            if let Some(cursor) = self.pending_center_cursor_scroll.take() {
                let expected_scroll =
                    self.target_vertical_scroll_offset_for_line(cursor.0, cursor.1);
                if (new_scroll - expected_scroll).abs() <= self.line_height * 0.5 {
                    return self.clamp_horizontal_scroll_task();
                }
            }

            let visual_lines = self.visual_lines_cached(new_width);
            if !visual_lines.is_empty() {
                let target_visual = ((new_scroll / self.line_height).round() as usize)
                    .min(visual_lines.len().saturating_sub(1));
                let target_vl = &visual_lines[target_visual];
                let target_line_len = self.buffer.line_len(target_vl.logical_line);
                let new_col = (target_vl.start_col + self.preferred_column.min(target_vl.len()))
                    .min(target_line_len);
                let new_cursor = (target_vl.logical_line, new_col);
                if self.cursor != new_cursor {
                    self.cursor = new_cursor;
                    self.overlay_cache.clear();
                }
            }
        }

        self.clamp_horizontal_scroll_task()
    }

    fn clamp_horizontal_scroll_task(&mut self) -> Task<Message> {
        if self.clamp_horizontal_scroll_offset() {
            return iced::widget::operation::scroll_to(
                self.horizontal_scrollable_id.clone(),
                iced::widget::scrollable::AbsoluteOffset {
                    x: self.horizontal_scroll_offset,
                    y: 0.0,
                },
            );
        }

        Task::none()
    }

    /// Handles horizontal scrollbar scrolled event (only active when wrap is disabled).
    ///
    /// Updates `horizontal_scroll_offset` and clears render caches when the offset
    /// changes by more than 0.1 pixels to avoid unnecessary redraws.
    ///
    /// # Arguments
    ///
    /// * `viewport` - The viewport information after scrolling
    ///
    /// # Returns
    ///
    /// A `Task<Message>` (currently `Task::none()`)
    fn handle_horizontal_scrolled_msg(
        &mut self,
        viewport: iced::widget::scrollable::Viewport,
    ) -> Task<Message> {
        let new_x = viewport
            .absolute_offset()
            .x
            .clamp(0.0, self.max_horizontal_scroll_offset());
        if (self.horizontal_scroll_offset - new_x).abs() > 0.1 {
            self.horizontal_scroll_offset = new_x;
            self.content_cache.clear();
            self.overlay_cache.clear();
        }
        Task::none()
    }

    /// Handles horizontal wheel or trackpad scrolling in the code area.
    fn handle_horizontal_wheel_scrolled_msg(&mut self, delta_x: f32) -> Task<Message> {
        if self.wrap_enabled || delta_x.abs() <= 0.1 {
            return Task::none();
        }

        let new_x = (self.horizontal_scroll_offset + delta_x)
            .clamp(0.0, self.max_horizontal_scroll_offset());

        if (self.horizontal_scroll_offset - new_x).abs() > 0.1 {
            self.horizontal_scroll_offset = new_x;
            self.content_cache.clear();
            self.overlay_cache.clear();

            return iced::widget::operation::scroll_to(
                self.horizontal_scrollable_id.clone(),
                iced::widget::scrollable::AbsoluteOffset { x: new_x, y: 0.0 },
            );
        }

        Task::none()
    }

    // =========================================================================
    // Main Update Method
    // =========================================================================

    /// Updates the editor state based on messages and returns scroll commands.
    ///
    /// # Arguments
    ///
    /// * `message` - The message to process for updating the editor state
    ///
    /// # Returns
    /// A `Task<Message>` for any asynchronous operations, such as scrolling to keep the cursor visible after state updates
    pub fn update(&mut self, message: &Message) -> Task<Message> {
        if matches!(
            message,
            Message::ArrowKey(..)
                | Message::WordArrowKey(..)
                | Message::Backspace
                | Message::Delete
                | Message::DeleteWordBackward
                | Message::DeleteWordForward
                | Message::DeleteToLineStart
                | Message::DeleteToLineEnd
                | Message::DeleteSelection
                | Message::SelectAll
                | Message::InsertLineBelow
                | Message::InsertLineAbove
                | Message::DeleteLine
                | Message::MoveLineUp
                | Message::MoveLineDown
                | Message::CopyLineUp
                | Message::CopyLineDown
                | Message::JoinLines
                | Message::ToggleLineComment
                | Message::ToggleBlockComment
                | Message::SelectLine
                | Message::JumpToMatchingBracket
                | Message::Home(..)
                | Message::End(..)
                | Message::DocumentHome(..)
                | Message::DocumentEnd(..)
                | Message::GotoPosition(..)
                | Message::PageUp
                | Message::PageDown
                | Message::MouseClick(..)
                | Message::MouseDoubleClick(..)
                | Message::MouseTripleClick(..)
                | Message::MouseDrag(..)
                | Message::MouseHover(..)
                | Message::MouseRelease
                | Message::OpenSearch
                | Message::OpenSearchReplace
                | Message::OpenGotoLine
                | Message::CloseSearch
                | Message::CloseGotoLine
        ) {
            self.close_completion_for_interaction();
        }

        match message {
            // Text input operations
            Message::CharacterInput(ch) => self.handle_character_input_msg(*ch),
            Message::Tab => self.handle_tab(),
            Message::ShiftTab => self.handle_shift_tab(),
            Message::Enter => self.handle_enter(),
            Message::TriggerCompletion => {
                self.trigger_completion(true);
                self.completion_scroll_task()
            }
            Message::CloseCompletion => {
                self.close_completion(true);
                Task::none()
            }
            Message::CompletionNavigateUp => {
                self.handle_completion_navigation(-1);
                self.completion_scroll_task()
            }
            Message::CompletionNavigateDown => {
                self.handle_completion_navigation(1);
                self.completion_scroll_task()
            }
            Message::CompletionConfirm => {
                if self.apply_selected_completion(None) {
                    self.scroll_to_cursor()
                } else {
                    Task::none()
                }
            }
            Message::CompletionSelected(index) => {
                if self.apply_selected_completion(Some(*index)) {
                    self.scroll_to_cursor()
                } else {
                    Task::none()
                }
            }

            // Deletion operations
            Message::Backspace => self.handle_backspace(),
            Message::Delete => self.handle_delete(),
            Message::DeleteWordBackward => self.handle_delete_word_backward(),
            Message::DeleteWordForward => self.handle_delete_word_forward(),
            Message::DeleteToLineStart => self.handle_delete_to_line_start(),
            Message::DeleteToLineEnd => self.handle_delete_to_line_end(),
            Message::DeleteSelection => self.handle_delete_selection(),
            Message::SelectAll => self.handle_select_all(),
            Message::InsertLineBelow => self.handle_insert_line_below(),
            Message::InsertLineAbove => self.handle_insert_line_above(),
            Message::DeleteLine => self.handle_delete_line(),
            Message::MoveLineUp => self.handle_move_line(false),
            Message::MoveLineDown => self.handle_move_line(true),
            Message::CopyLineUp => self.handle_copy_line(false),
            Message::CopyLineDown => self.handle_copy_line(true),
            Message::OpenGotoLine => self.handle_open_goto_line_msg(),
            Message::CloseGotoLine => self.handle_close_goto_line_msg(),
            Message::GotoLineQueryChanged(query) => self.handle_goto_line_query_changed_msg(query),
            Message::SubmitGotoLine => self.handle_submit_goto_line_msg(),
            Message::JoinLines => self.handle_join_lines(),
            Message::ToggleLineComment => self.handle_toggle_line_comment(),
            Message::ToggleBlockComment => self.handle_toggle_block_comment(),
            Message::SelectLine => self.handle_select_line(),
            Message::JumpToMatchingBracket => self.handle_jump_to_matching_bracket(),

            // Navigation operations
            Message::ArrowKey(direction, shift) => self.handle_arrow_key(*direction, *shift),
            Message::WordArrowKey(direction, shift) => {
                self.handle_word_arrow_key(*direction, *shift)
            }
            Message::Home(shift) => self.handle_home(*shift),
            Message::End(shift) => self.handle_end(*shift),
            Message::DocumentHome(shift) => self.handle_document_home(*shift),
            Message::DocumentEnd(shift) => self.handle_document_end(*shift),
            Message::GotoPosition(line, col) => self.handle_goto_position(*line, *col),
            Message::PageUp => self.handle_page_up(),
            Message::PageDown => self.handle_page_down(),

            // Mouse and selection operations
            Message::MouseClick(point) => self.handle_mouse_click_msg(*point),
            Message::MouseDoubleClick(point) => self.handle_mouse_double_click_msg(*point),
            Message::MouseTripleClick(point) => self.handle_mouse_triple_click_msg(*point),
            Message::MouseDrag(point) => self.handle_mouse_drag_msg(*point),
            Message::MouseHover(point) => self.handle_mouse_drag_msg(*point),
            Message::MouseRelease => self.handle_mouse_release_msg(),

            // Clipboard operations
            Message::Copy => self.copy_selection(),
            Message::Paste(text) => self.handle_paste_msg(text),

            // History operations
            Message::Undo => self.handle_undo_msg(),
            Message::Redo => self.handle_redo_msg(),

            // Search and replace operations
            Message::OpenSearch => self.handle_open_search_msg(),
            Message::OpenSearchReplace => self.handle_open_search_replace_msg(),
            Message::CloseSearch => self.handle_close_search_msg(),
            Message::SearchQueryChanged(query) => self.handle_search_query_changed_msg(query),
            Message::ReplaceQueryChanged(text) => self.handle_replace_query_changed_msg(text),
            Message::ToggleCaseSensitive => self.handle_toggle_case_sensitive_msg(),
            Message::FindNext => self.handle_find_next_msg(),
            Message::FindPrevious => self.handle_find_previous_msg(),
            Message::ReplaceNext => self.handle_replace_next_msg(),
            Message::ReplaceAll => self.handle_replace_all_msg(),
            Message::SearchDialogTab => self.handle_search_dialog_tab_msg(),
            Message::SearchDialogShiftTab => self.handle_search_dialog_shift_tab_msg(),
            Message::FocusNavigationTab => self.handle_focus_navigation_tab(),
            Message::FocusNavigationShiftTab => self.handle_focus_navigation_shift_tab(),

            // Focus and IME operations
            Message::CanvasFocusGained => self.handle_canvas_focus_gained_msg(),
            Message::CanvasFocusLost => self.handle_canvas_focus_lost_msg(),
            Message::ImeOpened => self.handle_ime_opened_msg(),
            Message::ImePreedit(content, selection) => {
                self.handle_ime_preedit_msg(content, selection)
            }
            Message::ImeCommit(text) => self.handle_ime_commit_msg(text),
            Message::ImeClosed => self.handle_ime_closed_msg(),

            // UI update operations
            Message::Tick => self.handle_tick_msg(),
            Message::Scrolled(viewport) => self.handle_scrolled_msg(*viewport),
            Message::HorizontalScrolled(viewport) => self.handle_horizontal_scrolled_msg(*viewport),
            Message::HorizontalWheelScrolled(delta_x) => {
                self.handle_horizontal_wheel_scrolled_msg(*delta_x)
            }

            // Handle the "Jump to Definition" action triggered by Ctrl+Click.
            // Currently, this returns `Task::none()` as the actual navigation logic
            // is delegated to the `LspClient` implementation or handled elsewhere.
            Message::JumpClick(_point) => Task::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas_editor::ArrowDirection;

    #[test]
    fn test_horizontal_scroll_initial_state() {
        let editor = CodeEditor::new("short line", "rs");
        assert!(
            (editor.horizontal_scroll_offset - 0.0).abs() < f32::EPSILON,
            "Initial horizontal scroll offset should be 0"
        );
    }

    #[test]
    fn test_set_wrap_enabled_resets_horizontal_offset() {
        let mut editor = CodeEditor::new("long line", "rs");
        editor.wrap_enabled = false;
        // Simulate a non-zero horizontal scroll
        editor.horizontal_scroll_offset = 100.0;

        // Re-enabling wrap should reset horizontal offset
        editor.set_wrap_enabled(true);
        assert!(
            (editor.horizontal_scroll_offset - 0.0).abs() < f32::EPSILON,
            "Horizontal scroll offset should be reset when wrap is re-enabled"
        );
    }

    #[test]
    fn test_resize_clamps_horizontal_scroll_offset() {
        let mut editor = CodeEditor::new(
            "this is a very long line that definitely needs horizontal scrolling",
            "rs",
        );
        editor.wrap_enabled = false;
        editor.viewport_width = 120.0;
        editor.horizontal_scroll_offset = 180.0;

        editor.viewport_width = 4000.0;
        let changed = editor.clamp_horizontal_scroll_offset();

        assert!(
            changed,
            "Resize should clamp an out-of-range horizontal scroll"
        );
        assert!(
            (editor.horizontal_scroll_offset - 0.0).abs() < f32::EPSILON,
            "Horizontal scroll offset should reset when the content fits after resize"
        );
    }

    #[test]
    fn test_canvas_focus_lost() {
        let mut editor = CodeEditor::new("test", "rs");
        editor.has_canvas_focus = true;

        let _ = editor.update(&Message::CanvasFocusLost);

        assert!(!editor.has_canvas_focus);
        assert!(!editor.show_cursor);
        assert!(editor.focus_locked, "Focus should be locked when lost");
    }

    #[test]
    fn test_canvas_focus_gained_resets_lock() {
        let mut editor = CodeEditor::new("test", "rs");
        editor.has_canvas_focus = false;
        editor.focus_locked = true;

        let _ = editor.update(&Message::CanvasFocusGained);

        assert!(editor.has_canvas_focus);
        assert!(
            !editor.focus_locked,
            "Focus lock should be reset when focus is gained"
        );
    }

    #[test]
    fn test_focus_lock_state() {
        let mut editor = CodeEditor::new("test", "rs");

        // Initially, focus should not be locked
        assert!(!editor.focus_locked);

        // When focus is lost, it should be locked
        let _ = editor.update(&Message::CanvasFocusLost);
        assert!(editor.focus_locked, "Focus should be locked when lost");

        // When focus is regained, it should be unlocked
        editor.request_focus();
        let _ = editor.update(&Message::CanvasFocusGained);
        assert!(
            !editor.focus_locked,
            "Focus should be unlocked when regained"
        );

        // Can manually reset focus lock
        editor.focus_locked = true;
        editor.reset_focus_lock();
        assert!(!editor.focus_locked, "Focus lock should be resetable");
    }

    #[test]
    fn test_reset_focus_lock() {
        let mut editor = CodeEditor::new("test", "rs");
        editor.focus_locked = true;

        editor.reset_focus_lock();

        assert!(!editor.focus_locked);
    }

    #[test]
    fn test_home_key() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 5); // Move to middle of line
        let _ = editor.update(&Message::Home(false));
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_end_key() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 0);
        let _ = editor.update(&Message::End(false));
        assert_eq!(editor.cursor, (0, 11)); // Length of "hello world"
    }

    #[test]
    fn test_smart_home_toggles_indent_and_line_start() {
        let mut editor = CodeEditor::new("    hello", "py");
        editor.cursor = (0, 7);

        let _ = editor.update(&Message::Home(false));
        assert_eq!(editor.cursor, (0, 4));

        let _ = editor.update(&Message::Home(false));
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_smart_end_toggles_trimmed_end_and_line_end() {
        let mut editor = CodeEditor::new("hello   ", "py");
        editor.cursor = (0, 0);

        let _ = editor.update(&Message::End(false));
        assert_eq!(editor.cursor, (0, 5));

        let _ = editor.update(&Message::End(false));
        assert_eq!(editor.cursor, (0, 8));
    }

    #[test]
    fn test_arrow_key_with_shift_creates_selection() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 0);

        // Shift+Right should start selection
        let _ = editor.update(&Message::ArrowKey(ArrowDirection::Right, true));
        assert!(editor.selection_start.is_some());
        assert!(editor.selection_end.is_some());
    }

    #[test]
    fn test_arrow_key_without_shift_clears_selection() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((0, 5));

        // Regular arrow key should clear selection
        let _ = editor.update(&Message::ArrowKey(ArrowDirection::Right, false));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_typing_with_selection() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;

        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((0, 5));

        let _ = editor.update(&Message::CharacterInput('X'));
        assert_eq!(editor.buffer.line(0), "X world");
        assert_eq!(editor.cursor, (0, 1));
    }

    #[test]
    fn test_document_home() {
        let mut editor = CodeEditor::new("line1\nline2\nline3", "py");
        editor.cursor = (2, 5);
        let _ = editor.update(&Message::DocumentHome(false));
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_document_end() {
        let mut editor = CodeEditor::new("line1\nline2\nline3", "py");
        editor.cursor = (0, 0);
        let _ = editor.update(&Message::DocumentEnd(false));
        assert_eq!(editor.cursor, (2, 5));
    }

    #[test]
    fn test_document_home_clears_selection() {
        let mut editor = CodeEditor::new("line1\nline2\nline3", "py");
        editor.cursor = (2, 5);
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((2, 5));

        let _ = editor.update(&Message::DocumentHome(false));
        assert_eq!(editor.cursor, (0, 0));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_document_end_clears_selection() {
        let mut editor = CodeEditor::new("line1\nline2\nline3", "py");
        editor.cursor = (0, 0);
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((1, 3));

        let _ = editor.update(&Message::DocumentEnd(false));
        assert_eq!(editor.cursor, (2, 5));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_goto_position_sets_cursor_and_clears_selection() {
        let mut editor = CodeEditor::new("line1\nline2\nline3", "py");
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((1, 2));

        let _ = editor.update(&Message::GotoPosition(1, 3));

        assert_eq!(editor.cursor, (1, 3));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_goto_position_clamps_out_of_range() {
        let mut editor = CodeEditor::new("a\nbb", "py");

        let _ = editor.update(&Message::GotoPosition(99, 99));

        // Clamped to last line (index 1) and end of that line (len = 2)
        assert_eq!(editor.cursor, (1, 2));
    }

    #[test]
    fn test_scroll_sets_initial_cache_window() {
        let content = (0..200).map(|i| format!("line{}\n", i)).collect::<String>();
        let mut editor = CodeEditor::new(&content, "py");

        // Simulate initial viewport
        let height = 400.0;
        let width = 800.0;
        let scroll = 0.0;

        // Expected derived ranges
        let visible_lines_count = (height / editor.line_height).ceil() as usize + 2;
        let first_visible_line = (scroll / editor.line_height).floor() as usize;
        let last_visible_line = first_visible_line + visible_lines_count;
        let margin = visible_lines_count * 2;
        let window_start = first_visible_line.saturating_sub(margin);
        let window_end = last_visible_line + margin;

        // Apply logic similar to Message::Scrolled branch
        editor.viewport_height = height;
        editor.viewport_width = width;
        editor.viewport_scroll = -1.0;
        let scroll_changed = (editor.viewport_scroll - scroll).abs() > 0.1;
        let need_rewindow = true;
        if (editor.viewport_height - height).abs() > 1.0
            || (editor.viewport_width - width).abs() > 1.0
            || (scroll_changed && need_rewindow)
        {
            editor.cache_window_start_line = window_start;
            editor.cache_window_end_line = window_end;
            editor.last_first_visible_line = first_visible_line;
        }
        editor.viewport_scroll = scroll;

        assert_eq!(editor.last_first_visible_line, first_visible_line);
        assert!(editor.cache_window_end_line > editor.cache_window_start_line);
        assert_eq!(editor.cache_window_start_line, window_start);
        assert_eq!(editor.cache_window_end_line, window_end);
    }

    #[test]
    fn test_small_scroll_keeps_window() {
        let content = (0..200).map(|i| format!("line{}\n", i)).collect::<String>();
        let mut editor = CodeEditor::new(&content, "py");
        let height = 400.0;
        let width = 800.0;
        let initial_scroll = 0.0;
        let visible_lines_count = (height / editor.line_height).ceil() as usize + 2;
        let first_visible_line = (initial_scroll / editor.line_height).floor() as usize;
        let last_visible_line = first_visible_line + visible_lines_count;
        let margin = visible_lines_count * 2;
        let window_start = first_visible_line.saturating_sub(margin);
        let window_end = last_visible_line + margin;
        editor.cache_window_start_line = window_start;
        editor.cache_window_end_line = window_end;
        editor.viewport_height = height;
        editor.viewport_width = width;
        editor.viewport_scroll = initial_scroll;

        // Small scroll inside window
        let small_scroll = editor.line_height * (visible_lines_count as f32 / 4.0);
        let first_visible_line2 = (small_scroll / editor.line_height).floor() as usize;
        let last_visible_line2 = first_visible_line2 + visible_lines_count;
        let lower_boundary_trigger = editor.cache_window_start_line > 0
            && first_visible_line2
                < editor
                    .cache_window_start_line
                    .saturating_add(visible_lines_count / 2);
        let upper_boundary_trigger = last_visible_line2
            > editor
                .cache_window_end_line
                .saturating_sub(visible_lines_count / 2);
        let need_rewindow = lower_boundary_trigger || upper_boundary_trigger;

        assert!(!need_rewindow, "Small scroll should be inside the window");
        // Window remains unchanged
        assert_eq!(editor.cache_window_start_line, window_start);
        assert_eq!(editor.cache_window_end_line, window_end);
    }

    #[test]
    fn test_large_scroll_rewindows() {
        let content = (0..1000)
            .map(|i| format!("line{}\n", i))
            .collect::<String>();
        let mut editor = CodeEditor::new(&content, "py");
        let height = 400.0;
        let width = 800.0;
        let initial_scroll = 0.0;
        let visible_lines_count = (height / editor.line_height).ceil() as usize + 2;
        let first_visible_line = (initial_scroll / editor.line_height).floor() as usize;
        let last_visible_line = first_visible_line + visible_lines_count;
        let margin = visible_lines_count * 2;
        editor.cache_window_start_line = first_visible_line.saturating_sub(margin);
        editor.cache_window_end_line = last_visible_line + margin;
        editor.viewport_height = height;
        editor.viewport_width = width;
        editor.viewport_scroll = initial_scroll;

        // Large scroll beyond window boundary
        let large_scroll = editor.line_height * ((visible_lines_count * 4) as f32);
        let first_visible_line2 = (large_scroll / editor.line_height).floor() as usize;
        let last_visible_line2 = first_visible_line2 + visible_lines_count;
        let window_start2 = first_visible_line2.saturating_sub(margin);
        let window_end2 = last_visible_line2 + margin;
        let need_rewindow = first_visible_line2
            < editor
                .cache_window_start_line
                .saturating_add(visible_lines_count / 2)
            || last_visible_line2
                > editor
                    .cache_window_end_line
                    .saturating_sub(visible_lines_count / 2);
        assert!(need_rewindow, "Large scroll should trigger window update");

        // Apply rewindow
        editor.cache_window_start_line = window_start2;
        editor.cache_window_end_line = window_end2;
        editor.last_first_visible_line = first_visible_line2;

        assert_eq!(editor.cache_window_start_line, window_start2);
        assert_eq!(editor.cache_window_end_line, window_end2);
        assert_eq!(editor.last_first_visible_line, first_visible_line2);
    }

    #[test]
    fn test_delete_selection_message() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 0);
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((0, 5));

        let _ = editor.update(&Message::DeleteSelection);
        assert_eq!(editor.buffer.line(0), " world");
        assert_eq!(editor.cursor, (0, 0));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_delete_selection_multiline() {
        let mut editor = CodeEditor::new("line1\nline2\nline3", "py");
        editor.cursor = (0, 2);
        editor.selection_start = Some((0, 2));
        editor.selection_end = Some((2, 2));

        let _ = editor.update(&Message::DeleteSelection);
        assert_eq!(editor.buffer.line(0), "line3");
        assert_eq!(editor.cursor, (0, 2));
        assert_eq!(editor.selection_start, None);
    }

    #[test]
    fn test_delete_selection_no_selection() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 5);

        let _ = editor.update(&Message::DeleteSelection);
        // Should do nothing if there's no selection
        assert_eq!(editor.buffer.line(0), "hello world");
        assert_eq!(editor.cursor, (0, 5));
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_ime_preedit_and_commit_chinese() {
        let mut editor = CodeEditor::new("", "py");
        // Simulate IME opened
        let _ = editor.update(&Message::ImeOpened);
        assert!(editor.ime_preedit.is_none());

        // Preedit with Chinese content and a selection range
        let content = "安全与合规".to_string();
        let selection = Some(0..3); // range aligned to UTF-8 character boundary
        let _ = editor.update(&Message::ImePreedit(content.clone(), selection.clone()));

        assert!(editor.ime_preedit.is_some());
        assert_eq!(
            editor.ime_preedit.as_ref().unwrap().content.clone(),
            content
        );
        assert_eq!(
            editor.ime_preedit.as_ref().unwrap().selection.clone(),
            selection
        );

        // Commit should insert the text and clear preedit
        let _ = editor.update(&Message::ImeCommit("安全与合规".to_string()));
        assert!(editor.ime_preedit.is_none());
        assert_eq!(editor.buffer.line(0), "安全与合规");
        assert_eq!(editor.cursor, (0, "安全与合规".chars().count()));
    }

    #[test]
    fn test_ime_commit_does_not_duplicate_plain_space_key_input() {
        let mut editor = CodeEditor::new("foo", "toml");
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;
        editor.cursor = (0, 3);

        let _ = editor.update(&Message::CharacterInput(' '));
        let _ = editor.update(&Message::ImeCommit(" ".to_string()));

        assert_eq!(editor.buffer.line(0), "foo ");
    }

    #[test]
    fn test_undo_char_insert() {
        let mut editor = CodeEditor::new("hello", "py");
        // Ensure editor has focus for character input
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;

        editor.cursor = (0, 5);

        // Type a character
        let _ = editor.update(&Message::CharacterInput('!'));
        assert_eq!(editor.buffer.line(0), "hello!");
        assert_eq!(editor.cursor, (0, 6));

        // Undo should remove it (but first end the grouping)
        editor.history.end_group();
        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "hello");
        assert_eq!(editor.cursor, (0, 5));
    }

    #[test]
    fn test_undo_redo_char_insert() {
        let mut editor = CodeEditor::new("hello", "py");
        // Ensure editor has focus for character input
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;

        editor.cursor = (0, 5);

        // Type a character
        let _ = editor.update(&Message::CharacterInput('!'));
        editor.history.end_group();

        // Undo
        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "hello");

        // Redo
        let _ = editor.update(&Message::Redo);
        assert_eq!(editor.buffer.line(0), "hello!");
        assert_eq!(editor.cursor, (0, 6));
    }

    #[test]
    fn test_undo_backspace() {
        let mut editor = CodeEditor::new("hello", "py");
        editor.cursor = (0, 5);

        // Backspace
        let _ = editor.update(&Message::Backspace);
        assert_eq!(editor.buffer.line(0), "hell");
        assert_eq!(editor.cursor, (0, 4));

        // Undo
        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "hello");
        assert_eq!(editor.cursor, (0, 5));
    }

    #[test]
    fn test_undo_newline() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 5);

        // Insert newline
        let _ = editor.update(&Message::Enter);
        assert_eq!(editor.buffer.line(0), "hello");
        assert_eq!(editor.buffer.line(1), " world");
        assert_eq!(editor.cursor, (1, 0));

        // Undo
        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "hello world");
        assert_eq!(editor.cursor, (0, 5));
    }

    #[test]
    fn test_undo_grouped_typing() {
        let mut editor = CodeEditor::new("hello", "py");
        // Ensure editor has focus for character input
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;

        editor.cursor = (0, 5);

        // Type multiple characters (they should be grouped)
        let _ = editor.update(&Message::CharacterInput(' '));
        let _ = editor.update(&Message::CharacterInput('w'));
        let _ = editor.update(&Message::CharacterInput('o'));
        let _ = editor.update(&Message::CharacterInput('r'));
        let _ = editor.update(&Message::CharacterInput('l'));
        let _ = editor.update(&Message::CharacterInput('d'));

        assert_eq!(editor.buffer.line(0), "hello world");

        // End the group
        editor.history.end_group();

        // Single undo should remove all grouped characters
        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "hello");
        assert_eq!(editor.cursor, (0, 5));
    }

    #[test]
    fn test_navigation_ends_grouping() {
        let mut editor = CodeEditor::new("hello", "py");
        // Ensure editor has focus for character input
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;

        editor.cursor = (0, 5);

        // Type a character (starts grouping)
        let _ = editor.update(&Message::CharacterInput('!'));
        assert!(editor.is_grouping);

        // Move cursor (ends grouping)
        let _ = editor.update(&Message::ArrowKey(ArrowDirection::Left, false));
        assert!(!editor.is_grouping);

        // Type another character (starts new group)
        let _ = editor.update(&Message::CharacterInput('?'));
        assert!(editor.is_grouping);

        editor.history.end_group();

        // Two separate undo operations
        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "hello!");

        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "hello");
    }

    #[test]
    fn test_edit_increments_revision_and_clears_visual_lines_cache() {
        let mut editor = CodeEditor::new("hello", "rs");
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;
        editor.cursor = (0, 5);

        let _ = editor.visual_lines_cached(800.0);
        assert!(
            editor.visual_lines_cache.borrow().is_some(),
            "visual_lines_cached should populate the cache"
        );

        let previous_revision = editor.buffer_revision;

        let _ = editor.update(&Message::CharacterInput('!'));
        assert_eq!(
            editor.buffer_revision,
            previous_revision.wrapping_add(1),
            "buffer_revision should change on buffer edits"
        );
        // `scroll_to_cursor` repopulates the cache after the edit with the new
        // revision, so the cache may be `Some`.  What must never happen is that
        // stale data (an old revision) survives an edit.
        assert!(
            editor
                .visual_lines_cache
                .borrow()
                .as_ref()
                .is_none_or(|c| c.key.buffer_revision == editor.buffer_revision),
            "buffer edits should not leave stale data in the visual lines cache"
        );
    }

    #[test]
    fn test_multiple_undo_redo() {
        let mut editor = CodeEditor::new("a", "py");
        // Ensure editor has focus for character input
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.focus_locked = false;

        editor.cursor = (0, 1);

        // Make several changes
        let _ = editor.update(&Message::CharacterInput('b'));
        editor.history.end_group();

        let _ = editor.update(&Message::CharacterInput('c'));
        editor.history.end_group();

        let _ = editor.update(&Message::CharacterInput('d'));
        editor.history.end_group();

        assert_eq!(editor.buffer.line(0), "abcd");

        // Undo all
        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "abc");

        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "ab");

        let _ = editor.update(&Message::Undo);
        assert_eq!(editor.buffer.line(0), "a");

        // Redo all
        let _ = editor.update(&Message::Redo);
        assert_eq!(editor.buffer.line(0), "ab");

        let _ = editor.update(&Message::Redo);
        assert_eq!(editor.buffer.line(0), "abc");

        let _ = editor.update(&Message::Redo);
        assert_eq!(editor.buffer.line(0), "abcd");
    }

    #[test]
    fn test_delete_key_with_selection() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((0, 5));
        editor.cursor = (0, 5);

        let _ = editor.update(&Message::Delete);

        assert_eq!(editor.buffer.line(0), " world");
        assert_eq!(editor.cursor, (0, 0));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_delete_key_without_selection() {
        let mut editor = CodeEditor::new("hello", "py");
        editor.cursor = (0, 0);

        let _ = editor.update(&Message::Delete);

        // Should delete the 'h'
        assert_eq!(editor.buffer.line(0), "ello");
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_backspace_with_selection() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.selection_start = Some((0, 6));
        editor.selection_end = Some((0, 11));
        editor.cursor = (0, 11);

        let _ = editor.update(&Message::Backspace);

        assert_eq!(editor.buffer.line(0), "hello ");
        assert_eq!(editor.cursor, (0, 6));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_backspace_without_selection() {
        let mut editor = CodeEditor::new("hello", "py");
        editor.cursor = (0, 5);

        let _ = editor.update(&Message::Backspace);

        // Should delete the 'o'
        assert_eq!(editor.buffer.line(0), "hell");
        assert_eq!(editor.cursor, (0, 4));
    }

    #[test]
    fn test_backspace_deletes_one_space_without_selection() {
        let mut editor = CodeEditor::new("key  ", "toml");
        editor.cursor = (0, 5);

        let _ = editor.update(&Message::Backspace);

        assert_eq!(editor.buffer.line(0), "key ");
        assert_eq!(editor.cursor, (0, 4));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_delete_multiline_selection() {
        let mut editor = CodeEditor::new("line1\nline2\nline3", "py");
        editor.selection_start = Some((0, 2));
        editor.selection_end = Some((2, 2));
        editor.cursor = (2, 2);

        let _ = editor.update(&Message::Delete);

        assert_eq!(editor.buffer.line(0), "line3");
        assert_eq!(editor.cursor, (0, 2));
        assert_eq!(editor.selection_start, None);
    }

    #[test]
    fn test_canvas_focus_gained() {
        let mut editor = CodeEditor::new("hello world", "py");
        assert!(!editor.has_canvas_focus);
        assert!(!editor.show_cursor);

        let _ = editor.update(&Message::CanvasFocusGained);

        assert!(editor.has_canvas_focus);
        assert!(editor.show_cursor);
    }

    #[test]
    fn test_mouse_click_gains_focus() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.has_canvas_focus = false;
        editor.show_cursor = false;

        let _ = editor.update(&Message::MouseClick(iced::Point::new(100.0, 10.0)));

        assert!(editor.has_canvas_focus);
        assert!(editor.show_cursor);
    }

    fn point_for(editor: &CodeEditor, line: usize, column: usize) -> iced::Point {
        iced::Point::new(
            editor.gutter_width() + 5.0 + editor.char_width() * column as f32 + 1.0,
            editor.line_height() * line as f32 + 1.0,
        )
    }

    #[test]
    fn mouse_click_with_center_cursor_scrolls_to_clicked_cursor() {
        let content = (0..40)
            .map(|index| format!("line {index} abcdefghijklmnopqrstuvwxyz"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = CodeEditor::new(&content, "ly");
        editor.set_center_cursor(true);
        editor.set_viewport_size(800.0, 120.0);

        let click_col = 6;
        let click_x = editor.gutter_width() + 5.0 + editor.char_width() * click_col as f32 + 1.0;
        let click_y = editor.line_height() * 4.0 + 1.0;
        let expected_scroll = editor.target_vertical_scroll_offset_for_line(4, click_col);
        let task = editor.update(&Message::MouseClick(iced::Point::new(click_x, click_y)));
        let _ = editor.update(&Message::MouseRelease);

        assert_eq!(editor.cursor_position(), (4, click_col));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
        assert!(!editor.is_dragging);
        assert_eq!(editor.viewport_scroll(), 0.0);
        assert_eq!(expected_scroll, editor.line_height() * 4.0);
        assert_eq!(task.units(), 2);
    }

    #[test]
    fn mouse_drag_selects_from_click_anchor() {
        let mut editor = CodeEditor::new("abcdef", "txt");
        let start = point_for(&editor, 0, 1);
        let end = point_for(&editor, 0, 4);

        let _ = editor.update(&Message::MouseClick(start));
        let _ = editor.update(&Message::MouseDrag(end));

        assert_eq!(editor.cursor_position(), (0, 4));
        assert_eq!(editor.selection_start, Some((0, 1)));
        assert_eq!(editor.selection_end, Some((0, 4)));
    }

    #[test]
    fn mouse_release_after_plain_click_clears_collapsed_selection() {
        let mut editor = CodeEditor::new("abcdef", "txt");
        let point = point_for(&editor, 0, 2);

        let _ = editor.update(&Message::MouseClick(point));
        let _ = editor.update(&Message::MouseRelease);

        assert_eq!(editor.cursor_position(), (0, 2));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
        assert!(!editor.is_dragging);
    }

    #[test]
    fn shift_click_extends_selection_from_previous_cursor() {
        let mut editor = CodeEditor::new("abcdef", "txt");
        editor.cursor = (0, 1);
        editor.modifiers.set(iced::keyboard::Modifiers::SHIFT);
        let end = point_for(&editor, 0, 4);

        let _ = editor.update(&Message::MouseClick(end));

        assert_eq!(editor.cursor_position(), (0, 4));
        assert_eq!(editor.selection_start, Some((0, 1)));
        assert_eq!(editor.selection_end, Some((0, 4)));
        assert!(!editor.is_dragging);
    }

    #[test]
    fn center_cursor_scroll_events_do_not_override_recent_clicked_cursor() {
        let content = (0..40)
            .map(|index| format!("line {index} abcdefghijklmnopqrstuvwxyz"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = CodeEditor::new(&content, "ly");
        editor.set_center_cursor(true);
        editor.set_viewport_size(800.0, 120.0);
        let click_col = 8;
        let click_x = editor.gutter_width() + 5.0 + editor.char_width() * click_col as f32 + 1.0;
        let click_y = editor.line_height() * 4.0 + 1.0;

        let _ = editor.update(&Message::MouseClick(iced::Point::new(click_x, click_y)));
        let _ = editor.update(&Message::MouseRelease);
        let _ = editor.handle_vertical_scroll(editor.line_height() * 4.0, 800.0, 120.0);

        assert_eq!(editor.cursor_position(), (4, click_col));
        assert_eq!(editor.pending_center_cursor_scroll, None);
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn user_scroll_with_center_cursor_moves_cursor_to_center_line() {
        let content = (0..40)
            .map(|index| format!("line {index} abcdefghijklmnopqrstuvwxyz"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = CodeEditor::new(&content, "ly");
        editor.set_center_cursor(true);
        editor.set_viewport_size(800.0, 120.0);
        editor.cursor = (4, 8);
        editor.preferred_column = 8;

        let _ = editor.handle_vertical_scroll(editor.line_height() * 10.0, 800.0, 120.0);

        assert_eq!(editor.cursor_position(), (10, 8));
        assert_eq!(editor.selection_start, None);
        assert_eq!(editor.selection_end, None);
    }

    #[test]
    fn test_word_navigation_right() {
        let mut editor = CodeEditor::new("hello   world", "py");
        editor.cursor = (0, 0);

        let _ = editor.update(&Message::WordArrowKey(ArrowDirection::Right, false));
        assert_eq!(editor.cursor, (0, 5));

        let _ = editor.update(&Message::WordArrowKey(ArrowDirection::Right, false));
        assert_eq!(editor.cursor, (0, 8));
    }

    #[test]
    fn test_word_navigation_left() {
        let mut editor = CodeEditor::new("hello   world", "py");
        editor.cursor = (0, 13);

        let _ = editor.update(&Message::WordArrowKey(ArrowDirection::Left, false));
        assert_eq!(editor.cursor, (0, 8));

        let _ = editor.update(&Message::WordArrowKey(ArrowDirection::Left, false));
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_delete_word_backward() {
        let mut editor = CodeEditor::new("hello   world", "py");
        editor.cursor = (0, 13);

        let _ = editor.update(&Message::DeleteWordBackward);
        assert_eq!(editor.buffer.line(0), "hello   ");
        assert_eq!(editor.cursor, (0, 8));
    }

    #[test]
    fn test_delete_word_forward() {
        let mut editor = CodeEditor::new("hello   world", "py");
        editor.cursor = (0, 5);

        let _ = editor.update(&Message::DeleteWordForward);
        assert_eq!(editor.buffer.line(0), "helloworld");
        assert_eq!(editor.cursor, (0, 5));
    }

    #[test]
    fn test_delete_to_line_start() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 6);

        let _ = editor.update(&Message::DeleteToLineStart);
        assert_eq!(editor.buffer.line(0), "world");
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_delete_to_line_end() {
        let mut editor = CodeEditor::new("hello world", "py");
        editor.cursor = (0, 6);

        let _ = editor.update(&Message::DeleteToLineEnd);
        assert_eq!(editor.buffer.line(0), "hello ");
        assert_eq!(editor.cursor, (0, 6));
    }

    #[test]
    fn test_shift_tab_outdents_line() {
        let mut editor = CodeEditor::new("    hello", "py");
        editor.cursor = (0, 4);

        let _ = editor.update(&Message::ShiftTab);
        assert_eq!(editor.buffer.line(0), "hello");
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_tab_multiline_selection_ignores_trailing_line_at_col_zero() {
        let mut editor = CodeEditor::new("a\nb", "py");
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((1, 0));

        let _ = editor.update(&Message::Tab);
        assert_eq!(editor.buffer.to_string(), "    a\nb");
    }

    #[test]
    fn test_enter_carries_indentation() {
        let mut editor = CodeEditor::new("    hello", "py");
        editor.cursor = (0, 9);

        let _ = editor.update(&Message::Enter);
        assert_eq!(editor.buffer.to_string(), "    hello\n    ");
        assert_eq!(editor.cursor, (1, 4));
    }

    #[test]
    fn test_enter_between_brackets_inserts_indented_block() {
        let mut editor = CodeEditor::new("{}", "py");
        editor.cursor = (0, 1);

        let _ = editor.update(&Message::Enter);
        assert_eq!(editor.buffer.to_string(), "{\n    \n}");
        assert_eq!(editor.cursor, (1, 4));
    }

    #[test]
    fn test_open_bracket_autopairs() {
        let mut editor = CodeEditor::new("", "py");
        editor.request_focus();
        editor.has_canvas_focus = true;

        let _ = editor.update(&Message::CharacterInput('('));
        assert_eq!(editor.buffer.line(0), "()");
        assert_eq!(editor.cursor, (0, 1));
    }

    #[test]
    fn test_closing_bracket_skips_over_existing_closer() {
        let mut editor = CodeEditor::new("()", "py");
        editor.request_focus();
        editor.has_canvas_focus = true;
        editor.cursor = (0, 1);

        let _ = editor.update(&Message::CharacterInput(')'));
        assert_eq!(editor.buffer.line(0), "()");
        assert_eq!(editor.cursor, (0, 2));
    }

    #[test]
    fn test_backspace_deletes_empty_pair() {
        let mut editor = CodeEditor::new("()", "py");
        editor.cursor = (0, 1);

        let _ = editor.update(&Message::Backspace);
        assert_eq!(editor.buffer.line(0), "");
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_insert_line_below_keeps_indentation() {
        let mut editor = CodeEditor::new("    alpha", "lilypond");
        editor.cursor = (0, 4);

        let _ = editor.update(&Message::InsertLineBelow);
        assert_eq!(editor.buffer.to_string(), "    alpha\n    ");
        assert_eq!(editor.cursor, (1, 4));
    }

    #[test]
    fn test_delete_line_removes_current_line() {
        let mut editor = CodeEditor::new("one\ntwo\nthree", "lilypond");
        editor.cursor = (1, 1);

        let _ = editor.update(&Message::DeleteLine);
        assert_eq!(editor.buffer.to_string(), "one\nthree");
        assert_eq!(editor.cursor, (1, 1));
    }

    #[test]
    fn test_move_line_down_moves_current_line() {
        let mut editor = CodeEditor::new("one\ntwo\nthree", "lilypond");
        editor.cursor = (0, 2);

        let _ = editor.update(&Message::MoveLineDown);
        assert_eq!(editor.buffer.to_string(), "two\none\nthree");
        assert_eq!(editor.cursor, (1, 2));
    }

    #[test]
    fn test_copy_line_down_duplicates_selection_when_present() {
        let mut editor = CodeEditor::new("hello", "lilypond");
        editor.selection_start = Some((0, 1));
        editor.selection_end = Some((0, 4));

        let _ = editor.update(&Message::CopyLineDown);
        assert_eq!(editor.buffer.to_string(), "hellello");
        assert_eq!(editor.selection_start, Some((0, 4)));
        assert_eq!(editor.selection_end, Some((0, 7)));
    }

    #[test]
    fn test_toggle_line_comment_for_lilypond() {
        let mut editor = CodeEditor::new("foo\n  bar", "lilypond");
        editor.selection_start = Some((0, 0));
        editor.selection_end = Some((2, 0));

        let _ = editor.update(&Message::ToggleLineComment);
        assert_eq!(editor.buffer.to_string(), "% foo\n  % bar");

        let _ = editor.update(&Message::ToggleLineComment);
        assert_eq!(editor.buffer.to_string(), "foo\n  bar");
    }

    #[test]
    fn test_toggle_block_comment_without_selection_inserts_pair_at_cursor() {
        let mut editor = CodeEditor::new("foo", "lilypond");
        editor.cursor = (0, 1);

        let _ = editor.update(&Message::ToggleBlockComment);
        assert_eq!(editor.buffer.to_string(), "f%{%}oo");
        assert_eq!(editor.cursor, (0, 3));
    }

    #[test]
    fn test_select_line_includes_newline_when_possible() {
        let mut editor = CodeEditor::new("one\ntwo", "lilypond");
        editor.cursor = (0, 1);

        let _ = editor.update(&Message::SelectLine);
        assert_eq!(editor.selection_start, Some((0, 0)));
        assert_eq!(editor.selection_end, Some((1, 0)));
    }

    #[test]
    fn test_select_all_selects_full_document() {
        let mut editor = CodeEditor::new("one\ntwo", "lilypond");

        let _ = editor.update(&Message::SelectAll);
        assert_eq!(editor.selection_start, Some((0, 0)));
        assert_eq!(editor.selection_end, Some((1, 3)));
    }

    #[test]
    fn test_jump_to_matching_bracket_moves_cursor() {
        let mut editor = CodeEditor::new("foo(bar)", "lilypond");
        editor.cursor = (0, 3);

        let _ = editor.update(&Message::JumpToMatchingBracket);
        assert_eq!(editor.cursor, (0, 7));
    }

    #[test]
    fn test_jump_to_matching_bracket_moves_to_nearest_bracket_first() {
        let mut editor = CodeEditor::new("foo(bar)", "lilypond");
        editor.cursor = (0, 0);

        let _ = editor.update(&Message::JumpToMatchingBracket);
        assert_eq!(editor.cursor, (0, 3));
    }

    #[test]
    fn test_mouse_double_click_selects_word() {
        let mut editor = CodeEditor::new("hello world\nnext", "lilypond");
        let x = crate::canvas_editor::GUTTER_WIDTH + 5.0 + editor.char_width() * 1.5;
        let point = iced::Point::new(x, 10.0);

        let _ = editor.update(&Message::MouseDoubleClick(point));
        assert_eq!(editor.selection_start, Some((0, 0)));
        assert_eq!(editor.selection_end, Some((0, 5)));
    }

    #[test]
    fn test_mouse_triple_click_selects_line() {
        let mut editor = CodeEditor::new("hello world\nnext", "lilypond");
        let x = crate::canvas_editor::GUTTER_WIDTH + 5.0 + editor.char_width() * 1.5;
        let point = iced::Point::new(x, 10.0);

        let _ = editor.update(&Message::MouseTripleClick(point));
        assert_eq!(editor.selection_start, Some((0, 0)));
        assert_eq!(editor.selection_end, Some((1, 0)));
    }

    #[test]
    fn test_submit_goto_line_moves_cursor() {
        let mut editor = CodeEditor::new("one\ntwo\nthree", "lilypond");
        editor.goto_line_state.query = "3:2".to_string();

        let _ = editor.update(&Message::SubmitGotoLine);
        assert_eq!(editor.cursor, (2, 1));
    }

    #[test]
    fn test_join_lines_avoids_space_before_closing_paren() {
        let mut editor = CodeEditor::new("foo(\n)", "lilypond");
        editor.cursor = (0, 0);

        let _ = editor.update(&Message::JoinLines);
        assert_eq!(editor.buffer.to_string(), "foo()");
    }
}
