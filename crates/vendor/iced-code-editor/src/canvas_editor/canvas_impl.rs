//! Canvas rendering implementation using Iced's `canvas::Program`.

use super::highlighting::{self, HighlightedDocument, StyledSpan};
use crate::language;
use iced::advanced::input_method;
use iced::mouse;
use iced::widget::canvas::{self, Geometry};
use iced::{Color, Event, Point, Rectangle, Size, Theme, keyboard};
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Computes geometry (x start and width) for a text segment used in rendering or highlighting.
///
/// # Arguments
///
/// * `line_content`: full text content of the current line.
/// * `visual_start_col`: start column index of the current visual line.
/// * `segment_start_col`: start column index of the target segment (e.g. highlight).
/// * `segment_end_col`: end column index of the target segment.
/// * `base_offset`: base X offset (usually gutter_width + padding).
///
/// # Returns
///
/// x_start, width
///
/// # Remark
///
/// This function handles CJK character widths correctly to keep highlights accurate.
fn calculate_segment_geometry(
    line_content: &str,
    visual_start_col: usize,
    segment_start_col: usize,
    segment_end_col: usize,
    base_offset: f32,
    full_char_width: f32,
    char_width: f32,
) -> (f32, f32) {
    // Clamp the segment to the current visual line so callers can safely pass
    // logical selection/match columns without worrying about wrapping boundaries.
    let segment_start_col = segment_start_col.max(visual_start_col);
    let segment_end_col = segment_end_col.max(segment_start_col);

    let mut prefix_width = 0.0;
    let mut segment_width = 0.0;

    // Compute widths directly from the source string to avoid allocating
    // intermediate `String` slices for prefix/segment.
    for (i, c) in line_content.chars().enumerate() {
        if i >= segment_end_col {
            break;
        }

        let w = super::measure_char_width(c, full_char_width, char_width);

        if i >= visual_start_col && i < segment_start_col {
            prefix_width += w;
        } else if i >= segment_start_col {
            segment_width += w;
        }
    }

    (base_offset + prefix_width, segment_width)
}

fn expand_tabs(text: &str, tab_width: usize) -> Cow<'_, str> {
    if !text.contains('\t') {
        return Cow::Borrowed(text);
    }

    let mut expanded = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\t' {
            for _ in 0..tab_width {
                expanded.push(' ');
            }
        } else {
            expanded.push(ch);
        }
    }

    Cow::Owned(expanded)
}

use super::wrapping::{VisualLine, WrappingCalculator};
use super::{
    ArrowDirection, CodeEditor, DOUBLE_CLICK_DISTANCE, DOUBLE_CLICK_INTERVAL, Message,
    measure_text_width,
};
use iced::widget::canvas::Action;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

/// Context for canvas rendering operations.
///
/// This struct packages commonly used rendering parameters to reduce
/// method signature complexity and improve code maintainability.
struct RenderContext<'a> {
    /// Visual lines calculated from wrapping
    visual_lines: &'a [VisualLine],
    /// Width of the canvas bounds
    bounds_width: f32,
    /// Width of the line number gutter
    gutter_width: f32,
    /// Height of each line in pixels
    line_height: f32,
    /// Font size in pixels
    font_size: f32,
    /// Full character width for wide characters (e.g., CJK)
    full_char_width: f32,
    /// Character width for narrow characters
    char_width: f32,
    /// Font to use for rendering text
    font: iced::Font,
    /// Horizontal scroll offset in pixels (subtracted from text X positions)
    horizontal_scroll_offset: f32,
}

impl CodeEditor {
    /// Draws line numbers and wrap indicators in the gutter area.
    ///
    /// # Arguments
    ///
    /// * `frame` - The canvas frame to draw on
    /// * `ctx` - Rendering context containing visual lines and metrics
    /// * `visual_line` - The visual line to render
    /// * `y` - Y position for rendering
    fn draw_line_numbers(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        visual_line: &VisualLine,
        y: f32,
    ) {
        if !self.line_numbers_enabled {
            return;
        }

        if visual_line.is_first_segment() {
            // Draw line number for first segment
            let line_num = visual_line.logical_line + 1;
            let line_num_text = format!("{}", line_num);
            // Calculate actual text width and center in gutter
            let text_width =
                measure_text_width(&line_num_text, ctx.full_char_width, ctx.char_width);
            let x_pos = (ctx.gutter_width - text_width) / 2.0;
            frame.fill_text(canvas::Text {
                content: line_num_text,
                position: Point::new(x_pos, y + 2.0),
                color: self.style.line_number_color,
                size: ctx.font_size.into(),
                font: ctx.font,
                ..canvas::Text::default()
            });
        } else {
            // Draw wrap indicator for continuation lines
            frame.fill_text(canvas::Text {
                content: "↪".to_string(),
                position: Point::new(ctx.gutter_width - 20.0, y + 2.0),
                color: self.style.line_number_color,
                size: ctx.font_size.into(),
                font: ctx.font,
                ..canvas::Text::default()
            });
        }
    }

    /// Draws the active line number in the gutter.
    fn draw_active_line_number(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        visual_line: &VisualLine,
        y: f32,
    ) {
        if !self.line_numbers_enabled
            || !visual_line.is_first_segment()
            || visual_line.logical_line != self.cursor.0
        {
            return;
        }

        let line_num_text = format!("{}", visual_line.logical_line + 1);
        let text_width = measure_text_width(&line_num_text, ctx.full_char_width, ctx.char_width);
        let x_pos = (ctx.gutter_width - text_width) / 2.0;

        frame.fill_text(canvas::Text {
            content: line_num_text,
            position: Point::new(x_pos, y + 2.0),
            color: self.style.active_line_number_color,
            size: ctx.font_size.into(),
            font: ctx.font,
            ..canvas::Text::default()
        });
    }

    /// Draws the background highlight for the current line.
    ///
    /// # Arguments
    ///
    /// * `frame` - The canvas frame to draw on
    /// * `ctx` - Rendering context containing visual lines and metrics
    /// * `visual_line` - The visual line to check
    /// * `y` - Y position for rendering
    fn draw_current_line_highlight(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        visual_line: &VisualLine,
        y: f32,
    ) {
        if visual_line.logical_line == self.cursor.0 {
            frame.fill_rectangle(
                Point::new(ctx.gutter_width, y),
                Size::new(ctx.bounds_width - ctx.gutter_width, ctx.line_height),
                self.style.current_line_highlight,
            );
        }
    }

    /// Draws text content with syntax highlighting or plain text fallback.
    ///
    /// # Arguments
    ///
    /// * `frame` - The canvas frame to draw on
    /// * `ctx` - Rendering context containing visual lines and metrics
    /// * `visual_line` - The visual line to render
    /// * `y` - Y position for rendering
    /// * `syntax_ref` - Optional syntax reference for highlighting
    /// * `syntax_set` - Syntax set for highlighting
    /// * `syntax_theme` - Theme for syntax highlighting
    #[allow(clippy::too_many_arguments)]
    fn draw_text_with_syntax_highlighting(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        visual_line: &VisualLine,
        y: f32,
        tree_sitter_document: Option<&HighlightedDocument>,
        syntax_ref: Option<&syntect::parsing::SyntaxReference>,
        syntax_set: &SyntaxSet,
        syntax_theme: Option<&syntect::highlighting::Theme>,
    ) {
        let full_line_content = self.buffer.line(visual_line.logical_line);

        // Convert character indices to byte indices for UTF-8 string slicing
        let start_byte = full_line_content
            .char_indices()
            .nth(visual_line.start_col)
            .map_or(full_line_content.len(), |(idx, _)| idx);
        let end_byte = full_line_content
            .char_indices()
            .nth(visual_line.end_col)
            .map_or(full_line_content.len(), |(idx, _)| idx);
        let line_segment = &full_line_content[start_byte..end_byte];

        if let Some(document) = tree_sitter_document {
            let styled_spans = document
                .lines
                .get(visual_line.logical_line)
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            self.draw_text_with_styled_spans(
                frame,
                ctx,
                full_line_content,
                visual_line.start_col,
                visual_line.end_col,
                styled_spans,
                y,
            );
        } else if let (Some(syntax), Some(syntax_theme)) = (syntax_ref, syntax_theme) {
            let mut highlighter = HighlightLines::new(syntax, syntax_theme);

            // Highlight the full line to get correct token colors
            let full_line_ranges = highlighter
                .highlight_line(full_line_content, syntax_set)
                .unwrap_or_else(|_| vec![(Style::default(), full_line_content)]);

            // Extract only the ranges that fall within our segment
            let mut x_offset = ctx.gutter_width + 5.0 - ctx.horizontal_scroll_offset;
            let mut char_pos = 0;

            for (style, text) in full_line_ranges {
                let text_len = text.chars().count();
                let text_end = char_pos + text_len;

                // Check if this token intersects with our segment
                if text_end > visual_line.start_col && char_pos < visual_line.end_col {
                    // Calculate the intersection
                    let segment_start = char_pos.max(visual_line.start_col);
                    let segment_end = text_end.min(visual_line.end_col);

                    let text_start_offset = segment_start.saturating_sub(char_pos);
                    let text_end_offset = text_start_offset + (segment_end - segment_start);

                    // Convert character offsets to byte offsets for UTF-8 slicing
                    let start_byte = text
                        .char_indices()
                        .nth(text_start_offset)
                        .map_or(text.len(), |(idx, _)| idx);
                    let end_byte = text
                        .char_indices()
                        .nth(text_end_offset)
                        .map_or(text.len(), |(idx, _)| idx);

                    let segment_text = &text[start_byte..end_byte];
                    let display_text = expand_tabs(segment_text, super::TAB_WIDTH).into_owned();
                    let display_width =
                        measure_text_width(&display_text, ctx.full_char_width, ctx.char_width);

                    let color = Color::from_rgb(
                        f32::from(style.foreground.r) / 255.0,
                        f32::from(style.foreground.g) / 255.0,
                        f32::from(style.foreground.b) / 255.0,
                    );

                    frame.fill_text(canvas::Text {
                        content: display_text,
                        position: Point::new(x_offset, y + 2.0),
                        color,
                        size: ctx.font_size.into(),
                        font: ctx.font,
                        ..canvas::Text::default()
                    });

                    x_offset += display_width;
                }

                char_pos = text_end;
            }
        } else {
            // Fallback to plain text
            let display_text = expand_tabs(line_segment, super::TAB_WIDTH).into_owned();
            frame.fill_text(canvas::Text {
                content: display_text,
                position: Point::new(
                    ctx.gutter_width + 5.0 - ctx.horizontal_scroll_offset,
                    y + 2.0,
                ),
                color: self.style.text_color,
                size: ctx.font_size.into(),
                font: ctx.font,
                ..canvas::Text::default()
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_with_styled_spans(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        full_line_content: &str,
        visual_start_col: usize,
        visual_end_col: usize,
        spans: &[StyledSpan],
        y: f32,
    ) {
        let mut current_col = visual_start_col;
        let mut x_offset = ctx.gutter_width + 5.0 - ctx.horizontal_scroll_offset;

        for span in spans {
            if span.end_col <= visual_start_col || span.start_col >= visual_end_col {
                continue;
            }

            if current_col < span.start_col {
                x_offset += self.draw_text_segment(
                    frame,
                    ctx,
                    full_line_content,
                    current_col,
                    span.start_col.min(visual_end_col),
                    y,
                    self.style.text_color,
                );
            }

            let segment_start = span.start_col.max(visual_start_col);
            let segment_end = span.end_col.min(visual_end_col);

            if segment_start < segment_end {
                x_offset += self.draw_text_segment(
                    frame,
                    ctx,
                    full_line_content,
                    segment_start,
                    segment_end,
                    y,
                    span.color,
                );
                current_col = segment_end;
            }
        }

        if current_col < visual_end_col {
            let _ = self.draw_text_segment(
                frame,
                ctx,
                full_line_content,
                current_col,
                visual_end_col,
                y,
                self.style.text_color,
            );
        } else {
            let _ = x_offset;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_segment(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        full_line_content: &str,
        start_col: usize,
        end_col: usize,
        y: f32,
        color: Color,
    ) -> f32 {
        if start_col >= end_col {
            return 0.0;
        }

        let start_byte = full_line_content
            .char_indices()
            .nth(start_col)
            .map_or(full_line_content.len(), |(idx, _)| idx);
        let end_byte = full_line_content
            .char_indices()
            .nth(end_col)
            .map_or(full_line_content.len(), |(idx, _)| idx);
        let segment_text = &full_line_content[start_byte..end_byte];
        let display_text = expand_tabs(segment_text, super::TAB_WIDTH).into_owned();
        let display_width = measure_text_width(&display_text, ctx.full_char_width, ctx.char_width);
        let x_offset = ctx.gutter_width + 5.0 - ctx.horizontal_scroll_offset
            + measure_text_width(
                &expand_tabs(&full_line_content[..start_byte], super::TAB_WIDTH),
                ctx.full_char_width,
                ctx.char_width,
            );

        frame.fill_text(canvas::Text {
            content: display_text,
            position: Point::new(x_offset, y + 2.0),
            color,
            size: ctx.font_size.into(),
            font: ctx.font,
            ..canvas::Text::default()
        });

        display_width
    }

    /// Draws search match highlights for all visible matches.
    ///
    /// # Arguments
    ///
    /// * `frame` - The canvas frame to draw on
    /// * `ctx` - Rendering context containing visual lines and metrics
    /// * `first_visible_line` - First visible visual line index
    /// * `last_visible_line` - Last visible visual line index
    fn draw_search_highlights(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        start_visual_idx: usize,
        end_visual_idx: usize,
    ) {
        if !self.search_state.is_open || self.search_state.query.is_empty() {
            return;
        }

        let query_len = self.search_state.query.chars().count();

        let start_visual_idx = start_visual_idx.min(ctx.visual_lines.len());
        let end_visual_idx = end_visual_idx.min(ctx.visual_lines.len());

        let end_visual_inclusive = end_visual_idx
            .saturating_sub(1)
            .min(ctx.visual_lines.len().saturating_sub(1));

        if let (Some(start_vl), Some(end_vl)) = (
            ctx.visual_lines.get(start_visual_idx),
            ctx.visual_lines.get(end_visual_inclusive),
        ) {
            let min_logical_line = start_vl.logical_line;
            let max_logical_line = end_vl.logical_line;

            // Optimization: Use get_visible_match_range to find matches in view
            // This uses binary search + early termination for O(log N) performance
            let match_range = super::search::get_visible_match_range(
                &self.search_state.matches,
                min_logical_line,
                max_logical_line,
            );

            for (match_idx, search_match) in self
                .search_state
                .matches
                .iter()
                .enumerate()
                .skip(match_range.start)
                .take(match_range.len())
            {
                // Determine if this is the current match
                let is_current = self.search_state.current_match_index == Some(match_idx);

                let highlight_color = if is_current {
                    // Orange for current match
                    Color {
                        r: 1.0,
                        g: 0.6,
                        b: 0.0,
                        a: 0.4,
                    }
                } else {
                    // Yellow for other matches
                    Color {
                        r: 1.0,
                        g: 1.0,
                        b: 0.0,
                        a: 0.3,
                    }
                };

                // Convert logical position to visual line
                let start_visual = WrappingCalculator::logical_to_visual(
                    ctx.visual_lines,
                    search_match.line,
                    search_match.col,
                );
                let end_visual = WrappingCalculator::logical_to_visual(
                    ctx.visual_lines,
                    search_match.line,
                    search_match.col + query_len,
                );

                if let (Some(start_v), Some(end_v)) = (start_visual, end_visual) {
                    if start_v == end_v {
                        // Match within same visual line
                        let y = start_v as f32 * ctx.line_height;
                        let vl = &ctx.visual_lines[start_v];
                        let line_content = self.buffer.line(vl.logical_line);

                        // Use calculate_segment_geometry to compute match position and width
                        let (x_start, match_width) = calculate_segment_geometry(
                            line_content,
                            vl.start_col,
                            search_match.col,
                            search_match.col + query_len,
                            ctx.gutter_width + 5.0,
                            ctx.full_char_width,
                            ctx.char_width,
                        );
                        let x_start = x_start - ctx.horizontal_scroll_offset;
                        let x_end = x_start + match_width;

                        frame.fill_rectangle(
                            Point::new(x_start, y + 2.0),
                            Size::new(x_end - x_start, ctx.line_height - 4.0),
                            highlight_color,
                        );
                    } else {
                        // Match spans multiple visual lines
                        for (v_idx, vl) in ctx
                            .visual_lines
                            .iter()
                            .enumerate()
                            .skip(start_v)
                            .take(end_v - start_v + 1)
                        {
                            let y = v_idx as f32 * ctx.line_height;

                            let match_start_col = search_match.col;
                            let match_end_col = search_match.col + query_len;

                            let sel_start_col = if v_idx == start_v {
                                match_start_col
                            } else {
                                vl.start_col
                            };
                            let sel_end_col = if v_idx == end_v {
                                match_end_col
                            } else {
                                vl.end_col
                            };

                            let line_content = self.buffer.line(vl.logical_line);

                            let (x_start, sel_width) = calculate_segment_geometry(
                                line_content,
                                vl.start_col,
                                sel_start_col,
                                sel_end_col,
                                ctx.gutter_width + 5.0,
                                ctx.full_char_width,
                                ctx.char_width,
                            );
                            let x_start = x_start - ctx.horizontal_scroll_offset;
                            let x_end = x_start + sel_width;

                            frame.fill_rectangle(
                                Point::new(x_start, y + 2.0),
                                Size::new(x_end - x_start, ctx.line_height - 4.0),
                                highlight_color,
                            );
                        }
                    }
                }
            }
        }
    }

    fn draw_matching_bracket_highlight(&self, frame: &mut canvas::Frame, _ctx: &RenderContext<'_>) {
        let Some((start, end)) = self.matching_bracket_pair() else {
            return;
        };

        let highlight_color = Color {
            a: 0.18,
            ..self.style.text_color
        };

        for position in [start, end] {
            let Some(ch) = self.char_at(position) else {
                continue;
            };
            let Some(point) = self.point_from_position(position.0, position.1) else {
                continue;
            };

            frame.fill_rectangle(
                Point::new(point.x - self.horizontal_scroll_offset, point.y + 1.0),
                Size::new(
                    super::measure_char_width(ch, self.full_char_width, self.char_width),
                    (self.line_height - 2.0).max(1.0),
                ),
                highlight_color,
            );
        }
    }

    /// Draws text selection highlights.
    ///
    /// # Arguments
    ///
    /// * `frame` - The canvas frame to draw on
    /// * `ctx` - Rendering context containing visual lines and metrics
    fn draw_selection_highlight(&self, frame: &mut canvas::Frame, ctx: &RenderContext<'_>) {
        if let Some((start, end)) = self.get_selection_range()
            && start != end
        {
            let selection_color = Color {
                r: 0.3,
                g: 0.5,
                b: 0.8,
                a: 0.3,
            };

            if start.0 == end.0 {
                // Single line selection - need to handle wrapped segments
                let start_visual =
                    WrappingCalculator::logical_to_visual(ctx.visual_lines, start.0, start.1);
                let end_visual =
                    WrappingCalculator::logical_to_visual(ctx.visual_lines, end.0, end.1);

                if let (Some(start_v), Some(end_v)) = (start_visual, end_visual) {
                    if start_v == end_v {
                        // Selection within same visual line
                        let y = start_v as f32 * ctx.line_height;
                        let vl = &ctx.visual_lines[start_v];
                        let line_content = self.buffer.line(vl.logical_line);

                        let (x_start, sel_width) = calculate_segment_geometry(
                            line_content,
                            vl.start_col,
                            start.1,
                            end.1,
                            ctx.gutter_width + 5.0,
                            ctx.full_char_width,
                            ctx.char_width,
                        );
                        let x_start = x_start - ctx.horizontal_scroll_offset;
                        let x_end = x_start + sel_width;

                        frame.fill_rectangle(
                            Point::new(x_start, y),
                            Size::new(x_end - x_start, ctx.line_height),
                            selection_color,
                        );
                    } else {
                        // Selection spans multiple visual lines (same logical line)
                        for (v_idx, vl) in ctx
                            .visual_lines
                            .iter()
                            .enumerate()
                            .skip(start_v)
                            .take(end_v - start_v + 1)
                        {
                            let y = v_idx as f32 * ctx.line_height;

                            let sel_start_col = if v_idx == start_v {
                                start.1
                            } else {
                                vl.start_col
                            };
                            let sel_end_col = if v_idx == end_v { end.1 } else { vl.end_col };

                            let line_content = self.buffer.line(vl.logical_line);

                            let (x_start, sel_width) = calculate_segment_geometry(
                                line_content,
                                vl.start_col,
                                sel_start_col,
                                sel_end_col,
                                ctx.gutter_width + 5.0,
                                ctx.full_char_width,
                                ctx.char_width,
                            );
                            let x_start = x_start - ctx.horizontal_scroll_offset;
                            let x_end = x_start + sel_width;

                            frame.fill_rectangle(
                                Point::new(x_start, y),
                                Size::new(x_end - x_start, ctx.line_height),
                                selection_color,
                            );
                        }
                    }
                }
            } else {
                // Multi-line selection
                let start_visual =
                    WrappingCalculator::logical_to_visual(ctx.visual_lines, start.0, start.1);
                let end_visual =
                    WrappingCalculator::logical_to_visual(ctx.visual_lines, end.0, end.1);

                if let (Some(start_v), Some(end_v)) = (start_visual, end_visual) {
                    for (v_idx, vl) in ctx
                        .visual_lines
                        .iter()
                        .enumerate()
                        .skip(start_v)
                        .take(end_v - start_v + 1)
                    {
                        let y = v_idx as f32 * ctx.line_height;

                        let sel_start_col = if vl.logical_line == start.0 && v_idx == start_v {
                            start.1
                        } else {
                            vl.start_col
                        };

                        let sel_end_col = if vl.logical_line == end.0 && v_idx == end_v {
                            end.1
                        } else {
                            vl.end_col
                        };

                        let line_content = self.buffer.line(vl.logical_line);

                        let (x_start, sel_width) = calculate_segment_geometry(
                            line_content,
                            vl.start_col,
                            sel_start_col,
                            sel_end_col,
                            ctx.gutter_width + 5.0,
                            ctx.full_char_width,
                            ctx.char_width,
                        );
                        let x_start = x_start - ctx.horizontal_scroll_offset;
                        let x_end = x_start + sel_width;

                        frame.fill_rectangle(
                            Point::new(x_start, y),
                            Size::new(x_end - x_start, ctx.line_height),
                            selection_color,
                        );
                    }
                }
            }
        }
    }

    /// Draws the cursor (normal caret or IME preedit cursor).
    ///
    /// # Arguments
    ///
    /// * `frame` - The canvas frame to draw on
    /// * `ctx` - Rendering context containing visual lines and metrics
    fn draw_cursor(&self, frame: &mut canvas::Frame, ctx: &RenderContext<'_>) {
        // Cursor drawing logic (only when the editor has focus)
        // -------------------------------------------------------------------------
        // Core notes:
        // 1. Choose the drawing path based on whether IME preedit is present.
        // 2. Require both `is_focused()` (Iced focus) and `has_canvas_focus()` (internal focus)
        //    so the cursor is drawn only in the active editor, avoiding multiple cursors.
        // 3. Use `WrappingCalculator` to map logical (line, col) to visual (x, y)
        //    for correct cursor positioning with line wrapping.
        // -------------------------------------------------------------------------
        if self.show_cursor && self.cursor_visible && self.has_focus() && self.ime_preedit.is_some()
        {
            // [Branch A] IME preedit rendering mode
            // ---------------------------------------------------------------------
            // When the user is composing with an IME (e.g. pinyin before commit),
            // draw a preedit region instead of the normal caret, including:
            // - preedit background (highlighting the composing text)
            // - preedit text content (preedit.content)
            // - preedit selection (underline or selection background)
            // - preedit caret
            // ---------------------------------------------------------------------
            if let Some(cursor_visual) = WrappingCalculator::logical_to_visual(
                ctx.visual_lines,
                self.cursor.0,
                self.cursor.1,
            ) {
                let vl = &ctx.visual_lines[cursor_visual];
                let line_content = self.buffer.line(vl.logical_line);

                // Compute the preedit region start X
                // Use calculate_segment_geometry to ensure correct CJK width handling
                let (cursor_x_content, _) = calculate_segment_geometry(
                    line_content,
                    vl.start_col,
                    self.cursor.1,
                    self.cursor.1,
                    ctx.gutter_width + 5.0,
                    ctx.full_char_width,
                    ctx.char_width,
                );
                let cursor_x = cursor_x_content - ctx.horizontal_scroll_offset;
                let cursor_y = cursor_visual as f32 * ctx.line_height;

                if let Some(preedit) = self.ime_preedit.as_ref() {
                    let preedit_width =
                        measure_text_width(&preedit.content, ctx.full_char_width, ctx.char_width);

                    // 1. Draw preedit background (light translucent)
                    // This indicates the text is not committed yet
                    frame.fill_rectangle(
                        Point::new(cursor_x, cursor_y + 2.0),
                        Size::new(preedit_width, ctx.line_height - 4.0),
                        Color {
                            r: 1.0,
                            g: 1.0,
                            b: 1.0,
                            a: 0.08,
                        },
                    );

                    // 2. Draw preedit selection (if any)
                    // IME may mark a selection inside preedit text (e.g. segmentation)
                    // The range uses UTF-8 byte indices, so slices must be safe
                    if let Some(range) = preedit.selection.as_ref()
                        && range.start != range.end
                    {
                        // Validate indices before slicing to prevent panic
                        if let Some((start, end)) =
                            validate_selection_indices(&preedit.content, range.start, range.end)
                        {
                            let selected_prefix = &preedit.content[..start];
                            let selected_text = &preedit.content[start..end];

                            let selection_x = cursor_x
                                + measure_text_width(
                                    selected_prefix,
                                    ctx.full_char_width,
                                    ctx.char_width,
                                );
                            let selection_w = measure_text_width(
                                selected_text,
                                ctx.full_char_width,
                                ctx.char_width,
                            );

                            frame.fill_rectangle(
                                Point::new(selection_x, cursor_y + 2.0),
                                Size::new(selection_w, ctx.line_height - 4.0),
                                Color {
                                    r: 0.3,
                                    g: 0.5,
                                    b: 0.8,
                                    a: 0.3,
                                },
                            );
                        }
                    }

                    // 3. Draw preedit text itself
                    frame.fill_text(canvas::Text {
                        content: preedit.content.clone(),
                        position: Point::new(cursor_x, cursor_y + 2.0),
                        color: self.style.text_color,
                        size: ctx.font_size.into(),
                        font: ctx.font,
                        ..canvas::Text::default()
                    });

                    // 4. Draw bottom underline (IME state indicator)
                    frame.fill_rectangle(
                        Point::new(cursor_x, cursor_y + ctx.line_height - 3.0),
                        Size::new(preedit_width, 1.0),
                        self.style.text_color,
                    );

                    // 5. Draw preedit caret
                    // If IME provides a caret position (usually selection end), draw a thin bar
                    if let Some(range) = preedit.selection.as_ref() {
                        let caret_end = range.end.min(preedit.content.len());

                        // Validate caret position to avoid panic on invalid UTF-8 boundary
                        if caret_end <= preedit.content.len()
                            && preedit.content.is_char_boundary(caret_end)
                        {
                            let caret_prefix = &preedit.content[..caret_end];
                            let caret_x = cursor_x
                                + measure_text_width(
                                    caret_prefix,
                                    ctx.full_char_width,
                                    ctx.char_width,
                                );

                            frame.fill_rectangle(
                                Point::new(caret_x, cursor_y + 2.0),
                                Size::new(2.0, ctx.line_height - 4.0),
                                self.style.text_color,
                            );
                        }
                    }
                }
            }
        } else if self.show_cursor && self.cursor_visible && self.has_focus() {
            // [Branch B] Normal caret rendering mode
            // ---------------------------------------------------------------------
            // When there is no IME preedit, draw the standard editor caret.
            // Key checks:
            // - is_focused(): the widget has Iced focus
            // - has_canvas_focus: internal focus state (mouse clicks, etc.)
            // - draw only when both are true to avoid ghost cursors
            // ---------------------------------------------------------------------

            // Map logical cursor position (Line, Col) to visual line index
            // to handle line wrapping changes
            if let Some(cursor_visual) = WrappingCalculator::logical_to_visual(
                ctx.visual_lines,
                self.cursor.0,
                self.cursor.1,
            ) {
                let vl = &ctx.visual_lines[cursor_visual];
                let line_content = self.buffer.line(vl.logical_line);

                // Compute exact caret X position
                // Account for gutter width, left padding, and rendered prefix width
                let (cursor_x_content, _) = calculate_segment_geometry(
                    line_content,
                    vl.start_col,
                    self.cursor.1,
                    self.cursor.1,
                    ctx.gutter_width + 5.0,
                    ctx.full_char_width,
                    ctx.char_width,
                );
                let cursor_x = cursor_x_content - ctx.horizontal_scroll_offset;
                let cursor_y = cursor_visual as f32 * ctx.line_height;

                // Draw standard caret (2px vertical bar)
                frame.fill_rectangle(
                    Point::new(cursor_x, cursor_y + 2.0),
                    Size::new(2.0, ctx.line_height - 4.0),
                    self.style.text_color,
                );
            }
        }
    }

    /// Checks if the editor has focus (both Iced focus and internal canvas focus).
    ///
    /// # Returns
    ///
    /// `true` if the editor has both Iced focus and internal canvas focus and is not focus-locked; `false` otherwise
    pub(crate) fn has_focus(&self) -> bool {
        // Check if this editor has Iced focus
        let focused_id = super::FOCUSED_EDITOR_ID.load(std::sync::atomic::Ordering::Relaxed);
        focused_id == self.editor_id && self.has_canvas_focus && !self.focus_locked
    }

    /// Handles widget-local keyboard controls that are not part of the app
    /// shortcut binding system.
    ///
    /// This implementation includes focus chain management for Tab and Shift+Tab
    /// navigation between editors.
    ///
    /// # Arguments
    ///
    /// * `key` - The keyboard key that was pressed
    /// * `modifiers` - The keyboard modifiers (Ctrl, Shift, Alt, etc.)
    ///
    /// # Returns
    ///
    /// `Some(Action<Message>)` if a shortcut was matched, `None` otherwise
    fn handle_keyboard_shortcuts(
        &self,
        key: &keyboard::Key,
        _modifiers: &keyboard::Modifiers,
    ) -> Option<Action<Message>> {
        if self.completion_state.visible {
            return match key {
                keyboard::Key::Named(keyboard::key::Named::Escape) => {
                    Some(Action::publish(Message::CloseCompletion).and_capture())
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                    Some(Action::publish(Message::CompletionNavigateUp).and_capture())
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                    Some(Action::publish(Message::CompletionNavigateDown).and_capture())
                }
                keyboard::Key::Named(keyboard::key::Named::Enter)
                | keyboard::Key::Named(keyboard::key::Named::Tab) => {
                    Some(Action::publish(Message::CompletionConfirm).and_capture())
                }
                _ => None,
            };
        }

        // Handle Escape (close search dialog if open)
        if self.search_state.is_open
            && matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
        {
            return Some(Action::publish(Message::CloseSearch).and_capture());
        }

        if self.goto_line_state.is_open
            && matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
        {
            return Some(Action::publish(Message::CloseGotoLine).and_capture());
        }

        None
    }

    /// Handles character input and special navigation keys.
    ///
    /// This implementation includes focus event propagation and focus chain management
    /// for proper focus handling without mouse bounds checking.
    ///
    /// # Arguments
    ///
    /// * `key` - The keyboard key that was pressed
    /// * `modifiers` - The keyboard modifiers (Ctrl, Shift, Alt, etc.)
    /// * `text` - Optional text content from the keyboard event
    ///
    /// # Returns
    ///
    /// `Some(Action<Message>)` if input should be processed, `None` otherwise
    #[allow(clippy::unused_self)]
    fn handle_character_input(
        &self,
        key: &keyboard::Key,
        modifiers: &keyboard::Modifiers,
        text: Option<&str>,
    ) -> Option<Action<Message>> {
        // Early exit: Only process character input when editor has focus
        // This prevents focus stealing where characters typed in other input fields
        // appear in the editor
        if !self.has_focus() {
            return None;
        }

        // PRIORITY 1: Check if 'text' field has valid printable character
        // This handles:
        // - Numpad keys with NumLock ON (key=Named(ArrowDown), text=Some("2"))
        // - Regular typing with shift, accents, international layouts
        if let Some(text_content) = text
            && !text_content.is_empty()
            && !modifiers.control()
            && !modifiers.command()
            && !modifiers.alt()
        {
            // Check if it's a printable character (not a control character)
            // This filters out Enter (\n), Tab (\t), Delete (U+007F), etc.
            if let Some(first_char) = text_content.chars().next()
                && !first_char.is_control()
            {
                return Some(Action::publish(Message::CharacterInput(first_char)).and_capture());
            }
        }

        // PRIORITY 2: Handle special named keys (navigation, editing)
        // These are only processed if text didn't contain a printable character
        let plain_editing_modifiers =
            !modifiers.command() && !modifiers.control() && !modifiers.alt();

        let message = match key {
            keyboard::Key::Named(keyboard::key::Named::Backspace) => {
                if plain_editing_modifiers {
                    Some(Message::Backspace)
                } else {
                    None
                }
            }
            keyboard::Key::Named(keyboard::key::Named::Delete) => {
                if plain_editing_modifiers {
                    Some(Message::Delete)
                } else {
                    None
                }
            }
            keyboard::Key::Named(keyboard::key::Named::Enter) => {
                if self.search_state.is_open
                    && !modifiers.command()
                    && !modifiers.control()
                    && !modifiers.alt()
                {
                    Some(if modifiers.shift() {
                        Message::FindPrevious
                    } else if self.search_state.is_replace_mode
                        && self.search_state.focused_field
                            == crate::canvas_editor::search::SearchFocusedField::Replace
                    {
                        Message::ReplaceNext
                    } else {
                        Message::FindNext
                    })
                } else {
                    plain_editing_modifiers.then_some(Message::Enter)
                }
            }
            keyboard::Key::Named(keyboard::key::Named::Tab) => {
                if !plain_editing_modifiers {
                    None
                } else if modifiers.shift() {
                    if self.search_state.is_open {
                        Some(Message::SearchDialogShiftTab)
                    } else {
                        Some(Message::ShiftTab)
                    }
                } else {
                    if self.search_state.is_open {
                        Some(Message::SearchDialogTab)
                    } else {
                        Some(Message::Tab)
                    }
                }
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                if plain_editing_modifiers {
                    Some(Message::ArrowKey(ArrowDirection::Up, modifiers.shift()))
                } else {
                    None
                }
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                if plain_editing_modifiers {
                    Some(Message::ArrowKey(ArrowDirection::Down, modifiers.shift()))
                } else {
                    None
                }
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => {
                if plain_editing_modifiers {
                    Some(Message::ArrowKey(ArrowDirection::Left, modifiers.shift()))
                } else {
                    None
                }
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowRight) => {
                if plain_editing_modifiers {
                    Some(Message::ArrowKey(ArrowDirection::Right, modifiers.shift()))
                } else {
                    None
                }
            }
            keyboard::Key::Named(keyboard::key::Named::PageUp) => {
                plain_editing_modifiers.then_some(Message::PageUp)
            }
            keyboard::Key::Named(keyboard::key::Named::PageDown) => {
                plain_editing_modifiers.then_some(Message::PageDown)
            }
            keyboard::Key::Named(keyboard::key::Named::Home) => {
                plain_editing_modifiers.then_some(Message::Home(modifiers.shift()))
            }
            keyboard::Key::Named(keyboard::key::Named::End) => {
                plain_editing_modifiers.then_some(Message::End(modifiers.shift()))
            }
            // PRIORITY 3: Fallback to extracting from 'key' if text was empty/control char
            // This handles edge cases where text field is not populated
            _ => {
                if !modifiers.control()
                    && !modifiers.command()
                    && !modifiers.alt()
                    && let keyboard::Key::Character(c) = key
                    && !c.is_empty()
                {
                    return c
                        .chars()
                        .next()
                        .map(Message::CharacterInput)
                        .map(|msg| Action::publish(msg).and_capture());
                }
                None
            }
        };

        message.map(|msg| Action::publish(msg).and_capture())
    }

    /// Handles keyboard events with focus event propagation through widget hierarchy.
    ///
    /// This implementation completes focus handling without mouse bounds checking
    /// and ensures proper focus chain management.
    ///
    /// # Arguments
    ///
    /// * `key` - The keyboard key that was pressed
    /// * `modifiers` - The keyboard modifiers (Ctrl, Shift, Alt, etc.)
    /// * `text` - Optional text content from the keyboard event
    /// * `bounds` - The rectangle bounds of the canvas widget (unused in this implementation)
    /// * `cursor` - The current mouse cursor position and status (unused in this implementation)
    ///
    /// # Returns
    ///
    /// `Some(Action<Message>)` if the event was handled, `None` otherwise
    fn handle_keyboard_event(
        &self,
        key: &keyboard::Key,
        modifiers: &keyboard::Modifiers,
        text: &Option<iced::advanced::graphics::core::SmolStr>,
        _bounds: Rectangle,
        _cursor: &mouse::Cursor,
    ) -> Option<Action<Message>> {
        // Early exit: Check if editor has focus and is not focus-locked
        // This prevents focus stealing where keyboard input meant for other widgets
        // is incorrectly processed by this editor during focus transitions
        if !self.has_focus() || self.focus_locked {
            return None;
        }

        // Skip if IME is active (unless Ctrl/Command is pressed)
        if self.ime_preedit.is_some() && !(modifiers.control() || modifiers.command()) {
            return None;
        }

        // Try keyboard shortcuts first
        if let Some(action) = self.handle_keyboard_shortcuts(key, modifiers) {
            return Some(action);
        }

        // Handle character input and special keys
        // Convert Option<SmolStr> to Option<&str>
        let text_str = text.as_ref().map(|s| s.as_str());
        self.handle_character_input(key, modifiers, text_str)
    }

    /// Handles mouse events (button presses, movement, releases).
    ///
    /// # Arguments
    ///
    /// * `event` - The mouse event to handle
    /// * `bounds` - The rectangle bounds of the canvas widget
    /// * `cursor` - The current mouse cursor position and status
    ///
    /// # Returns
    ///
    /// `Some(Action<Message>)` if the event was handled, `None` otherwise
    #[allow(clippy::unused_self)]
    fn handle_mouse_event(
        &self,
        event: &mouse::Event,
        bounds: Rectangle,
        cursor: &mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            mouse::Event::WheelScrolled { delta } => {
                if !cursor.is_over(bounds) || self.wrap_enabled {
                    return None;
                }

                let horizontal_delta = match delta {
                    mouse::ScrollDelta::Lines { x, y } => {
                        if x.abs() > f32::EPSILON {
                            -x * 40.0
                        } else if self.modifiers.get().shift() {
                            -y * 40.0
                        } else {
                            0.0
                        }
                    }
                    mouse::ScrollDelta::Pixels { x, y } => {
                        if x.abs() > f32::EPSILON {
                            -*x
                        } else if self.modifiers.get().shift() {
                            -*y
                        } else {
                            0.0
                        }
                    }
                };

                if horizontal_delta.abs() > 0.1 {
                    Some(
                        Action::publish(Message::HorizontalWheelScrolled(horizontal_delta))
                            .and_capture(),
                    )
                } else {
                    None
                }
            }
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                cursor.position_in(bounds).map(|position| {
                    // Check for Ctrl (or Command on macOS) + Click
                    #[cfg(target_os = "macos")]
                    let is_jump_click = self.modifiers.get().command();
                    #[cfg(not(target_os = "macos"))]
                    let is_jump_click = self.modifiers.get().control();

                    if is_jump_click {
                        return Action::publish(Message::JumpClick(position));
                    }

                    let now = super::Instant::now();
                    let repeated_click = self.last_click_at.get().is_some_and(|last_click| {
                        now.duration_since(last_click) <= DOUBLE_CLICK_INTERVAL
                            && self
                                .last_click_position
                                .borrow()
                                .is_some_and(|last_position| {
                                    (last_position.x - position.x).abs() <= DOUBLE_CLICK_DISTANCE
                                        && (last_position.y - position.y).abs()
                                            <= DOUBLE_CLICK_DISTANCE
                                })
                    });

                    let click_count = if repeated_click {
                        self.last_click_count.get().saturating_add(1)
                    } else {
                        1
                    };

                    self.last_click_at.set(Some(now));
                    *self.last_click_position.borrow_mut() = Some(position);
                    self.last_click_count.set(click_count);

                    if click_count >= 3 {
                        self.last_click_count.set(0);
                        self.last_click_at.set(None);
                        *self.last_click_position.borrow_mut() = None;
                        return Action::publish(Message::MouseTripleClick(position));
                    }

                    if click_count == 2 {
                        return Action::publish(Message::MouseDoubleClick(position));
                    }

                    // Don't capture the event so it can bubble up for focus management
                    // This implements focus event propagation through the widget hierarchy
                    Action::publish(Message::MouseClick(position))
                })
            }
            mouse::Event::CursorMoved { .. } => {
                cursor.position_in(bounds).map(|position| {
                    if self.is_dragging {
                        // Handle mouse drag for selection only when cursor is within bounds
                        Action::publish(Message::MouseDrag(position)).and_capture()
                    } else {
                        // Forward hover events when not dragging to enable LSP hover.
                        Action::publish(Message::MouseHover(position))
                    }
                })
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                if cursor.is_over(bounds) || self.is_dragging {
                    Some(Action::publish(Message::MouseRelease).and_capture())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Handles IME (Input Method Editor) events for complex text input.
    ///
    /// # Arguments
    ///
    /// * `event` - The IME event to handle
    /// * `bounds` - The rectangle bounds of the canvas widget
    /// * `cursor` - The current mouse cursor position and status
    ///
    /// # Returns
    ///
    /// `Some(Action<Message>)` if the event was handled, `None` otherwise
    fn handle_ime_event(
        &self,
        event: &input_method::Event,
        _bounds: Rectangle,
        _cursor: &mouse::Cursor,
    ) -> Option<Action<Message>> {
        // Early exit: Check if editor has focus and is not focus-locked
        // This prevents focus stealing where IME events meant for other widgets
        // are incorrectly processed by this editor during focus transitions
        if !self.has_focus() || self.focus_locked {
            return None;
        }

        // IME event handling
        // ---------------------------------------------------------------------
        // Core mapping: convert Iced IME events into editor Messages
        //
        // Flow:
        // 1. Opened: IME activated (e.g. switching input method). Clear old preedit state.
        // 2. Preedit: User is composing (e.g. typing "nihao" before commit).
        //    - content: current candidate text
        //    - selection: selection range within the text, in bytes
        // 3. Commit: User confirms a candidate and commits text into the buffer.
        // 4. Closed: IME closed or lost focus.
        //
        // Safety checks:
        // - handle only when `focused_id` matches this editor ID
        // - handle only when `has_canvas_focus` is true
        // This ensures IME events are not delivered to the wrong widget.
        // ---------------------------------------------------------------------
        let message = match event {
            input_method::Event::Opened => Message::ImeOpened,
            input_method::Event::Preedit(content, selection) => {
                Message::ImePreedit(content.clone(), selection.clone())
            }
            input_method::Event::Commit(content) => Message::ImeCommit(content.clone()),
            input_method::Event::Closed => Message::ImeClosed,
        };

        Some(Action::publish(message).and_capture())
    }
}

impl CodeEditor {
    /// Draws underlines for jumpable links when modifier is held.
    fn draw_jump_link_highlight(
        &self,
        frame: &mut canvas::Frame,
        ctx: &RenderContext<'_>,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) {
        #[cfg(target_os = "macos")]
        let modifier_active = self.modifiers.get().command();
        #[cfg(not(target_os = "macos"))]
        let modifier_active = self.modifiers.get().control();

        if !modifier_active {
            return;
        }

        let Some(point) = cursor.position_in(bounds) else {
            return;
        };

        if let Some((line, col)) = self.calculate_cursor_from_point(point) {
            let line_content = self.buffer.line(line);

            let start_col = Self::word_start_in_line(line_content, col);
            let end_col = Self::word_end_in_line(line_content, col);

            if start_col >= end_col {
                return;
            }

            // Find the first visual line for this logical line
            if let Some(mut idx) = WrappingCalculator::logical_to_visual(ctx.visual_lines, line, 0)
            {
                // Iterate all visual lines belonging to this logical line
                while idx < ctx.visual_lines.len() {
                    let visual_line = &ctx.visual_lines[idx];
                    if visual_line.logical_line != line {
                        break;
                    }

                    // Check intersection
                    let seg_start = visual_line.start_col.max(start_col);
                    let seg_end = visual_line.end_col.min(end_col);

                    if seg_start < seg_end {
                        let (x, width) = calculate_segment_geometry(
                            line_content,
                            visual_line.start_col,
                            seg_start,
                            seg_end,
                            ctx.gutter_width + 5.0 - ctx.horizontal_scroll_offset,
                            ctx.full_char_width,
                            ctx.char_width,
                        );

                        let y = idx as f32 * ctx.line_height + ctx.line_height; // Underline at bottom

                        // Draw underline
                        let path = canvas::Path::line(Point::new(x, y), Point::new(x + width, y));

                        frame.stroke(
                            &path,
                            canvas::Stroke::default()
                                .with_color(self.style.text_color) // Use text color or link color
                                .with_width(1.0),
                        );
                    }

                    idx += 1;
                }
            }
        }
    }
}

impl canvas::Program<Message> for CodeEditor {
    type State = ();

    /// Renders the code editor's visual elements on the canvas, including text layout, syntax highlighting,
    /// cursor positioning, and other graphical aspects.
    ///
    /// # Arguments
    ///
    /// * `state` - The current state of the canvas
    /// * `renderer` - The renderer used for drawing
    /// * `theme` - The theme for styling
    /// * `bounds` - The rectangle bounds of the canvas
    /// * `cursor` - The mouse cursor position
    ///
    /// # Returns
    ///
    /// A vector of `Geometry` objects representing the drawn elements
    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let visual_lines: Rc<Vec<VisualLine>> = self.visual_lines_cached(bounds.width);

        // Prefer the tracked viewport height when available, but fall back to
        // the current bounds during initial layout when viewport metrics have
        // not been populated yet.
        let effective_viewport_height = if self.viewport_height > 0.0 {
            self.viewport_height
        } else {
            bounds.height
        };
        let first_visible_line =
            (self.content_viewport_scroll() / self.line_height).floor() as usize;
        let visible_lines_count =
            (effective_viewport_height / self.line_height).ceil() as usize + 2;
        let last_visible_line = (first_visible_line + visible_lines_count).min(visual_lines.len());

        let (start_idx, end_idx) = if self.cache_window_end_line > self.cache_window_start_line {
            let s = self.cache_window_start_line.min(visual_lines.len());
            let e = self.cache_window_end_line.min(visual_lines.len());
            (s, e)
        } else {
            (first_visible_line, last_visible_line)
        };

        // Split rendering into two cached layers:
        // - content: expensive, mostly static text/gutter rendering
        // - overlay: frequently changing highlights/cursor/IME
        //
        // This keeps selection dragging and cursor blinking smooth by avoiding
        // invalidation of the text layer on every overlay update.
        let visual_lines_for_content = visual_lines.clone();
        let content_geometry = self.content_cache.draw(renderer, bounds.size(), |frame| {
            let tree_sitter_document = highlighting::highlight_document(
                self.syntax.as_str(),
                &self.buffer.to_string(),
                &self.style,
            );

            // syntect initialization is relatively expensive; keep it global.
            let syntax_set = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
            let theme_set = THEME_SET.get_or_init(ThemeSet::load_defaults);
            let syntax_theme = theme_set
                .themes
                .get("base16-ocean.dark")
                .or_else(|| theme_set.themes.values().next());

            let syntax_ref = language::find_syntect_syntax(self.syntax.as_str(), syntax_set)
                .or(Some(syntax_set.find_syntax_plain_text()));

            let ctx = RenderContext {
                visual_lines: visual_lines_for_content.as_ref(),
                bounds_width: bounds.width,
                gutter_width: self.gutter_width(),
                line_height: self.line_height,
                font_size: self.font_size,
                full_char_width: self.full_char_width,
                char_width: self.char_width,
                font: self.font,
                horizontal_scroll_offset: self.horizontal_scroll_offset,
            };

            // Clip code text to the code area (right of gutter) so that
            // horizontal scrolling cannot cause text to bleed into the gutter.
            // Note: iced renders ALL text on top of ALL geometry, so a
            // fill_rectangle cannot mask text bleed — with_clip is required.
            let code_clip = Rectangle {
                x: ctx.gutter_width,
                y: 0.0,
                width: (bounds.width - ctx.gutter_width).max(0.0),
                height: bounds.height,
            };
            frame.with_clip(code_clip, |f| {
                for (idx, visual_line) in visual_lines_for_content
                    .iter()
                    .enumerate()
                    .skip(start_idx)
                    .take(end_idx.saturating_sub(start_idx))
                {
                    let y = idx as f32 * self.line_height;
                    self.draw_text_with_syntax_highlighting(
                        f,
                        &ctx,
                        visual_line,
                        y,
                        tree_sitter_document.as_ref(),
                        syntax_ref,
                        syntax_set,
                        syntax_theme,
                    );
                }
            });

            // Draw line numbers in the gutter (no clip — fixed position)
            for (idx, visual_line) in visual_lines_for_content
                .iter()
                .enumerate()
                .skip(start_idx)
                .take(end_idx.saturating_sub(start_idx))
            {
                let y = idx as f32 * self.line_height;
                self.draw_line_numbers(frame, &ctx, visual_line, y);
            }
        });

        let visual_lines_for_overlay = visual_lines;
        let overlay_geometry = self.overlay_cache.draw(renderer, bounds.size(), |frame| {
            // The overlay layer shares the same visual lines, but draws only
            // elements that change without modifying the buffer content.
            let ctx = RenderContext {
                visual_lines: visual_lines_for_overlay.as_ref(),
                bounds_width: bounds.width,
                gutter_width: self.gutter_width(),
                line_height: self.line_height,
                font_size: self.font_size,
                full_char_width: self.full_char_width,
                char_width: self.char_width,
                font: self.font,
                horizontal_scroll_offset: self.horizontal_scroll_offset,
            };
            let code_clip = Rectangle {
                x: ctx.gutter_width,
                y: 0.0,
                width: (bounds.width - ctx.gutter_width).max(0.0),
                height: bounds.height,
            };

            for (idx, visual_line) in visual_lines_for_overlay
                .iter()
                .enumerate()
                .skip(start_idx)
                .take(end_idx.saturating_sub(start_idx))
            {
                let y = idx as f32 * self.line_height;
                self.draw_active_line_number(frame, &ctx, visual_line, y);
            }

            frame.with_clip(code_clip, |f| {
                for (idx, visual_line) in visual_lines_for_overlay
                    .iter()
                    .enumerate()
                    .skip(start_idx)
                    .take(end_idx.saturating_sub(start_idx))
                {
                    let y = idx as f32 * self.line_height;
                    self.draw_current_line_highlight(f, &ctx, visual_line, y);
                }

                self.draw_search_highlights(f, &ctx, start_idx, end_idx);
                self.draw_matching_bracket_highlight(f, &ctx);
                self.draw_selection_highlight(f, &ctx);
                self.draw_jump_link_highlight(f, &ctx, bounds, _cursor);
                self.draw_cursor(f, &ctx);
            });
        });

        vec![content_geometry, overlay_geometry]
    }

    /// Handles Canvas trait events, specifically keyboard input events and focus management for the code editor widget.
    ///
    /// # Arguments
    ///
    /// * `_state` - The mutable state of the canvas (unused in this implementation)
    /// * `event` - The input event to handle, such as keyboard presses
    /// * `bounds` - The rectangle bounds of the canvas widget
    /// * `cursor` - The current mouse cursor position and status
    ///
    /// # Returns
    ///
    /// An optional `Action<Message>` to perform, such as sending a message or redrawing the canvas
    fn update(
        &self,
        _state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                self.modifiers.set(*modifiers);
                None
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                text,
                ..
            }) => {
                self.modifiers.set(*modifiers);
                self.handle_keyboard_event(key, modifiers, text, bounds, &cursor)
            }
            Event::Keyboard(keyboard::Event::KeyReleased { modifiers, .. }) => {
                self.modifiers.set(*modifiers);
                None
            }
            Event::Mouse(mouse_event) => self.handle_mouse_event(mouse_event, bounds, &cursor),
            Event::InputMethod(ime_event) => self.handle_ime_event(ime_event, bounds, &cursor),
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

/// Validates that the selection indices fall on valid UTF-8 character boundaries
/// to prevent panics during string slicing.
///
/// # Arguments
///
/// * `content` - The string content to check against
/// * `start` - The start byte index
/// * `end` - The end byte index
///
/// # Returns
///
/// `Some((start, end))` if indices are valid, `None` otherwise.
fn validate_selection_indices(content: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let len = content.len();
    // Clamp indices to content length
    let start = start.min(len);
    let end = end.min(len);

    // Ensure start is not greater than end
    if start > end {
        return None;
    }

    // Verify that indices fall on valid UTF-8 character boundaries
    if content.is_char_boundary(start) && content.is_char_boundary(end) {
        Some((start, end))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas_editor::{CHAR_WIDTH, FONT_SIZE, compare_floats};
    use std::cmp::Ordering;

    #[test]
    fn test_calculate_segment_geometry_ascii() {
        // "Hello World"
        // "Hello " (6 chars) -> prefix
        // "World" (5 chars) -> segment
        // width("Hello ") = 6 * CHAR_WIDTH
        // width("World") = 5 * CHAR_WIDTH
        let content = "Hello World";
        let (x, w) = calculate_segment_geometry(content, 0, 6, 11, 0.0, FONT_SIZE, CHAR_WIDTH);

        let expected_x = CHAR_WIDTH * 6.0;
        let expected_w = CHAR_WIDTH * 5.0;

        assert_eq!(
            compare_floats(x, expected_x),
            Ordering::Equal,
            "X position mismatch for ASCII"
        );
        assert_eq!(
            compare_floats(w, expected_w),
            Ordering::Equal,
            "Width mismatch for ASCII"
        );
    }

    #[test]
    fn test_calculate_segment_geometry_cjk() {
        // "你好世界"
        // "你好" (2 chars) -> prefix
        // "世界" (2 chars) -> segment
        // width("你好") = 2 * FONT_SIZE
        // width("世界") = 2 * FONT_SIZE
        let content = "你好世界";
        let (x, w) = calculate_segment_geometry(content, 0, 2, 4, 10.0, FONT_SIZE, CHAR_WIDTH);

        let expected_x = 10.0 + FONT_SIZE * 2.0;
        let expected_w = FONT_SIZE * 2.0;

        assert_eq!(
            compare_floats(x, expected_x),
            Ordering::Equal,
            "X position mismatch for CJK"
        );
        assert_eq!(
            compare_floats(w, expected_w),
            Ordering::Equal,
            "Width mismatch for CJK"
        );
    }

    #[test]
    fn test_calculate_segment_geometry_mixed() {
        // "Hi你好"
        // "Hi" (2 chars) -> prefix
        // "你好" (2 chars) -> segment
        // width("Hi") = 2 * CHAR_WIDTH
        // width("你好") = 2 * FONT_SIZE
        let content = "Hi你好";
        let (x, w) = calculate_segment_geometry(content, 0, 2, 4, 0.0, FONT_SIZE, CHAR_WIDTH);

        let expected_x = CHAR_WIDTH * 2.0;
        let expected_w = FONT_SIZE * 2.0;

        assert_eq!(
            compare_floats(x, expected_x),
            Ordering::Equal,
            "X position mismatch for mixed content"
        );
        assert_eq!(
            compare_floats(w, expected_w),
            Ordering::Equal,
            "Width mismatch for mixed content"
        );
    }

    #[test]
    fn test_calculate_segment_geometry_empty_range() {
        let content = "Hello";
        let (x, w) = calculate_segment_geometry(content, 0, 0, 0, 0.0, FONT_SIZE, CHAR_WIDTH);
        assert!((x - 0.0).abs() < f32::EPSILON);
        assert!((w - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_calculate_segment_geometry_with_visual_offset() {
        // content: "0123456789"
        // visual_start_col: 2 (starts at '2')
        // segment: "34" (indices 3 to 5)
        // prefix: from visual start (2) to segment start (3) -> "2" (length 1)
        // prefix width: 1 * CHAR_WIDTH
        // segment width: 2 * CHAR_WIDTH
        let content = "0123456789";
        let (x, w) = calculate_segment_geometry(content, 2, 3, 5, 5.0, FONT_SIZE, CHAR_WIDTH);

        let expected_x = 5.0 + CHAR_WIDTH * 1.0;
        let expected_w = CHAR_WIDTH * 2.0;

        assert_eq!(
            compare_floats(x, expected_x),
            Ordering::Equal,
            "X position mismatch with visual offset"
        );
        assert_eq!(
            compare_floats(w, expected_w),
            Ordering::Equal,
            "Width mismatch with visual offset"
        );
    }

    #[test]
    fn test_calculate_segment_geometry_out_of_bounds() {
        // Content length is 5 ("Hello")
        // Request start at 10, end at 15
        // visual_start 0
        // Prefix should consume whole string ("Hello") and stop.
        // Segment should be empty.
        let content = "Hello";
        let (x, w) = calculate_segment_geometry(content, 0, 10, 15, 0.0, FONT_SIZE, CHAR_WIDTH);

        let expected_x = CHAR_WIDTH * 5.0; // Width of "Hello"
        let expected_w = 0.0;

        assert_eq!(
            compare_floats(x, expected_x),
            Ordering::Equal,
            "X position mismatch for out of bounds start"
        );
        assert!(
            (w - expected_w).abs() < f32::EPSILON,
            "Width should be 0 for out of bounds segment"
        );
    }

    #[test]
    fn test_calculate_segment_geometry_special_chars() {
        // Emoji "👋" (width > 1 => FONT_SIZE)
        // Tab "\t" (width = 4 * CHAR_WIDTH)
        let content = "A👋\tB";
        // Measure "👋" (index 1 to 2)
        // Indices in chars: 'A' (0), '👋' (1), '\t' (2), 'B' (3)

        // Segment covering Emoji
        let (x, w) = calculate_segment_geometry(content, 0, 1, 2, 0.0, FONT_SIZE, CHAR_WIDTH);
        let expected_x_emoji = CHAR_WIDTH; // 'A'
        let expected_w_emoji = FONT_SIZE; // '👋'

        assert_eq!(
            compare_floats(x, expected_x_emoji),
            Ordering::Equal,
            "X pos for emoji"
        );
        assert_eq!(
            compare_floats(w, expected_w_emoji),
            Ordering::Equal,
            "Width for emoji"
        );

        // Segment covering Tab
        let (x_tab, w_tab) =
            calculate_segment_geometry(content, 0, 2, 3, 0.0, FONT_SIZE, CHAR_WIDTH);
        let expected_x_tab = CHAR_WIDTH + FONT_SIZE; // 'A' + '👋'
        let expected_w_tab = CHAR_WIDTH * crate::canvas_editor::TAB_WIDTH as f32;

        assert_eq!(
            compare_floats(x_tab, expected_x_tab),
            Ordering::Equal,
            "X pos for tab"
        );
        assert_eq!(
            compare_floats(w_tab, expected_w_tab),
            Ordering::Equal,
            "Width for tab"
        );
    }

    #[test]
    fn test_calculate_segment_geometry_inverted_range() {
        // Start 5, End 3
        // Should result in empty segment at start 5
        let content = "0123456789";
        let (x, w) = calculate_segment_geometry(content, 0, 5, 3, 0.0, FONT_SIZE, CHAR_WIDTH);

        let expected_x = CHAR_WIDTH * 5.0;
        let expected_w = 0.0;

        assert_eq!(
            compare_floats(x, expected_x),
            Ordering::Equal,
            "X pos for inverted range"
        );
        assert!(
            (w - expected_w).abs() < f32::EPSILON,
            "Width for inverted range"
        );
    }

    #[test]
    fn test_validate_selection_indices() {
        // Test valid ASCII indices
        let content = "Hello";
        assert_eq!(validate_selection_indices(content, 0, 5), Some((0, 5)));
        assert_eq!(validate_selection_indices(content, 1, 3), Some((1, 3)));

        // Test valid multi-byte indices (Chinese "你好")
        // "你" is 3 bytes (0-3), "好" is 3 bytes (3-6)
        let content = "你好";
        assert_eq!(validate_selection_indices(content, 0, 6), Some((0, 6)));
        assert_eq!(validate_selection_indices(content, 0, 3), Some((0, 3)));
        assert_eq!(validate_selection_indices(content, 3, 6), Some((3, 6)));

        // Test invalid indices (splitting multi-byte char)
        assert_eq!(validate_selection_indices(content, 1, 3), None); // Split first char
        assert_eq!(validate_selection_indices(content, 0, 4), None); // Split second char

        // Test out of bounds (should be clamped if on boundary, but here len is 6)
        // If we pass start=0, end=100 -> clamped to 0, 6. 6 is boundary.
        assert_eq!(validate_selection_indices(content, 0, 100), Some((0, 6)));

        // Test inverted range
        assert_eq!(validate_selection_indices(content, 3, 0), None);
    }
}
