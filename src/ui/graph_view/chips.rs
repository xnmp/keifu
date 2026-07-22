//! Branch-chip construction for the graph view.
//!
//! `optimize_branch_display` turns a commit's raw branch names into the compact
//! `[name]` chips the row renders — collapsing synced local/remote pairs, adding
//! remote/synced icons, deduping remote twins, budgeting width, and folding the
//! overflow into a `+N` marker. Each resulting [`BranchChip`] also carries the
//! real branch name it resolves to, so the render tail and mouse hit-testing no
//! longer re-derive it from the decorated label.

use ratatui::style::{Modifier, Style};

use super::metrics::{display_width, truncate_to_width};
use crate::ui::theme::Theme;

/// Nerd Font cloud glyph marking a branch that only exists on a remote.
pub const REMOTE_ONLY_ICON: &str = "\u{f0c2}"; //
/// Marks a local branch whose remote counterpart points at the same commit.
pub(super) const SYNCED_ICON: &str = "↔";

/// A rendered branch chip: its decorated label, the style it draws with, and the
/// real branch name it resolves to (for click hit-testing), or `None` when the
/// chip decoration maps to no branch. Plain data feeding the render tail — the
/// resolved `branch` is computed once, at construction, from the same inputs the
/// render tail used to re-derive it, so hit-testing is unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchChip {
    pub label: String,
    pub style: Style,
    pub branch: Option<String>,
}

/// Strip a real remote prefix (e.g. "origin/", "upstream/") from a ref,
/// returning the bare branch name. Local branches that merely contain a slash
/// ("feature/foo") are not remote refs.
pub(super) fn strip_remote<'n>(name: &'n str, remotes: &[String]) -> Option<&'n str> {
    remotes.iter().find_map(|r| {
        name.strip_prefix(r.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
    })
}

/// Single source of truth for whether graph label chips drop the remote prefix:
/// only when the repo has exactly one remote (the cloud icon then conveys
/// remoteness, so `<remote>/` is redundant). Multi-remote repos keep prefixes to
/// disambiguate which remote a ref belongs to.
fn strip_prefix_in_labels(remotes: &[String]) -> bool {
    remotes.len() == 1
}

/// The name shown on a branch chip: for a remote ref in a single-remote repo,
/// the remote prefix is dropped; otherwise the full ref name is used.
fn chip_display_name<'n>(name: &'n str, remotes: &[String]) -> &'n str {
    if strip_prefix_in_labels(remotes) {
        strip_remote(name, remotes).unwrap_or(name)
    } else {
        name
    }
}

/// Optimize branch name display, returning one [`BranchChip`] per rendered chip
/// (currently always zero or one, as multi-branch rows collapse into a single
/// combined chip). Each chip carries its resolved real branch name for click
/// hit-testing.
///
/// - A lone remote ref gets a cloud icon; a local+remote pair collapses to one ↔ label
/// - If a local branch matches its origin/xxx among other branches, show "xxx <-> origin"
/// - Otherwise, show each name separately
/// - Render in bold with the graph color, wrapped in brackets
/// - Selected branch is shown with inverted colors
pub(super) fn optimize_branch_display(
    branch_names: &[String],
    is_head: bool,
    color_index: usize,
    selected_branch_name: Option<&str>,
    theme: &Theme,
    remotes: &[String],
) -> Vec<BranchChip> {
    // Build the decorated (label, style) pairs, then fold in the click-resolution
    // that used to run at render time so each chip carries its real branch name.
    // Resolution uses the same inputs (`label`, `branch_names`, `remotes`) the
    // render tail passed to `resolve_chip_branch`, so hit-testing is unchanged.
    optimize_branch_labels(
        branch_names,
        is_head,
        color_index,
        selected_branch_name,
        theme,
        remotes,
    )
    .into_iter()
    .map(|(label, style)| {
        let branch = resolve_chip_branch(&label, branch_names, remotes);
        BranchChip {
            label,
            style,
            branch,
        }
    })
    .collect()
}

/// The decorated `(label, style)` pairs before branch resolution. Kept as a
/// separate function so the layout logic is untouched by the typed-chip return.
fn optimize_branch_labels(
    branch_names: &[String],
    is_head: bool,
    color_index: usize,
    selected_branch_name: Option<&str>,
    theme: &Theme,
    remotes: &[String],
) -> Vec<(String, Style)> {
    use std::collections::HashSet;

    if branch_names.is_empty() {
        return Vec::new();
    }

    // Max width for a single branch label (e.g., "[fix/feature-name]")
    const MAX_LABEL_WIDTH: usize = 40;

    // Split local and remote branches by real remote prefix (HashSet for O(1)
    // lookup). A name is remote only when it starts with a configured remote
    // (e.g. "origin/", "upstream/"), never merely for containing a slash.
    let local_branches: HashSet<&str> = branch_names
        .iter()
        .filter(|n| strip_remote(n, remotes).is_none())
        .map(|s| s.as_str())
        .collect();

    // Badge color always resolves through the same palette lookup as the
    // graph line/node for this commit's lane (`theme.lane_color`), so a
    // branch's badge and its line never diverge. HEAD is distinguished by
    // weight (bold, below), not by a separate color.
    let base_color = theme.lane_color(color_index);

    // Helper to create style based on selection state. Restraint: color is the
    // single emphasis device for ordinary chips; only the checked-out HEAD's
    // chips also carry bold, reserving the stronger accent for the one ref that
    // matters most. Selection adds REVERSED on top (orthogonal affordance).
    let make_style = |branch_name: &str| -> Style {
        let mut style = Style::default().fg(base_color);
        if is_head {
            style = style.add_modifier(Modifier::BOLD);
        }
        if selected_branch_name == Some(branch_name) {
            // Reverse video rather than an explicit bg: when this branch's row
            // is also the highlighted row, the list's highlight_style patches a
            // bg over the whole line, which would clobber an explicit bg and
            // leave the label invisible. REVERSED is resolved after that patch,
            // so the selected branch stays legible on any terminal theme.
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    };

    // Helper to create label with optional icon prefix and abbreviation
    let make_label = |prefix: &str, name: &str, suffix: Option<&str>| -> String {
        let prefix_width = display_width(prefix);
        let (label, abbrev_width) = if let Some(s) = suffix {
            (
                format!("[{}{} {}]", prefix, name, s),
                MAX_LABEL_WIDTH.saturating_sub(s.len() + 3 + prefix_width),
            )
        } else {
            (
                format!("[{}{}]", prefix, name),
                MAX_LABEL_WIDTH.saturating_sub(prefix_width),
            )
        };

        if display_width(&label) <= MAX_LABEL_WIDTH {
            return label;
        }

        let abbrev = abbreviate_branch_label(name, abbrev_width, 0);
        let abbrev = if prefix.is_empty() {
            abbrev
        } else {
            abbrev.replacen('[', &format!("[{}", prefix), 1)
        };
        if let Some(s) = suffix {
            abbrev.replace(']', &format!(" {}]", s))
        } else {
            abbrev
        }
    };

    // Single-branch icon labels (VSCode Git Graph style): a lone remote ref
    // gets a cloud icon; a local ref whose remote counterpart sits on the same
    // commit collapses into one ↔-prefixed label. Commits carrying more than
    // one distinct branch keep the standard rendering below.
    if branch_names.len() == 1 {
        let name = &branch_names[0];
        if strip_remote(name, remotes).is_some() {
            let prefix = format!("{} ", REMOTE_ONLY_ICON);
            // Single-remote repos drop the "<remote>/" prefix (cloud conveys it).
            let display = chip_display_name(name, remotes);
            return vec![(make_label(&prefix, display, None), make_style(name))];
        }
    } else if branch_names.len() == 2 {
        let synced_local = branch_names.iter().find(|name| {
            strip_remote(name, remotes).is_none()
                && branch_names
                    .iter()
                    .any(|other| strip_remote(other, remotes) == Some(name.as_str()))
        });
        if let Some(local) = synced_local {
            let prefix = format!("{} ", SYNCED_ICON);
            return vec![(make_label(&prefix, local, None), make_style(local))];
        }
    }

    // Process branches in original order (matches tab order from filter_remote_duplicates)
    let mut result: Vec<(String, Style)> = Vec::new();
    for name in branch_names {
        if let Some(bare) = strip_remote(name, remotes) {
            // Remote branch: skip if matching local exists (dedup keeps a
            // stripped remote-only chip from colliding with its local twin).
            if local_branches.contains(bare) {
                continue;
            }
            if strip_prefix_in_labels(remotes) {
                // Single remote: drop the prefix but add the cloud icon so this
                // remote-only chip still reads as remote in a multi-branch row.
                let prefix = format!("{} ", REMOTE_ONLY_ICON);
                result.push((make_label(&prefix, bare, None), make_style(name)));
            } else {
                result.push((make_label("", name, None), make_style(name)));
            }
        } else {
            // Local branch: mark with the ↔ icon (same convention as the
            // single-branch synced chip) when a remote ref points at the same
            // bare name — dropping the redundant "↔ <remote>" text suffix.
            let has_synced_remote = branch_names
                .iter()
                .any(|other| strip_remote(other, remotes) == Some(name.as_str()));
            let prefix = if has_synced_remote {
                format!("{} ", SYNCED_ICON)
            } else {
                String::new()
            };
            result.push((make_label(&prefix, name, None), make_style(name)));
        }
    }

    // Collapse multiple branches: show up to two labels, then "+N" for the rest
    if result.len() > 1 {
        // Number of labels to display inline before collapsing the remainder
        const SHOWN_LABELS: usize = 2;

        let shown = SHOWN_LABELS.min(result.len());
        let extra_count = result.len() - shown;

        // Helper: split a formatted label into its leading icon prefix (cloud
        // for a remote-only chip, ↔ for a synced local, or empty) and the bare
        // branch name. The name is recovered for re-abbreviation; the icon is
        // returned separately so the collapse below can re-attach it — a
        // multi-branch row must keep its remote/synced markers, not drop them.
        let split_label = |label: &str| -> (String, String) {
            let s = label.trim_start_matches('[');
            let (icon, rest) = if let Some(r) = s.strip_prefix(REMOTE_ONLY_ICON) {
                (format!("{} ", REMOTE_ONLY_ICON), r.trim_start())
            } else if let Some(r) = s.strip_prefix(SYNCED_ICON) {
                (format!("{} ", SYNCED_ICON), r.trim_start())
            } else {
                (String::new(), s)
            };
            let bare = rest.split([']', ' ']).next().unwrap_or(label).to_string();
            (icon, bare)
        };

        // Budget the available width across the shown labels
        let per_label = MAX_LABEL_WIDTH / shown;

        // Display order: always the stable badge order from `result` (which
        // in turn follows `branch_names`, itself deterministically sorted at
        // the source — see `BranchInfo::list_all`). Never reordered by which
        // branch is selected/navigated to: that would make badge order flap
        // as the cursor moves, which is the bug this fixes.
        let mut combined = String::new();
        for (pos, (label, _)) in result.iter().take(shown).enumerate() {
            let (icon, clean_name) = split_label(label);
            // Only the last shown label carries the "+N" suffix
            let extra = if pos == shown - 1 { extra_count } else { 0 };
            // Reserve budget for the icon so the abbreviated name plus its
            // re-attached marker still fits the per-label allowance.
            let budget = per_label.saturating_sub(display_width(&icon));
            let abbrev = abbreviate_branch_label(&clean_name, budget, extra);
            // Re-attach the icon marker inside the brackets (mirrors make_label).
            let abbrev = if icon.is_empty() {
                abbrev
            } else {
                abbrev.replacen('[', &format!("[{}", icon), 1)
            };
            combined.push_str(&abbrev);
        }

        // Style follows the selected branch when it's among these labels
        // (highlight only — does not affect display order above).
        let selected_idx = selected_branch_name
            .and_then(|sel| {
                branch_names
                    .iter()
                    .position(|n| n == sel || n.ends_with(&format!("/{}", sel)))
            })
            .unwrap_or(0)
            .min(result.len().saturating_sub(1));
        let style = result[selected_idx].1;
        return vec![(combined, style)];
    }

    result
}

/// Abbreviate branch name to max_width, showing "+N" if more branches exist
/// Uses format: prefix/head...tail (preserving last 5 chars)
fn abbreviate_branch_label(name: &str, max_width: usize, extra_count: usize) -> String {
    const TAIL_LEN: usize = 5;
    const ELLIPSIS: &str = "...";

    let suffix = if extra_count > 0 {
        format!(" +{}", extra_count)
    } else {
        String::new()
    };

    let suffix_len = display_width(&suffix);
    let available = max_width.saturating_sub(suffix_len).saturating_sub(2); // -2 for brackets

    // If name fits, return as-is
    if display_width(name) <= available {
        return format!("[{}]{}", name, suffix);
    }

    // Find "/" position to preserve prefix
    let slash_pos = name.find('/');

    // Split into prefix and rest
    let (prefix, rest) = match slash_pos {
        Some(pos) => (&name[..=pos], &name[pos + 1..]),
        None => ("", name),
    };

    let prefix_width = display_width(prefix);
    let ellipsis_width = display_width(ELLIPSIS);

    // Get last TAIL_LEN characters from rest
    let rest_chars: Vec<char> = rest.chars().collect();
    let tail: String = if rest_chars.len() > TAIL_LEN {
        rest_chars[rest_chars.len() - TAIL_LEN..].iter().collect()
    } else {
        rest.to_string()
    };
    let tail_width = display_width(&tail);

    // Calculate available width for head portion
    let head_available = available.saturating_sub(prefix_width + ellipsis_width + tail_width);

    if head_available == 0 {
        // Not enough space for head, just show truncated name
        let truncated = truncate_to_width(name, available.saturating_sub(3));
        return format!("[{}...]{}", truncated, suffix);
    }

    let head = truncate_to_width(rest, head_available);

    format!("[{}{}{}{}]{}", prefix, head, ELLIPSIS, tail, suffix)
}

/// Recover the branch name a rendered chip `label` refers to, matching it
/// against the node's `branch_names`. Chip labels are decorated (`[name]`, an
/// optional remote/synced icon prefix, a possible ` +N` overflow suffix), so we
/// strip the decoration to a bare name and find the branch whose bare form
/// matches (a local branch, or a remote ref bare-equal to it). Returns `None`
/// when nothing matches (e.g. a non-branch decoration).
fn resolve_chip_branch(label: &str, branch_names: &[String], remotes: &[String]) -> Option<String> {
    // Strip the leading '[' and any icon prefix, then take up to the first
    // delimiter (']', ' ', or the start of a "+N" overflow marker).
    let s = label.trim_start_matches('[');
    let s = s
        .strip_prefix(REMOTE_ONLY_ICON)
        .or_else(|| s.strip_prefix(SYNCED_ICON))
        .map(str::trim_start)
        .unwrap_or(s);
    let bare = s.split([']', ' ']).next().unwrap_or(s);
    if bare.is_empty() {
        return None;
    }
    // Exact local match first, then a remote ref whose bare name matches.
    branch_names
        .iter()
        .find(|n| n.as_str() == bare)
        .or_else(|| {
            branch_names
                .iter()
                .find(|n| strip_remote(n, remotes) == Some(bare))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;

    // ── optimize_branch_display icons ────────────────────────────────

    fn labels(branch_names: &[&str], remotes: &[&str]) -> Vec<String> {
        let names: Vec<String> = branch_names.iter().map(|s| s.to_string()).collect();
        let remotes: Vec<String> = remotes.iter().map(|s| s.to_string()).collect();
        let theme = Theme::dark();
        optimize_branch_display(&names, false, 0, None, &theme, &remotes)
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    #[test]
    fn lone_remote_ref_gets_a_cloud_icon() {
        // Single-remote repo: the "origin/" prefix is dropped (cloud conveys it).
        let out = labels(&["origin/feature"], &["origin"]);
        assert_eq!(out, vec![format!("[{} feature]", REMOTE_ONLY_ICON)]);
    }

    #[test]
    fn lone_remote_ref_respects_non_origin_remotes() {
        // Multi-remote repo: the prefix is kept to disambiguate the remote.
        let out = labels(&["upstream/main"], &["origin", "upstream"]);
        assert_eq!(out, vec![format!("[{} upstream/main]", REMOTE_ONLY_ICON)]);
    }

    #[test]
    fn synced_local_and_remote_collapse_to_one_sync_label() {
        let out = labels(&["main", "origin/main"], &["origin"]);
        assert_eq!(out, vec![format!("[{} main]", SYNCED_ICON)]);
    }

    #[test]
    fn synced_pair_order_does_not_matter() {
        let out = labels(&["origin/main", "main"], &["origin"]);
        assert_eq!(out, vec![format!("[{} main]", SYNCED_ICON)]);
    }

    #[test]
    fn slashed_local_branch_is_not_mistaken_for_a_remote_ref() {
        let out = labels(&["feature/foo"], &["origin"]);
        assert_eq!(out, vec!["[feature/foo]".to_string()]);
    }

    #[test]
    fn multiple_distinct_branches_get_no_icons() {
        let out = labels(&["main", "dev"], &["origin"]);
        for label in &out {
            assert!(!label.contains(REMOTE_ONLY_ICON), "no cloud icon: {label}");
            assert!(!label.contains(SYNCED_ICON), "no sync icon: {label}");
        }
    }

    #[test]
    fn two_unrelated_refs_do_not_collapse() {
        // A local `main` and an unrelated remote `origin/dev` (no local twin):
        // they must NOT merge into a single ↔ synced label, but the remote-only
        // ref still carries its cloud marker (issue #74 — the cloud must not be
        // dropped just because another chip shares the row).
        let out = labels(&["main", "origin/dev"], &["origin"]);
        assert!(
            !out.iter().any(|l| l.contains(SYNCED_ICON)),
            "unrelated local+remote must not be treated as synced: {out:?}"
        );
        let joined = out.join("");
        assert!(
            joined.contains(REMOTE_ONLY_ICON),
            "remote-only ref keeps its cloud: {out:?}"
        );
        assert!(joined.contains("main") && joined.contains("dev"), "{out:?}");
    }

    #[test]
    fn long_remote_ref_with_icon_stays_within_label_budget() {
        let long = format!("origin/{}", "x".repeat(60));
        let out = labels(&[&long], &["origin"]);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with(&format!("[{} ", REMOTE_ONLY_ICON)));
        assert!(display_width(&out[0]) <= 40, "label too wide: {}", out[0]);
    }

    // ── chip click resolution (resolve_chip_branch) ──────────────────────

    #[test]
    fn resolve_chip_branch_recovers_the_branch_name() {
        let remotes = vec!["origin".to_string()];
        let names = vec!["main".to_string(), "feature/x".to_string()];
        // Plain local label.
        assert_eq!(
            resolve_chip_branch("[main]", &names, &remotes).as_deref(),
            Some("main")
        );
        // Label with a slash in the name.
        assert_eq!(
            resolve_chip_branch("[feature/x]", &names, &remotes).as_deref(),
            Some("feature/x")
        );
        // Synced-icon prefix is stripped before matching.
        let synced = format!("[{} main]", SYNCED_ICON);
        assert_eq!(
            resolve_chip_branch(&synced, &names, &remotes).as_deref(),
            Some("main")
        );
        // A cloud-icon remote-only chip (single-remote repo drops the prefix)
        // resolves back to the full remote ref.
        let remote_names = vec!["origin/dev".to_string()];
        let cloud = format!("[{} dev]", REMOTE_ONLY_ICON);
        assert_eq!(
            resolve_chip_branch(&cloud, &remote_names, &remotes).as_deref(),
            Some("origin/dev")
        );
        // No matching branch → None.
        assert_eq!(resolve_chip_branch("[nope]", &names, &remotes), None);
    }

    #[test]
    fn optimize_branch_display_chip_carries_resolved_branch() {
        // The click-resolution folded into chip construction (#77): each chip's
        // `branch` is the real ref name a click on it checks out — the same
        // value the render tail used to re-derive via `resolve_chip_branch`.
        let theme = Theme::dark();
        let remotes = vec!["origin".to_string()];

        // Lone remote ref (single remote drops the prefix on the label, but the
        // chip still resolves back to the full remote ref).
        let out = optimize_branch_display(
            &["origin/dev".to_string()],
            false,
            0,
            None,
            &theme,
            &remotes,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].branch.as_deref(), Some("origin/dev"));

        // Synced local+remote pair resolves to the local branch.
        let out = optimize_branch_display(
            &["main".to_string(), "origin/main".to_string()],
            false,
            0,
            None,
            &theme,
            &remotes,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].branch.as_deref(), Some("main"));

        // Collapsed multi-branch chip resolves to the first shown branch.
        let out = optimize_branch_display(
            &["alpha".to_string(), "beta".to_string()],
            false,
            0,
            None,
            &theme,
            &remotes,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].branch.as_deref(), Some("alpha"));
    }

    // ── remote classification (strip_remote / optimize_branch_display) ───

    #[test]
    fn strip_remote_classifies_by_configured_remote_only() {
        let remotes = vec!["origin".to_string(), "upstream".to_string()];
        assert_eq!(strip_remote("upstream/main", &remotes), Some("main"));
        assert_eq!(strip_remote("origin/feature/x", &remotes), Some("feature/x"));
        // A slash alone does not make a branch remote.
        assert_eq!(strip_remote("feature/x", &remotes), None);
        assert_eq!(strip_remote("main", &remotes), None);
        // Not a configured remote.
        assert_eq!(strip_remote("fork/main", &remotes), None);
    }

    #[test]
    fn multi_branch_dedupes_non_origin_remote_counterpart() {
        let theme = Theme::dark();
        // main + its upstream counterpart + an unrelated local branch. Three
        // names skip the single/pair early-returns and hit the general loop.
        let names = vec![
            "main".to_string(),
            "upstream/main".to_string(),
            "dev".to_string(),
        ];
        let remotes = vec!["upstream".to_string()];
        let out = optimize_branch_display(&names, false, 0, None, &theme, &remotes);
        // Collapses to a single combined label.
        assert_eq!(out.len(), 1);
        let label = &out[0].label;
        // upstream/main is recognized as main's remote counterpart and deduped,
        // leaving two real branches — so no "+N" overflow marker appears.
        assert!(
            !label.contains('+'),
            "non-origin remote counterpart should be deduped: {label:?}"
        );
        assert!(label.contains("main"), "expected main: {label:?}");
        assert!(label.contains("dev"), "expected dev: {label:?}");
    }

    #[test]
    fn slashed_local_branch_is_not_treated_as_remote() {
        let theme = Theme::dark();
        // "feature/x" contains a slash but "feature" is not a remote.
        let names = vec!["feature/x".to_string()];
        let remotes = vec!["origin".to_string()];
        let out = optimize_branch_display(&names, false, 0, None, &theme, &remotes);
        assert_eq!(out.len(), 1);
        // No cloud icon (remote-only marker); rendered as a plain local label.
        assert!(
            !out[0].label.contains(REMOTE_ONLY_ICON),
            "local branch must not get the remote icon: {:?}",
            out[0].label
        );
    }

    // ── single-remote prefix stripping on labels ─────────────────────

    #[test]
    fn single_remote_strips_prefix_and_keeps_cloud_icon() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["origin/feat".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].label;
        assert!(label.contains(REMOTE_ONLY_ICON), "cloud icon kept: {label:?}");
        assert!(label.contains("feat"));
        assert!(!label.contains("origin/"), "prefix stripped: {label:?}");
    }

    #[test]
    fn upstream_only_remote_strips_prefix() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["upstream/main".to_string()],
            false,
            0,
            None,
            &theme,
            &["upstream".to_string()],
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].label.contains("main"));
        assert!(!out[0].label.contains("upstream/"), "{:?}", out[0].label);
    }

    #[test]
    fn multi_remote_keeps_prefix() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["origin/feat".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string(), "upstream".to_string()],
        );
        assert_eq!(out.len(), 1);
        assert!(
            out[0].label.contains("origin/feat"),
            "multi-remote keeps prefix: {:?}",
            out[0].label
        );
    }

    #[test]
    fn single_remote_synced_pair_still_collapses() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["main".to_string(), "origin/main".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        // The local+remote pair collapses to one ↔ chip — no duplicate [main].
        assert_eq!(out.len(), 1);
        assert!(out[0].label.contains("main"));
        assert!(
            out[0].label.contains(SYNCED_ICON),
            "synced icon: {:?}",
            out[0].label
        );
        assert!(!out[0].label.contains("origin/"));
    }

    #[test]
    fn single_remote_multi_branch_dedups_without_duplicate_or_prefix() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &[
                "foo".to_string(),
                "origin/foo".to_string(),
                "bar".to_string(),
            ],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].label;
        // origin/foo is the remote twin of local foo → deduped, no duplicate and
        // no leftover prefix; two real branches remain, so no "+N".
        assert!(!label.contains("origin/"), "{label:?}");
        assert!(!label.contains('+'), "no overflow marker: {label:?}");
        assert!(label.contains("foo"));
        assert!(label.contains("bar"));
    }

    #[test]
    fn single_remote_remote_only_chip_in_multi_branch_resolves_to_name() {
        let theme = Theme::dark();
        // A remote-only ref alongside a local branch: the combined label must
        // show the stripped name, never the bare cloud glyph.
        let out = optimize_branch_display(
            &["origin/lonely".to_string(), "bar".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].label;
        assert!(label.contains("lonely"), "stripped name present: {label:?}");
        assert!(!label.contains("origin/"), "{label:?}");
    }

    // ── issue #74: multi-branch rows keep their cloud/synced markers ──
    //
    // Regression: the collapse path (result.len() > 1) rebuilt each chip from
    // the cleaned bare name and dropped the icon prefix, so a row with two
    // remote refs — or a synced pair alongside another branch — rendered as
    // bare local-looking labels (`[mac][main]`). Assert the markers survive.

    #[test]
    fn two_remote_refs_on_one_commit_both_keep_cloud_icon() {
        // The #74 repro: origin/mac + origin/main sit together on an older
        // commit (no local ref there). Single remote → prefix dropped, but each
        // chip must carry the cloud so it still reads as remote-only.
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["origin/mac".to_string(), "origin/main".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].label;
        assert!(label.contains("mac") && label.contains("main"), "{label:?}");
        assert!(!label.contains("origin/"), "prefix dropped: {label:?}");
        // Two cloud glyphs — one per remote-only chip.
        assert_eq!(
            label.matches(REMOTE_ONLY_ICON).count(),
            2,
            "cloud on both remote chips: {label:?}"
        );
        assert!(!label.contains(SYNCED_ICON), "not synced: {label:?}");
    }

    #[test]
    fn synced_pair_in_multi_branch_row_keeps_synced_icon() {
        // A synced local+remote pair (main / origin/main) alongside an
        // unrelated local branch: the pair collapses to one ↔ chip and the ↔
        // marker must survive the multi-branch collapse.
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &[
                "main".to_string(),
                "origin/main".to_string(),
                "dev".to_string(),
            ],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].label;
        assert!(label.contains(SYNCED_ICON), "synced marker kept: {label:?}");
        assert!(label.contains("main") && label.contains("dev"), "{label:?}");
        assert!(!label.contains("origin/"), "{label:?}");
    }

    #[test]
    fn local_only_multi_branch_row_never_gets_a_cloud() {
        // Two purely local branches with no remote counterparts: neither chip
        // may acquire a cloud (or synced) marker.
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["feature".to_string(), "hotfix".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].label;
        assert!(
            !label.contains(REMOTE_ONLY_ICON),
            "no cloud on local chips: {label:?}"
        );
        assert!(!label.contains(SYNCED_ICON), "no synced marker: {label:?}");
        assert!(label.contains("feature") && label.contains("hotfix"), "{label:?}");
    }

    // ── badge order is stable regardless of selection (issue #50) ────

    #[test]
    fn badge_order_is_independent_of_which_branch_is_selected() {
        // Three branches on one commit — more than SHOWN_LABELS (2), so this
        // also exercises the collapse path. Regression: selecting each
        // branch in turn (as branch-cycling navigation does) used to move
        // that branch's chip to the front, flipping the visible order.
        let theme = Theme::dark();
        let names = [
            "alpha".to_string(),
            "beta".to_string(),
            "origin/gamma".to_string(),
        ];
        let remotes = ["origin".to_string()];

        let no_selection = optimize_branch_display(&names, false, 0, None, &theme, &remotes);
        let selected_alpha =
            optimize_branch_display(&names, false, 0, Some("alpha"), &theme, &remotes);
        let selected_beta =
            optimize_branch_display(&names, false, 0, Some("beta"), &theme, &remotes);
        let selected_gamma =
            optimize_branch_display(&names, false, 0, Some("origin/gamma"), &theme, &remotes);

        // Only the label text (not the style/highlight) needs to stay fixed.
        let text = |v: &[BranchChip]| v.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
        assert_eq!(text(&no_selection), text(&selected_alpha));
        assert_eq!(text(&no_selection), text(&selected_beta));
        assert_eq!(text(&no_selection), text(&selected_gamma));
    }

    #[test]
    fn badge_order_matches_source_order_two_labels() {
        // Two non-synced branches render as one combined chip, e.g.
        // "[mac][origin/mac]" (issue #50's exact example) — assert the
        // bracket groups keep `branch_names`' order and never flip when a
        // different branch becomes the "selected" one.
        let theme = Theme::dark();
        let names = ["mac".to_string(), "zzz-other".to_string()];
        let out = optimize_branch_display(&names, false, 0, None, &theme, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "[mac][zzz-other]");

        // Selecting the second branch must not move it first.
        let out_selected =
            optimize_branch_display(&names, false, 0, Some("zzz-other"), &theme, &[]);
        assert_eq!(out_selected[0].label, "[mac][zzz-other]");
    }

    // ── badge color matches lane color (issue #53) ────────────────────

    // ── chip click resolution: edge cases (decomposition seam #77) ────────

    #[test]
    fn overflow_marker_chip_resolves_to_first_branch() {
        // FINDING (item 1): three+ branches collapse into ONE combined chip that
        // carries a "+N" overflow marker — there is no separate "+N" chip (see
        // the `optimize_branch_display` doc: it returns zero or one chip). That
        // single chip resolves via its FIRST bracket group, so a click anywhere
        // on it — including the "+N" region — checks out the first branch, NOT
        // nothing. Pinning actual behavior; the intent ("+N resolves to None")
        // does not hold because +N is not an independent chip.
        let theme = Theme::dark();
        let names = ["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let out = optimize_branch_display(&names, false, 0, None, &theme, &[]);
        assert_eq!(out.len(), 1, "multi-branch collapses to a single combined chip");
        assert!(
            out[0].label.contains('+'),
            "the +N overflow marker is present: {:?}",
            out[0].label
        );
        assert_eq!(
            out[0].branch.as_deref(),
            Some("alpha"),
            "the combined/overflow chip resolves to the first branch, not None: {:?}",
            out[0]
        );
    }

    #[test]
    fn remote_only_chip_resolution() {
        let theme = Theme::dark();

        // Multi-remote repo: the remote-only chip keeps its "<remote>/" prefix in
        // the label AND resolves to the full remote ref.
        let multi = optimize_branch_display(
            &["origin/feat".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string(), "upstream".to_string()],
        );
        assert_eq!(multi.len(), 1);
        assert!(
            multi[0].label.contains("origin/feat"),
            "multi-remote keeps the prefix: {:?}",
            multi[0].label
        );
        assert_eq!(multi[0].branch.as_deref(), Some("origin/feat"));

        // Single-remote repo: the prefix is dropped from the label, but the chip
        // still resolves back to the full remote ref.
        let single = optimize_branch_display(
            &["origin/feat".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(single.len(), 1);
        assert!(
            !single[0].label.contains("origin/"),
            "single-remote drops the prefix: {:?}",
            single[0].label
        );
        assert_eq!(single[0].branch.as_deref(), Some("origin/feat"));
    }

    #[test]
    fn abbreviated_label_resolution() {
        // A branch name long enough to blow the 40-column label budget
        // (MAX_LABEL_WIDTH) is abbreviated with an ellipsis ("...").
        let theme = Theme::dark();
        let long = format!("feature/{}", "a".repeat(50));
        let out = optimize_branch_display(&[long], false, 0, None, &theme, &[]);
        assert_eq!(out.len(), 1);
        assert!(
            display_width(&out[0].label) <= 40,
            "label stays within the width budget: {:?}",
            out[0].label
        );
        assert!(
            out[0].label.contains("..."),
            "the abbreviation ellipsis is present: {:?}",
            out[0].label
        );
        // FINDING (item 4): the ellipsized label no longer contains the full
        // branch name, so `resolve_chip_branch` cannot match it and the chip
        // resolves to None — a click on a truncated branch chip checks out
        // nothing. Pinning actual behavior (the intent was Some(<full name>)).
        assert_eq!(
            out[0].branch, None,
            "an abbreviated chip does not resolve back to its branch: {:?}",
            out[0]
        );
    }

    #[test]
    fn edge_input_branch_names_are_well_formed() {
        let theme = Theme::dark();

        // A name containing bracket delimiters (not a valid git ref, but the
        // builder must not panic and must still emit a non-empty label).
        let bracketed =
            optimize_branch_display(&["wip[1]".to_string()], false, 0, None, &theme, &[]);
        assert_eq!(bracketed.len(), 1);
        assert!(
            !bracketed[0].label.is_empty(),
            "bracketed name still yields a non-empty label: {:?}",
            bracketed[0].label
        );
        assert!(bracketed[0].label.contains("wip"), "{:?}", bracketed[0].label);
        // FINDING (item 5): the ']' inside the name collides with the chip's own
        // ']' delimiter, so resolution stops early and the chip resolves to None.
        assert_eq!(
            bracketed[0].branch, None,
            "a ']' inside the name breaks resolution: {:?}",
            bracketed[0]
        );

        // A wide-emoji name resolves cleanly and keeps a non-empty label.
        let emoji =
            optimize_branch_display(&["🎉feature".to_string()], false, 0, None, &theme, &[]);
        assert_eq!(emoji.len(), 1);
        assert!(
            !emoji[0].label.is_empty(),
            "emoji name yields a non-empty label: {:?}",
            emoji[0].label
        );
        assert_eq!(
            emoji[0].branch.as_deref(),
            Some("🎉feature"),
            "emoji branch name resolves to itself: {:?}",
            emoji[0]
        );
    }

    #[test]
    fn empty_branch_names_yield_no_chips() {
        let theme = Theme::dark();
        let out = optimize_branch_display(&[], false, 0, None, &theme, &[]);
        assert!(out.is_empty(), "no branch names → no chips: {out:?}");
    }

    #[test]
    fn head_badge_color_matches_lane_color_not_a_fixed_head_color() {
        // Regression: the checked-out HEAD branch's badge used to be forced
        // to a fixed color (green) regardless of the commit's actual lane
        // color, diverging from the graph line/node drawn in that lane.
        let theme = Theme::dark();
        for color_index in 0..theme.lane_colors.len() {
            if color_index == crate::graph::colors::MAIN_BRANCH_COLOR {
                continue; // main branch is blue either way — not the regression case
            }
            let out = optimize_branch_display(
                &["feature".to_string()],
                true, // is_head
                color_index,
                None,
                &theme,
                &[],
            );
            assert_eq!(out.len(), 1);
            assert_eq!(
                out[0].style.fg,
                Some(theme.lane_color(color_index)),
                "HEAD badge fg must equal the lane's own color at index {color_index}"
            );
        }
    }
}
