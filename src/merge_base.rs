//! Merge-base / fork-point navigation: "where did this diverge?".
//!
//! Pure helpers over the loaded [`GraphLayout`] plus a `merge_base` closure
//! (backed by `git2` at the call site). The `App` wires these to jump the
//! selection to the fork point.

use git2::Oid;

use crate::git::graph::GraphLayout;

/// The newest loaded commit painted as the main branch — i.e. the one the
/// color assigner tagged with `MAIN_BRANCH_COLOR` (the main lane). Reuses the
/// existing main-branch heuristic rather than re-deriving "main" by name.
pub fn main_branch_tip(layout: &GraphLayout) -> Option<Oid> {
    layout.nodes.iter().find_map(|n| {
        let commit = n.commit.as_ref()?;
        (n.color_index == crate::graph::colors::MAIN_BRANCH_COLOR).then_some(commit.oid)
    })
}

/// Row index of the loaded node carrying `oid`, if it's within the window.
pub fn row_of_commit(layout: &GraphLayout, oid: Oid) -> Option<usize> {
    layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(oid))
}

/// Where a fork-point jump should land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkTarget {
    /// Jump to this commit (the merge base).
    Jump(Oid),
    /// The selection meets no other line — linear history, no jump.
    Linear,
    /// No common ancestor at all (unrelated histories).
    NoBase,
}

/// Decide the fork point for `selected`: the merge base with `main_tip` when
/// that diverges from the selection; otherwise (the selection is on main) the
/// merge base with the current branch (`head_tip`); otherwise linear.
///
/// `merge_base(a, b)` yields the base OID, or `None` when the two share no
/// history. Injecting it keeps this pure and testable.
pub fn fork_target(
    selected: Oid,
    main_tip: Oid,
    head_tip: Option<Oid>,
    merge_base: impl Fn(Oid, Oid) -> Option<Oid>,
) -> ForkTarget {
    match merge_base(selected, main_tip) {
        // Diverges from main → the merge base is the fork commit.
        Some(base) if base != selected => ForkTarget::Jump(base),
        // On the main line — answer with the current (HEAD) branch instead.
        Some(_) => match head_tip.and_then(|h| merge_base(selected, h)) {
            Some(hb) if hb != selected => ForkTarget::Jump(hb),
            _ => ForkTarget::Linear,
        },
        None => ForkTarget::NoBase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::graph::build_graph;
    use crate::git::{BranchInfo, CommitInfo};
    use chrono::Local;

    fn oid(b: u8) -> Oid {
        Oid::from_bytes(&[b; 20]).unwrap()
    }

    fn commit(id: u8, parents: &[u8]) -> CommitInfo {
        CommitInfo {
            oid: oid(id),
            short_id: format!("{id:07}"),
            author_name: "a".into(),
            author_email: "a@b".into(),
            timestamp: Local::now(),
            message: format!("commit {id}"),
            full_message: format!("commit {id}"),
            parent_oids: parents.iter().map(|p| oid(*p)).collect(),
        }
    }

    fn branch(name: &str, tip: u8, head: bool) -> BranchInfo {
        BranchInfo {
            name: name.into(),
            tip_oid: oid(tip),
            is_head: head,
            is_remote: false,
            upstream: None,
            ahead: 0,
            behind: 0,
        }
    }

    // ── main_branch_tip / row_of_commit ────────────────────────────────

    #[test]
    fn main_tip_is_the_first_commit_the_assigner_colors_main() {
        // c3 -> c2 -> c1; the newest commit anchors the main lane.
        let commits = vec![commit(3, &[2]), commit(2, &[1]), commit(1, &[])];
        let branches = vec![branch("main", 3, true)];
        let layout = build_graph(&commits, &branches, &[], &[], None, None, &[]);
        assert_eq!(main_branch_tip(&layout), Some(oid(3)));
    }

    #[test]
    fn row_of_commit_finds_and_misses() {
        let commits = vec![commit(3, &[2]), commit(2, &[1]), commit(1, &[])];
        let branches = vec![branch("main", 3, true)];
        let layout = build_graph(&commits, &branches, &[], &[], None, None, &[]);
        assert_eq!(row_of_commit(&layout, oid(2)), Some(1));
        // A merge base outside the loaded window is not found → caller messages.
        assert_eq!(row_of_commit(&layout, oid(99)), None);
    }

    // ── fork_target decision ───────────────────────────────────────────

    #[test]
    fn feature_commit_forks_at_the_merge_base_with_main() {
        // Selected feature commit F; merge base with main is the fork commit B.
        let f = oid(10);
        let main_tip = oid(1);
        let fork = oid(5);
        let mb = |_a: Oid, _b: Oid| Some(fork);
        assert_eq!(fork_target(f, main_tip, Some(oid(1)), mb), ForkTarget::Jump(fork));
    }

    #[test]
    fn commit_on_main_falls_back_to_the_head_merge_base() {
        // merge_base(selected, main) == selected → on main; then use HEAD.
        let sel = oid(3);
        let main_tip = oid(1);
        let head_tip = oid(7);
        let fork = oid(2);
        let mb = |a: Oid, b: Oid| {
            if b == main_tip {
                Some(sel) // selected is an ancestor of main → base == selected
            } else {
                assert_eq!((a, b), (sel, head_tip));
                Some(fork)
            }
        };
        assert_eq!(
            fork_target(sel, main_tip, Some(head_tip), mb),
            ForkTarget::Jump(fork)
        );
    }

    #[test]
    fn linear_history_reports_no_divergence() {
        // On main AND on HEAD (both bases == selected) → linear.
        let sel = oid(3);
        let mb = |_a: Oid, _b: Oid| Some(sel);
        assert_eq!(fork_target(sel, oid(1), Some(oid(7)), mb), ForkTarget::Linear);
        // Same, but detached HEAD (no current branch) still resolves to linear.
        assert_eq!(fork_target(sel, oid(1), None, mb), ForkTarget::Linear);
    }

    #[test]
    fn unrelated_histories_have_no_base() {
        let mb = |_a: Oid, _b: Oid| None;
        assert_eq!(fork_target(oid(3), oid(1), Some(oid(7)), mb), ForkTarget::NoBase);
    }
}
