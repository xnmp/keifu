//! The graph row's message tail: a pure decision pass (`resolve_row_model`)
//! that turns a node + frame context into a [`RowModel`] of styling/label/message
//! *decisions* (no widths, no spans), and a layout pass (`layout_row`) that turns
//! that model into the final `Line` + `ChipHit`s. `render_graph_line_tail`
//! orchestrates the two after the shared prelude (separator, compare marker, and
//! the uncommitted/connector early returns).

use chrono::{DateTime, Local};
use std::collections::{HashMap, HashSet};

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::{
    config::MetadataColumns,
    git::graph::GraphNode,
    git::CommitInfo,
    mouse::{ChipHit, ChipTarget},
    pr::{PrContext, PrInfo},
    ui::theme::Theme,
};

use super::badges::{merged_badge, merged_style, pr_for_row, PrBadge, PR_BADGE_ICON};
use super::chips::{optimize_branch_display, BranchChip};
use super::metrics::{display_width, format_date_field, truncate_to_width};
use super::MERGE_ICON;

/// Style for a *merged branch name chip* (issue #90): take the chip's own
/// unmerged style and mute its lane color toward the recessive tone (via
/// [`Theme::merged_chip_color`]) rather than flattening it to grey, then DIM it.
/// A landed branch's chip thus still reads as *its* lane — only faded — keeping
/// the branch identity the old flat-grey `merged_style` erased, while staying
/// visibly distinct from an active unmerged chip. Selection's REVERSED and any
/// HEAD BOLD carried on `base` survive; the highlighted row's `selection_style`
/// still subtracts DIM so a selected merged chip never renders muddy.
pub(super) fn merged_chip_style(base: Style, theme: &Theme) -> Style {
    let muted = base
        .fg
        .map_or(theme.text_muted, |c| theme.merged_chip_color(c));
    base.fg(muted).add_modifier(Modifier::DIM)
}

/// Determine which right-side elements (date, author, hash) to display based on available width.
/// Returns (show_date, show_author, show_hash, total_right_width).
///
/// Only columns enabled in `cols` are eligible. Among the eligible ones, when
/// the row is too narrow they drop by priority — hash first, then date, then
/// author (author is kept longest). A hidden/dropped column's width is not
/// counted, so it flows to the commit-message budget via `right_width`.
pub(super) fn compute_right_side_visibility(
    remaining_for_content: usize,
    cols: MetadataColumns,
) -> (bool, bool, bool, usize) {
    // Rendered width of each element incl. its leading separator (see the
    // right-block rendering below): date " "+4, author "  "+8, hash "  "+7.
    const DATE_W: usize = 5;
    const AUTHOR_W: usize = 10;
    const HASH_W: usize = 9;
    const TRAILING_W: usize = 1; // single trailing space when anything shows

    // Ensure minimum space for branch + commit message before showing right-side info
    const CONTENT_MIN_WIDTH: usize = 50;
    let available = remaining_for_content.saturating_sub(CONTENT_MIN_WIDTH);

    let width = |d: bool, a: bool, h: bool| -> usize {
        if !d && !a && !h {
            return 0;
        }
        (if d { DATE_W } else { 0 })
            + (if a { AUTHOR_W } else { 0 })
            + (if h { HASH_W } else { 0 })
            + TRAILING_W
    };

    // Start from the user's enabled set, then shed low-priority columns until
    // the block fits. (With all three enabled this reproduces the original
    // 35/26/11 breakpoints exactly.)
    let mut show_date = cols.date;
    let mut show_author = cols.author;
    let mut show_hash = cols.hash;

    if width(show_date, show_author, show_hash) > available {
        show_hash = false;
    }
    if width(show_date, show_author, show_hash) > available {
        show_date = false;
    }
    if width(show_date, show_author, show_hash) > available {
        show_author = false;
    }

    let right_width = width(show_date, show_author, show_hash);
    (show_date, show_author, show_hash, right_width)
}

/// Format tag labels for a node. Tags render as `<name>` in the tag color,
/// visually distinct from `[branch]` and `{stash}` labels. Long tag names are
/// truncated with an ellipsis so a single tag can't dominate the row.
pub(super) fn build_tag_labels(tag_names: &[String], theme: &Theme) -> Vec<(String, Style)> {
    // Total label width including the enclosing `<` `>` delimiters.
    const MAX_TAG_LABEL_WIDTH: usize = 24;
    // Restraint: the tag color alone distinguishes the chip; no bold (bold is
    // reserved for the HEAD branch and actionable PR badges), so tags read as
    // one quiet, consistent family alongside non-HEAD branch chips.
    let style = Style::default().fg(theme.tag_label);
    tag_names
        .iter()
        .map(|name| {
            let label = if display_width(name) + 2 <= MAX_TAG_LABEL_WIDTH {
                format!("<{}>", name)
            } else {
                // -3: two delimiters plus one ellipsis character.
                let head = truncate_to_width(name, MAX_TAG_LABEL_WIDTH - 3);
                format!("<{}…>", head)
            };
            (label, style)
        })
        .collect()
}

/// Whether a row is a base-update ("back-merge") merge that should render
/// strongly muted (#55): its message greys+dims and its own graph connector is
/// force-dimmed so the noisy back-merge line recedes. This is the single source
/// of truth shared by BOTH renderers — the unicode path (`render_cells_unicode`
/// force_dim) and the pixel path (`dim_pixel_specs_window` per-row force-dim) —
/// so they agree on which rows mute. HEAD is never muted.
pub(super) fn is_base_update_row(
    node: &GraphNode,
    mute_base_merges: bool,
    base_update_merges: &HashSet<git2::Oid>,
) -> bool {
    mute_base_merges
        && !node.is_head
        && node.is_merge()
        && node
            .commit
            .as_ref()
            .is_some_and(|c| base_update_merges.contains(&c.oid))
}

/// Frame-constant render inputs, built once per frame and shared by every row.
/// Bundles the ~13 values that used to be threaded individually through
/// `render_graph_line` / `render_graph_line_tail`; per-row values travel
/// separately as `node` and `RowFlags`.
pub(super) struct RowRenderCtx<'a> {
    pub theme: &'a Theme,
    pub now: DateTime<Local>,
    pub pixel_mode: bool,
    pub remotes: &'a [String],
    pub open_prs: &'a HashMap<String, PrInfo>,
    pub pr_ctx: &'a PrContext<'a>,
    pub merged_branches: &'a HashSet<String>,
    /// Whether a merged branch shown in the graph should render dimmed (issue
    /// #106) — chip color + "merged" badge. Independent of whether merged
    /// branches are hidden entirely (that's decided upstream: a hidden branch
    /// never reaches this row at all). Applies uniformly regardless of how
    /// `merged_branches` classified the branch (ancestry, fast-forward, or
    /// squash all live in the same set).
    pub merged_dim: bool,
    /// Commits exclusive to a merged branch's lane (issue #108), or `None` when
    /// merged-lane dimming is off (`dim` off or `hide` on). When `Some`, a row
    /// whose commit is in the set greys its text like a muted merge, and its
    /// graph strokes (any cell edge touching one of these commits) dim — the
    /// same strokes hide-merged would remove.
    pub merged_lane_oids: Option<&'a HashSet<git2::Oid>>,
    pub base_update_merges: &'a HashSet<git2::Oid>,
    pub metadata_columns: MetadataColumns,
    /// Graph column width cap (glyph budget), same for every row this frame.
    pub graph_width: usize,
    /// Total drawable inner width available to a row.
    pub total_width: usize,
    pub selected_branch_name: Option<&'a str>,
    /// Selected commit's lit-edge trace set; `None` when tracing is off.
    pub trace: Option<&'a std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>>,
}

/// Per-row render flags decided at the call site.
#[derive(Clone, Copy)]
pub(super) struct RowFlags {
    pub is_selected: bool,
    pub is_marked: bool,
}

/// How a row's message column should render, decided before any width is known.
/// Truncation and the icon/prefix formatting belong to [`layout_row`], since
/// they need the message budget.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum RowMessage {
    /// A collapsed merge (#59): render as the bare [`MERGE_ICON`], untruncated.
    Collapse,
    /// A PR-landed subject rewrite (#99/#101): render as `{PR_BADGE_ICON} {title}`,
    /// truncated to the budget. The number is intentionally dropped.
    PrSubject { title: String },
    /// The raw commit subject, truncated to the budget.
    Raw { text: String },
}

/// The pure styling/label/message *decisions* for one graph row — everything the
/// tail resolves before any width is known. Layout ([`layout_row`]) consumes this
/// to place spans; it never re-derives a decision (chip styles here are FINAL —
/// `merged_chip_style` is already applied — so layout no longer consults
/// `merged_branches`).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RowModel {
    pub row_is_muted: bool,
    pub msg_style: Style,
    pub hash_style: Style,
    pub author_style: Style,
    pub date_style: Style,
    pub pr_badge: Option<PrBadge>,
    pub branch_chips: Vec<BranchChip>,
    pub tag_chips: Vec<(String, Style)>,
    pub merged_badge: Option<String>,
    pub message: RowMessage,
}

/// Resolve the pure per-row decisions into a [`RowModel`]: which mute (if any)
/// wins for the message and metadata, the branch/tag chips with FINAL styling,
/// the PR badge, the "merged" badge, and which message form the row shows. No
/// widths, no spans, no truncation — those belong to [`layout_row`].
pub(super) fn resolve_row_model(
    node: &GraphNode,
    commit: &CommitInfo,
    ctx: &RowRenderCtx<'_>,
    flags: RowFlags,
    is_base_update: bool,
    is_merged_lane: bool,
) -> RowModel {
    // PR-merge commit (#52): a merge that landed a GitHub PR reads as machinery,
    // not authored work, so its message renders greyed (an explicit muted color,
    // distinct from the plain DIM used for ordinary muted merges). Detection is
    // data-driven (second parent is a known PR head) with the GitHub merge
    // message format as fallback for already-merged PRs; both inputs come off
    // this row's commit, so the check stays inside the windowed per-row path.
    // HEAD is never greyed, matching the muted-merge rule below.
    let is_pr_merge = !node.is_head
        && ctx.pr_ctx.is_pr_merge(
            node.is_merge(),
            commit.parent_oids.get(1).copied(),
            &commit.message,
        );
    // Muted merge: dim only the message text (VSCode-style) — the graph dot and
    // lines stay full-strength. HEAD is never muted so its message stays legible.
    let muted_merge = ctx.metadata_columns.mute_merges && node.is_merge() && !node.is_head;
    // Collapse merges (#59): a stronger form of muting that replaces the merge
    // commit's message with a bare merge glyph. Same detection seam as
    // `mute_merges` (any merge, never HEAD), so the two options unify rather than
    // duplicate. When on it implies muting even if `mute_merges` is off.
    let collapse_merge = ctx.metadata_columns.collapse_merges && node.is_merge() && !node.is_head;
    // PR-landed subject rewrite (#99): a commit whose raw subject is GitHub
    // merge/squash machinery — "Merge pull request #123 from …" or "Title
    // (#123)" — reads better as "<icon> #123 <PR title>". Qualifies via the
    // same `is_pr_merge` detection for merge commits (so it agrees with the
    // greying above), or the squash-subject parser for single-parent commits.
    // `collapse_merge` is a stronger reduction (drops the message entirely),
    // so it wins when both would otherwise apply — resolved down where the
    // message text is actually chosen.
    let pr_landed_subject = ctx
        .metadata_columns
        .pr_subjects
        .then(|| {
            if is_pr_merge {
                crate::pr::pr_landed_subject(&commit.message, &commit.full_message, true)
            } else if commit.parent_oids.len() == 1 {
                crate::pr::pr_landed_subject(&commit.message, &commit.full_message, false)
            } else {
                None
            }
        })
        .flatten();
    // #92: any of the muted-merge categories above should read grey across the
    // whole row, not just the message — DIM alone is too subtle (terminal
    // support is inconsistent), so the hash/author/date columns get the same
    // explicit `text_muted` foreground the message uses.
    // A merged-lane commit (#108) reads muted too — its message, hash, author,
    // and date all grey, matching the other muted categories (#92).
    let row_is_muted =
        is_base_update || is_pr_merge || muted_merge || collapse_merge || is_merged_lane;
    let hash_style = if row_is_muted {
        Style::default().fg(ctx.theme.text_muted)
    } else {
        Style::default().fg(ctx.theme.hash_color)
    };
    let author_style = if row_is_muted {
        Style::default().fg(ctx.theme.text_muted)
    } else {
        Style::default().fg(ctx.theme.author_color)
    };
    let date_style = if row_is_muted {
        Style::default().fg(ctx.theme.text_muted)
    } else {
        Style::default().fg(ctx.theme.date_color)
    };
    // Three separate style domains, decided independently and never merged here:
    //   1. message-text precedence (this chain) — which mute wins for the message;
    //   2. connector-cell dim (force_dim ∪ trace) — see `render_cells_unicode`
    //      and `dim_pixel_specs_window`, applied to the graph glyphs, not text;
    //   3. chip styling (branch/tag/merged badges) — see `optimize_branch_display`
    //      and `merged_style`.
    // The widget-level selection highlight (`Theme::selection_style`) then patches
    // BOLD over the whole selected line and subtracts DIM, so BOLD wins when a row
    // is selected without any domain going muddy (DIM+BOLD).
    //
    // Precedence: a base-update back-merge (#55) is the noisiest, so it wins with
    // the strongest mute (muted color + DIM). Then a PR-landing merge (#52), then
    // ordinary merge muting/collapse, then selection bold.
    let msg_style = if is_base_update {
        Style::default()
            .fg(ctx.theme.text_muted)
            .add_modifier(Modifier::DIM)
    } else if is_pr_merge {
        Style::default().fg(ctx.theme.text_muted)
    } else if muted_merge || collapse_merge || is_merged_lane {
        // Merged-lane commits (#108) share the ordinary muted-merge treatment:
        // an explicit muted foreground plus DIM.
        Style::default()
            .fg(ctx.theme.text_muted)
            .add_modifier(Modifier::DIM)
    } else if flags.is_selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    // Optimize branch names (compact when local matches origin/local). The
    // per-chip merged-dim decision is applied HERE (relocated from the render
    // loop, #106), so the chip styles are FINAL and layout no longer consults
    // `merged_branches`. A merged branch's chip keeps its lane hue, muted and
    // dimmed (#90), so it recedes while still reading as its own branch; the
    // chip's `branch` field is unchanged and still drives ChipHit emission.
    let branch_chips: Vec<BranchChip> = optimize_branch_display(
        &node.branch_names,
        node.is_head,
        node.color_index,
        ctx.selected_branch_name,
        ctx.theme,
        ctx.remotes,
    )
    .into_iter()
    .map(|mut chip| {
        let is_merged = ctx.merged_dim
            && chip
                .branch
                .as_deref()
                .is_some_and(|n| ctx.merged_branches.contains(n));
        if is_merged {
            chip.style = merged_chip_style(chip.style, ctx.theme);
        }
        chip
    })
    .collect();

    // Tag labels render after branch labels with a distinct color.
    let tag_chips = build_tag_labels(&node.tag_names, ctx.theme);

    // Open-PR badge (#42): shown exactly once per PR, on the PR's head commit.
    // Colored by CI status, with review/comment markers (#43).
    let pr_badge = pr_for_row(
        commit.oid,
        &node.branch_names,
        ctx.remotes,
        ctx.pr_ctx,
        ctx.open_prs,
        ctx.theme,
    );

    // Merged badge: shown when one of this node's local branches has already
    // landed on the trunk (merge commit, fast-forward, or squash) AND the
    // dim-merged-branches setting is on (issue #106) — the branch chips are
    // dimmed to match. Hidden merged branches never reach this row at all (the
    // hide-merged toggle removes them from the graph entirely upstream), so
    // `merged_dim` is the only gate a shown merged branch still needs; it
    // applies identically regardless of classification kind.
    let has_merged_branch = ctx.merged_dim
        && node
            .branch_names
            .iter()
            .any(|n| ctx.merged_branches.contains(n));
    let merged_badge = has_merged_branch.then(merged_badge);

    // Collapse (#59) replaces the whole message with a single merge glyph — the
    // strongest reduction, so it wins over a PR-subject rewrite (#99) on the
    // same row. Otherwise a qualifying PR-landed commit shows "<icon> <title>"
    // — no number (#101: the parsed number is sometimes an issue reference,
    // and the title alone reads cleaner); anything else shows the raw message.
    let message = if collapse_merge {
        RowMessage::Collapse
    } else if let Some((_, title)) = &pr_landed_subject {
        RowMessage::PrSubject {
            title: title.clone(),
        }
    } else {
        RowMessage::Raw {
            text: commit.message.clone(),
        }
    };

    RowModel {
        row_is_muted,
        msg_style,
        hash_style,
        author_style,
        date_style,
        pr_badge,
        branch_chips,
        tag_chips,
        merged_badge,
        message,
    }
}

/// Assemble a resolved [`RowModel`] into the final `Line` + `ChipHit`s: span
/// order, width accounting, truncation, padding, and the right-aligned metadata
/// block. `spans`/`left_width` carry the already-rendered graph column + prelude.
fn layout_row<'a>(
    mut spans: Vec<Span<'a>>,
    mut left_width: usize,
    model: &RowModel,
    node: &GraphNode,
    commit: &CommitInfo,
    ctx: &RowRenderCtx<'_>,
) -> (Line<'a>, Vec<ChipHit>) {
    let mut chips: Vec<ChipHit> = Vec::new();

    // Chip plus a trailing space.
    let pr_badge_width = model.pr_badge.as_ref().map_or(0, |b| display_width(&b.text) + 1);
    let merged_badge_width = model
        .merged_badge
        .as_deref()
        .map_or(0, |b| display_width(b) + 1);

    // === Right-aligned: date author hash (fixed width) ===
    let date = format_date_field(commit.timestamp, ctx.now); // DATE_FIELD_WIDTH chars
    let author = truncate_to_width(&commit.author_name, 8);
    let author_formatted = format!("{:<8}", author); // fixed 8 chars
    let hash = truncate_to_width(&commit.short_id, 7);
    let hash_formatted = format!("{:<7}", hash); // fixed 7 chars

    // Calculate branch width first (before rendering)
    let branch_width: usize = model
        .branch_chips
        .iter()
        .enumerate()
        .map(|(i, chip)| display_width(&chip.label) + if i > 0 { 1 } else { 0 })
        .sum::<usize>()
        + if !model.branch_chips.is_empty() { 1 } else { 0 };

    // Each tag label carries a trailing space (see rendering below).
    let tag_width: usize = model
        .tag_chips
        .iter()
        .map(|(label, _)| display_width(label) + 1)
        .sum();

    // Calculate remaining space for branch + message + right info
    let graph_width = left_width;
    let remaining_for_content = ctx.total_width.saturating_sub(graph_width);

    // Determine which right-side elements to show based on available space
    let (show_date, show_author, show_hash, right_width) =
        compute_right_side_visibility(remaining_for_content, ctx.metadata_columns);

    // Render open-PR badge (#98: moved before the branch pill so PR context
    // leads the row instead of trailing it). Emits nothing when absent, so
    // there's no leading gap before the branch labels on PR-less rows.
    if let Some(badge) = &model.pr_badge {
        let style = Style::default().fg(badge.color).add_modifier(Modifier::BOLD);
        let chip_start = left_width;
        left_width += display_width(&badge.text) + 1;
        chips.push(ChipHit {
            x_start: chip_start as u16,
            x_end: (chip_start + display_width(&badge.text)) as u16,
            target: ChipTarget::PrBadge,
        });
        spans.push(Span::styled(badge.text.clone(), style));
        spans.push(Span::raw(" "));
    }

    // Render branch labels
    for (i, chip) in model.branch_chips.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            left_width += 1;
        }
        let chip_start = left_width;
        left_width += display_width(&chip.label);
        // Each chip already carries the branch a click on it resolves to (folded
        // into chip construction, #77), and its FINAL style (merged-dim already
        // applied in `resolve_row_model`), so the layout pass just places it.
        if let Some(name) = &chip.branch {
            chips.push(ChipHit {
                x_start: chip_start as u16,
                x_end: left_width as u16,
                target: ChipTarget::Branch(name.clone()),
            });
        }
        spans.push(Span::styled(chip.label.clone(), chip.style));
    }
    if !model.branch_chips.is_empty() {
        spans.push(Span::raw(" "));
        left_width += 1;
    }

    // Render merged badge (after branch labels)
    if let Some(badge) = &model.merged_badge {
        left_width += display_width(badge) + 1;
        spans.push(Span::styled(badge.clone(), merged_style(ctx.theme)));
        spans.push(Span::raw(" "));
    }

    // Render tag labels (after branches, before the stash label)
    for (label, style) in &model.tag_chips {
        left_width += display_width(label) + 1;
        spans.push(Span::styled(label.clone(), *style));
        spans.push(Span::raw(" "));
    }

    // Render stash label
    let stash_width = if let Some(stash_label) = &node.stash_label {
        let label = format!("{{{}}}", stash_label);
        let stash_style = Style::default()
            .fg(ctx.theme.text_muted)
            .add_modifier(Modifier::ITALIC);
        let w = display_width(&label) + 1;
        left_width += w;
        spans.push(Span::styled(label, stash_style));
        spans.push(Span::raw(" "));
        w
    } else {
        0
    };

    // Compute max message width (remaining space after branch, stash label, and right side)
    let available_for_message = remaining_for_content
        .saturating_sub(branch_width)
        .saturating_sub(merged_badge_width)
        .saturating_sub(pr_badge_width)
        .saturating_sub(tag_width)
        .saturating_sub(stash_width)
        .saturating_sub(right_width);
    // Collapse (#59) is a bare glyph, untruncated; a PR-landed subject shows
    // "<icon> <title>" and the raw message its plain text — both truncated to
    // the budget. (The decision of which form lives in `resolve_row_model`.)
    let message = match &model.message {
        RowMessage::Collapse => MERGE_ICON.to_string(),
        RowMessage::PrSubject { title } => {
            truncate_to_width(&format!("{PR_BADGE_ICON} {title}"), available_for_message)
        }
        RowMessage::Raw { text } => truncate_to_width(text, available_for_message),
    };
    let message_width = display_width(&message);
    spans.push(Span::styled(message, model.msg_style));
    left_width += message_width;

    // Padding so the right-aligned block starts at a fixed column
    let padding = ctx
        .total_width
        .saturating_sub(left_width)
        .saturating_sub(right_width);
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }

    // === Append right-aligned block (display: date, author, hash) ===
    if show_date {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(date, model.date_style));
    }
    if show_author {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(author_formatted, model.author_style));
    }
    if show_hash {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(hash_formatted, model.hash_style));
    }
    if show_date || show_author || show_hash {
        spans.push(Span::raw(" "));
    }

    (Line::from(spans), chips)
}

/// Render everything after the graph column: separator, compare marker,
/// branch/tag/stash labels, message, and the right-aligned metadata block. A
/// thin orchestrator — the shared prelude (separator, compare marker, and the
/// uncommitted/connector early returns) then [`resolve_row_model`] + [`layout_row`].
pub(super) fn render_graph_line_tail<'a>(
    mut spans: Vec<Span<'a>>,
    mut left_width: usize,
    node: &GraphNode,
    ctx: &RowRenderCtx<'_>,
    flags: RowFlags,
    is_base_update: bool,
    is_merged_lane: bool,
) -> (Line<'a>, Vec<ChipHit>) {
    // Separator between graph and commit info
    spans.push(Span::raw(" "));
    left_width += 1;

    // Compare marker: flags commits that are marked or a comparison endpoint.
    if flags.is_marked {
        spans.push(Span::styled(
            "◆ ",
            Style::default()
                .fg(ctx.theme.search_cursor)
                .add_modifier(Modifier::BOLD),
        ));
        left_width += 2;
    }

    // Handle uncommitted changes row
    if node.is_uncommitted {
        let text = match node.uncommitted_count {
            Some(count) => format!("uncommitted changes ({})", count),
            None => "uncommitted changes".to_string(),
        };
        let style = Style::default().fg(ctx.theme.text_primary);
        spans.push(Span::styled(text, style));
        return (Line::from(spans), Vec::new());
    }

    // Early return for connector-only rows
    let commit = match &node.commit {
        Some(c) => c,
        None => return (Line::from(spans), Vec::new()),
    };

    let model = resolve_row_model(node, commit, ctx, flags, is_base_update, is_merged_lane);
    layout_row(spans, left_width, &model, node, commit, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_tag_labels ─────────────────────────────────────────────

    #[test]
    fn no_tags_produces_no_labels() {
        let theme = Theme::dark();
        assert!(build_tag_labels(&[], &theme).is_empty());
    }

    #[test]
    fn short_tag_is_wrapped_in_angle_brackets() {
        let theme = Theme::dark();
        let labels = build_tag_labels(&["v1.0".to_string()], &theme);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].0, "<v1.0>");
        // Distinct from branch labels: rendered in the tag color.
        assert_eq!(labels[0].1.fg, Some(theme.tag_label));
    }

    #[test]
    fn each_tag_gets_its_own_label() {
        let theme = Theme::dark();
        let labels = build_tag_labels(&["v1.0".to_string(), "release".to_string()], &theme);
        let rendered: Vec<&str> = labels.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(rendered, vec!["<v1.0>", "<release>"]);
    }

    #[test]
    fn overlong_tag_is_truncated_within_the_width_budget() {
        let theme = Theme::dark();
        let long = "an-extremely-long-tag-name-that-will-not-fit-on-one-line".to_string();
        let labels = build_tag_labels(&[long], &theme);
        let label = &labels[0].0;
        // Never exceeds the label budget, stays bracketed, and is elided.
        assert!(display_width(label) <= 24, "label too wide: {label:?}");
        assert!(label.starts_with('<') && label.ends_with('>'));
        assert!(label.contains('…'), "expected an ellipsis: {label:?}");
    }

    // ── merged-branch chip styling (#90) ─────────────────────────────

    #[test]
    fn merged_chip_keeps_lane_hue_muted_not_flat_grey() {
        // A merged branch chip (#90) must derive from its own lane color, muted —
        // NOT collapse to the flat `text_muted` grey the old style used, and NOT
        // stay identical to the active unmerged chip.
        let theme = Theme::dark();
        let lane = theme.lane_color(1); // second lane, a distinct hue
        let unmerged = Style::default().fg(lane);
        let merged = merged_chip_style(unmerged, &theme);

        let fg = merged.fg.expect("merged chip keeps an fg color");
        // Derived from the lane hue, not the flat muted grey.
        assert_ne!(fg, theme.text_muted, "must not be flat grey");
        // Actually muted — moved off the raw lane color.
        assert_ne!(fg, lane, "must be muted, not the raw lane color");
        // Matches the theme's blend helper exactly (derivation is lane-based).
        assert_eq!(fg, theme.merged_chip_color(lane));
        // And it is visibly distinct from the unmerged chip's style.
        assert_ne!(merged, unmerged, "merged chip differs from the unmerged chip");
        assert!(
            merged.add_modifier.contains(Modifier::DIM),
            "merged chip is dimmed"
        );
    }

    // ── metadata column visibility / width budget ────────────────────

    fn cols(author: bool, hash: bool, date: bool) -> MetadataColumns {
        MetadataColumns {
            author,
            hash,
            date,
            mute_merges: true,
            mute_base_merges: false,
            collapse_merges: false,
            avatars: false,
            pr_subjects: true,
        }
    }

    #[test]
    fn all_columns_shown_when_enabled_and_wide() {
        // Compact date (4) + separators: date 5 + author 10 + hash 9 + trailing 1 = 25.
        assert_eq!(
            compute_right_side_visibility(200, cols(true, true, true)),
            (true, true, true, 25)
        );
    }

    #[test]
    fn disabled_column_is_never_shown_and_reclaims_its_width() {
        // Hash off: only date+author, width drops from 25 to 16 (9 reclaimed).
        assert_eq!(
            compute_right_side_visibility(200, cols(true, false, true)),
            (true, true, false, 16)
        );
        // Everything off: no right block at all.
        assert_eq!(
            compute_right_side_visibility(200, cols(false, false, false)),
            (false, false, false, 0)
        );
        // Only date enabled: date " "+4 = 5, +1 trailing = 6.
        assert_eq!(
            compute_right_side_visibility(200, cols(false, false, true)),
            (true, false, false, 6)
        );
    }

    #[test]
    fn enabled_columns_still_drop_by_priority_when_narrow() {
        // available = remaining - 50. Hash drops first, then date, author last.
        // all (25): avail >= 25 → remaining >= 75.
        assert_eq!(
            compute_right_side_visibility(75, cols(true, true, true)),
            (true, true, true, 25)
        );
        // date+author (16): 16 <= avail < 25 → remaining 66..74.
        assert_eq!(
            compute_right_side_visibility(74, cols(true, true, true)),
            (true, true, false, 16)
        );
        // author only (11): 11 <= avail < 16 → remaining 61..65.
        assert_eq!(
            compute_right_side_visibility(65, cols(true, true, true)),
            (false, true, false, 11)
        );
        // none (0): avail < 11 → remaining < 61.
        assert_eq!(
            compute_right_side_visibility(60, cols(true, true, true)),
            (false, false, false, 0)
        );
    }
}
