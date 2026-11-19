//! Commit graph lane topology layout algorithm.

use std::collections::HashMap;

use crate::model::{CommitNode, GraphLayout, LaneSegment, NodeKind, RowLayout, SegmentKind};

/// Options controlling [`compute_layout_with_options`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutOptions {
    /// 为 true 时，如果某条提交的 parent 不在当前 `commits` 列表中，
    /// 则截断该连线（不为其分配向下泳道，避免在搜索/筛选结果中产生悬空线瀑布），
    /// 仅保留当前可见提交之间的真实父子连线。
    pub truncate_missing_parents: bool,
}

#[derive(Clone, Copy)]
struct ActiveLane<'a> {
    target_hash: &'a str,
    color_index: usize,
    dashed: bool,
}

/// Computes lane positions, palette colors, node kinds, and per-row line segments for a commit list.
///
/// Commits are expected in topological display order (children before parents, with an optional
/// uncommitted-changes row at the top).
///
/// Guarantees the cross-row continuity invariant: for every `i`, the set of `to_lane` indices
/// across row `i`'s `NodeToBottom` and `PassThrough` segments strictly equals the set of
/// `from_lane` indices across row `i + 1`'s `TopToNode` and `PassThrough` segments.
pub fn compute_layout(commits: &[CommitNode], head_hash: Option<&str>) -> GraphLayout {
    compute_layout_with_options(commits, head_hash, LayoutOptions::default())
}

/// Computes lane positions, palette colors, node kinds, and per-row line segments for a commit list
/// with configurable [`LayoutOptions`].
pub fn compute_layout_with_options(
    commits: &[CommitNode],
    head_hash: Option<&str>,
    options: LayoutOptions,
) -> GraphLayout {
    let visible_last_row = options.truncate_missing_parents.then(|| {
        let mut map = HashMap::with_capacity(commits.len());
        for (idx, commit) in commits.iter().enumerate() {
            if !commit.hash.is_empty() {
                map.insert(commit.hash.as_str(), idx);
            }
        }
        map
    });

    let mut lanes: Vec<Option<ActiveLane<'_>>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    let mut next_color: usize = 0;
    let mut max_lanes: usize = 0;

    for (row_idx, commit) in commits.iter().enumerate() {
        let row = layout_row(
            commit,
            head_hash,
            row_idx,
            visible_last_row.as_ref(),
            &mut lanes,
            &mut next_color,
        );
        max_lanes = max_lanes.max(row.active_lanes);
        rows.push(row);
    }

    GraphLayout { rows, max_lanes }
}

fn layout_row<'a>(
    commit: &'a CommitNode,
    head_hash: Option<&'a str>,
    row_idx: usize,
    visible_last_row: Option<&HashMap<&'a str, usize>>,
    lanes: &mut Vec<Option<ActiveLane<'a>>>,
    next_color: &mut usize,
) -> RowLayout {
    let incoming_first = lanes.iter().enumerate().find_map(|(idx, slot)| match slot {
        Some(active) if active.target_hash == commit.hash => Some((idx, active.color_index)),
        _ => None,
    });

    let (node_lane, node_color) = match incoming_first {
        Some((idx, color)) => (idx, color),
        None => {
            let idx = alloc_free_lane(lanes);
            let color = alloc_color(next_color);
            (idx, color)
        }
    };

    let mut segments = Vec::with_capacity(lanes.len() + commit.parents.len());

    // Emit TopToNode for every lane waiting on this commit (consuming those slots),
    // and PassThrough for every other active lane.
    for (idx, slot) in lanes.iter_mut().enumerate() {
        let Some(active) = *slot else {
            continue;
        };
        if active.target_hash == commit.hash {
            segments.push(LaneSegment {
                from_lane: idx,
                to_lane: node_lane,
                color_index: active.color_index,
                kind: SegmentKind::TopToNode,
                dashed: active.dashed,
            });
            *slot = None;
        } else {
            segments.push(LaneSegment {
                from_lane: idx,
                to_lane: idx,
                color_index: active.color_index,
                kind: SegmentKind::PassThrough,
                dashed: active.dashed,
            });
        }
    }

    let edge_dashed = commit.is_uncommitted;
    let parents = collect_effective_parents(commit, head_hash, row_idx, visible_last_row);

    for (p_idx, &parent_hash) in parents.iter().enumerate() {
        if p_idx == 0 {
            // Keep the first-parent chain on `node_lane`, unless a solid lane strictly to the
            // left is already tracking `parent_hash` (in which case merge back left early).
            let left_existing = if !edge_dashed {
                find_solid_waiting_lane(lanes, parent_hash, Some(node_lane))
            } else {
                None
            };

            if let Some((target_lane, _)) = left_existing {
                segments.push(LaneSegment {
                    from_lane: node_lane,
                    to_lane: target_lane,
                    color_index: node_color,
                    kind: SegmentKind::NodeToBottom,
                    dashed: edge_dashed,
                });
            } else {
                lanes[node_lane] = Some(ActiveLane {
                    target_hash: parent_hash,
                    color_index: node_color,
                    dashed: edge_dashed,
                });
                segments.push(LaneSegment {
                    from_lane: node_lane,
                    to_lane: node_lane,
                    color_index: node_color,
                    kind: SegmentKind::NodeToBottom,
                    dashed: edge_dashed,
                });
            }
        } else if !edge_dashed
            && let Some((target_lane, target_color)) =
                find_solid_waiting_lane(lanes, parent_hash, None)
        {
            segments.push(LaneSegment {
                from_lane: node_lane,
                to_lane: target_lane,
                color_index: target_color,
                kind: SegmentKind::NodeToBottom,
                dashed: false,
            });
        } else {
            let target_lane = alloc_free_lane(lanes);
            let branch_color = alloc_color(next_color);
            lanes[target_lane] = Some(ActiveLane {
                target_hash: parent_hash,
                color_index: branch_color,
                dashed: edge_dashed,
            });
            segments.push(LaneSegment {
                from_lane: node_lane,
                to_lane: target_lane,
                color_index: branch_color,
                kind: SegmentKind::NodeToBottom,
                dashed: edge_dashed,
            });
        }
    }

    // Shrink trailing idle slots so active width contracts after branches converge.
    while lanes.last().is_some_and(Option::is_none) {
        lanes.pop();
    }

    let max_seg_lane = segments
        .iter()
        .map(|s| s.from_lane.max(s.to_lane) + 1)
        .max()
        .unwrap_or(0);
    let active_lanes = (node_lane + 1).max(max_seg_lane).max(lanes.len());

    let is_head = !commit.is_uncommitted && head_hash.is_some_and(|h| h == commit.hash);
    let is_merge = !commit.is_uncommitted && commit.parents.len() > 1;
    let node_kind = classify_node(commit.is_uncommitted, commit.is_stash, is_head, is_merge);

    RowLayout {
        lane: node_lane,
        color_index: node_color,
        node_kind,
        is_head,
        is_merge,
        active_lanes,
        segments,
    }
}

fn collect_effective_parents<'a>(
    commit: &'a CommitNode,
    head_hash: Option<&'a str>,
    row_idx: usize,
    visible_last_row: Option<&HashMap<&'a str, usize>>,
) -> Vec<&'a str> {
    let is_visible_parent = |hash: &str| -> bool {
        match visible_last_row {
            Some(last_row_by_hash) => last_row_by_hash
                .get(hash)
                .is_some_and(|&target_idx| target_idx > row_idx),
            None => true,
        }
    };

    let mut parents = Vec::with_capacity(commit.parents.len().max(1));
    if !commit.parents.is_empty() {
        for parent in &commit.parents {
            let hash = parent.as_str();
            if is_visible_parent(hash) && !parents.contains(&hash) {
                parents.push(hash);
            }
        }
    } else if commit.is_uncommitted
        && let Some(head) = head_hash
        && is_visible_parent(head)
    {
        parents.push(head);
    }
    parents
}

fn find_solid_waiting_lane(
    lanes: &[Option<ActiveLane<'_>>],
    target_hash: &str,
    max_exclusive_lane: Option<usize>,
) -> Option<(usize, usize)> {
    let limit = max_exclusive_lane.unwrap_or(lanes.len());
    lanes
        .iter()
        .take(limit)
        .enumerate()
        .find_map(|(idx, slot)| match slot {
            Some(active) if !active.dashed && active.target_hash == target_hash => {
                Some((idx, active.color_index))
            }
            _ => None,
        })
}

fn alloc_free_lane(lanes: &mut Vec<Option<ActiveLane<'_>>>) -> usize {
    if let Some(idx) = lanes.iter().position(Option::is_none) {
        idx
    } else {
        lanes.push(None);
        lanes.len() - 1
    }
}

fn alloc_color(next_color: &mut usize) -> usize {
    let color = *next_color;
    *next_color += 1;
    color
}

fn classify_node(is_uncommitted: bool, is_stash: bool, is_head: bool, is_merge: bool) -> NodeKind {
    if is_uncommitted {
        NodeKind::Uncommitted
    } else if is_stash {
        NodeKind::Stash
    } else if is_head {
        NodeKind::Head
    } else if is_merge {
        NodeKind::Merge
    } else {
        NodeKind::Normal
    }
}
