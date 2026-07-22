//! Two-commit comparison ("mark for compare").

use super::*;

impl App {
    /// The OID of the selected graph node's commit, if it has one (excludes the
    /// uncommitted-changes row and connector-only rows).
    fn selected_commit_oid(&self) -> Option<Oid> {
        self.selected_commit_node()
            .and_then(|node| node.commit.as_ref())
            .map(|commit| commit.oid)
    }

    /// Committer time in seconds since epoch (0 if the commit can't be read).
    fn commit_time(&self, oid: Oid) -> i64 {
        self.repo
            .repo()
            .find_commit(oid)
            .map(|c| c.time().seconds())
            .unwrap_or(0)
    }

    /// Order two commits older → newer by commit time (stable on ties).
    fn order_older_to_newer(&self, a: Oid, b: Oid) -> (Oid, Oid) {
        if self.commit_time(a) <= self.commit_time(b) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// Handle the "mark for compare" action (graph `m` / commit menu). First
    /// invocation marks the selected commit; a second invocation on a different
    /// commit opens the comparison. Invoking while a comparison is active starts
    /// a fresh mark.
    pub(crate) fn mark_or_compare_selected(&mut self) {
        let Some(oid) = self.selected_commit_oid() else {
            self.toast(crate::toast::ToastKind::Info, "Select a commit to compare");
            return;
        };

        // A comparison is already showing — begin a new selection.
        if self.compare_range.is_some() {
            self.compare_range = None;
            self.compare_marked = Some(oid);
            self.toast(crate::toast::ToastKind::Info, format!(
                "Marked {} for compare (previous comparison cleared)",
                short_hash(oid)
            ));
            return;
        }

        match self.compare_marked {
            None => {
                self.compare_marked = Some(oid);
                self.toast(crate::toast::ToastKind::Info, format!(
                    "Marked {} — select another commit and press m to compare",
                    short_hash(oid)
                ));
            }
            Some(marked) if marked == oid => {
                self.compare_marked = None;
                self.toast(crate::toast::ToastKind::Info, "Unmarked comparison commit");
            }
            Some(marked) => {
                let (old, new) = self.order_older_to_newer(marked, oid);
                self.compare_marked = None;
                self.compare_range = Some((old, new));
                // Focus stays on the graph so a single Esc clears the
                // comparison; the files pane and detail pane already reflect it.
                self.commit_detail_scroll = 0;
                self.toast(crate::toast::ToastKind::Success, format!(
                    "Comparing {} → {} (older → newer). Space opens a file; Esc clears.",
                    short_hash(old),
                    short_hash(new)
                ));
            }
        }
    }

    /// Clear any pending mark and/or active comparison. Returns whether there
    /// was anything to clear (so Esc can consume the key instead of quitting).
    pub(crate) fn clear_compare(&mut self) -> bool {
        if self.compare_range.is_some() || self.compare_marked.is_some() {
            self.compare_range = None;
            self.compare_marked = None;
            self.toast(crate::toast::ToastKind::Info, "Cleared comparison");
            true
        } else {
            false
        }
    }

    /// Short id + subject line for a commit, for comparison display. Reuses a
    /// loaded `CommitInfo` when present, else reads it from the repo.
    pub(crate) fn commit_short_and_subject(&self, oid: Oid) -> (String, String) {
        if let Some(commit) = self.commits.iter().find(|c| c.oid == oid) {
            return (commit.short_id.clone(), commit.message.clone());
        }
        match self.repo.repo().find_commit(oid) {
            Ok(commit) => {
                let info = crate::git::CommitInfo::from_git2_commit(&commit);
                (info.short_id, info.message)
            }
            Err(_) => (short_hash(oid), String::new()),
        }
    }
}
