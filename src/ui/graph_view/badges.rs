//! PR-badge and merged-badge decisions for the graph view.
//!
//! Pure functions over `PrInfo` / `PrContext` / `Theme`: which open PR (if any)
//! a row should badge, the badge's compact text and color, and the "merged"
//! decoration a landed branch carries. `pr_for_row` returns a typed [`PrBadge`]
//! the render tail draws directly.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};

use super::chips::strip_remote;
use super::MERGE_ICON;
use crate::pr::{CiStatus, PrContext, PrInfo, ReviewState};
use crate::ui::theme::Theme;

/// Nerd Font octicons for the open-PR badge and its actioned markers.
pub(super) const PR_BADGE_ICON: char = '\u{f407}'; // nf-oct-git_pull_request
const PR_APPROVED_ICON: char = '\u{f42e}'; // nf-oct-check
const PR_CHANGES_ICON: char = '\u{f440}'; // nf-oct-diff (±)
const PR_COMMENT_ICON: char = '\u{f41f}'; // nf-oct-comment

/// A rendered PR badge: its compact text (icon, number, and any review/comment
/// markers) and the chip color the CI/merge state resolves to. Plain data the
/// render tail styles and places; the badge *decision* (which PR, what text,
/// what color) is made here.
#[derive(Debug, Clone, PartialEq)]
pub struct PrBadge {
    pub text: String,
    pub color: Color,
}

/// Badge appended to a branch already merged into the trunk (merge or squash).
/// Rendered muted/dimmed; the branch chips themselves are dimmed to match.
/// Derived from [`MERGE_ICON`] so the glyph's codepoint exists in one place.
pub(super) fn merged_badge() -> String {
    format!("{MERGE_ICON} merged")
}

/// Style for merged-branch *decorations* (the "⚡ merged" pill): flat muted grey
/// and dimmed, so a landed branch's badge recedes without disappearing (the
/// hide-merged toggle removes it entirely). Name chips use `merged_chip_style`
/// instead, so they keep their lane hue.
pub(super) fn merged_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.text_muted)
        .add_modifier(Modifier::DIM)
}

/// The first open PR (in branch-label order) whose head branch matches one of
/// this node's branch labels. Remote refs are matched by their stripped name
/// (handles non-origin remotes), so `origin/feat` and a local `feat` both match
/// a PR whose `headRefName` is `feat`.
pub fn pr_for_branch_labels<'p>(
    branch_names: &[String],
    remotes: &[String],
    open_prs: &'p HashMap<String, PrInfo>,
) -> Option<&'p PrInfo> {
    branch_names.iter().find_map(|name| {
        let bare = strip_remote(name, remotes).unwrap_or(name.as_str());
        open_prs.get(bare)
    })
}

/// The badge to draw on a graph row, or `None`. Resolves the row's open PR (see
/// [`pr_for_row_info`]) and packages its compact text and CI/merge color into a
/// [`PrBadge`], so the render tail no longer re-derives either.
pub(super) fn pr_for_row(
    commit_oid: git2::Oid,
    branch_names: &[String],
    remotes: &[String],
    pr_ctx: &PrContext<'_>,
    open_prs: &HashMap<String, PrInfo>,
    theme: &Theme,
) -> Option<PrBadge> {
    let pr = pr_for_row_info(commit_oid, branch_names, remotes, pr_ctx, open_prs)?;
    Some(PrBadge {
        text: pr_badge_text(pr),
        color: pr_badge_color(pr, theme),
    })
}

/// The open PR to badge on a graph row, or `None`. Primary, data-driven rule
/// (#42): the row's commit is a PR's head commit — this pins the badge to
/// exactly one row per PR, even when a local and a remote ref for the same
/// branch sit on different commits. Fallback: a head-branch *name* label, but
/// only for PRs `gh` gave no head OID for. A PR that has a head OID is therefore
/// only ever badged on that exact commit and can never double-render via a
/// branch label, which is what old name-only matching did.
fn pr_for_row_info<'p>(
    commit_oid: git2::Oid,
    branch_names: &[String],
    remotes: &[String],
    pr_ctx: &PrContext<'p>,
    open_prs: &'p HashMap<String, PrInfo>,
) -> Option<&'p PrInfo> {
    if let Some(pr) = pr_ctx.pr_for_head_commit(commit_oid) {
        return Some(pr);
    }
    branch_names.iter().find_map(|name| {
        let bare = strip_remote(name, remotes).unwrap_or(name.as_str());
        open_prs.get(bare).filter(|pr| pr.head_oid.is_none())
    })
}

/// Compact badge text for an open PR, e.g. ` #12 ✓ ` (approved with outside
/// comments). Review marker first (approved / changes-requested), then a
/// comment marker when a non-author has commented.
fn pr_badge_text(pr: &PrInfo) -> String {
    let mut s = format!("{} #{}", PR_BADGE_ICON, pr.number);
    match pr.review {
        ReviewState::Approved => {
            s.push(' ');
            s.push(PR_APPROVED_ICON);
        }
        ReviewState::ChangesRequested => {
            s.push(' ');
            s.push(PR_CHANGES_ICON);
        }
        ReviewState::None => {}
    }
    if pr.outside_activity {
        s.push(' ');
        s.push(PR_COMMENT_ICON);
    }
    s
}

/// Badge chip color across four states (#88): failing checks (red) and running
/// checks (orange) take precedence over merge readiness — a red/pending PR isn't
/// mergeable anyway. Only once checks are green does merge readiness split the
/// tone: full green when clear to merge, chartreuse when passing-but-blocked
/// (changes requested, conflicts, draft, behind base). No checks → neutral blue.
/// Pure and frame-free so the decision can be unit-tested directly.
fn pr_badge_color(pr: &PrInfo, theme: &Theme) -> Color {
    match pr.ci {
        CiStatus::None => theme.pr_badge,
        CiStatus::Fail => theme.pr_ci_fail,
        CiStatus::Pending => theme.pr_ci_pending,
        CiStatus::Pass if pr.is_merge_blocked() => theme.pr_ci_pass_blocked,
        CiStatus::Pass => theme.pr_ci_pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::MergeState;
    use crate::ui::graph_view::display_width;

    fn pr(number: u64) -> PrInfo {
        PrInfo {
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: "t".to_string(),
            ci: CiStatus::None,
            review: ReviewState::None,
            merge_state: MergeState::Clear,
            outside_activity: false,
            head_oid: None,
            base_ref: None,
        }
    }

    fn pr_with(number: u64, ci: CiStatus, review: ReviewState, outside: bool) -> PrInfo {
        pr_full(number, ci, review, MergeState::Clear, outside)
    }

    fn pr_full(
        number: u64,
        ci: CiStatus,
        review: ReviewState,
        merge_state: MergeState,
        outside: bool,
    ) -> PrInfo {
        PrInfo {
            number,
            url: "u".to_string(),
            title: "t".to_string(),
            ci,
            review,
            merge_state,
            outside_activity: outside,
            head_oid: None,
            base_ref: None,
        }
    }

    fn prs(pairs: &[(&str, u64)]) -> HashMap<String, PrInfo> {
        pairs.iter().map(|(b, n)| (b.to_string(), pr(*n))).collect()
    }

    fn oid(b: u8) -> git2::Oid {
        git2::Oid::from_bytes(&[b; 20]).unwrap()
    }

    #[test]
    fn pr_matches_local_branch_label() {
        let open = prs(&[("feat/x", 12)]);
        let names = vec!["feat/x".to_string()];
        let found = pr_for_branch_labels(&names, &[], &open);
        assert_eq!(found.map(|p| p.number), Some(12));
    }

    #[test]
    fn pr_matches_remote_ref_by_stripped_name() {
        let open = prs(&[("feat/x", 3)]);
        // Both origin and a non-origin remote strip to the PR's head branch.
        let remotes = vec!["origin".to_string(), "upstream".to_string()];
        assert_eq!(
            pr_for_branch_labels(&["origin/feat/x".to_string()], &remotes, &open).map(|p| p.number),
            Some(3)
        );
        assert_eq!(
            pr_for_branch_labels(&["upstream/feat/x".to_string()], &remotes, &open)
                .map(|p| p.number),
            Some(3)
        );
    }

    #[test]
    fn pr_no_match_returns_none() {
        let open = prs(&[("feat/x", 1)]);
        assert!(pr_for_branch_labels(&["other".to_string()], &[], &open).is_none());
        // A slashed local branch is not stripped, so it won't accidentally match
        // a PR whose head is the trailing segment.
        let open2 = prs(&[("x", 9)]);
        assert!(
            pr_for_branch_labels(&["feature/x".to_string()], &["origin".to_string()], &open2)
                .is_none()
        );
    }

    #[test]
    fn pr_first_matching_label_wins() {
        let open = prs(&[("b", 2), ("a", 1)]);
        // Labels checked in order; "a" comes first, so PR #1.
        let names = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            pr_for_branch_labels(&names, &[], &open).map(|p| p.number),
            Some(1)
        );
    }

    #[test]
    fn pr_badge_text_is_compact() {
        let text = pr_badge_text(&pr(42));
        assert!(text.contains("#42"));
        assert!(text.starts_with(PR_BADGE_ICON));
        // Icon(1) + space(1) + "#42"(3) = 5 display columns.
        assert_eq!(display_width(&text), 5);
    }

    #[test]
    fn pr_badge_appends_review_then_comment_markers() {
        // Plain: no markers.
        let plain = pr_badge_text(&pr_with(1, CiStatus::Pass, ReviewState::None, false));
        assert!(!plain.contains(PR_APPROVED_ICON));
        assert!(!plain.contains(PR_COMMENT_ICON));

        // Approved → check glyph; changes-requested → diff glyph (mutually exclusive).
        let approved = pr_badge_text(&pr_with(1, CiStatus::Pass, ReviewState::Approved, false));
        assert!(approved.contains(PR_APPROVED_ICON));
        assert!(!approved.contains(PR_CHANGES_ICON));
        let changes =
            pr_badge_text(&pr_with(1, CiStatus::Fail, ReviewState::ChangesRequested, false));
        assert!(changes.contains(PR_CHANGES_ICON));
        assert!(!changes.contains(PR_APPROVED_ICON));

        // Outside comment → comment glyph, appended after the review marker.
        let both = pr_badge_text(&pr_with(12, CiStatus::Pass, ReviewState::Approved, true));
        assert!(both.contains(PR_APPROVED_ICON) && both.contains(PR_COMMENT_ICON));
        let check_at = both.find(PR_APPROVED_ICON).unwrap();
        let comment_at = both.find(PR_COMMENT_ICON).unwrap();
        assert!(check_at < comment_at, "review marker precedes comment: {both:?}");
        // Icon(1)+" #12"(4) + " ✓"(2) + " ⌘"(2) = 9 columns.
        assert_eq!(display_width(&both), 9);
    }

    #[test]
    fn pr_badge_color_follows_ci_status() {
        let theme = Theme::dark();
        assert_eq!(
            pr_badge_color(&pr_with(1, CiStatus::None, ReviewState::None, false), &theme),
            theme.pr_badge
        );
        assert_eq!(
            pr_badge_color(&pr_with(1, CiStatus::Pass, ReviewState::None, false), &theme),
            theme.pr_ci_pass
        );
        assert_eq!(
            pr_badge_color(&pr_with(1, CiStatus::Pending, ReviewState::None, false), &theme),
            theme.pr_ci_pending
        );
        assert_eq!(
            pr_badge_color(&pr_with(1, CiStatus::Fail, ReviewState::None, false), &theme),
            theme.pr_ci_fail
        );
    }

    #[test]
    fn pr_badge_color_splits_passing_by_merge_readiness() {
        let theme = Theme::dark();
        // Green checks + clear to merge → full green.
        let clear = pr_full(1, CiStatus::Pass, ReviewState::None, MergeState::Clear, false);
        assert_eq!(pr_badge_color(&clear, &theme), theme.pr_ci_pass);
        // Green checks but a blocking merge state → chartreuse "passing-but-blocked".
        let blocked = pr_full(1, CiStatus::Pass, ReviewState::None, MergeState::Blocked, false);
        assert_eq!(pr_badge_color(&blocked, &theme), theme.pr_ci_pass_blocked);
        // Green checks but changes requested → also chartreuse, via the review path.
        let changes = pr_full(
            1,
            CiStatus::Pass,
            ReviewState::ChangesRequested,
            MergeState::Clear,
            false,
        );
        assert_eq!(pr_badge_color(&changes, &theme), theme.pr_ci_pass_blocked);
        // The blocked tone is distinct from both full-green and pending.
        assert_ne!(theme.pr_ci_pass_blocked, theme.pr_ci_pass);
        assert_ne!(theme.pr_ci_pass_blocked, theme.pr_ci_pending);
    }

    #[test]
    fn pr_badge_color_ci_precedes_merge_state() {
        // A blocked merge state must not override failing/pending CI: those states
        // aren't mergeable regardless, so red/orange still win.
        let theme = Theme::dark();
        let fail_blocked = pr_full(
            1,
            CiStatus::Fail,
            ReviewState::ChangesRequested,
            MergeState::Blocked,
            false,
        );
        assert_eq!(pr_badge_color(&fail_blocked, &theme), theme.pr_ci_fail);
        let pending_blocked = pr_full(
            1,
            CiStatus::Pending,
            ReviewState::ChangesRequested,
            MergeState::Blocked,
            false,
        );
        assert_eq!(pr_badge_color(&pending_blocked, &theme), theme.pr_ci_pending);
    }

    #[test]
    fn pr_badge_encodes_approved_and_comment_state() {
        // #43: the badge for a PR resolved by head commit encodes the approved +
        // outside-comment markers. Asserts the badge *decision* (text) directly
        // rather than scanning a full render.
        let theme = Theme::dark();
        let mut approved = pr(1);
        approved.head_oid = Some(oid(5).to_string());
        approved.review = ReviewState::Approved;
        approved.outside_activity = true;
        let open: HashMap<String, PrInfo> = [("feat".to_string(), approved)].into_iter().collect();
        let pr_ctx = PrContext::new(&open);

        // Resolved by OID (the head commit), with no branch label needed.
        let badge = pr_for_row(oid(5), &[], &[], &pr_ctx, &open, &theme)
            .expect("head commit resolves a badge");
        assert!(
            badge.text.contains(PR_APPROVED_ICON),
            "approved glyph present: {:?}",
            badge.text
        );
        assert!(
            badge.text.contains(PR_COMMENT_ICON),
            "comment glyph present: {:?}",
            badge.text
        );
    }

    #[test]
    fn pr_for_row_with_no_labels_is_none() {
        // No branch labels AND a commit OID that is no PR's head → no badge.
        let theme = Theme::dark();
        let mut p = pr(1);
        p.head_oid = Some(oid(5).to_string());
        let open: HashMap<String, PrInfo> = [("feat".to_string(), p)].into_iter().collect();
        let pr_ctx = PrContext::new(&open);
        // oid(9) is not the PR's head (5), and there is no branch label to fall
        // back on, so nothing resolves.
        assert!(pr_for_row(oid(9), &[], &[], &pr_ctx, &open, &theme).is_none());

        // An empty open-PR map with empty labels is likewise None.
        let empty: HashMap<String, PrInfo> = HashMap::new();
        let empty_ctx = PrContext::new(&empty);
        assert!(pr_for_row(oid(9), &[], &[], &empty_ctx, &empty, &theme).is_none());
    }
}
