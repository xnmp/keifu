//! Git layer

pub mod branch;
pub mod commit;
pub mod diff;
pub mod extensions;
pub mod graph;
pub mod operations;
pub mod patch;
pub mod repository;

pub use branch::BranchInfo;
pub use commit::CommitInfo;
pub use diff::{
    CommitDiffInfo, DiffHunkContent, DiffLineContent, DiffLineOrigin, FileChangeKind,
    FileDiffContent, FileDiffInfo, StageStatus,
};
pub use patch::{extract_hunk_from_working_tree, render_hunk_patch, HunkPatch, PatchLine, PatchLineKind};
pub use extensions::configure_git_extensions;
pub use graph::build_graph;
pub use repository::{GitRepository, StashInfo, WorkingTreeStatus};

use std::path::PathBuf;

/// Convert raw bytes from git2 into a PathBuf.
#[cfg(unix)]
pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

/// Convert raw bytes from git2 into a PathBuf.
#[cfg(not(unix))]
pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}
