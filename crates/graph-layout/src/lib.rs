//! Pure Rust Git commit graph lane layout and ref-badge grouping.

pub mod badges;
pub mod layout;
pub mod model;

pub use badges::{RefBadge, group_ref_badges};
pub use layout::{LayoutOptions, compute_layout, compute_layout_with_options};
pub use model::{
    CommitNode, GraphLayout, LaneSegment, NodeKind, RefKind, RefLabel, RowLayout, SegmentKind,
};
