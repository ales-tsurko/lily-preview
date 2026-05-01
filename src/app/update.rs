use std::fs;
use std::path::Path;
use std::sync::mpsc::TryRecvError;

use iced::widget::{pane_grid, svg};
use iced_core::{Bytes, image};
use notify::event::EventKind;
use resvg::tiny_skia;
use resvg::usvg;

use super::messages::{KeyPress, MixerMessage};
use super::score_cursor;
use super::*;
use crate::error_prompt::{ErrorFatality, ErrorPrompt, PromptButtons};
use crate::midi;
use crate::settings::{DockGroupSettings, DockNodeSettings};
use crate::shortcuts::{self, ShortcutAction, ShortcutInput};
use crate::state::{self, GlobalState, ProjectState};

const DRAG_START_THRESHOLD: f32 = 8.0;
const SCORE_PREVIEW_FALLBACK_MAX_DIMENSION: f32 = 2200.0;
const SCORE_PREVIEW_PRIMARY_MAX_DIMENSION: f32 = 3600.0;
const SCORE_PREVIEW_FALLBACK_MIN_ZOOM: f32 = 1.0;
const SCORE_PREVIEW_PRIMARY_MIN_ZOOM: f32 = 1.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum TabDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum PaneCycleDirection {
    Previous,
    Next,
}

mod editor;
mod files;
mod input;
mod mixer;
mod pane;
mod persistence;
mod piano_roll;
mod playback;
mod score;
mod track_names;
mod track_selection;

pub(super) fn update(app: &mut Lilypalooza, message: Message) -> Task<Message> {
    if app.renaming_target.is_some() && should_commit_track_rename_before_message(&message) {
        app.apply_pending_track_rename();
    }

    match message {
        Message::Noop => Task::none(),
        Message::StartupChecked(result) => app.handle_startup_checked(result),
        Message::BrowserHistoryCleanupFinished(result) => {
            if let Err(error) = result {
                app.logger
                    .push(format!("Browser history cleanup failed: {error}"));
            }
            Task::none()
        }
        Message::PluginScanCacheSaved(result) => {
            if let Err(error) = result {
                app.logger.push(error);
            }
            Task::none()
        }
        Message::Pane(message) => app.handle_pane_message(message),
        Message::File(message) => app.handle_file_message(message),
        Message::Viewer(message) => app.handle_viewer_message(message),
        Message::ScorePreviewReady(result) => app.handle_score_preview_ready(result),
        Message::CompileOutputsReady(result) => app.handle_compile_outputs_ready(result),
        Message::PianoRoll(message) => app.handle_piano_roll_message(message),
        Message::Mixer(message) => app.handle_mixer_message(message),
        Message::Editor(message) => app.handle_editor_message(message),
        Message::Logger(message) => app.handle_logger_message(message),
        Message::Shortcuts(message) => app.handle_shortcuts_message(message),
        Message::Prompt(message) => app.handle_prompt_message(message),
        Message::WindowOpened(window_id) => app.handle_window_opened(window_id),
        Message::WindowClosed(window_id) => app.handle_window_closed(window_id),
        Message::WindowFocused(window_id) => app.handle_processor_editor_focused(window_id),
        Message::WindowSnapshotCaptured {
            window_id,
            host,
            parent,
        } => app.handle_processor_editor_attached(window_id, host, parent),
        Message::KeyPressed(key_press) => app.handle_key_pressed(key_press),
        Message::TrackRenameFocusChanged(focused) => app.handle_track_rename_focus_changed(focused),
        Message::ModifiersChanged(modifiers) => app.handle_modifiers_changed(modifiers),
        Message::PrimaryMousePressed(pressed) => app.handle_primary_mouse_pressed(pressed),
        Message::Tick => app.handle_tick(),
        Message::Frame(now) => app.handle_frame(now),
        Message::WindowResized { window_id, size } => app.handle_window_resized(window_id, size),
        Message::WindowCloseRequested(window_id) => app.handle_window_close_requested(window_id),
    }
}

fn should_commit_track_rename_before_message(message: &Message) -> bool {
    !matches!(
        message,
        Message::Noop
            | Message::StartupChecked(_)
            | Message::BrowserHistoryCleanupFinished(_)
            | Message::PluginScanCacheSaved(_)
            | Message::ScorePreviewReady(_)
            | Message::CompileOutputsReady(_)
            | Message::Pane(_)
            | Message::Viewer(
                ViewerMessage::ScrollPositionChanged { .. }
                    | ViewerMessage::ViewportCursorMoved(_)
                    | ViewerMessage::ViewportCursorLeft
            )
            | Message::KeyPressed(_)
            | Message::TrackRenameFocusChanged(_)
            | Message::ModifiersChanged(_)
            | Message::PrimaryMousePressed(_)
            | Message::Tick
            | Message::Frame(_)
            | Message::WindowOpened(_)
            | Message::WindowClosed(_)
            | Message::WindowFocused(_)
            | Message::WindowResized { .. }
            | Message::WindowCloseRequested(_)
            | Message::PianoRoll(
                PianoRollMessage::StartTrackRename(_)
                    | PianoRollMessage::OpenTrackColorPickerForTrack(_)
                    | PianoRollMessage::TrackRenameInputChanged(_)
                    | PianoRollMessage::OpenTrackColorPicker
                    | PianoRollMessage::SubmitTrackColor(_)
                    | PianoRollMessage::PreviewTrackColor(_)
                    | PianoRollMessage::CommitTrackRename
                    | PianoRollMessage::CancelTrackRename
                    | PianoRollMessage::ViewportCursorMoved(_)
                    | PianoRollMessage::ViewportCursorLeft
                    | PianoRollMessage::RollScrolled { .. }
            )
            | Message::Mixer(
                MixerMessage::StartTrackRename(_)
                    | MixerMessage::StartBusRename(_)
                    | MixerMessage::TrackRenameInputChanged(_)
                    | MixerMessage::OpenTrackColorPicker
                    | MixerMessage::SubmitTrackColor(_)
                    | MixerMessage::PreviewTrackColor(_)
                    | MixerMessage::CommitTrackRename
                    | MixerMessage::CancelTrackRename
                    | MixerMessage::InstrumentViewportScrolled(_)
                    | MixerMessage::BusViewportScrolled(_)
            )
    )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{
        parse_svg_coordinate_size_from_source, parse_svg_size_from_source,
        should_commit_track_rename_before_message,
    };
    use crate::app::WorkspacePaneKind;
    use crate::app::messages::{Message, MixerMessage, PaneMessage, PianoRollMessage};

    #[test]
    fn track_rename_commit_filter_keeps_editing_messages() {
        assert!(!should_commit_track_rename_before_message(
            &Message::PianoRoll(PianoRollMessage::TrackRenameInputChanged("x".into()),)
        ));
        assert!(!should_commit_track_rename_before_message(&Message::Mixer(
            MixerMessage::CommitTrackRename,
        )));
    }

    #[test]
    fn track_rename_commit_filter_commits_on_unrelated_actions() {
        assert!(should_commit_track_rename_before_message(
            &Message::PianoRoll(PianoRollMessage::TrackMuteToggled(0),)
        ));
        assert!(should_commit_track_rename_before_message(&Message::Mixer(
            MixerMessage::ToggleTrackSolo(0),
        )));
    }

    #[test]
    fn track_rename_commit_filter_ignores_passive_messages() {
        assert!(!should_commit_track_rename_before_message(&Message::Pane(
            PaneMessage::FocusWorkspacePane(WorkspacePaneKind::Mixer),
        )));
        assert!(!should_commit_track_rename_before_message(
            &Message::PianoRoll(PianoRollMessage::ViewportCursorMoved(iced::Point::ORIGIN),)
        ));
    }

    #[test]
    fn svg_size_parser_normalizes_old_and_cairo_a4_outputs() {
        let old_backend = r#"<svg width="210.00mm" height="297.00mm" viewBox="0.0000 -0.0000 119.5016 169.0094"></svg>"#;
        let cairo_backend = r#"<svg width="596" height="842" viewBox="0 0 596 842"></svg>"#;

        let old_size = parse_svg_size_from_source(old_backend).expect("old backend size");
        let cairo_size = parse_svg_size_from_source(cairo_backend).expect("cairo backend size");

        assert!((old_size.width - 595.2756).abs() < 0.001);
        assert!((old_size.height - 841.8898).abs() < 0.001);
        assert!((cairo_size.width - 596.0).abs() < 0.001);
        assert!((cairo_size.height - 842.0).abs() < 0.001);
    }

    #[test]
    fn old_and_cairo_svg_sizes_render_at_same_zoom_scale() {
        let old_backend = r#"<svg width="210.00mm" height="297.00mm" viewBox="0.0000 -0.0000 119.5016 169.0094"></svg>"#;
        let cairo_backend = r#"<svg width="596" height="842" viewBox="0 0 596 842"></svg>"#;

        let old_size = parse_svg_size_from_source(old_backend).expect("old backend size");
        let cairo_size = parse_svg_size_from_source(cairo_backend).expect("cairo backend size");
        let scale = super::score_view::score_base_scale();

        let old_width = old_size.width * scale;
        let cairo_width = cairo_size.width * scale;

        assert!((old_width - cairo_width).abs() < 1.0);
    }

    #[test]
    fn svg_coordinate_size_prefers_viewbox_for_interaction_mapping() {
        let old_backend = r#"<svg width="210.00mm" height="297.00mm" viewBox="0.0000 -0.0000 119.5016 169.0094"></svg>"#;

        let size = parse_svg_coordinate_size_from_source(old_backend).expect("coord size");

        assert!((size.width - 119.5016).abs() < 0.001);
        assert!((size.height - 169.0094).abs() < 0.001);
    }

    #[test]
    fn svg_coordinate_size_falls_back_to_display_size_without_viewbox() {
        let svg = r#"<svg width="596" height="842"></svg>"#;

        let size = parse_svg_coordinate_size_from_source(svg).expect("coord size");

        assert!((size.width - 596.0).abs() < 0.001);
        assert!((size.height - 842.0).abs() < 0.001);
    }

    #[test]
    fn display_to_svg_coordinate_mapping_preserves_old_backend_click_space() {
        let display_width = 595.2756_f32;
        let coord_width = 119.5016_f32;
        let display_x = 250.0_f32;

        let svg_x = display_x * coord_width / display_width;

        assert!((svg_x - 50.1875).abs() < 0.001);
    }
}

fn dock_node_to_settings(
    node: &DockNode,
    groups: &std::collections::HashMap<DockGroupId, DockGroup>,
) -> DockNodeSettings {
    match node {
        DockNode::Group(group_id) => DockNodeSettings::Group(
            groups
                .get(group_id)
                .map(|group| DockGroupSettings {
                    tabs: group.tabs.clone(),
                    active: group.active,
                })
                .unwrap_or_default(),
        ),
        DockNode::Split {
            axis,
            ratio,
            first,
            second,
        } => DockNodeSettings::Split {
            axis: dock_axis_to_settings(*axis),
            ratio: *ratio,
            first: Box::new(dock_node_to_settings(first, groups)),
            second: Box::new(dock_node_to_settings(second, groups)),
        },
    }
}

fn collect_workspace_group_bounds(
    state: &pane_grid::State<DockGroupId>,
    node: &pane_grid::Node,
    bounds: iced::Rectangle,
    group_bounds: &mut std::collections::HashMap<DockGroupId, iced::Rectangle>,
) {
    match node {
        pane_grid::Node::Pane(pane) => {
            if let Some(group_id) = state.get(*pane) {
                group_bounds.insert(*group_id, bounds);
            }
        }
        pane_grid::Node::Split {
            axis, ratio, a, b, ..
        } => match axis {
            pane_grid::Axis::Horizontal => {
                let first_height = bounds.height * ratio;
                collect_workspace_group_bounds(
                    state,
                    a,
                    iced::Rectangle {
                        height: first_height,
                        ..bounds
                    },
                    group_bounds,
                );
                collect_workspace_group_bounds(
                    state,
                    b,
                    iced::Rectangle {
                        y: bounds.y + first_height,
                        height: bounds.height - first_height,
                        ..bounds
                    },
                    group_bounds,
                );
            }
            pane_grid::Axis::Vertical => {
                let first_width = bounds.width * ratio;
                collect_workspace_group_bounds(
                    state,
                    a,
                    iced::Rectangle {
                        width: first_width,
                        ..bounds
                    },
                    group_bounds,
                );
                collect_workspace_group_bounds(
                    state,
                    b,
                    iced::Rectangle {
                        x: bounds.x + first_width,
                        width: bounds.width - first_width,
                        ..bounds
                    },
                    group_bounds,
                );
            }
        },
    }
}

fn collect_visible_group_order(node: &DockNode, group_ids: &mut Vec<DockGroupId>) {
    match node {
        DockNode::Group(group_id) => group_ids.push(*group_id),
        DockNode::Split { first, second, .. } => {
            collect_visible_group_order(first, group_ids);
            collect_visible_group_order(second, group_ids);
        }
    }
}

fn split_children(
    node: &pane_grid::Node,
    split: pane_grid::Split,
) -> Option<(&pane_grid::Node, &pane_grid::Node)> {
    match node {
        pane_grid::Node::Pane(_) => None,
        pane_grid::Node::Split { id, a, b, .. } => {
            if *id == split {
                Some((a.as_ref(), b.as_ref()))
            } else {
                split_children(a, split).or_else(|| split_children(b, split))
            }
        }
    }
}

fn dock_node_min_width(
    node: &pane_grid::Node,
    state: &pane_grid::State<DockGroupId>,
    app: &Lilypalooza,
) -> f32 {
    match node {
        pane_grid::Node::Pane(pane) => state
            .get(*pane)
            .map(|group_id| super::dock_view::workspace_group_min_width(app, *group_id))
            .unwrap_or(0.0),
        pane_grid::Node::Split { axis, a, b, .. } => {
            let first = dock_node_min_width(a, state, app);
            let second = dock_node_min_width(b, state, app);

            match axis {
                pane_grid::Axis::Horizontal => first.max(second),
                pane_grid::Axis::Vertical => first + second,
            }
        }
    }
}

fn dock_node_min_height(
    node: &pane_grid::Node,
    state: &pane_grid::State<DockGroupId>,
    app: &Lilypalooza,
) -> f32 {
    match node {
        pane_grid::Node::Pane(pane) => state
            .get(*pane)
            .map(|group_id| super::dock_view::workspace_group_min_height(app, *group_id))
            .unwrap_or(0.0),
        pane_grid::Node::Split { axis, a, b, .. } => {
            let first = dock_node_min_height(a, state, app);
            let second = dock_node_min_height(b, state, app);

            match axis {
                pane_grid::Axis::Horizontal => first + second,
                pane_grid::Axis::Vertical => first.max(second),
            }
        }
    }
}

fn dock_drop_region(bounds: iced::Rectangle, position: iced::Point) -> DockDropRegion {
    let relative_x = ((position.x - bounds.x) / bounds.width.max(1.0)).clamp(0.0, 1.0);
    let relative_y = ((position.y - bounds.y) / bounds.height.max(1.0)).clamp(0.0, 1.0);
    let center_min = 1.0 / 3.0;
    let center_max = 2.0 / 3.0;

    if (center_min..=center_max).contains(&relative_x)
        && (center_min..=center_max).contains(&relative_y)
    {
        return DockDropRegion::Center;
    }

    let top_distance = relative_y;
    let right_distance = 1.0 - relative_x;
    let bottom_distance = 1.0 - relative_y;
    let left_distance = relative_x;
    let mut closest = (DockDropRegion::Top, top_distance);

    for candidate in [
        (DockDropRegion::Right, right_distance),
        (DockDropRegion::Bottom, bottom_distance),
        (DockDropRegion::Left, left_distance),
    ] {
        if candidate.1 < closest.1 {
            closest = candidate;
        }
    }

    closest.0
}

fn move_tab_to_front(tabs: &mut Vec<WorkspacePaneKind>, pane: WorkspacePaneKind) {
    if let Some(index) = tabs.iter().position(|candidate| *candidate == pane) {
        let pane = tabs.remove(index);
        tabs.insert(0, pane);
    }
}

fn drag_distance(a: iced::Point, b: iced::Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    (dx * dx + dy * dy).sqrt()
}

fn remove_pane_from_group(
    groups: &mut std::collections::HashMap<DockGroupId, DockGroup>,
    group_id: DockGroupId,
    pane: WorkspacePaneKind,
) -> bool {
    let Some(group) = groups.get_mut(&group_id) else {
        return false;
    };

    group.tabs.retain(|candidate| *candidate != pane);

    if group.active == pane {
        group.active = group
            .tabs
            .first()
            .copied()
            .unwrap_or(WorkspacePaneKind::Score);
    }

    group.tabs.is_empty()
}

fn prune_group_from_layout(layout: DockNode, group_id: DockGroupId) -> DockNode {
    prune_group_from_layout_inner(layout, group_id).unwrap_or(DockNode::Group(group_id))
}

fn prune_group_from_layout_inner(layout: DockNode, group_id: DockGroupId) -> Option<DockNode> {
    match layout {
        DockNode::Group(candidate) => (candidate != group_id).then_some(DockNode::Group(candidate)),
        DockNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first = prune_group_from_layout_inner(*first, group_id);
            let second = prune_group_from_layout_inner(*second, group_id);

            match (first, second) {
                (Some(first), Some(second)) => Some(DockNode::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            }
        }
    }
}

fn replace_group_with_split(
    node: &mut DockNode,
    target_group_id: DockGroupId,
    axis: pane_grid::Axis,
    ratio: f32,
    new_group_id: DockGroupId,
    insert_first: bool,
) -> bool {
    match node {
        DockNode::Group(group_id) if *group_id == target_group_id => {
            let existing_group = DockNode::Group(*group_id);
            let new_group = DockNode::Group(new_group_id);
            *node = DockNode::Split {
                axis,
                ratio,
                first: Box::new(if insert_first {
                    new_group.clone()
                } else {
                    existing_group.clone()
                }),
                second: Box::new(if insert_first {
                    existing_group
                } else {
                    new_group
                }),
            };
            true
        }
        DockNode::Group(_) => false,
        DockNode::Split { first, second, .. } => {
            replace_group_with_split(
                first,
                target_group_id,
                axis,
                ratio,
                new_group_id,
                insert_first,
            ) || replace_group_with_split(
                second,
                target_group_id,
                axis,
                ratio,
                new_group_id,
                insert_first,
            )
        }
    }
}

fn split_restore_target_for_group(
    node: &DockNode,
    group_id: DockGroupId,
    groups: &std::collections::HashMap<DockGroupId, DockGroup>,
) -> Option<(
    pane_grid::Axis,
    f32,
    bool,
    WorkspacePaneKind,
    Vec<WorkspacePaneKind>,
)> {
    match node {
        DockNode::Group(_) => None,
        DockNode::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            if contains_group(first, group_id) {
                if let Some(target) = split_restore_target_for_group(first, group_id, groups) {
                    return Some(target);
                }

                let sibling_panes = panes_in_node(second, groups);
                Some((
                    *axis,
                    *ratio,
                    true,
                    first_pane_in_node(second, groups)?,
                    sibling_panes,
                ))
            } else if contains_group(second, group_id) {
                if let Some(target) = split_restore_target_for_group(second, group_id, groups) {
                    return Some(target);
                }

                let sibling_panes = panes_in_node(first, groups);
                Some((
                    *axis,
                    *ratio,
                    false,
                    first_pane_in_node(first, groups)?,
                    sibling_panes,
                ))
            } else {
                None
            }
        }
    }
}

fn first_pane_in_node(
    node: &DockNode,
    groups: &std::collections::HashMap<DockGroupId, DockGroup>,
) -> Option<WorkspacePaneKind> {
    match node {
        DockNode::Group(group_id) => groups
            .get(group_id)
            .and_then(|group| group.tabs.first().copied()),
        DockNode::Split { first, second, .. } => {
            first_pane_in_node(first, groups).or_else(|| first_pane_in_node(second, groups))
        }
    }
}

fn panes_in_node(
    node: &DockNode,
    groups: &std::collections::HashMap<DockGroupId, DockGroup>,
) -> Vec<WorkspacePaneKind> {
    let mut panes = Vec::new();
    collect_panes_in_node(node, groups, &mut panes);
    panes.sort_by_key(|pane| pane_sort_key(*pane));
    panes.dedup();
    panes
}

fn collect_panes_in_node(
    node: &DockNode,
    groups: &std::collections::HashMap<DockGroupId, DockGroup>,
    panes: &mut Vec<WorkspacePaneKind>,
) {
    match node {
        DockNode::Group(group_id) => {
            if let Some(group) = groups.get(group_id) {
                panes.extend(group.tabs.iter().copied());
            }
        }
        DockNode::Split { first, second, .. } => {
            collect_panes_in_node(first, groups, panes);
            collect_panes_in_node(second, groups, panes);
        }
    }
}

fn replace_subtree_with_split(
    node: &mut DockNode,
    axis: pane_grid::Axis,
    ratio: f32,
    new_group_id: DockGroupId,
    insert_first: bool,
    target_panes: &[WorkspacePaneKind],
    groups: &std::collections::HashMap<DockGroupId, DockGroup>,
) -> bool {
    if panes_in_node(node, groups) == target_panes {
        let existing = node.clone();
        let new_group = DockNode::Group(new_group_id);
        *node = DockNode::Split {
            axis,
            ratio,
            first: Box::new(if insert_first {
                new_group.clone()
            } else {
                existing.clone()
            }),
            second: Box::new(if insert_first { existing } else { new_group }),
        };
        return true;
    }

    match node {
        DockNode::Group(_) => false,
        DockNode::Split { first, second, .. } => {
            replace_subtree_with_split(
                first,
                axis,
                ratio,
                new_group_id,
                insert_first,
                target_panes,
                groups,
            ) || replace_subtree_with_split(
                second,
                axis,
                ratio,
                new_group_id,
                insert_first,
                target_panes,
                groups,
            )
        }
    }
}

fn pane_sort_key(pane: WorkspacePaneKind) -> u8 {
    match pane {
        WorkspacePaneKind::Score => 0,
        WorkspacePaneKind::PianoRoll => 1,
        WorkspacePaneKind::Mixer => 2,
        WorkspacePaneKind::Editor => 3,
        WorkspacePaneKind::Logger => 4,
    }
}

fn first_group_id_in_layout(node: &DockNode) -> Option<DockGroupId> {
    match node {
        DockNode::Group(group_id) => Some(*group_id),
        DockNode::Split { first, second, .. } => {
            first_group_id_in_layout(first).or_else(|| first_group_id_in_layout(second))
        }
    }
}

fn snap_zoom_to_step(value: f32, step: f32) -> f32 {
    if step <= f32::EPSILON {
        return value;
    }

    (value / step).round() * step
}

fn next_zoom_step_up(current: f32, step: f32, max_zoom: f32) -> f32 {
    let snapped = snap_zoom_to_step(current, step);

    if (current - snapped).abs() <= 1e-4 {
        (snapped + step).clamp(MIN_SVG_ZOOM, max_zoom)
    } else if current < snapped {
        snapped.clamp(MIN_SVG_ZOOM, max_zoom)
    } else {
        (snapped + step).clamp(MIN_SVG_ZOOM, max_zoom)
    }
}

fn next_zoom_step_down(current: f32, step: f32, min_zoom: f32) -> f32 {
    let snapped = snap_zoom_to_step(current, step);

    if (current - snapped).abs() <= 1e-4 {
        (snapped - step).clamp(min_zoom, MAX_SVG_ZOOM)
    } else if current > snapped {
        snapped.clamp(min_zoom, MAX_SVG_ZOOM)
    } else {
        (snapped - step).clamp(min_zoom, MAX_SVG_ZOOM)
    }
}

fn smooth_zoom(
    current_zoom: f32,
    delta: iced::mouse::ScrollDelta,
    min_zoom: f32,
    max_zoom: f32,
) -> f32 {
    let intensity = match delta {
        iced::mouse::ScrollDelta::Lines { y, .. } => y * 0.14,
        iced::mouse::ScrollDelta::Pixels { y, .. } => y * 0.0035,
    };

    (current_zoom * intensity.exp()).clamp(min_zoom, max_zoom)
}

fn anchored_scroll(current_scroll: f32, cursor_in_viewport: f32, scale: f32) -> f32 {
    ((current_scroll + cursor_in_viewport) * scale - cursor_in_viewport).max(0.0)
}

fn score_preview_target_zoom(zoom: f32, tier: ScoreZoomPreviewTier) -> f32 {
    match tier {
        ScoreZoomPreviewTier::Fallback => zoom.max(SCORE_PREVIEW_FALLBACK_MIN_ZOOM),
        ScoreZoomPreviewTier::Primary => zoom.max(SCORE_PREVIEW_PRIMARY_MIN_ZOOM),
    }
}

fn render_score_zoom_preview(
    svg_bytes: Bytes,
    page_size: SvgSize,
    request: ScoreZoomPreviewRequest,
) -> Result<super::messages::ScorePreviewReady, String> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes.as_ref(), &options)
        .map_err(|error| format!("Failed to parse score SVG: {error}"))?;

    let logical_width =
        (page_size.width * super::score_view::score_base_scale() * request.zoom).max(1.0);
    let logical_height =
        (page_size.height * super::score_view::score_base_scale() * request.zoom).max(1.0);
    let longest_edge = logical_width.max(logical_height).max(1.0);
    let max_dimension = match request.tier {
        ScoreZoomPreviewTier::Fallback => SCORE_PREVIEW_FALLBACK_MAX_DIMENSION,
        ScoreZoomPreviewTier::Primary => SCORE_PREVIEW_PRIMARY_MAX_DIMENSION,
    };
    let raster_scale = (max_dimension / longest_edge).min(1.0);
    let raster_width = (logical_width * raster_scale).round().max(1.0) as u32;
    let raster_height = (logical_height * raster_scale).round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(raster_width, raster_height)
        .ok_or_else(|| "Failed to allocate score preview pixmap".to_string())?;

    let tree_size = tree.size().to_int_size().to_size();
    let transform = tiny_skia::Transform::from_scale(
        raster_width as f32 / tree_size.width(),
        raster_height as f32 / tree_size.height(),
    );

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Ok(super::messages::ScorePreviewReady {
        page_index: request.page_index,
        zoom: request.zoom,
        tier: request.tier,
        handle: image::Handle::from_rgba(raster_width, raster_height, pixmap.take()),
    })
}

fn is_relevant_score_change(event: &notify::Event, watched_path: &Path) -> bool {
    let kind_matches = matches!(
        event.kind,
        EventKind::Any | EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );

    if !kind_matches {
        return false;
    }

    event.paths.is_empty() || event.paths.iter().any(|path| path == watched_path)
}

fn is_relevant_editor_file_change(event: &notify::Event, watched_path: &Path) -> bool {
    let kind_matches = matches!(
        event.kind,
        EventKind::Any | EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );

    if !kind_matches {
        return false;
    }

    let Some(parent) = watched_path.parent() else {
        return false;
    };

    event.paths.iter().any(|path| {
        path == watched_path
            || path.parent().is_some_and(|candidate| candidate == parent)
            || path == parent
    })
}

fn is_relevant_browser_file_change(event: &notify::Event, watched_root: &Path) -> bool {
    let kind_matches = matches!(
        event.kind,
        EventKind::Any | EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );

    if !kind_matches {
        return false;
    }

    event.paths.is_empty()
        || event
            .paths
            .iter()
            .any(|path| path == watched_root || path.starts_with(watched_root))
}

fn is_svg_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
}

fn svg_page_index(file_stem: &str, score_stem: &str) -> Option<u32> {
    if file_stem == score_stem {
        return Some(1);
    }

    let suffix = file_stem.strip_prefix(score_stem)?.strip_prefix('-')?;

    if let Some(page_suffix) = suffix.strip_prefix("page") {
        return page_suffix.parse::<u32>().ok();
    }

    suffix.parse::<u32>().ok()
}

fn read_svg_size(path: &Path) -> Option<SvgSize> {
    let source = fs::read_to_string(path).ok()?;
    parse_svg_size_from_source(&source)
}

fn parse_svg_size_from_source(source: &str) -> Option<SvgSize> {
    let width = svg_attribute_value(source, "width").and_then(parse_svg_dimension_points);
    let height = svg_attribute_value(source, "height").and_then(parse_svg_dimension_points);

    match (width, height) {
        (Some(width), Some(height)) if width > 0.0 && height > 0.0 => {
            return Some(SvgSize { width, height });
        }
        _ => {}
    }

    if let Some(view_box) = svg_attribute_value(source, "viewBox") {
        let numbers: Vec<f32> = view_box
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|value| !value.is_empty())
            .filter_map(|value| value.parse::<f32>().ok())
            .collect();

        if numbers.len() >= 4 {
            let width = numbers[2].abs();
            let height = numbers[3].abs();

            if width > 0.0 && height > 0.0 {
                return Some(SvgSize { width, height });
            }
        }
    }

    None
}

fn parse_svg_coordinate_size_from_source(source: &str) -> Option<SvgSize> {
    if let Some(view_box) = svg_attribute_value(source, "viewBox") {
        let numbers: Vec<f32> = view_box
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|value| !value.is_empty())
            .filter_map(|value| value.parse::<f32>().ok())
            .collect();

        if numbers.len() >= 4 {
            let width = numbers[2].abs();
            let height = numbers[3].abs();

            if width > 0.0 && height > 0.0 {
                return Some(SvgSize { width, height });
            }
        }
    }

    parse_svg_size_from_source(source)
}

fn svg_attribute_value<'a>(source: &'a str, attribute_name: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let needle = format!("{attribute_name}={quote}");
        let Some(start) = source.find(&needle) else {
            continue;
        };
        let value_start = start + needle.len();
        let tail = &source[value_start..];
        let Some(value_end) = tail.find(quote) else {
            continue;
        };

        return Some(&tail[..value_end]);
    }

    None
}

fn parse_svg_dimension_points(raw_value: &str) -> Option<f32> {
    let trimmed = raw_value.trim();
    let numeric_prefix: String = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'))
        .collect();

    if numeric_prefix.is_empty() {
        return None;
    }

    let value = numeric_prefix.parse::<f32>().ok()?;
    let unit = trimmed[numeric_prefix.len()..].trim().to_ascii_lowercase();

    let points = match unit.as_str() {
        "" | "px" | "pt" => value,
        "mm" => value * 72.0 / 25.4,
        "cm" => value * 72.0 / 2.54,
        "in" => value * 72.0,
        "pc" => value * 12.0,
        _ => value,
    };

    Some(points)
}

fn closest_system_band(
    bands: &[score_cursor::SystemBand],
    x: f32,
    min_y: f32,
    max_y: f32,
) -> Option<score_cursor::SystemBand> {
    let target_y = (min_y + max_y) * 0.5;
    bands
        .iter()
        .copied()
        .filter(|band| x >= band.x_start && x <= band.x_end)
        .min_by(|left, right| {
            let left_center = (left.min_y + left.max_y) * 0.5;
            let right_center = (right.min_y + right.max_y) * 0.5;
            let left_distance = (target_y - left_center).abs();
            let right_distance = (target_y - right_center).abs();
            left_distance.total_cmp(&right_distance)
        })
}
