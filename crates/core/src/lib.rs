//! Core of SpaceScope: a parallel directory scanner, a compact size tree,
//! a filter language and a squarified treemap layout. No UI code lives here.

pub mod export;
pub mod filter;
pub mod fmt;
pub mod kind;
pub mod layout;
pub mod scan;
pub mod tree;

pub use filter::Filter;
pub use layout::{Layout, LayoutOptions, Metric, Tile, TileKind};
pub use scan::{refresh_dir, rescan_subtree, scan_path, Progress, Refresh, ScanOptions};
pub use tree::{flags, Node, NodeId, Shared, Tree, NO_NODE};
