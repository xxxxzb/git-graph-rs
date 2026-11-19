//! Input and output data models for commit graph lane layout and ref-badge grouping.

/// A commit (or synthetic row such as uncommitted changes / stash) passed into [`crate::compute_layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitNode {
    /// Commit hash, or a synthetic identifier (e.g. `"*"` for uncommitted changes).
    pub hash: String,
    /// Ordered parent hashes (first parent at index `0`).
    pub parents: Vec<String>,
    /// Whether this node represents a Git stash entry.
    pub is_stash: bool,
    /// Whether this node represents the synthetic "Uncommitted Changes" row.
    pub is_uncommitted: bool,
}

/// Classification of a Git ref attached to a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    /// Local branch (`refs/heads/<name>`).
    LocalBranch,
    /// Remote-tracking branch (`refs/remotes/<remote>/<name>`).
    RemoteBranch {
        /// Remote name, e.g. `"origin"`.
        remote: String,
    },
    /// Tag (`refs/tags/<name>`).
    Tag {
        /// `true` for annotated tags, `false` for lightweight tags.
        annotated: bool,
    },
    /// Stash ref, e.g. `"stash@{0}"`.
    Stash,
}

/// A raw ref label attached to a commit before badge grouping.
///
/// For [`RefKind::RemoteBranch`], `name` may either include the `"<remote>/"` prefix
/// (e.g. `"origin/main"` with `remote: "origin"`) or omit it (e.g. `"main"` with
/// `remote: "origin"`). [`crate::group_ref_badges`] normalizes both forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

/// Visual node style priority: `Uncommitted` > `Stash` > `Head` > `Merge` > `Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Normal,
    Merge,
    Head,
    Stash,
    Uncommitted,
}

/// Vertical span of a [`LaneSegment`] within a single graph row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Connects the top boundary of the row (`from_lane`) to the commit dot (`to_lane`).
    TopToNode,
    /// Connects the commit dot (`from_lane`) to the bottom boundary of the row (`to_lane`).
    NodeToBottom,
    /// Passes straight through the row from top (`from_lane`) to bottom (`to_lane`).
    PassThrough,
}

/// One edge segment rendered inside a graph row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneSegment {
    pub from_lane: usize,
    pub to_lane: usize,
    pub color_index: usize,
    pub kind: SegmentKind,
    pub dashed: bool,
}

/// Layout information for a single commit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLayout {
    /// Column index where the commit dot is placed.
    pub lane: usize,
    /// Palette color index assigned to this commit dot / chain.
    pub color_index: usize,
    /// Primary visual node classification (`Uncommitted > Stash > Head > Merge > Normal`).
    pub node_kind: NodeKind,
    /// Whether this row's commit matches `head_hash` (useful when a commit is both HEAD and Merge).
    pub is_head: bool,
    /// Whether this row's commit has more than one parent (excluding uncommitted rows).
    pub is_merge: bool,
    /// Minimum number of lanes needed to render this row (`> max lane index` used in this row).
    pub active_lanes: usize,
    /// All line segments passing through or connecting to this row.
    pub segments: Vec<LaneSegment>,
}

/// Complete lane layout across all commit rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphLayout {
    pub rows: Vec<RowLayout>,
    pub max_lanes: usize,
}
