use std::collections::BTreeSet;
use std::time::Instant;

use git_graph_layout::{
    CommitNode, GraphLayout, LayoutOptions, NodeKind, RefBadge, RefKind, RefLabel, SegmentKind,
    compute_layout, compute_layout_with_options, group_ref_badges,
};

fn commit(hash: &str, parents: &[&str]) -> CommitNode {
    CommitNode {
        hash: hash.to_string(),
        parents: parents.iter().map(|s| (*s).to_string()).collect(),
        is_stash: false,
        is_uncommitted: false,
    }
}

fn assert_layout_invariants(commits: &[CommitNode], layout: &GraphLayout) {
    assert_eq!(layout.rows.len(), commits.len(), "row count mismatch");

    let expected_max = layout
        .rows
        .iter()
        .map(|r| r.active_lanes)
        .max()
        .unwrap_or(0);
    assert_eq!(layout.max_lanes, expected_max, "max_lanes mismatch");

    for (idx, row) in layout.rows.iter().enumerate() {
        assert!(
            row.lane < row.active_lanes,
            "row {idx}: node lane {} >= active_lanes {}",
            row.lane,
            row.active_lanes
        );

        let mut incoming_lanes = Vec::new();
        for seg in &row.segments {
            assert!(
                seg.from_lane < row.active_lanes && seg.to_lane < row.active_lanes,
                "row {idx}: segment {:?} out of active_lanes {}",
                seg,
                row.active_lanes
            );
            match seg.kind {
                SegmentKind::TopToNode => {
                    assert_eq!(
                        seg.to_lane, row.lane,
                        "row {idx}: TopToNode must end at node"
                    );
                    incoming_lanes.push(seg.from_lane);
                }
                SegmentKind::NodeToBottom => {
                    assert_eq!(
                        seg.from_lane, row.lane,
                        "row {idx}: NodeToBottom must start at node"
                    );
                }
                SegmentKind::PassThrough => {
                    assert_eq!(
                        seg.from_lane, seg.to_lane,
                        "row {idx}: PassThrough must stay in lane"
                    );
                    incoming_lanes.push(seg.from_lane);
                }
            }
        }

        let unique_incoming: BTreeSet<usize> = incoming_lanes.iter().copied().collect();
        assert_eq!(
            unique_incoming.len(),
            incoming_lanes.len(),
            "row {idx}: duplicate incoming lane at top boundary: {:?}",
            incoming_lanes
        );
    }

    // Cross-row continuity invariant: bottom to_lane set(i) == top from_lane set(i + 1).
    for i in 0..layout.rows.len().saturating_sub(1) {
        let bottom_lanes: BTreeSet<usize> = layout.rows[i]
            .segments
            .iter()
            .filter(|s| matches!(s.kind, SegmentKind::NodeToBottom | SegmentKind::PassThrough))
            .map(|s| s.to_lane)
            .collect();

        let next_top_lanes: BTreeSet<usize> = layout.rows[i + 1]
            .segments
            .iter()
            .filter(|s| matches!(s.kind, SegmentKind::TopToNode | SegmentKind::PassThrough))
            .map(|s| s.from_lane)
            .collect();

        assert_eq!(
            bottom_lanes,
            next_top_lanes,
            "cross-row discontinuity between row {} and row {}",
            i,
            i + 1
        );
    }
}

#[test]
fn empty_commits_returns_empty_layout() {
    let layout = compute_layout(&[], None);
    assert!(layout.rows.is_empty());
    assert_eq!(layout.max_lanes, 0);
}

#[test]
fn linear_history_stays_in_lane_zero_with_single_color() {
    let commits = vec![
        commit("c3", &["c2"]),
        commit("c2", &["c1"]),
        commit("c1", &[]),
    ];
    let layout = compute_layout(&commits, Some("c3"));
    assert_layout_invariants(&commits, &layout);
    assert_eq!(layout.max_lanes, 1);

    for (idx, row) in layout.rows.iter().enumerate() {
        assert_eq!(row.lane, 0);
        assert_eq!(row.color_index, 0);
        assert_eq!(row.active_lanes, 1);
        if idx == 0 {
            assert_eq!(row.node_kind, NodeKind::Head);
            assert!(row.is_head);
        } else {
            assert_eq!(row.node_kind, NodeKind::Normal);
            assert!(!row.is_head);
        }
        assert!(!row.is_merge);
    }
}

#[test]
fn fork_and_merge_allocates_new_lane_then_shrinks_back() {
    // Branch f1 appears before c2: converges at c1 via TopToNode(1 -> 0), then c0 is 1-lane wide.
    let commits = vec![
        commit("c4", &["c3", "f1"]),
        commit("c3", &["c2"]),
        commit("f1", &["c1"]),
        commit("c2", &["c1"]),
        commit("c1", &["c0"]),
        commit("c0", &[]),
    ];
    let layout = compute_layout(&commits, Some("c4"));
    assert_layout_invariants(&commits, &layout);
    assert_eq!(layout.max_lanes, 2);

    // c4: Head & Merge
    assert_eq!(layout.rows[0].lane, 0);
    assert_eq!(layout.rows[0].node_kind, NodeKind::Head);
    assert!(layout.rows[0].is_head);
    assert!(layout.rows[0].is_merge);
    assert_eq!(layout.rows[0].active_lanes, 2);

    // c3 stays on first-parent lane 0 with same color 0
    assert_eq!(layout.rows[1].lane, 0);
    assert_eq!(layout.rows[1].color_index, layout.rows[0].color_index);

    // f1 sits on lane 1 with distinct color 1
    assert_eq!(layout.rows[2].lane, 1);
    assert_ne!(layout.rows[2].color_index, layout.rows[0].color_index);

    // c1 converges lane 1 into lane 0 via TopToNode(1 -> 0)
    assert_eq!(layout.rows[4].lane, 0);
    assert!(
        layout.rows[4]
            .segments
            .iter()
            .any(|s| { s.kind == SegmentKind::TopToNode && s.from_lane == 1 && s.to_lane == 0 })
    );

    // After convergence at c1, c0 shrinks back to 1 active lane
    assert_eq!(layout.rows[5].lane, 0);
    assert_eq!(layout.rows[5].active_lanes, 1);
}

#[test]
fn early_merge_back_to_waiting_left_lane_shrinks_immediately() {
    // When c2 runs before f1, lane 0 is already waiting on c1 so f1 merges left immediately.
    let commits = vec![
        commit("c4", &["c3", "f1"]),
        commit("c3", &["c2"]),
        commit("c2", &["c1"]),
        commit("f1", &["c1"]),
        commit("c1", &[]),
    ];
    let layout = compute_layout(&commits, Some("c4"));
    assert_layout_invariants(&commits, &layout);
    assert_eq!(layout.max_lanes, 2);

    // f1 (row 3) merges directly to lane 0 at bottom
    assert_eq!(layout.rows[3].lane, 1);
    assert!(
        layout.rows[3]
            .segments
            .iter()
            .any(|s| { s.kind == SegmentKind::NodeToBottom && s.from_lane == 1 && s.to_lane == 0 })
    );

    // c1 (row 4) only needs 1 active lane
    assert_eq!(layout.rows[4].lane, 0);
    assert_eq!(layout.rows[4].active_lanes, 1);
}

#[test]
fn node_kind_priority_and_dual_flags() {
    let commits = vec![
        CommitNode {
            hash: "*".to_string(),
            parents: vec!["s1".to_string()],
            is_stash: true, // Uncommitted beats Stash
            is_uncommitted: true,
        },
        CommitNode {
            hash: "s1".to_string(),
            parents: vec!["h1".to_string(), "s1_idx".to_string()],
            is_stash: true, // Stash beats Merge and Head
            is_uncommitted: false,
        },
        commit("h1", &["b1", "b2"]), // Head beats Merge, both is_head & is_merge are true
        commit("b1", &["m1"]),
        commit("b2", &["m1"]),
        commit("m1", &["r1", "r2"]), // Merge beats Normal
        commit("r1", &[]),
        commit("r2", &[]),
        commit("s1_idx", &[]),
    ];

    let layout = compute_layout(&commits, Some("h1"));
    assert_layout_invariants(&commits, &layout);

    assert_eq!(layout.rows[0].node_kind, NodeKind::Uncommitted);
    assert!(!layout.rows[0].is_head);
    assert!(!layout.rows[0].is_merge);

    assert_eq!(layout.rows[1].node_kind, NodeKind::Stash);
    assert!(layout.rows[1].is_merge);

    assert_eq!(layout.rows[2].node_kind, NodeKind::Head);
    assert!(layout.rows[2].is_head);
    assert!(layout.rows[2].is_merge);

    assert_eq!(layout.rows[5].node_kind, NodeKind::Merge);
    assert!(!layout.rows[5].is_head);
    assert!(layout.rows[5].is_merge);

    assert_eq!(layout.rows[6].node_kind, NodeKind::Normal);
}

#[test]
fn uncommitted_dashed_edge_reaches_head_across_intermediate_commits() {
    // Uncommitted (no explicit parents, resolved via head_hash) -> side branch commit -> HEAD -> root
    let commits = vec![
        CommitNode {
            hash: "*".to_string(),
            parents: Vec::new(),
            is_stash: false,
            is_uncommitted: true,
        },
        commit("feature_tip", &["head_c"]),
        commit("head_c", &["root"]),
        commit("root", &[]),
    ];

    let layout = compute_layout(&commits, Some("head_c"));
    assert_layout_invariants(&commits, &layout);

    // Row 0: Uncommitted at lane 0 with dashed NodeToBottom -> lane 0
    assert_eq!(layout.rows[0].node_kind, NodeKind::Uncommitted);
    assert_eq!(layout.rows[0].lane, 0);
    assert!(layout.rows[0].segments.iter().any(|s| {
        s.kind == SegmentKind::NodeToBottom && s.from_lane == 0 && s.to_lane == 0 && s.dashed
    }));

    // Row 1: feature_tip gets lane 1; lane 0 passes through dashed to HEAD
    assert_eq!(layout.rows[1].lane, 1);
    assert!(layout.rows[1].segments.iter().any(|s| {
        s.kind == SegmentKind::PassThrough && s.from_lane == 0 && s.to_lane == 0 && s.dashed
    }));
    assert!(layout.rows[1].segments.iter().any(|s| {
        s.kind == SegmentKind::NodeToBottom && s.from_lane == 1 && s.to_lane == 1 && !s.dashed
    }));

    // Row 2: HEAD sits at lane 0; dashed TopToNode from 0->0 and solid TopToNode from 1->0
    assert_eq!(layout.rows[2].node_kind, NodeKind::Head);
    assert_eq!(layout.rows[2].lane, 0);
    assert!(layout.rows[2].segments.iter().any(|s| {
        s.kind == SegmentKind::TopToNode && s.from_lane == 0 && s.to_lane == 0 && s.dashed
    }));
    assert!(layout.rows[2].segments.iter().any(|s| {
        s.kind == SegmentKind::TopToNode && s.from_lane == 1 && s.to_lane == 0 && !s.dashed
    }));

    // Row 3: root contracts back to 1 lane
    assert_eq!(layout.rows[3].lane, 0);
    assert_eq!(layout.rows[3].active_lanes, 1);
}

#[test]
fn complex_interleaved_branches_octopus_merge_and_slot_reuse() {
    let commits = vec![
        CommitNode {
            hash: "*".to_string(),
            parents: vec!["m_head".to_string()],
            is_stash: false,
            is_uncommitted: true,
        },
        commit("m_head", &["a3", "b2", "c2"]), // octopus merge
        commit("d_tip", &["a2"]),              // independent branch tip
        CommitNode {
            hash: "stash@{0}".to_string(),
            parents: vec!["a3".to_string()],
            is_stash: true,
            is_uncommitted: false,
        },
        commit("a3", &["a2"]),
        commit("b2", &["b1"]),
        commit("c2", &["a1"]),
        commit("b1", &["a2", "e1"]),
        commit("a2", &["a1"]),
        commit("e1", &["a0"]),
        commit("a1", &["a0"]),
        commit("a0", &["root"]),
        commit("root", &[]),
    ];

    let layout = compute_layout(&commits, Some("m_head"));
    assert_layout_invariants(&commits, &layout);

    // Ensure octopus merge row reports is_merge & is_head
    assert_eq!(layout.rows[1].node_kind, NodeKind::Head);
    assert!(layout.rows[1].is_head);
    assert!(layout.rows[1].is_merge);

    // Ensure stash row reports NodeKind::Stash
    assert_eq!(layout.rows[3].node_kind, NodeKind::Stash);

    // Final root commit contracts back to lane 0 with 1 active lane
    let last = layout.rows.last().unwrap();
    assert_eq!(last.lane, 0);
    assert_eq!(last.active_lanes, 1);
}

#[test]
fn ref_badges_group_and_sort_correctly() {
    let refs = vec![
        RefLabel {
            name: "stash@{10}".to_string(),
            kind: RefKind::Stash,
        },
        RefLabel {
            name: "stash@{2}".to_string(),
            kind: RefKind::Stash,
        },
        RefLabel {
            name: "stash@{0}".to_string(),
            kind: RefKind::Stash,
        },
        RefLabel {
            name: "v1.0.0".to_string(),
            kind: RefKind::Tag { annotated: false },
        },
        // Duplicate peeled/unpeeled tag should coalesce with annotated = true
        RefLabel {
            name: "v1.0.0".to_string(),
            kind: RefKind::Tag { annotated: true },
        },
        RefLabel {
            name: "v0.9.0".to_string(),
            kind: RefKind::Tag { annotated: false },
        },
        RefLabel {
            name: "origin/HEAD".to_string(),
            kind: RefKind::RemoteBranch {
                remote: "origin".to_string(),
            },
        },
        RefLabel {
            name: "origin/orphan".to_string(),
            kind: RefKind::RemoteBranch {
                remote: "origin".to_string(),
            },
        },
        // Remote without "remote/" prefix matching local "main"
        RefLabel {
            name: "main".to_string(),
            kind: RefKind::RemoteBranch {
                remote: "upstream".to_string(),
            },
        },
        // Remote with "remote/" prefix matching local "main"
        RefLabel {
            name: "origin/main".to_string(),
            kind: RefKind::RemoteBranch {
                remote: "origin".to_string(),
            },
        },
        // Nested slash branch name with prefix
        RefLabel {
            name: "origin/feature/ui".to_string(),
            kind: RefKind::RemoteBranch {
                remote: "origin".to_string(),
            },
        },
        RefLabel {
            name: "feature/ui".to_string(),
            kind: RefKind::LocalBranch,
        },
        RefLabel {
            name: "alpha".to_string(),
            kind: RefKind::LocalBranch,
        },
        RefLabel {
            name: "main".to_string(),
            kind: RefKind::LocalBranch,
        },
    ];

    let badges = group_ref_badges(&refs, Some("main"));
    assert_eq!(
        badges,
        vec![
            RefBadge::Branch {
                name: "main".to_string(),
                remotes: vec!["origin".to_string(), "upstream".to_string()],
                is_head: true,
            },
            RefBadge::Branch {
                name: "alpha".to_string(),
                remotes: Vec::new(),
                is_head: false,
            },
            RefBadge::Branch {
                name: "feature/ui".to_string(),
                remotes: vec!["origin".to_string()],
                is_head: false,
            },
            RefBadge::RemoteBranch {
                remote: "origin".to_string(),
                name: "HEAD".to_string(),
                is_remote_head: true,
            },
            RefBadge::RemoteBranch {
                remote: "origin".to_string(),
                name: "orphan".to_string(),
                is_remote_head: false,
            },
            RefBadge::Tag {
                name: "v0.9.0".to_string(),
                annotated: false,
            },
            RefBadge::Tag {
                name: "v1.0.0".to_string(),
                annotated: true,
            },
            RefBadge::Stash {
                selector: "stash@{0}".to_string(),
            },
            RefBadge::Stash {
                selector: "stash@{2}".to_string(),
            },
            RefBadge::Stash {
                selector: "stash@{10}".to_string(),
            },
        ]
    );
}

#[test]
fn stress_test_10000_commits_with_branches_and_merges() {
    // Build a 10,000-node DAG with cyclic branching and merging (oldest = c0, newest = c9998 + "*")
    let total = 10_000usize;
    let mut commits = Vec::with_capacity(total);

    // Precompute forward (oldest -> newest) links, then reverse into display order.
    let mut nodes: Vec<CommitNode> = Vec::with_capacity(total - 1);
    for i in 0..(total - 1) {
        let hash = format!("c{i:05}");
        let parents = if i == 0 {
            Vec::new()
        } else if i % 25 == 0 && i >= 6 {
            // Merge main line (i - 1) and a side branch chain (i - 5)
            vec![format!("c{:05}", i - 1), format!("c{:05}", i - 5)]
        } else if i % 25 == 20 && i >= 15 {
            // Start side branch from i - 15
            vec![format!("c{:05}", i - 15)]
        } else {
            vec![format!("c{:05}", i - 1)]
        };

        nodes.push(CommitNode {
            hash,
            parents,
            is_stash: i % 500 == 123,
            is_uncommitted: false,
        });
    }
    nodes.reverse();

    let head_hash = nodes[0].hash.clone();
    commits.push(CommitNode {
        hash: "*".to_string(),
        parents: vec![head_hash.clone()],
        is_stash: false,
        is_uncommitted: true,
    });
    commits.extend(nodes);
    assert_eq!(commits.len(), total);

    let start = Instant::now();
    let layout = compute_layout(&commits, Some(&head_hash));
    let elapsed = start.elapsed();

    assert_layout_invariants(&commits, &layout);
    assert!(
        layout.max_lanes <= 16,
        "unexpected lane explosion: max_lanes={}",
        layout.max_lanes
    );
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "10k layout took too long: {elapsed:?}"
    );
}

#[test]
fn truncate_missing_parents_prevents_dangling_waterfall_and_preserves_visible_edges() {
    // Discontinuous filtered subset: isolated commits mixed with adjacent and gapped parent-child pairs.
    let commits = vec![
        commit("iso1", &["missing_1"]),
        commit("iso2", &["missing_2"]),
        commit("g_child", &["g_parent"]),
        commit("iso3", &["missing_3"]),
        commit("adj_child", &["adj_parent"]),
        commit("adj_parent", &["missing_4"]),
        commit("g_parent", &["missing_5"]),
        commit("iso4", &["missing_6"]),
    ];

    let legacy_layout = compute_layout(&commits, None);
    assert_layout_invariants(&commits, &legacy_layout);
    assert!(
        legacy_layout.max_lanes >= 5,
        "expected legacy mode to accumulate dangling lanes, got {}",
        legacy_layout.max_lanes
    );

    let truncated_layout = compute_layout_with_options(
        &commits,
        None,
        LayoutOptions {
            truncate_missing_parents: true,
        },
    );
    assert_layout_invariants(&commits, &truncated_layout);
    assert_eq!(truncated_layout.max_lanes, 2);

    // Isolated commits at rows 0 and 1 emit no downward edges and stay on lane 0.
    for row_idx in [0, 1] {
        assert_eq!(truncated_layout.rows[row_idx].lane, 0);
        assert_eq!(truncated_layout.rows[row_idx].active_lanes, 1);
        assert!(truncated_layout.rows[row_idx].segments.is_empty());
    }

    // g_child (row 2) -> PassThrough at iso3 (row 3), adj_child (row 4) -> adj_parent (row 5) on lane 1,
    // then g_parent (row 6) closes lane 0 so iso4 (row 7) contracts back to lane 0 with 1 lane.
    assert_eq!(truncated_layout.rows[2].lane, 0);
    assert!(truncated_layout.rows[2].segments.iter().any(|s| {
        s.kind == SegmentKind::NodeToBottom && s.from_lane == 0 && s.to_lane == 0
    }));

    assert_eq!(truncated_layout.rows[3].lane, 1);
    assert!(truncated_layout.rows[3].segments.iter().any(|s| {
        s.kind == SegmentKind::PassThrough && s.from_lane == 0 && s.to_lane == 0
    }));
    assert!(
        !truncated_layout.rows[3]
            .segments
            .iter()
            .any(|s| s.kind == SegmentKind::NodeToBottom)
    );

    assert_eq!(truncated_layout.rows[4].lane, 1);
    assert!(truncated_layout.rows[4].segments.iter().any(|s| {
        s.kind == SegmentKind::NodeToBottom && s.from_lane == 1 && s.to_lane == 1
    }));

    assert_eq!(truncated_layout.rows[5].lane, 1);
    assert!(truncated_layout.rows[5].segments.iter().any(|s| {
        s.kind == SegmentKind::TopToNode && s.from_lane == 1 && s.to_lane == 1
    }));
    assert!(
        !truncated_layout.rows[5]
            .segments
            .iter()
            .any(|s| s.kind == SegmentKind::NodeToBottom)
    );

    assert_eq!(truncated_layout.rows[6].lane, 0);
    assert!(truncated_layout.rows[6].segments.iter().any(|s| {
        s.kind == SegmentKind::TopToNode && s.from_lane == 0 && s.to_lane == 0
    }));
    assert!(
        !truncated_layout.rows[6]
            .segments
            .iter()
            .any(|s| s.kind == SegmentKind::NodeToBottom)
    );

    assert_eq!(truncated_layout.rows[7].lane, 0);
    assert_eq!(truncated_layout.rows[7].active_lanes, 1);
    assert!(truncated_layout.rows[7].segments.is_empty());
}

#[test]
fn truncate_missing_parents_preserves_merge_flags_and_node_kind() {
    let commits = vec![
        // First parent missing, second parent visible
        commit("m_partial", &["missing_p1", "p2"]),
        // Both parents missing
        commit("m_isolated", &["missing_a", "missing_b"]),
        commit("p2", &["missing_root"]),
    ];

    let layout = compute_layout_with_options(
        &commits,
        None,
        LayoutOptions {
            truncate_missing_parents: true,
        },
    );
    assert_layout_invariants(&commits, &layout);
    assert_eq!(layout.max_lanes, 2);

    // m_partial keeps Merge identity even though missing_p1 was truncated.
    assert!(layout.rows[0].is_merge);
    assert_eq!(layout.rows[0].node_kind, NodeKind::Merge);
    assert_eq!(layout.rows[0].lane, 0);
    assert_eq!(
        layout.rows[0]
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::NodeToBottom)
            .count(),
        1
    );

    // m_isolated keeps Merge identity even with all parents truncated.
    assert!(layout.rows[1].is_merge);
    assert_eq!(layout.rows[1].node_kind, NodeKind::Merge);
    assert_eq!(layout.rows[1].lane, 1);
    assert!(
        !layout.rows[1]
            .segments
            .iter()
            .any(|s| s.kind == SegmentKind::NodeToBottom)
    );

    // p2 receives incoming edge from m_partial on lane 0 and truncates missing_root.
    assert_eq!(layout.rows[2].lane, 0);
    assert_eq!(layout.rows[2].active_lanes, 1);
    assert!(layout.rows[2].segments.iter().any(|s| {
        s.kind == SegmentKind::TopToNode && s.from_lane == 0 && s.to_lane == 0
    }));
    assert!(
        !layout.rows[2]
            .segments
            .iter()
            .any(|s| s.kind == SegmentKind::NodeToBottom)
    );
}

#[test]
fn truncate_missing_parents_handles_uncommitted_row_when_head_visible_or_missing() {
    let opts = LayoutOptions {
        truncate_missing_parents: true,
    };

    // Case 1: HEAD is visible (resolved via head_hash when uncommitted.parents is empty).
    let visible_head_commits = vec![
        CommitNode {
            hash: "*".to_string(),
            parents: Vec::new(),
            is_stash: false,
            is_uncommitted: true,
        },
        commit("other", &["missing_parent"]),
        commit("head_c", &["missing_base"]),
    ];
    let visible_layout = compute_layout_with_options(&visible_head_commits, Some("head_c"), opts);
    assert_layout_invariants(&visible_head_commits, &visible_layout);
    assert_eq!(visible_layout.rows[0].node_kind, NodeKind::Uncommitted);
    assert!(visible_layout.rows[0].segments.iter().any(|s| {
        s.kind == SegmentKind::NodeToBottom && s.from_lane == 0 && s.to_lane == 0 && s.dashed
    }));
    assert!(visible_layout.rows[1].segments.iter().any(|s| {
        s.kind == SegmentKind::PassThrough && s.from_lane == 0 && s.to_lane == 0 && s.dashed
    }));
    assert_eq!(visible_layout.rows[2].node_kind, NodeKind::Head);
    assert!(visible_layout.rows[2].segments.iter().any(|s| {
        s.kind == SegmentKind::TopToNode && s.from_lane == 0 && s.to_lane == 0 && s.dashed
    }));

    // Case 2: HEAD is NOT in the visible list (both implicit via head_hash and explicit parent forms).
    let missing_head_implicit = vec![
        CommitNode {
            hash: "*".to_string(),
            parents: Vec::new(),
            is_stash: false,
            is_uncommitted: true,
        },
        commit("filtered_c", &["missing_parent"]),
    ];
    let layout_implicit =
        compute_layout_with_options(&missing_head_implicit, Some("hidden_head"), opts);
    assert_layout_invariants(&missing_head_implicit, &layout_implicit);
    assert_eq!(layout_implicit.max_lanes, 1);
    assert_eq!(layout_implicit.rows[0].node_kind, NodeKind::Uncommitted);
    assert!(layout_implicit.rows[0].segments.is_empty());
    assert_eq!(layout_implicit.rows[1].lane, 0);
    assert!(layout_implicit.rows[1].segments.is_empty());

    let missing_head_explicit = vec![
        CommitNode {
            hash: "*".to_string(),
            parents: vec!["hidden_head".to_string()],
            is_stash: false,
            is_uncommitted: true,
        },
        commit("filtered_c", &["missing_parent"]),
    ];
    let layout_explicit =
        compute_layout_with_options(&missing_head_explicit, Some("hidden_head"), opts);
    assert_layout_invariants(&missing_head_explicit, &layout_explicit);
    assert_eq!(layout_explicit.max_lanes, 1);
    assert_eq!(layout_explicit.rows[0].node_kind, NodeKind::Uncommitted);
    assert!(layout_explicit.rows[0].segments.is_empty());
    assert_eq!(layout_explicit.rows[1].lane, 0);
    assert!(layout_explicit.rows[1].segments.is_empty());
}

#[test]
fn truncate_missing_parents_ignores_empty_hashes_and_non_forward_parents() {
    let commits = vec![
        commit("c_earlier", &["c_earlier"]), // self-loop should not leak a dangling lane
        commit("c_child", &["c_earlier", ""]), // parent above current row & empty parent hash
    ];

    let layout = compute_layout_with_options(
        &commits,
        None,
        LayoutOptions {
            truncate_missing_parents: true,
        },
    );
    assert_layout_invariants(&commits, &layout);
    assert_eq!(layout.max_lanes, 1);
    assert!(layout.rows[0].segments.is_empty());
    assert!(layout.rows[1].segments.is_empty());
}
