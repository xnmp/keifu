//! Commit graph construction

use std::collections::{HashMap, HashSet};

use git2::Oid;

use super::{BranchInfo, CommitInfo};
use crate::graph::colors::{
    ColorAssigner, MAIN_BRANCH_COLOR, SQUASH_LINK_COLOR_INDEX, UNCOMMITTED_COLOR_INDEX,
};

/// Graph node
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// Commit info (None means connector row only or uncommitted changes row)
    pub commit: Option<CommitInfo>,
    /// Lane position for this commit
    pub lane: usize,
    /// Color index for this node
    pub color_index: usize,
    /// Branch names pointing to this commit
    pub branch_names: Vec<String>,
    /// Tag names pointing to this commit
    pub tag_names: Vec<String>,
    /// Whether HEAD points to this commit
    pub is_head: bool,
    /// Whether this is an uncommitted changes node
    pub is_uncommitted: bool,
    /// Whether this is a stash commit
    pub is_stash: bool,
    /// Stash label (e.g. "stash@{0}: WIP on main")
    pub stash_label: Option<String>,
    /// Number of uncommitted files (None when count is inaccurate, e.g.
    /// collapsed untracked directories).  Valid only when is_uncommitted is true.
    pub uncommitted_count: Option<usize>,
    /// Render info for this row
    pub cells: Vec<CellType>,
    /// The commit-edge identity of each cell, parallel to `cells`, for branch
    /// tracing. `.0` is the primary edge — the `(child, parent)` OID pair the
    /// stroke draws (a lane pipe, a curve, or the commit dot as a self-edge);
    /// `.1` is the secondary edge for `HorizontalPipe` cells (the vertical lane
    /// crossed underneath the horizontal stroke). An edge is on the traced line
    /// only when BOTH its endpoints are in the selected commit's lineage —
    /// identifying a stroke by a single endpoint would light a merged feature
    /// branch's lead-in strokes (fork commit is on the trunk lineage). A cell is
    /// traced when either edge is. `None` = no edge (e.g. the grey uncommitted
    /// connector).
    pub cell_oids: Vec<CellOids>,
}

/// A single traced stroke's identity: the `(child, parent)` commit OIDs it
/// connects. Both endpoints must be in a lineage for the edge to be traced.
pub type CellEdge = (Oid, Oid);

/// Per-cell edge identity: `(primary, secondary)` edges. See
/// [`GraphNode::cell_oids`].
pub type CellOids = (Option<CellEdge>, Option<CellEdge>);

impl GraphNode {
    /// A merge commit (2+ parents). Stash commits are excluded — their extra
    /// parents are truncated to one at load time, so they never count as merges.
    pub fn is_merge(&self) -> bool {
        self.commit
            .as_ref()
            .is_some_and(|c| c.parent_oids.len() >= 2)
    }

    /// A pure connector row: a fork/merge join line carrying no commit and not
    /// the uncommitted-changes row. These are de-emphasized in the graph.
    pub fn is_connector(&self) -> bool {
        self.commit.is_none() && !self.is_uncommitted
    }
}

/// Cell types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellType {
    /// Empty
    Empty,
    /// Vertical line (active lane)
    Pipe(usize),
    /// Commit node
    Commit(usize),
    /// Start branch to the right ╭ (branch goes up-right)
    BranchRight(usize),
    /// Start branch to the left ╮ (branch goes up-left)
    BranchLeft(usize),
    /// Merge from the right ╰ (branch joins from down-right)
    MergeRight(usize),
    /// Merge from the left ╯ (branch joins from down-left)
    MergeLeft(usize),
    /// Horizontal line
    Horizontal(usize),
    /// Horizontal line (lane crossing)
    HorizontalPipe(usize, usize), // (horizontal_lane, pipe_lane)
    /// T junction to the right ├
    TeeRight(usize),
    /// T junction to the left ┤
    TeeLeft(usize),
    /// Upward T junction (fork point) ┴
    TeeUp(usize),
}

/// Graph layout
#[derive(Debug, Clone)]
pub struct GraphLayout {
    pub nodes: Vec<GraphNode>,
    pub max_lane: usize,
}

/// The first-parent "line" through HEAD among the loaded commits: HEAD, its
/// first-parent ancestors walking DOWN, and any descendants that continue the
/// same line via *their* first parent walking UP. This is the run of commits
/// pinned to lane 0 so the checked-out work is anchored at the far left (like
/// VSCode Git Graph).
///
/// The upward walk matters when a branch is fast-forwarded ahead of HEAD: those
/// newer commits share HEAD's line, so they take lane 0 too rather than shoving
/// HEAD's tip off it. When two children share a first parent (a fork), the first
/// one encountered (newest, by walk order) wins the continuation — an arbitrary
/// but harmless tie-break, since it only decides which descendant shares lane 0.
///
/// Returns an empty set when HEAD is unknown or not loaded, in which case
/// `build_graph` falls back to its historical "first tip processed owns lane 0"
/// behaviour.
fn head_first_parent_line(
    commits: &[CommitInfo],
    oid_to_row: &HashMap<Oid, usize>,
    head_oid: Option<Oid>,
) -> HashSet<Oid> {
    let mut line = HashSet::new();
    let Some(head) = head_oid.filter(|h| oid_to_row.contains_key(h)) else {
        return line;
    };

    // DOWN: HEAD and its first-parent ancestors, while they stay loaded.
    let mut cur = head;
    loop {
        if !line.insert(cur) {
            break; // cycle guard (should not happen in a DAG)
        }
        let row = oid_to_row[&cur];
        match commits[row].parent_oids.first().copied() {
            Some(p) if oid_to_row.contains_key(&p) => cur = p,
            _ => break,
        }
    }

    // UP: descendants whose FIRST parent continues this line (a branch ahead of
    // HEAD shares its lane instead of displacing it).
    let mut cur = head;
    loop {
        let child = commits
            .iter()
            .find(|c| c.parent_oids.first() == Some(&cur) && !line.contains(&c.oid))
            .map(|c| c.oid);
        match child {
            Some(ch) => {
                line.insert(ch);
                cur = ch;
            }
            None => break,
        }
    }

    line
}

/// Build a graph from commit list
/// uncommitted_count: None if no uncommitted changes, Some(count) if there
/// are uncommitted changes.  The inner Option is None when the exact file
/// count is unavailable (e.g. collapsed untracked directories).
/// head_commit_oid: The OID of the commit that HEAD points to (for uncommitted
/// changes and for anchoring HEAD's first-parent line to lane 0)
/// squash_links: `(branch_tip, squash_commit)` pairs to draw a muted-grey link
/// line between (issue #81). Empty (the option off) leaves the layout
/// byte-identical to before — the links are a pure post-pass overlay; see
/// [`inject_squash_links`]. A pair whose endpoints aren't both loaded is skipped.
pub fn build_graph(
    commits: &[CommitInfo],
    branches: &[BranchInfo],
    tags: &[super::repository::TagInfo],
    stashes: &[super::repository::StashInfo],
    uncommitted_count: Option<Option<usize>>,
    head_commit_oid: Option<Oid>,
    squash_links: &[(Oid, Oid)],
) -> GraphLayout {
    // Map stash oid -> short label like "stash@{0}"
    let stash_oid_labels: HashMap<Oid, String> = stashes
        .iter()
        .map(|s| (s.oid, format!("stash@{{{}}}", s.index)))
        .collect();
    let stash_oids: HashSet<Oid> = stashes.iter().map(|s| s.oid).collect();

    // Map commit oid -> tag names pointing at it.
    let mut oid_to_tags: HashMap<Oid, Vec<String>> = HashMap::new();
    for tag in tags {
        oid_to_tags
            .entry(tag.target_oid)
            .or_default()
            .push(tag.name.clone());
    }

    if commits.is_empty() {
        if let Some(count) = uncommitted_count {
            return GraphLayout {
                nodes: vec![GraphNode {
                    commit: None,
                    lane: 0,
                    color_index: UNCOMMITTED_COLOR_INDEX,
                    branch_names: Vec::new(),
                    tag_names: Vec::new(),
                    is_head: false,
                    is_uncommitted: true,
                    is_stash: false,
                    stash_label: None,
                    uncommitted_count: count,
                    cells: vec![CellType::Commit(UNCOMMITTED_COLOR_INDEX)],
                    cell_oids: vec![(None, None)],
                }],
                max_lane: 0,
            };
        }

        return GraphLayout {
            nodes: Vec::new(),
            max_lane: 0,
        };
    }

    // OID -> branch name mapping
    let mut oid_to_branches: HashMap<Oid, Vec<String>> = HashMap::new();
    for branch in branches {
        oid_to_branches
            .entry(branch.tip_oid)
            .or_default()
            .push(branch.name.clone());
    }

    // OID -> row index mapping
    let oid_to_row: HashMap<Oid, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.oid, i))
        .collect();

    // HEAD's first-parent line is pinned to lane 0 (leftmost), so the
    // checked-out work is anchored at the far left. When HEAD is unknown or not
    // loaded this is empty and lane assignment keeps its historical behaviour.
    // `head_commit_oid` (not the branch-derived `head_oid` above) is used so a
    // detached HEAD is anchored too.
    let head_line = head_first_parent_line(commits, &oid_to_row, head_commit_oid);
    // Whether lane 0 is reserved for HEAD's line. When true, only commits on
    // `head_line` may occupy lane 0; every other tip shifts one lane right.
    let head_line_reserved = !head_line.is_empty();

    // Lowest free lane a commit may take. Lane 0 is off-limits unless the commit
    // is on HEAD's line (or nothing is reserved). Returns None when every
    // eligible lane is occupied and a new one must be pushed.
    let eligible_empty_lane = |lanes: &[Option<Oid>], zero_ok: bool| -> Option<usize> {
        lanes.iter().enumerate().find_map(|(i, l)| {
            (l.is_none() && (zero_ok || !head_line_reserved || i != 0)).then_some(i)
        })
    };

    // Detect fork points (commits with multiple children)
    // parent_oid -> list of child commits
    // Check ALL parents, not just first parent, to detect fork points like
    // hotfix branches that are merged into multiple release branches
    let mut parent_children: HashMap<Oid, Vec<Oid>> = HashMap::new();
    for commit in commits {
        for parent_oid in &commit.parent_oids {
            if oid_to_row.contains_key(parent_oid) {
                parent_children
                    .entry(*parent_oid)
                    .or_default()
                    .push(commit.oid);
            }
        }
    }
    // Fork points: commits with 2+ children
    let fork_points: std::collections::HashSet<Oid> = parent_children
        .iter()
        .filter(|(_, children)| children.len() >= 2)
        .map(|(parent, _)| *parent)
        .collect();

    // Lane tracking: OID tracked by each lane
    let mut lanes: Vec<Option<Oid>> = Vec::new();
    // Parallel to `lanes`: the child commit whose edge this lane's pipe carries,
    // i.e. the commit that placed (or continued) the lane toward `lanes[i]`. A
    // lane pipe's traced edge is `(lane_children[i], lanes[i])` — both endpoints
    // needed for the pair-identity trace. Kept in lock-step with every `lanes`
    // mutation below.
    let mut lane_children: Vec<Option<Oid>> = Vec::new();
    // Reserve lane 0 for HEAD's line by seeding it empty: a non-HEAD tip then
    // skips it (via `eligible_empty_lane`) and, crucially, the push-a-new-lane
    // fallback lands at lane 1+ instead of claiming index 0 when `lanes` is
    // empty. The slot stays `None` (unrendered) until HEAD's line fills it.
    if head_line_reserved {
        lanes.push(None);
        lane_children.push(None);
    }
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut max_lane: usize = 0;
    // The main (blue, reserved) colour is claimed once, by the first commit that
    // lands on lane 0 — HEAD's line when it is anchored there, otherwise the
    // first tip processed (the historical main branch).
    let mut main_color_assigned = false;

    // Color management
    let mut color_assigner = ColorAssigner::new();
    // When HEAD's line is anchored to lane 0, its main (blue) colour is claimed
    // partway through the walk (at HEAD's row, not row 0). Reserve blue up front
    // so an earlier-processed branch cannot grab it and collide with HEAD's line.
    if head_line_reserved {
        color_assigner.reserve_color(MAIN_BRANCH_COLOR);
    }
    // OID -> color index mapping
    let mut oid_color_index: HashMap<Oid, usize> = HashMap::new();
    // Lane -> color index mapping (keep colors during forks)
    let mut lane_color_index: HashMap<usize, usize> = HashMap::new();

    for (row_idx, commit) in commits.iter().enumerate() {
        // Start processing a new row
        color_assigner.advance_row();

        // Find the lane tracking this commit OID
        let commit_lane_opt = lanes
            .iter()
            .position(|l| l.map(|oid| oid == commit.oid).unwrap_or(false));

        // Determine the lane
        let lane = if let Some(l) = commit_lane_opt {
            l
        } else {
            // Find an empty lane or create one. Lane 0 is reserved for HEAD's
            // line, so a tip not on that line takes the next lane instead.
            let zero_ok = head_line.contains(&commit.oid);
            if let Some(l) = eligible_empty_lane(&lanes, zero_ok) {
                l
            } else {
                lanes.push(None);
                lane_children.push(None);
                lanes.len() - 1
            }
        };

        // Fork point handling: multiple lanes track this commit
        // Build fork connector and release extra lanes
        let fork_lanes: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.map(|oid| oid == commit.oid).unwrap_or(false))
            .map(|(i, _)| i)
            .collect();

        if fork_lanes.len() >= 2 {
            // Use the smallest lane as main
            let main_lane = *fork_lanes.iter().min().unwrap();
            let merging_lanes: Vec<(usize, usize)> = fork_lanes
                .iter()
                .filter(|&&l| l != main_lane)
                .map(|&l| {
                    // Use lane color, else OID color, else lane index
                    let color = lane_color_index
                        .get(&l)
                        .copied()
                        .or_else(|| oid_color_index.get(&commit.oid).copied())
                        .unwrap_or(l);
                    (l, color)
                })
                .collect();

            // Update max_lane for fork connector
            for &(l, _) in &merging_lanes {
                max_lane = max_lane.max(l);
            }
            max_lane = max_lane.max(main_lane);

            let main_color = lane_color_index
                .get(&main_lane)
                .copied()
                .or_else(|| oid_color_index.get(&commit.oid).copied())
                .unwrap_or(main_lane);
            let (fork_connector_cells, fork_connector_oids) = build_fork_connector_cells(
                main_lane,
                main_color,
                commit.oid,
                &merging_lanes,
                &lanes,
                &lane_children,
                &oid_color_index,
                &lane_color_index,
                max_lane,
            );
            nodes.push(GraphNode {
                commit: None,
                lane: main_lane,
                color_index: main_color,
                branch_names: Vec::new(),
                tag_names: Vec::new(),
                is_head: false,
                is_uncommitted: false,
                is_stash: false,
                stash_label: None,
                uncommitted_count: None,
                cells: fork_connector_cells,
                cell_oids: fork_connector_oids,
            });

            // Release merging lanes
            for &(l, _) in &merging_lanes {
                if l < lanes.len() {
                    lanes[l] = None;
                    lane_children[l] = None;
                    color_assigner.release_lane(l);
                    lane_color_index.remove(&l);
                }
            }
        }

        // Determine color index.
        //
        // The line pinned to lane 0 owns the reserved main colour (blue): HEAD's
        // line when it is anchored there, otherwise the first tip processed (the
        // historical main branch). The very first commit to land on lane 0 claims
        // it — checked here before the continue/new-branch split so a lane-0 line
        // first reached via first-parent inheritance still gets the main colour.
        let commit_color_index = if lane == 0
            && !main_color_assigned
            && (head_line_reserved || nodes.is_empty())
        {
            main_color_assigned = true;
            color_assigner.assign_main_color(lane)
        } else if commit_lane_opt.is_some() {
            // Continue existing branch
            color_assigner.continue_lane(lane)
        } else {
            // New branch start - assign a new color (exclude reserved)
            color_assigner.assign_color(lane)
        };
        oid_color_index.insert(commit.oid, commit_color_index);
        // Record lane color (to preserve colors during forks)
        lane_color_index.insert(lane, commit_color_index);

        // Clear this commit lane (re-set below when a parent continues it).
        if lane < lanes.len() {
            lanes[lane] = None;
            lane_children[lane] = None;
        }

        // Process parent commits
        // (OID, lane, already tracked?, color index, already shown?)
        let mut parent_lanes: Vec<(Oid, usize, bool, usize, bool)> = Vec::new();
        let valid_parents: Vec<Oid> = commit
            .parent_oids
            .iter()
            .filter(|oid| oid_to_row.contains_key(oid))
            .copied()
            .collect();

        // Whether this is a fork sibling (parent is a fork point tracked on another lane)
        let mut is_fork_sibling = false;
        // Color for fork siblings (overrides commit_color_index)
        let mut fork_sibling_color: Option<usize> = None;

        // Start fork handling for merge commits (multiple parents)
        if valid_parents.len() >= 2 {
            color_assigner.begin_fork();
        }

        for (parent_idx, parent_oid) in valid_parents.iter().enumerate() {
            // Check if the parent is already in a lane
            let existing_parent_lane = lanes
                .iter()
                .position(|l| l.map(|oid| oid == *parent_oid).unwrap_or(false));

            // Check if parent commit has already been shown. Each commit in
            // `commits` produces exactly one node (at the row matching its
            // index), so "shown" is equivalent to "already processed" i.e.
            // its row index precedes the current one.
            let parent_already_shown = oid_to_row
                .get(parent_oid)
                .map(|&r| r < row_idx)
                .unwrap_or(false);

            let (parent_lane, was_existing, parent_color) = if let Some(pl) = existing_parent_lane {
                // If parent is a fork point, treat as fork sibling
                if parent_idx == 0 && fork_points.contains(parent_oid) {
                    // Track the parent on this lane as well (same OID on multiple lanes).
                    // This lane continues from the current commit down to the fork parent.
                    lanes[lane] = Some(*parent_oid);
                    lane_children[lane] = Some(commit.oid);
                    is_fork_sibling = true;
                    // Keep main lane color, otherwise use commit_color_index
                    let color = if color_assigner.is_main_lane(lane) {
                        color_assigner.get_main_color()
                    } else {
                        // Use current commit color (do not assign new)
                        commit_color_index
                    };
                    fork_sibling_color = Some(color);
                    lane_color_index.insert(lane, color);
                    (lane, false, color)
                } else {
                    // Existing lane - use the lane's color (from lane_color_index)
                    let color = lane_color_index
                        .get(&pl)
                        .copied()
                        .or_else(|| oid_color_index.get(parent_oid).copied())
                        .unwrap_or(pl);
                    (pl, true, color)
                }
            } else if parent_idx == 0 {
                // First parent uses the same lane - inherit color. The lane
                // continues from this commit down to its first parent.
                lanes[lane] = Some(*parent_oid);
                lane_children[lane] = Some(commit.oid);
                oid_color_index.insert(*parent_oid, commit_color_index);
                (lane, false, commit_color_index)
            } else {
                // Subsequent parents use new lanes - assign fork sibling colors.
                // A new lane is spawned by this (merge) commit toward the parent.
                // Lane 0 stays reserved unless this parent is on HEAD's line.
                let zero_ok = head_line.contains(parent_oid);
                let empty = eligible_empty_lane(&lanes, zero_ok);
                let new_lane = if let Some(l) = empty {
                    l
                } else {
                    lanes.push(None);
                    lane_children.push(None);
                    lanes.len() - 1
                };
                lanes[new_lane] = Some(*parent_oid);
                lane_children[new_lane] = Some(commit.oid);
                let new_color = color_assigner.assign_fork_sibling_color(new_lane);
                oid_color_index.insert(*parent_oid, new_color);
                lane_color_index.insert(new_lane, new_color);
                (new_lane, false, new_color)
            };

            // Include parent_already_shown flag for proper symbol selection
            parent_lanes.push((
                *parent_oid,
                parent_lane,
                was_existing,
                parent_color,
                parent_already_shown,
            ));
        }

        // Skip lane_merge for fork siblings
        let _ = is_fork_sibling; // Reserved for future use

        // Use fork sibling color if set
        let final_color_index = fork_sibling_color.unwrap_or(commit_color_index);

        // Update max_lane
        max_lane = max_lane.max(lane);
        for &(_, pl, _, _, _) in &parent_lanes {
            max_lane = max_lane.max(pl);
        }

        // Check whether lane merge is needed
        // If commit lane differs from parent lane and parent is already tracked
        // -> higher lane ends and merges into lower lane
        let lane_merge: Option<(usize, usize)> = parent_lanes
            .iter()
            .find(|(_, pl, was_existing, _, _)| *was_existing && *pl != lane)
            .map(|(_, pl, _, color, _)| (*pl, *color));

        // Build cells for this row
        // Include ALL parents to draw connections directly on the commit row
        let (cells, cell_oids) = build_row_cells_with_colors(
            lane,
            final_color_index,
            commit.oid,
            &parent_lanes,
            &lanes,
            &lane_children,
            &oid_color_index,
            &lane_color_index,
            max_lane,
        );

        // Get branch names
        let branch_names = oid_to_branches
            .get(&commit.oid)
            .cloned()
            .unwrap_or_default();

        // Identify HEAD by the actual HEAD oid (`head_commit_oid`), not by
        // branch identity: a detached HEAD carries no `is_head` branch, so
        // keying off branches would drop the star entirely (issue #89).
        let is_head = head_commit_oid == Some(commit.oid);

        // Add commit row
        let is_stash = stash_oids.contains(&commit.oid);
        let stash_label = stash_oid_labels.get(&commit.oid).cloned();
        let tag_names = oid_to_tags.get(&commit.oid).cloned().unwrap_or_default();
        nodes.push(GraphNode {
            commit: Some(commit.clone()),
            lane,
            color_index: final_color_index,
            branch_names,
            tag_names,
            is_head,
            is_uncommitted: false,
            is_stash,
            stash_label,
            uncommitted_count: None,
            cells,
            cell_oids,
        });

        // Handle lane merging: when a parent is already tracked on a different lane
        if let Some((parent_lane, _)) = lane_merge {
            // Lower lane is main, higher lane is ending
            let (main_lane, ending_lane) = if parent_lane < lane {
                (parent_lane, lane)
            } else {
                (lane, parent_lane)
            };

            // Check if the ending lane is tracking a commit that hasn't been shown yet
            let ending_lane_oid = lanes.get(ending_lane).and_then(|o| *o);
            let ending_oid_already_shown = ending_lane_oid
                .map(|oid| {
                    oid_to_row
                        .get(&oid)
                        .map(|&r| r < row_idx)
                        .unwrap_or(true)
                })
                .unwrap_or(true);

            let continues_down = !ending_oid_already_shown;

            // Release the ending lane only if:
            // 1. The first parent is NOT on the ending lane
            // 2. The OID on ending lane has already been shown (not continuing downward)
            if ending_lane < lanes.len() {
                let first_parent_on_ending_lane = parent_lanes
                    .first()
                    .map(|(_, pl, _, _, _)| *pl == ending_lane)
                    .unwrap_or(false);

                if !first_parent_on_ending_lane && !continues_down {
                    // Move the ending lane OID (and its child) into the main lane
                    if let Some(oid) = lanes[ending_lane] {
                        if lanes.get(main_lane).map(|l| l.is_none()).unwrap_or(false) {
                            lanes[main_lane] = Some(oid);
                            lane_children[main_lane] = lane_children[ending_lane];
                        }
                    }
                    lanes[ending_lane] = None;
                    lane_children[ending_lane] = None;
                    color_assigner.release_lane(ending_lane);
                    lane_color_index.remove(&ending_lane);
                }
            }
        }
    }

    // Insert uncommitted changes node at the beginning if there are uncommitted changes
    if let Some(count) = uncommitted_count {
        insert_uncommitted_node(&mut nodes, &mut max_lane, head_commit_oid, count);
    }

    // Overlay squash-merge link lines last, once every row (including the
    // uncommitted node) is in place, so endpoints are resolved against the final
    // row set (issue #81). A no-op when `squash_links` is empty.
    inject_squash_links(&mut nodes, &mut max_lane, squash_links);

    GraphLayout { nodes, max_lane }
}

/// Insert the synthetic "uncommitted changes" node at the top of the graph and
/// draw its dotted lane down to the HEAD commit.
///
/// When HEAD's own lane is free above it, the node simply sits on that lane. If
/// a newer branch line occupies HEAD's lane, the node takes the nearest free
/// lane and a connector curves across to HEAD on the HEAD row. That connector
/// must never sever a lane it crosses:
///
/// - The chosen lane is required to be free above HEAD **and on the HEAD row
///   itself** — the curve's endpoint lands on the HEAD row, so a lane that's
///   free above but occupied on the HEAD row would otherwise be clobbered
///   (this was the root cause of the "severed pink lane" bug).
/// - Intermediate lane pipes the connector passes through become
///   `HorizontalPipe` crossings, keeping both the grey connector and the
///   crossed lane visible.
fn insert_uncommitted_node(
    nodes: &mut Vec<GraphNode>,
    max_lane: &mut usize,
    head_commit_oid: Option<Oid>,
    count: Option<usize>,
) {
    // Find the node index that HEAD points to.
    let head_node_idx = head_commit_oid.and_then(|oid| {
        nodes
            .iter()
            .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(oid))
    });
    let Some(head_idx) = head_node_idx else {
        return;
    };

    let head_lane = nodes[head_idx].lane;

    // Whether a lane's column is Empty across the given rows.
    let column_free = |nodes: &[GraphNode], lane: usize, rows: std::ops::Range<usize>| {
        let cell_idx = lane * 2;
        rows.into_iter().all(|i| {
            nodes[i]
                .cells
                .get(cell_idx)
                .map(|c| *c == CellType::Empty)
                .unwrap_or(true)
        })
    };

    // HEAD's own lane is usable when its column is free in every row ABOVE HEAD;
    // the HEAD row itself holds the commit dot, which the lane connects into.
    let head_lane_available = column_free(nodes, head_lane, 0..head_idx);

    let uncommitted_lane = if head_lane_available {
        head_lane
    } else {
        // A different lane must be free above HEAD *and* on the HEAD row, since
        // the connector's curve endpoint lands on the HEAD row. Checking only
        // the rows above HEAD (the old behaviour) let the curve clobber a lane
        // still active on the HEAD row. `max_lane + 1` is always free.
        let mut best_lane = *max_lane + 1;
        let mut best_distance = usize::MAX;

        for candidate_lane in 0..=*max_lane + 1 {
            if column_free(nodes, candidate_lane, 0..head_idx + 1) {
                let distance = candidate_lane.abs_diff(head_lane);
                if distance < best_distance {
                    best_distance = distance;
                    best_lane = candidate_lane;
                }
            }
        }
        best_lane
    };

    // Update max_lane if needed.
    if uncommitted_lane > *max_lane {
        *max_lane = uncommitted_lane;
    }

    // Ensure all nodes have enough cells.
    let required_cells = (*max_lane + 1) * 2;
    for node in nodes.iter_mut() {
        while node.cells.len() < required_cells {
            node.cells.push(CellType::Empty);
        }
        while node.cell_oids.len() < required_cells {
            node.cell_oids.push((None, None));
        }
    }

    // Add Pipe to all nodes before HEAD commit.
    let cell_idx = uncommitted_lane * 2;
    for node in nodes.iter_mut().take(head_idx) {
        if node.cells[cell_idx] == CellType::Empty {
            node.cells[cell_idx] = CellType::Pipe(UNCOMMITTED_COLOR_INDEX);
        }
    }

    // If uncommitted_lane != head_lane, add a connector from HEAD to the lane.
    if uncommitted_lane != head_lane {
        let head_cell_idx = head_lane * 2;
        let uncommitted_cell_idx = uncommitted_lane * 2;
        // Columns strictly between the two lanes, regardless of direction.
        let (lo, hi) = if uncommitted_lane > head_lane {
            (head_cell_idx + 1, uncommitted_cell_idx)
        } else {
            (uncommitted_cell_idx + 1, head_cell_idx)
        };

        // Cross intermediate columns: an active lane pipe becomes a
        // HorizontalPipe crossing so both the connector and the crossed lane
        // stay visible; empty columns get the plain horizontal stroke.
        for col in lo..hi {
            match nodes[head_idx].cells[col] {
                CellType::Empty => {
                    nodes[head_idx].cells[col] = CellType::Horizontal(UNCOMMITTED_COLOR_INDEX);
                }
                CellType::Pipe(pipe_lane) => {
                    nodes[head_idx].cells[col] =
                        CellType::HorizontalPipe(UNCOMMITTED_COLOR_INDEX, pipe_lane);
                    // The grey connector isn't lineage, but the crossed lane
                    // pipe keeps its edge so tracing still lights it up.
                    let crossed = nodes[head_idx].cell_oids[col].0;
                    nodes[head_idx].cell_oids[col] = (None, crossed);
                }
                _ => {}
            }
        }

        // Curve endpoint. The chosen lane is free on the HEAD row, so this lands
        // on an empty cell and severs nothing.
        nodes[head_idx].cells[uncommitted_cell_idx] = if uncommitted_lane > head_lane {
            CellType::MergeLeft(UNCOMMITTED_COLOR_INDEX) // ╯
        } else {
            CellType::MergeRight(UNCOMMITTED_COLOR_INDEX) // ╰
        };
    }

    // Build cells for the uncommitted node.
    let mut cells = vec![CellType::Empty; required_cells];
    cells[uncommitted_lane * 2] = CellType::Commit(UNCOMMITTED_COLOR_INDEX);
    // The uncommitted row carries no commit, so nothing here is ever traced.
    let cell_oids = vec![(None, None); required_cells];

    // Insert uncommitted node at the beginning.
    nodes.insert(
        0,
        GraphNode {
            commit: None,
            lane: uncommitted_lane,
            color_index: UNCOMMITTED_COLOR_INDEX,
            branch_names: Vec::new(),
            tag_names: Vec::new(),
            is_head: false,
            is_uncommitted: true,
            is_stash: false,
            stash_label: None,
            uncommitted_count: count,
            cells,
            cell_oids,
        },
    );
}

/// Overlay muted-grey "squash link" connectors (issue #81): for each
/// `(branch_tip, squash_commit)` pair whose BOTH endpoints are present as loaded
/// commit rows, draw a faint grey line joining the two rows — a hint that the
/// squash-merged branch landed at that trunk commit.
///
/// This mirrors [`insert_uncommitted_node`]: a synthetic overlay in the reserved
/// [`SQUASH_LINK_COLOR_INDEX`], emitting the same cell vocabulary (lane pipes +
/// branch/merge curves) that both the Unicode and pixel renderers already draw —
/// no new renderer. It is a **layout-only** overlay: it never touches any
/// `CommitInfo`, so `is_merge`, [`lineage_oids`], diff targets and branch
/// classification are all unaffected, and every cell it writes carries a
/// `(None, None)` primary edge, so branch tracing can never light it — the link
/// stays permanently dim. (Crossed real lane pipes keep their edge as the
/// secondary, so tracing still lights *them*.)
///
/// A modelled-as-parent edge would be wrong here: the lane router only ever draws
/// toward OLDER commits (down the graph), but a squash commit is usually NEWER
/// than the branch tip it landed (it is created at merge time), so the link may
/// run in either direction between its two rows. This overlay connects the rows
/// regardless of their order.
fn inject_squash_links(nodes: &mut [GraphNode], max_lane: &mut usize, links: &[(Oid, Oid)]) {
    if links.is_empty() {
        return; // option off (or nothing to link) → layout untouched.
    }
    // oid -> row index, for commit-carrying rows only. Row indices are stable —
    // this overlay never inserts or removes rows — so one map serves every link.
    let oid_row: HashMap<Oid, usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n.commit.as_ref().map(|c| (c.oid, i)))
        .collect();

    for &(tip, target) in links {
        // Guard: only draw when BOTH endpoints are loaded (e.g. the branch tip is
        // filtered out when merged branches are hidden, or history is truncated).
        if let (Some(&r1), Some(&r2)) = (oid_row.get(&tip), oid_row.get(&target)) {
            if r1 != r2 {
                draw_squash_link(nodes, max_lane, r1, r2);
            }
        }
    }
}

/// Draw one squash link between the commit rows `r1` and `r2` on a grey lane.
///
/// Picks the connector column nearest both endpoints, runs a grey pipe down it
/// through the intermediate rows, and joins each endpoint with an elbow curve.
///
/// The connector column may be an **endpoint's own lane**: routing the pipe
/// straight up (or down) the endpoint's column and elbowing into the *other*
/// endpoint hugs the real lane geometry — exactly how a merge/fork connector
/// routes. Only if no such column is free does the link fall back to a distinct
/// lane, and only past `max_lane` if even that is unavailable. A previous version
/// always demanded a third lane distinct from *both* endpoints; when the lanes
/// between were busy, the nearest free column landed to the *right* of the tip,
/// so the link detoured out and back — a phantom curve ending in a void column
/// (issue #110). Anchoring on an endpoint's own column removes that detour.
///
/// See [`inject_squash_links`] for the invariants this preserves.
fn draw_squash_link(nodes: &mut [GraphNode], max_lane: &mut usize, r1: usize, r2: usize) {
    let (u, l) = (r1.min(r2), r1.max(r2));
    let ulane = nodes[u].lane;
    let llane = nodes[l].lane;

    // A lane is usable as the connector column when its column is Empty across
    // the span, EXCEPT at an endpoint row whose own lane this is: there the dot
    // sits in the column and anchors the connector (the elbow lands on the far
    // endpoint, this end just runs straight out of the dot). So a candidate must
    // be free at every intermediate row, and at each endpoint row unless it is
    // that endpoint's lane. This never severs a real lane — an endpoint's lane is
    // only accepted when its column is otherwise clear, and the dot already owns
    // that cell.
    let column_ok = |nodes: &[GraphNode], lane: usize| -> bool {
        let ci = lane * 2;
        (u..=l).all(|i| {
            if (i == u && lane == ulane) || (i == l && lane == llane) {
                return true; // the endpoint dot's own column
            }
            nodes[i]
                .cells
                .get(ci)
                .copied()
                .unwrap_or(CellType::Empty)
                == CellType::Empty
        })
    };
    // Prefer the usable lane nearest both endpoints (fewest crossings, and it
    // keeps the connector between/on the endpoints rather than detouring out);
    // `max_lane + 1` is always free as a last-resort fallback.
    let mut link_lane = *max_lane + 1;
    let mut best_dist = usize::MAX;
    for cand in 0..=*max_lane + 1 {
        if column_ok(nodes, cand) {
            let dist = cand.abs_diff(ulane) + cand.abs_diff(llane);
            if dist < best_dist {
                best_dist = dist;
                link_lane = cand;
            }
        }
    }

    if link_lane > *max_lane {
        *max_lane = link_lane;
    }
    // Rows built before a later lane appeared can be short; pad every row so the
    // link's columns are addressable (matches `insert_uncommitted_node`).
    let required = (*max_lane + 1) * 2;
    for n in nodes.iter_mut() {
        while n.cells.len() < required {
            n.cells.push(CellType::Empty);
        }
        while n.cell_oids.len() < required {
            n.cell_oids.push((None, None));
        }
    }

    let gcol = link_lane * 2;

    // Intermediate rows: a plain grey pipe down the link lane.
    for node in nodes[(u + 1)..l].iter_mut() {
        if node.cells[gcol] == CellType::Empty {
            node.cells[gcol] = CellType::Pipe(SQUASH_LINK_COLOR_INDEX);
            node.cell_oids[gcol] = (None, None);
        }
    }

    // Upper endpoint: the link leaves this commit and heads down the link lane.
    // When the link lane IS this endpoint's lane, the dot already sits in the
    // column and connects down into the pipe below — no elbow, no crossing run.
    if link_lane != ulane {
        squash_cross_row(nodes, u, ulane, link_lane);
        nodes[u].cells[gcol] = if link_lane > ulane {
            CellType::BranchLeft(SQUASH_LINK_COLOR_INDEX) // ╮
        } else {
            CellType::BranchRight(SQUASH_LINK_COLOR_INDEX) // ╭
        };
        nodes[u].cell_oids[gcol] = (None, None);
    }

    // Lower endpoint: the link lands on the older commit, ending the lane. Same
    // dot-anchored shortcut when the link lane is this endpoint's own lane.
    if link_lane != llane {
        squash_cross_row(nodes, l, llane, link_lane);
        nodes[l].cells[gcol] = if link_lane > llane {
            CellType::MergeLeft(SQUASH_LINK_COLOR_INDEX) // ╯
        } else {
            CellType::MergeRight(SQUASH_LINK_COLOR_INDEX) // ╰
        };
        nodes[l].cell_oids[gcol] = (None, None);
    }
}

/// Draw the grey horizontal run on `row` between a commit's lane and the link
/// lane, crossing intermediate columns: an empty column becomes a grey
/// horizontal, an active lane pipe becomes a `HorizontalPipe` crossing (keeping
/// the crossed lane visible AND keeping its trace edge as the secondary). The
/// two endpoint columns are left for the caller to place the commit dot / curve.
fn squash_cross_row(nodes: &mut [GraphNode], row: usize, commit_lane: usize, link_lane: usize) {
    let (lo, hi) = if link_lane > commit_lane {
        (commit_lane * 2 + 1, link_lane * 2)
    } else {
        (link_lane * 2 + 1, commit_lane * 2)
    };
    for col in lo..hi {
        match nodes[row].cells[col] {
            CellType::Empty => {
                nodes[row].cells[col] = CellType::Horizontal(SQUASH_LINK_COLOR_INDEX);
                nodes[row].cell_oids[col] = (None, None);
            }
            CellType::Pipe(pl) => {
                nodes[row].cells[col] = CellType::HorizontalPipe(SQUASH_LINK_COLOR_INDEX, pl);
                // The grey run isn't lineage, but the crossed lane pipe keeps its
                // edge (as secondary) so tracing still lights it.
                let crossed = nodes[row].cell_oids[col].0;
                nodes[row].cell_oids[col] = (None, crossed);
            }
            // A marker we can't cleanly redraw (a curve/tee): leave it be.
            _ => {}
        }
    }
}

/// Build cells for one row - color index version
/// parent_lanes: (parent OID, lane, existing-tracked flag, color index, already-shown flag)
#[allow(clippy::too_many_arguments)] // cohesive lane/color/oid inputs; a struct adds indirection
fn build_row_cells_with_colors(
    commit_lane: usize,
    commit_color: usize,
    commit_oid: Oid,
    parent_lanes: &[(Oid, usize, bool, usize, bool)],
    active_lanes: &[Option<Oid>],
    active_lane_children: &[Option<Oid>],
    oid_color_index: &HashMap<Oid, usize>,
    lane_color_index: &HashMap<usize, usize>,
    max_lane: usize,
) -> (Vec<CellType>, Vec<CellOids>) {
    let mut cells = vec![CellType::Empty; (max_lane + 1) * 2];
    // Parallel per-cell edge identity for branch tracing (see GraphNode::cell_oids).
    let mut oids: Vec<CellOids> = vec![(None, None); cells.len()];

    // Draw vertical lines for active lanes
    for (lane_idx, lane_oid) in active_lanes.iter().enumerate() {
        if let Some(oid) = lane_oid {
            if lane_idx != commit_lane {
                let cell_idx = lane_idx * 2;
                if cell_idx < cells.len() {
                    // Prefer lane color, else OID color, else lane index
                    let color = lane_color_index
                        .get(&lane_idx)
                        .copied()
                        .or_else(|| oid_color_index.get(oid).copied())
                        .unwrap_or(lane_idx);
                    cells[cell_idx] = CellType::Pipe(color);
                    // This pipe carries the edge (lane's child → awaited parent).
                    oids[cell_idx] = (lane_edge(active_lane_children, lane_idx, *oid), None);
                }
            }
        }
    }

    // Draw commit node — a self-edge, traced iff the commit itself is on lineage.
    let commit_cell_idx = commit_lane * 2;
    if commit_cell_idx < cells.len() {
        cells[commit_cell_idx] = CellType::Commit(commit_color);
        oids[commit_cell_idx] = (Some((commit_oid, commit_oid)), None);
    }

    // Draw connections to parents — each stroke's edge is (commit → parent).
    for &(parent_oid, parent_lane, was_existing, parent_color, already_shown) in parent_lanes.iter()
    {
        if parent_lane == commit_lane {
            // Same lane - only a vertical line (drawn on next row)
            continue;
        }
        let edge = (commit_oid, parent_oid);

        // Connection to a different lane
        if parent_lane > commit_lane {
            // Connection to a lane on the right
            // Horizontal line to the right from the commit position
            for col in (commit_lane * 2 + 1)..(parent_lane * 2) {
                if col < cells.len() {
                    let existing = cells[col];
                    if let CellType::Pipe(pl) = existing {
                        cells[col] = CellType::HorizontalPipe(parent_color, pl);
                        // Horizontal edge → parent; keep the crossed pipe's edge.
                        oids[col] = (Some(edge), oids[col].0);
                    } else if existing == CellType::Empty {
                        cells[col] = CellType::Horizontal(parent_color);
                        oids[col] = (Some(edge), None);
                    } else if oids[col].1.is_none() {
                        // Another connection already drew this column; co-route
                        // our edge so tracing either parent lights it.
                        oids[col].1 = Some(edge);
                    }
                }
            }
            // End marker
            let end_idx = parent_lane * 2;
            if end_idx < cells.len() {
                if was_existing && already_shown {
                    // Parent already shown: lane ends and merges ╯ (connect upward)
                    cells[end_idx] = CellType::MergeLeft(parent_color);
                } else if was_existing {
                    // Parent not yet shown but lane exists: ┤ (T-junction, line continues down)
                    cells[end_idx] = CellType::TeeLeft(parent_color);
                } else {
                    // New lane for parent: ╮ (branch starts here, continues down)
                    cells[end_idx] = CellType::BranchLeft(parent_color);
                }
                oids[end_idx] = (Some(edge), None);
            }
        } else {
            // Branch end: connect to the left lane (main line)
            // Horizontal line to the left from the commit position
            // Use the parent's color for the connection line
            for col in (parent_lane * 2 + 1)..(commit_lane * 2) {
                if col < cells.len() {
                    let existing = cells[col];
                    if let CellType::Pipe(pl) = existing {
                        cells[col] = CellType::HorizontalPipe(parent_color, pl);
                        oids[col] = (Some(edge), oids[col].0);
                    } else if existing == CellType::Empty {
                        cells[col] = CellType::Horizontal(parent_color);
                        oids[col] = (Some(edge), None);
                    } else if oids[col].1.is_none() {
                        // Another connection already drew this column; co-route
                        // our edge so tracing either parent lights it.
                        oids[col].1 = Some(edge);
                    }
                }
            }
            // Start marker
            let start_idx = parent_lane * 2;
            if start_idx < cells.len() {
                if was_existing && already_shown {
                    // Parent already shown: lane ends and merges ╰ (connect upward)
                    cells[start_idx] = CellType::MergeRight(parent_color);
                } else if was_existing {
                    // Parent not yet shown but lane exists: ├ (T-junction, line continues down)
                    cells[start_idx] = CellType::TeeRight(parent_color);
                } else {
                    // New lane for parent: ╭ (branch starts here, continues down)
                    cells[start_idx] = CellType::BranchRight(parent_color);
                }
                oids[start_idx] = (Some(edge), None);
            }
        }
    }

    (cells, oids)
}

/// The `(child, parent)` edge of the pass-through pipe on `lane`, whose awaited
/// parent is `awaited`. Returns `None` when the lane has no recorded child (it
/// then can't be a real traced edge, so it must not light up).
fn lane_edge(
    active_lane_children: &[Option<Oid>],
    lane: usize,
    awaited: Oid,
) -> Option<CellEdge> {
    active_lane_children
        .get(lane)
        .copied()
        .flatten()
        .map(|child| (child, awaited))
}

/// Build fork connector row cells (multiple branches from the same parent)
/// Example: ├─┴─╯ (main lane connecting to multiple branch lanes)
#[allow(clippy::too_many_arguments)] // cohesive lane/color/oid inputs; a struct adds indirection
fn build_fork_connector_cells(
    main_lane: usize,
    main_color: usize,
    fork_oid: Oid,
    merging_lanes: &[(usize, usize)], // (lane, color_index)
    active_lanes: &[Option<Oid>],
    active_lane_children: &[Option<Oid>],
    oid_color_index: &HashMap<Oid, usize>,
    lane_color_index: &HashMap<usize, usize>,
    max_lane: usize,
) -> (Vec<CellType>, Vec<CellOids>) {
    let mut cells = vec![CellType::Empty; (max_lane + 1) * 2];
    // Each stroke's edge is (that lane's child → the fork commit). A merging
    // lane's lead-in strokes therefore climb from the *feature* commit, not the
    // fork commit, so tracing the trunk does not light them. Pass-through pipes
    // keep their own edge.
    let mut oids: Vec<CellOids> = vec![(None, None); cells.len()];

    // Sorted list of merging lane numbers
    let mut merging_lane_nums: Vec<usize> = merging_lanes.iter().map(|(l, _)| *l).collect();
    merging_lane_nums.sort();

    // Draw a T junction on the main lane — edge (main lane's child → fork commit).
    let main_cell_idx = main_lane * 2;
    if main_cell_idx < cells.len() {
        cells[main_cell_idx] = CellType::TeeRight(main_color);
        oids[main_cell_idx] = (lane_edge(active_lane_children, main_lane, fork_oid), None);
    }

    // Draw vertical lines for active lanes (except main and merging lanes)
    for (lane_idx, lane_oid) in active_lanes.iter().enumerate() {
        if let Some(oid) = lane_oid {
            if lane_idx != main_lane && !merging_lane_nums.contains(&lane_idx) {
                let cell_idx = lane_idx * 2;
                if cell_idx < cells.len() {
                    let color = lane_color_index
                        .get(&lane_idx)
                        .copied()
                        .or_else(|| oid_color_index.get(oid).copied())
                        .unwrap_or(lane_idx);
                    cells[cell_idx] = CellType::Pipe(color);
                    oids[cell_idx] = (lane_edge(active_lane_children, lane_idx, *oid), None);
                }
            }
        }
    }

    // Rightmost merging lane
    let rightmost_lane = *merging_lane_nums.last().unwrap_or(&main_lane);

    // Draw connectors to merging lanes
    for &(merge_lane, merge_color) in merging_lanes {
        // This merging lane's stroke climbs from its own child up to the fork.
        let merge_edge = lane_edge(active_lane_children, merge_lane, fork_oid);
        // Horizontal line from main lane to merging lane
        for col in (main_lane * 2 + 1)..(merge_lane * 2) {
            if col < cells.len() {
                let existing = cells[col];
                if let CellType::Pipe(pl) = existing {
                    cells[col] = CellType::HorizontalPipe(merge_color, pl);
                    // Fork stroke crossing an unrelated pipe: keep both edges.
                    oids[col] = (merge_edge, oids[col].0);
                } else if existing == CellType::Empty {
                    cells[col] = CellType::Horizontal(merge_color);
                    oids[col] = (merge_edge, None);
                } else if matches!(existing, CellType::Horizontal(_)) {
                    // Shared run: a nearer merging lane already drew this
                    // column. Redraw in this lane's color but keep the earlier
                    // lane's edge, so tracing either branch lights the stroke.
                    cells[col] = CellType::Horizontal(merge_color);
                    oids[col] = (merge_edge, oids[col].0.or(oids[col].1));
                } else if oids[col].1.is_none() {
                    // A junction marker we can't redraw (a nearer lane's ┴):
                    // co-route this lane's edge through it for tracing.
                    oids[col].1 = merge_edge;
                }
            }
        }

        // End of merge lane
        let end_idx = merge_lane * 2;
        if end_idx < cells.len() {
            if merge_lane == rightmost_lane {
                // Rightmost lane uses ╯
                cells[end_idx] = CellType::MergeLeft(merge_color);
            } else {
                // Middle lanes use ┴
                cells[end_idx] = CellType::TeeUp(merge_color);
            }
            oids[end_idx] = (merge_edge, None);
        }
    }

    (cells, oids)
}

/// Branch tracing needs at least this many lanes to be worth doing — a linear
/// or single-branch graph has nothing to disambiguate, so tracing stays off.
pub const MIN_TRACE_LANES: usize = 3;

/// Whether the graph is branchy enough (`> 2` lanes) for tracing to help.
pub fn graph_has_enough_lanes(layout: &GraphLayout) -> bool {
    layout.max_lane + 1 >= MIN_TRACE_LANES
}

/// The set of commit OIDs forming the selected commit's branch line: the
/// commit itself, its first-parent ancestry walking DOWN the graph, and its
/// first-parent descendants walking UP along the same lane. Pure over the
/// layout — never re-walks git. Returns empty for a connector/uncommitted row.
pub fn lineage_oids(layout: &GraphLayout, selected_full_idx: usize) -> HashSet<Oid> {
    let mut set = HashSet::new();
    let Some(sel) = layout
        .nodes
        .get(selected_full_idx)
        .and_then(|n| n.commit.as_ref())
        .map(|c| c.oid)
    else {
        return set;
    };

    // oid -> row index, for commit-carrying rows only.
    let oid_row: HashMap<Oid, usize> = layout
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n.commit.as_ref().map(|c| (c.oid, i)))
        .collect();

    set.insert(sel);

    // DOWN: follow the first parent (which inherits the commit's lane) as long
    // as it's visible in the graph. An off-graph parent ends the walk: lanes
    // are only ever assigned loaded commits, so no rendered edge can reference
    // it and including it would change nothing.
    let mut cur = sel;
    while let Some(&row) = oid_row.get(&cur) {
        let first_parent = layout.nodes[row]
            .commit
            .as_ref()
            .and_then(|c| c.parent_oids.first().copied())
            .filter(|p| oid_row.contains_key(p));
        match first_parent {
            Some(p) if set.insert(p) => cur = p,
            _ => break,
        }
    }

    // UP: follow the child that continues this lane — the one whose first parent
    // is `cur` and which sits on the same lane (first-parent inheritance keeps
    // the lane number, so this uniquely picks the branch-line continuation and
    // never leaks onto a reused lane's unrelated occupant).
    let mut cur = sel;
    loop {
        let Some(&cur_row) = oid_row.get(&cur) else {
            break;
        };
        let cur_lane = layout.nodes[cur_row].lane;
        let child = layout.nodes.iter().find_map(|n| {
            let c = n.commit.as_ref()?;
            let first_parent = c.parent_oids.first()?;
            (*first_parent == cur && n.lane == cur_lane && !set.contains(&c.oid))
                .then_some(c.oid)
        });
        match child {
            Some(ch) => {
                set.insert(ch);
                cur = ch;
            }
            None => break,
        }
    }

    set
}

/// Row index of the next commit on the SAME graph lane as the selection,
/// following the line toward older history (Ctrl+Down): the selected
/// commit's first parent, when it is loaded and drawn in the graph.
///
/// First-parent edges always inherit the child's lane number (see
/// `build_graph`'s "First parent uses the same lane" step, including the
/// fork-point special case), so any row this finds is guaranteed to be on the
/// same visual line — no lane check is needed here, unlike
/// [`same_lane_descendant_row`]. Interleaved commits from other branches
/// (drawn on different lanes between the selection and its parent's row) are
/// skipped automatically since the target is found by OID, not by adjacent
/// row index.
///
/// Returns `None` for a connector/uncommitted row, a root commit (no
/// parents), or a first parent outside the loaded window — i.e. the end of
/// the lane.
pub fn same_lane_ancestor_row(layout: &GraphLayout, selected_full_idx: usize) -> Option<usize> {
    let commit = layout.nodes.get(selected_full_idx)?.commit.as_ref()?;
    let first_parent = *commit.parent_oids.first()?;
    layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(first_parent))
}

/// Row index of the next commit on the SAME graph lane as the selection,
/// following the line toward newer history (Ctrl+Up): the child whose first
/// parent is the selected commit AND which sits on the selection's lane.
///
/// The lane check disambiguates a fork point with several children — only
/// one of them continues this exact line via its first parent; the others
/// are merge commits that spawn onto new lanes (their non-first-parent edge
/// absorbs this line instead of continuing it).
///
/// When no same-lane first-parent child exists — the selection sits at the
/// tip of its lane — falls back to [`merge_commit_row`]: the commit on
/// another lane that merged this lane in (this lane's oid as one of ITS
/// non-first parents). This lets Ctrl+Up climb from a merged branch's tip
/// straight to the merge commit instead of no-opping there.
///
/// Returns `None` only when nothing continues this lane upward at all —
/// an unmerged branch tip, a connector/uncommitted row, or an out-of-range
/// index.
pub fn same_lane_descendant_row(layout: &GraphLayout, selected_full_idx: usize) -> Option<usize> {
    let node = layout.nodes.get(selected_full_idx)?;
    let commit = node.commit.as_ref()?;
    let (cur_oid, cur_lane) = (commit.oid, node.lane);
    layout
        .nodes
        .iter()
        .position(|n| {
            n.lane == cur_lane
                && n.commit
                    .as_ref()
                    .is_some_and(|c| c.parent_oids.first() == Some(&cur_oid))
        })
        .or_else(|| merge_commit_row(layout, cur_oid))
}

/// Row index of the commit that merged `oid` in — i.e. the earliest-row
/// commit whose `parent_oids` contains `oid` at some index `i > 0` (a
/// non-first, "merged in" parent edge; same rule `trace_lit_edges` uses to
/// light a merge arc). Lane-agnostic: the merge commit lives on the branch
/// being merged INTO, never on `oid`'s own lane.
///
/// If more than one commit merged this lane in, `layout.nodes` is walked
/// top to bottom (newest first) so `position` naturally returns the
/// topmost one.
fn merge_commit_row(layout: &GraphLayout, oid: Oid) -> Option<usize> {
    layout.nodes.iter().position(|n| {
        n.commit
            .as_ref()
            .is_some_and(|c| c.parent_oids.iter().skip(1).any(|p| *p == oid))
    })
}

/// The set of edges lit by tracing `lineage`, mapped to the commit whose lane
/// color the lit stroke should take.
///
/// An edge `(child, parent)` lights when:
/// - BOTH endpoints are on the lineage (the branch line itself) — colored by
///   the child, so each stroke matches the line it belongs to; or
/// - the PARENT is on the lineage and it is a non-first parent of the child —
///   a merge absorbing the traced branch. Colored by the on-line parent, so
///   the branch's merge arc reads in the branch's own color.
///
/// A single on-line endpoint is otherwise NOT enough: the fork commit sits on
/// the trunk lineage while another branch's lead-in stroke climbs into an
/// off-lineage lane (`first parent == on-line parent` distinguishes that
/// lead-in from a merge arc). Commit dots light via their `(oid, oid)` self
/// edge.
pub fn trace_lit_edges(
    layout: &GraphLayout,
    lineage: &HashSet<Oid>,
) -> HashMap<CellEdge, Oid> {
    let mut lit = HashMap::new();
    for node in &layout.nodes {
        let Some(c) = node.commit.as_ref() else {
            continue;
        };
        if lineage.contains(&c.oid) {
            lit.insert((c.oid, c.oid), c.oid);
        }
        for (i, p) in c.parent_oids.iter().enumerate() {
            if lineage.contains(&c.oid) && lineage.contains(p) {
                lit.insert((c.oid, *p), c.oid);
            } else if lineage.contains(p) && i > 0 {
                lit.insert((c.oid, *p), *p);
            }
        }
    }
    lit
}

/// Whether a single `(child, parent)` edge is lit by the trace. `None` (no
/// edge) is never lit. See [`trace_lit_edges`] for the rules.
pub fn edge_is_traced(edge: Option<CellEdge>, lit: &HashMap<CellEdge, Oid>) -> bool {
    edge.is_some_and(|e| lit.contains_key(&e))
}

/// Whether a cell (by its `(primary, secondary)` edges) is lit by the trace.
/// Either edge lights the cell — the secondary covers a lineage pipe crossed
/// underneath a `HorizontalPipe` or a co-routed shared horizontal. Used by the
/// Unicode text path; the pixel path handles the two edges independently (see
/// `apply_trace_dim`).
pub fn cell_is_traced(oids: CellOids, lit: &HashMap<CellEdge, Oid>) -> bool {
    edge_is_traced(oids.0, lit) || edge_is_traced(oids.1, lit)
}

/// The set of commit OIDs that would VANISH if "hide merged branches" were
/// toggled on — this is the selection the "dim merged branches" setting (issue
/// #108) greys: both the commit rows and the graph strokes touching them.
///
/// Mirrors hide-merged (#91) EXACTLY, by construction rather than by analogy
/// (#111): hide keeps precisely the first-parent chains of the live (visible,
/// non-merged) refs — `Revwalk::simplify_first_parent` over their tips — so the
/// dimmed set is the complement over the loaded commits:
///
/// ```text
///   loaded_commits  \  first_parent_chains(live_tips)
/// ```
///
/// The earlier formulation walked from *classified merged branches' tips*,
/// which missed every side lane whose ref no longer exists — the common case
/// for merge-commit PRs when GitHub deletes the branch on merge: hide removed
/// those lanes (the first-parent walk needs no ref) but dim left them bright.
/// A set difference from the live chains needs no merged tips at all.
///
/// Computed purely over the already-loaded `commits`' parent edges — no fresh
/// revwalk. Off-graph parents end a chain walk (only loaded commits can be
/// dimmed, matching what the renderer draws).
///
/// - `live_tips`: tip OIDs of every visible non-merged ref, plus HEAD and the
///   stash entries (their nodes must never dim); their first-parent chains are
///   protected, so trunk history — including everything past a merged branch's
///   fork point — stays lit.
pub fn merged_lane_oids(commits: &[CommitInfo], live_tips: &[Oid]) -> HashSet<Oid> {
    // oid -> its parent OIDs, for loaded commits only.
    let parents: HashMap<Oid, &[Oid]> = commits
        .iter()
        .map(|c| (c.oid, c.parent_oids.as_slice()))
        .collect();

    // Protected: each live tip's first-parent chain, walked while loaded.
    let mut live: HashSet<Oid> = HashSet::new();
    for &tip in live_tips {
        let mut cur = tip;
        while parents.contains_key(&cur) && live.insert(cur) {
            match parents[&cur].first().copied() {
                Some(p) => cur = p,
                None => break,
            }
        }
    }

    // Everything else that is loaded would vanish under hide-merged's
    // first-parent walk — merged branches' exclusive commits (classified or
    // not) and the side lanes of already-deleted merged-in branches alike.
    commits
        .iter()
        .map(|c| c.oid)
        .filter(|oid| !live.contains(oid))
        .collect()
}

/// Whether a single `(child, parent)` edge touches a dimmed (merged-lane)
/// commit: either endpoint is in `merged`. An edge to/from a vanishing commit
/// is exactly a stroke hide-merged would remove, so this is what the merged-lane
/// dim (issue #108) fades. `None` (no edge) never touches.
pub fn edge_touches_merged(edge: Option<CellEdge>, merged: &HashSet<Oid>) -> bool {
    edge.is_some_and(|(child, parent)| merged.contains(&child) || merged.contains(&parent))
}

/// Whether a cell (by its `(primary, secondary)` edges) touches a merged-lane
/// commit. Either edge counts — the Unicode text path dims the whole glyph;
/// the pixel path fades each edge independently (see `apply_merged_lane_dim`).
pub fn cell_touches_merged(oids: CellOids, merged: &HashSet<Oid>) -> bool {
    edge_touches_merged(oids.0, merged) || edge_touches_merged(oids.1, merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_with_parents(n: usize) -> CommitInfo {
        CommitInfo {
            oid: Oid::zero(),
            short_id: "0000000".to_string(),
            author_name: "a".to_string(),
            author_email: "a@b".to_string(),
            timestamp: chrono::Local::now(),
            message: "m".to_string(),
            full_message: "m".to_string(),
            parent_oids: vec![Oid::zero(); n],
        }
    }

    fn node(commit: Option<CommitInfo>, is_uncommitted: bool) -> GraphNode {
        GraphNode {
            commit,
            lane: 0,
            color_index: 0,
            branch_names: Vec::new(),
            tag_names: Vec::new(),
            is_head: false,
            is_uncommitted,
            is_stash: false,
            stash_label: None,
            uncommitted_count: None,
            cells: Vec::new(),
            cell_oids: Vec::new(),
        }
    }

    #[test]
    fn is_merge_needs_two_or_more_parents() {
        assert!(node(Some(commit_with_parents(2)), false).is_merge());
        assert!(node(Some(commit_with_parents(3)), false).is_merge());
        assert!(!node(Some(commit_with_parents(1)), false).is_merge());
        assert!(!node(Some(commit_with_parents(0)), false).is_merge());
        // A connector row (no commit) is never a merge.
        assert!(!node(None, false).is_merge());
    }

    #[test]
    fn is_connector_only_for_commitless_non_uncommitted_rows() {
        assert!(node(None, false).is_connector());
        // The uncommitted-changes row also has no commit but is not a connector.
        assert!(!node(None, true).is_connector());
        // A real commit row is not a connector.
        assert!(!node(Some(commit_with_parents(1)), false).is_connector());
    }

    // ── insert_uncommitted_node: crossing lanes stay visible ─────────

    fn oid(byte: u8) -> Oid {
        Oid::from_bytes(&[byte; 20]).unwrap()
    }

    /// A commit node carrying a specific oid, lane, and cell row.
    fn commit_node(oid: Oid, lane: usize, cells: Vec<CellType>) -> GraphNode {
        let mut c = commit_with_parents(1);
        c.oid = oid;
        GraphNode {
            commit: Some(c),
            lane,
            color_index: 0,
            branch_names: Vec::new(),
            tag_names: Vec::new(),
            is_head: false,
            is_uncommitted: false,
            is_stash: false,
            stash_label: None,
            uncommitted_count: None,
            cells,
            cell_oids: Vec::new(),
        }
    }

    #[test]
    fn uncommitted_node_uses_head_lane_when_free_above() {
        // HEAD is the newest commit on lane 0, its lane clear above → no
        // connector, just the uncommitted dot on the same lane.
        let head = oid(1);
        let mut nodes = vec![commit_node(head, 0, vec![CellType::Commit(0), CellType::Empty])];
        let mut max_lane = 0;
        insert_uncommitted_node(&mut nodes, &mut max_lane, Some(head), Some(3));

        assert_eq!(nodes.len(), 2);
        assert!(nodes[0].is_uncommitted);
        assert_eq!(nodes[0].lane, 0);
        assert_eq!(nodes[0].cells[0], CellType::Commit(UNCOMMITTED_COLOR_INDEX));
        // No connector markers on the HEAD row.
        let head_row = &nodes[1];
        assert!(!head_row
            .cells
            .iter()
            .any(|c| matches!(c, CellType::MergeLeft(_) | CellType::MergeRight(_))));
    }

    #[test]
    fn uncommitted_connector_crosses_lanes_without_severing_them() {
        // Regression: the grey uncommitted connector used to clobber a lane that
        // was free above HEAD but active on the HEAD row, severing it.
        //
        // Row 0 (newer commit): occupies HEAD's lane 0 above HEAD, so HEAD's lane
        //   is unavailable and a connector is needed.
        // Row 1 (HEAD, lane 0): a pink pipe (color 4) sits on lane 1 (col 2),
        //   active on the HEAD row itself.
        let newer = oid(1);
        let head = oid(2);
        let mut nodes = vec![
            commit_node(
                newer,
                1,
                vec![
                    CellType::Pipe(3), // col 0: occupies HEAD's lane above HEAD
                    CellType::Empty,
                    CellType::Empty, // col 2: free above HEAD
                    CellType::Empty,
                ],
            ),
            commit_node(
                head,
                0,
                vec![
                    CellType::Commit(0),
                    CellType::Empty,
                    CellType::Pipe(4), // col 2: the pink lane, active on the HEAD row
                    CellType::Empty,
                ],
            ),
        ];
        let mut max_lane = 1;
        insert_uncommitted_node(&mut nodes, &mut max_lane, Some(head), Some(1));

        // The old off-by-one would pick lane 1 (col 2 free *above* HEAD) and its
        // curve would overwrite the pink pipe. The fix requires the lane free on
        // the HEAD row too, so it moves right to lane 2 (col 4).
        assert!(nodes[0].is_uncommitted);
        assert_eq!(nodes[0].lane, 2, "uncommitted lane avoids the active pink lane");
        assert_eq!(max_lane, 2);

        // HEAD is now at index 2 (uncommitted inserted at 0, newer at 1).
        let head_row = &nodes[2];
        // The pink lane survives as a HorizontalPipe crossing — the connector
        // draws over it without erasing it.
        assert_eq!(
            head_row.cells[2],
            CellType::HorizontalPipe(UNCOMMITTED_COLOR_INDEX, 4),
            "crossed pink lane must remain visible under the connector"
        );
        // The pink lane is NOT replaced by a curve.
        assert!(
            !matches!(
                head_row.cells[2],
                CellType::MergeLeft(_) | CellType::MergeRight(_)
            ),
            "crossing cell must not become the connector's curve"
        );
        // The curve endpoint lands on the free lane 2 (col 4).
        assert_eq!(
            head_row.cells[4],
            CellType::MergeLeft(UNCOMMITTED_COLOR_INDEX)
        );
    }

    // ── branch tracing: edge-pair identity ───────────────────────────────

    fn ci(oid: Oid, parents: Vec<Oid>) -> CommitInfo {
        let mut c = commit_with_parents(0);
        c.oid = oid;
        c.parent_oids = parents;
        c
    }

    /// Trunk A—B—C (A newest) with feature F1—F2 branched off C and merged into
    /// A: `A`'s 2nd parent is `F1`, `F2`'s first parent is `C`. `C`'s first
    /// parent `Z` is off-graph (below the loaded window). Returns the layout and
    /// the OIDs `[A, B, C, F1, F2, Z]`.
    ///
    /// Layout rows (verified): 0=A(merge) 1=B 2=F1 3=F2 4=fork-connector 5=C.
    fn trace_fixture() -> (GraphLayout, [Oid; 6]) {
        let (a, b, c, f1, f2, z) = (oid(1), oid(2), oid(3), oid(4), oid(5), oid(9));
        let commits = vec![
            ci(a, vec![b, f1]),
            ci(b, vec![c]),
            ci(f1, vec![f2]),
            ci(f2, vec![c]),
            ci(c, vec![z]),
        ];
        let layout = build_graph(&commits, &[], &[], &[], None, None, &[]);
        (layout, [a, b, c, f1, f2, z])
    }

    /// Row index of the commit carrying `target`.
    fn row_of(layout: &GraphLayout, target: Oid) -> usize {
        layout
            .nodes
            .iter()
            .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(target))
            .expect("commit present in layout")
    }

    /// Whether the cell at `(row, col)` is lit by the trace.
    fn traced(layout: &GraphLayout, lit: &HashMap<CellEdge, Oid>, row: usize, col: usize) -> bool {
        cell_is_traced(
            layout.nodes[row].cell_oids.get(col).copied().unwrap_or((None, None)),
            lit,
        )
    }

    #[test]
    fn tracing_trunk_lights_only_the_trunk_line() {
        let (layout, [_a, b, c, _f1, _f2, _z]) = trace_fixture();
        let lineage = trace_lit_edges(&layout, &lineage_oids(&layout, row_of(&layout, b)));

        let (a_row, b_row, c_row) = (row_of(&layout, oid(1)), row_of(&layout, b), row_of(&layout, c));
        let conn_row = c_row - 1; // fork connector immediately precedes C

        // Trunk commits and pipes are traced.
        assert!(traced(&layout, &lineage, a_row, 0), "A's dot is on the trunk lineage");
        assert!(traced(&layout, &lineage, b_row, 0), "B's dot");
        assert!(traced(&layout, &lineage, c_row, 0), "C's dot");
        assert!(traced(&layout, &lineage, row_of(&layout, oid(4)), 0), "trunk pipe B→C at F1's row");
        assert!(traced(&layout, &lineage, row_of(&layout, oid(5)), 0), "trunk pipe B→C at F2's row");

        // The merge into A (its curve/lead-in to F1) is NOT traced — the bug.
        assert!(!traced(&layout, &lineage, a_row, 1), "merge lead-in A→F1 stays dim");
        assert!(!traced(&layout, &lineage, a_row, 2), "merge curve A→F1 stays dim");
        // The feature pipe (spawned by the merge, edge A→F1) is NOT traced.
        assert!(!traced(&layout, &lineage, b_row, 2), "feature pipe at B's row stays dim");

        // Fork connector: the main-lane ├ (edge B→C) is traced; the merging
        // strokes climbing into the feature lane (edge F2→C) are NOT.
        assert!(traced(&layout, &lineage, conn_row, 0), "fork main-lane ├ is traced");
        assert!(!traced(&layout, &lineage, conn_row, 1), "fork merging lead-in stays dim");
        assert!(!traced(&layout, &lineage, conn_row, 2), "fork merging curve stays dim");
    }

    #[test]
    fn tracing_feature_lights_feature_and_fork_strokes() {
        let (layout, [_a, _b, c, f1, f2, _z]) = trace_fixture();
        let lineage = trace_lit_edges(&layout, &lineage_oids(&layout, row_of(&layout, f1)));

        let a_row = row_of(&layout, oid(1));
        let b_row = row_of(&layout, oid(2));
        let (f1_row, f2_row, c_row) = (row_of(&layout, f1), row_of(&layout, f2), row_of(&layout, c));
        let conn_row = c_row - 1;

        // The feature line and the fork parent are traced.
        assert!(traced(&layout, &lineage, f1_row, 2), "F1's dot");
        assert!(traced(&layout, &lineage, f2_row, 2), "F2's dot");
        assert!(traced(&layout, &lineage, c_row, 0), "C's dot (fork parent, on the F line)");
        // The fork commit's connector strokes for the F lane ARE traced —
        // both endpoints (F2, C) are on the feature lineage.
        assert!(traced(&layout, &lineage, conn_row, 1), "fork merging lead-in for F lane");
        assert!(traced(&layout, &lineage, conn_row, 2), "fork merging curve for F lane");

        // The merge arc absorbing the feature (edge A→F1, F1 a non-first
        // parent) lights from the branch side, completing the branch's arc.
        assert!(traced(&layout, &lineage, a_row, 1), "merge lead-in A→F1 lights");
        assert!(traced(&layout, &lineage, a_row, 2), "merge curve A→F1 lights");
        assert!(traced(&layout, &lineage, b_row, 2), "feature pipe above F1 lights");

        // UP from F1 finds no child (A's FIRST parent is B), so A's own dot
        // and the trunk-only cells stay untraced.
        assert!(!traced(&layout, &lineage, a_row, 0), "A's dot stays dim");
        assert!(!traced(&layout, &lineage, b_row, 0), "B's dot (trunk-only) stays dim");
        assert!(!traced(&layout, &lineage, conn_row, 0), "fork main-lane ├ stays dim");
    }

    #[test]
    fn off_graph_parent_never_appears_in_any_rendered_edge() {
        // Lanes are only ever assigned loaded commits, so a commit whose first
        // parent is off-graph must not produce a cell edge referencing it —
        // the lineage walk can safely stop at the graph boundary.
        let (layout, [_a, _b, _c, _f1, _f2, z]) = trace_fixture();
        let references_z = layout.nodes.iter().any(|n| {
            n.cell_oids.iter().any(|(p, s)| {
                p.is_some_and(|(c, pa)| c == z || pa == z)
                    || s.is_some_and(|(c, pa)| c == z || pa == z)
            })
        });
        assert!(!references_z, "no rendered edge may reference off-graph Z");
    }

    // ── same-lane navigation (Ctrl+Up / Ctrl+Down) ────────────────────────

    /// Trunk A—B—C—D (D root) interleaved with a feature F1—F2 branched off D
    /// and merged into B (`B`'s parents are `[C, F1]`), listed in an order
    /// that interleaves the two lines in the layout: A, B, F1, F2, C,
    /// (fork-connector), D. Verified rows: 0=A 1=B 2=F1 3=F2 4=fork-connector
    /// 5=D. Returns the layout and `[A, B, C, D, F1, F2]`.
    fn lane_nav_fixture() -> (GraphLayout, [Oid; 6]) {
        let (a, b, c, d, f1, f2) = (oid(1), oid(2), oid(3), oid(4), oid(5), oid(6));
        let commits = vec![
            ci(a, vec![b]),
            ci(b, vec![c, f1]),
            ci(f1, vec![f2]),
            ci(f2, vec![d]),
            ci(c, vec![d]),
            ci(d, vec![]),
        ];
        let layout = build_graph(&commits, &[], &[], &[], None, None, &[]);
        (layout, [a, b, c, d, f1, f2])
    }

    #[test]
    fn same_lane_ancestor_follows_first_parent_skipping_other_lanes() {
        let (layout, [a, b, c, d, _f1, _f2]) = lane_nav_fixture();

        // A -> B: adjacent rows, no skip needed.
        assert_eq!(
            same_lane_ancestor_row(&layout, row_of(&layout, a)),
            Some(row_of(&layout, b))
        );
        // B -> C: F1, F2 (and the fork connector) sit between them in the
        // rendered rows but are on a different lane, so the jump must skip
        // straight past them to C.
        let b_row = row_of(&layout, b);
        let c_row = row_of(&layout, c);
        assert!(c_row > b_row + 1, "fixture must interleave rows between B and C");
        assert_eq!(same_lane_ancestor_row(&layout, b_row), Some(c_row));
        // C -> D: trunk continues to the root.
        assert_eq!(
            same_lane_ancestor_row(&layout, c_row),
            Some(row_of(&layout, d))
        );
        // D is a root commit: no further ancestor on the lane.
        assert_eq!(same_lane_ancestor_row(&layout, row_of(&layout, d)), None);
    }

    #[test]
    fn same_lane_descendant_follows_first_parent_children_skipping_other_lanes() {
        let (layout, [a, b, c, _d, _f1, _f2]) = lane_nav_fixture();

        // C -> B: the inverse of the ancestor jump, skipping the same
        // interleaved feature rows.
        let b_row = row_of(&layout, b);
        let c_row = row_of(&layout, c);
        assert_eq!(same_lane_descendant_row(&layout, c_row), Some(b_row));
        // B -> A: adjacent rows.
        assert_eq!(
            same_lane_descendant_row(&layout, b_row),
            Some(row_of(&layout, a))
        );
        // A is the tip of the lane: no further descendant.
        assert_eq!(same_lane_descendant_row(&layout, row_of(&layout, a)), None);
    }

    #[test]
    fn same_lane_navigation_ignores_non_first_parent_merges_for_ancestor_walk() {
        // The feature line F1—F2 is absorbed into B as a *non-first* parent.
        // Descending (Ctrl+Down, first-parent only) must never treat that as
        // continuing the trunk lane.
        let (layout, [_a, _b, _c, d, f1, f2]) = lane_nav_fixture();

        let f1_row = row_of(&layout, f1);
        let f2_row = row_of(&layout, f2);
        // The feature line's own first-parent chain still works.
        assert_eq!(same_lane_ancestor_row(&layout, f1_row), Some(f2_row));
        // F2's first parent is the fork point D — reachable...
        assert_eq!(
            same_lane_ancestor_row(&layout, f2_row),
            Some(row_of(&layout, d))
        );
    }

    #[test]
    fn same_lane_descendant_at_lane_top_jumps_to_the_merge_commit() {
        // F1 is the tip of the feature lane: no commit's FIRST parent is F1
        // (B's first parent is C), so the old behavior no-opped here. B's
        // parents are [C, F1] though — F1 is B's non-first ("merged in")
        // parent — so Ctrl+Up from F1 should now land on B, the commit that
        // merged this lane into the trunk.
        let (layout, [_a, b, _c, _d, f1, _f2]) = lane_nav_fixture();

        assert_eq!(
            same_lane_descendant_row(&layout, row_of(&layout, f1)),
            Some(row_of(&layout, b))
        );
    }

    #[test]
    fn same_lane_descendant_unmerged_branch_tip_still_none() {
        // A branch tip that nothing ever merges (not even as a non-first
        // parent) must still no-op — a true end of the lane.
        let (a, b, tip) = (oid(1), oid(2), oid(7));
        let commits = vec![ci(tip, vec![b]), ci(a, vec![b]), ci(b, vec![])];
        let layout = build_graph(&commits, &[], &[], &[], None, None, &[]);

        assert_eq!(same_lane_descendant_row(&layout, row_of(&layout, tip)), None);
    }

    #[test]
    fn same_lane_descendant_prefers_first_parent_child_over_merge_fallback() {
        // Mid-lane navigation is unchanged: when a same-lane first-parent
        // child exists, it wins even though this commit is ALSO merged
        // elsewhere as a non-first parent — the fallback only kicks in when
        // the first-parent search comes up empty.
        let (layout, [_a, b, c, _d, _f1, _f2]) = lane_nav_fixture();

        // B's first parent is C, so walking up from C lands on B exactly as
        // before — B merging F1 in as a non-first parent doesn't interfere.
        assert_eq!(
            same_lane_descendant_row(&layout, row_of(&layout, c)),
            Some(row_of(&layout, b))
        );
    }

    #[test]
    fn same_lane_navigation_is_none_for_connector_and_missing_rows() {
        let (layout, _) = lane_nav_fixture();
        let connector_row = layout
            .nodes
            .iter()
            .position(|n| n.is_connector())
            .expect("fixture has a fork connector row");

        assert_eq!(same_lane_ancestor_row(&layout, connector_row), None);
        assert_eq!(same_lane_descendant_row(&layout, connector_row), None);
        // Out-of-range index.
        assert_eq!(same_lane_ancestor_row(&layout, layout.nodes.len()), None);
        assert_eq!(same_lane_descendant_row(&layout, layout.nodes.len()), None);
    }

    // ── HEAD anchored to lane 0 (leftmost, VSCode-like) ───────────────────

    fn branch(name: &str, tip: Oid, is_head: bool) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            is_head,
            is_remote: false,
            upstream: None,
            tip_oid: tip,
            ahead: 0,
            behind: 0,
        }
    }

    /// Two branches diverging from a shared base Z: `main` (M1—M2—Z) and a
    /// feature (F1—Z). Rows are ordered newest-first so the feature tip is NOT
    /// row 0 — M1 is processed before it — which exercises the anchoring rather
    /// than the incidental "first tip processed owns lane 0". Returns the commit
    /// list and `[M1, M2, F1, Z]`.
    fn diverged_fixture() -> (Vec<CommitInfo>, [Oid; 4]) {
        let (m1, m2, f1, z) = (oid(1), oid(2), oid(3), oid(9));
        let commits = vec![
            ci(m1, vec![m2]),
            ci(f1, vec![z]),
            ci(m2, vec![z]),
            ci(z, vec![]),
        ];
        (commits, [m1, m2, f1, z])
    }

    #[test]
    fn head_feature_branch_claims_lane_zero() {
        let (commits, [m1, m2, f1, z]) = diverged_fixture();
        let branches = [branch("main", m1, false), branch("feature", f1, true)];
        let layout = build_graph(&commits, &branches, &[], &[], None, Some(f1), &[]);

        // HEAD's line (F1 and its ancestor Z) sits at the far-left lane 0...
        assert_eq!(layout.nodes[row_of(&layout, f1)].lane, 0, "HEAD tip at lane 0");
        assert_eq!(
            layout.nodes[row_of(&layout, z)].lane, 0,
            "HEAD's first-parent ancestor stays at lane 0"
        );
        // ...even though an older branch's commit (M1) is drawn first (row 0),
        // and that branch shifts one lane to the right.
        assert!(layout.nodes[row_of(&layout, m1)].lane >= 1, "other branch shifts right");
        assert!(layout.nodes[row_of(&layout, m2)].lane >= 1, "other branch shifts right");
    }

    #[test]
    fn head_line_owns_the_main_color_others_do_not() {
        let (commits, [m1, _m2, f1, _z]) = diverged_fixture();
        let branches = [branch("main", m1, false), branch("feature", f1, true)];
        let layout = build_graph(&commits, &branches, &[], &[], None, Some(f1), &[]);

        // The lane-0 HEAD line owns the reserved main (blue) colour; the other
        // branch, though processed first, cannot claim it.
        assert_eq!(
            layout.nodes[row_of(&layout, f1)].color_index, MAIN_BRANCH_COLOR,
            "HEAD's line is the blue main line"
        );
        assert_ne!(
            layout.nodes[row_of(&layout, m1)].color_index, MAIN_BRANCH_COLOR,
            "a non-HEAD branch must not collide with the main colour"
        );
    }

    #[test]
    fn detached_head_still_anchored_to_lane_zero() {
        // No branch carries is_head (detached HEAD). Anchoring must use the
        // passed HEAD oid, not branch identity.
        let (commits, [_m1, _m2, f1, z]) = diverged_fixture();
        let layout = build_graph(&commits, &[], &[], &[], None, Some(f1), &[]);

        assert_eq!(layout.nodes[row_of(&layout, f1)].lane, 0, "detached HEAD at lane 0");
        assert_eq!(layout.nodes[row_of(&layout, z)].lane, 0);
        assert_eq!(
            layout.nodes[row_of(&layout, f1)].color_index, MAIN_BRANCH_COLOR,
            "detached HEAD's line is still the blue main line"
        );
    }

    /// The star renders purely on `node.is_head`. A detached HEAD carries no
    /// branch with `is_head`, so `is_head` must be keyed off the passed HEAD oid
    /// or the row loses its star entirely (issue #89).
    #[test]
    fn detached_head_off_branch_row_is_flagged_head() {
        // Detached at a commit that is NOT any branch tip: `x` is a child of the
        // feature tip F1, reachable from no branch. Only the HEAD oid identifies it.
        let (m1, f1, x, z) = (oid(1), oid(3), oid(7), oid(9));
        let commits = vec![
            ci(x, vec![f1]),
            ci(m1, vec![z]),
            ci(f1, vec![z]),
            ci(z, vec![]),
        ];
        let branches = [branch("main", m1, false), branch("feature", f1, false)];
        let layout = build_graph(&commits, &branches, &[], &[], None, Some(x), &[]);

        let head_rows: Vec<usize> = layout
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_head)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(head_rows, vec![row_of(&layout, x)], "exactly the HEAD row is flagged");
    }

    /// Detached at a commit that *is* on a branch (a branch tip). No branch is
    /// `is_head`, but the row must still be flagged (star), keyed off the oid.
    #[test]
    fn detached_head_on_branch_tip_row_is_flagged_head() {
        let (commits, [m1, _m2, f1, _z]) = diverged_fixture();
        // Detached exactly at feature's tip F1; no branch is is_head.
        let branches = [branch("main", m1, false), branch("feature", f1, false)];
        let layout = build_graph(&commits, &branches, &[], &[], None, Some(f1), &[]);

        assert!(layout.nodes[row_of(&layout, f1)].is_head, "detached-on-tip row is HEAD");
        assert!(
            !layout.nodes[row_of(&layout, m1)].is_head,
            "non-HEAD tip is not flagged"
        );
    }

    #[test]
    fn without_head_first_tip_owns_lane_zero() {
        // Fallback: with no HEAD oid, the historical behaviour holds — the
        // first-processed tip keeps lane 0 and the main colour.
        let (commits, [m1, _m2, f1, _z]) = diverged_fixture();
        let layout = build_graph(&commits, &[], &[], &[], None, None, &[]);

        assert_eq!(layout.nodes[row_of(&layout, m1)].lane, 0);
        assert_eq!(layout.nodes[row_of(&layout, m1)].color_index, MAIN_BRANCH_COLOR);
        assert!(layout.nodes[row_of(&layout, f1)].lane >= 1);
    }

    #[test]
    fn branch_ahead_of_head_shares_lane_zero() {
        // `bar` is HEAD (`foo`) plus one commit B1 ahead: B1's first parent is
        // HEAD. The shared first-parent line keeps HEAD at lane 0 with B1 above
        // it, rather than B1 pushing HEAD onto lane 1 and leaving lane 0 empty.
        // A diverged `other` tip is newest (row 0), processed before the line.
        let (foo, b1, other, z) = (oid(1), oid(2), oid(5), oid(9));
        let commits = vec![
            ci(other, vec![z]),
            ci(b1, vec![foo]),
            ci(foo, vec![z]),
            ci(z, vec![]),
        ];
        let layout = build_graph(&commits, &[], &[], &[], None, Some(foo), &[]);

        assert_eq!(layout.nodes[row_of(&layout, foo)].lane, 0, "HEAD stays leftmost");
        assert_eq!(
            layout.nodes[row_of(&layout, b1)].lane, 0,
            "a commit fast-forwarded ahead of HEAD shares lane 0"
        );
        assert!(
            layout.nodes[row_of(&layout, other)].lane >= 1,
            "the diverged branch shifts right"
        );
    }

    #[test]
    fn anchoring_preserves_first_parent_lane_inheritance() {
        // Same-lane navigation relies on first-parent edges inheriting the
        // child's lane. Anchoring only reorders which lane a tip starts on, so
        // the HEAD line's first-parent chain must remain walkable end to end.
        let (commits, [_m1, _m2, f1, z]) = diverged_fixture();
        let layout = build_graph(&commits, &[], &[], &[], None, Some(f1), &[]);

        // F1 -> Z along the (anchored) lane 0.
        assert_eq!(
            same_lane_ancestor_row(&layout, row_of(&layout, f1)),
            Some(row_of(&layout, z))
        );
        // ...and back up, Z -> F1 on the same lane.
        assert_eq!(
            same_lane_descendant_row(&layout, row_of(&layout, z)),
            Some(row_of(&layout, f1))
        );
    }

    // ── squash-merge link lines (#81) ─────────────────────────────────────

    /// The color index a cell is drawn in (`None` for `Empty`).
    fn cell_color(cell: CellType) -> Option<usize> {
        match cell {
            CellType::Empty => None,
            CellType::Pipe(c)
            | CellType::Commit(c)
            | CellType::BranchRight(c)
            | CellType::BranchLeft(c)
            | CellType::MergeRight(c)
            | CellType::MergeLeft(c)
            | CellType::Horizontal(c)
            | CellType::TeeRight(c)
            | CellType::TeeLeft(c)
            | CellType::TeeUp(c)
            | CellType::HorizontalPipe(c, _) => Some(c),
        }
    }

    /// Whether any cell in the layout is drawn in the reserved squash-link grey.
    fn has_squash_grey(layout: &GraphLayout) -> bool {
        layout.nodes.iter().any(|n| {
            n.cells
                .iter()
                .any(|c| cell_color(*c) == Some(SQUASH_LINK_COLOR_INDEX))
        })
    }

    /// A trunk tip `S` and a diverged feature tip `F1`, both loaded. Newest-first
    /// order: S, F1, T, Z. Returns the commit list and `[S, F1, T, Z]`.
    fn squash_link_fixture() -> (Vec<CommitInfo>, [Oid; 4]) {
        let (s, f1, t, z) = (oid(1), oid(4), oid(2), oid(9));
        let commits = vec![
            ci(s, vec![t]),
            ci(f1, vec![z]),
            ci(t, vec![z]),
            ci(z, vec![]),
        ];
        (commits, [s, f1, t, z])
    }

    #[test]
    fn squash_link_off_is_byte_identical_and_adds_no_grey() {
        let (commits, _) = squash_link_fixture();
        let base = build_graph(&commits, &[], &[], &[], None, None, &[]);
        // Building again with no links is byte-identical for every cell.
        let again = build_graph(&commits, &[], &[], &[], None, None, &[]);
        assert_eq!(base.max_lane, again.max_lane);
        for (a, b) in base.nodes.iter().zip(&again.nodes) {
            assert_eq!(a.cells, b.cells, "option-off layout must be stable");
            assert_eq!(a.cell_oids, b.cell_oids);
        }
        assert!(!has_squash_grey(&base), "no link cells when the option is off");
    }

    #[test]
    fn squash_link_draws_grey_and_preserves_real_cells() {
        let (commits, [s, f1, _t, _z]) = squash_link_fixture();
        let base = build_graph(&commits, &[], &[], &[], None, None, &[]);
        let linked = build_graph(&commits, &[], &[], &[], None, None, &[(f1, s)]);

        // The link introduces grey cells; the baseline had none.
        assert!(!has_squash_grey(&base));
        assert!(has_squash_grey(&linked), "the link draws grey cells");

        // Every real (non-Empty) baseline cell survives verbatim: the link only
        // fills empty space / new columns, never disturbing a real stroke.
        for (bi, bn) in base.nodes.iter().enumerate() {
            for (col, bcell) in bn.cells.iter().enumerate() {
                if *bcell != CellType::Empty {
                    assert_eq!(
                        linked.nodes[bi].cells.get(col),
                        Some(bcell),
                        "real cell at row {bi} col {col} must be unchanged by the link"
                    );
                }
            }
        }

        // No commit's parentage changes: is_merge and lineage are identical.
        for i in 0..base.nodes.len() {
            assert_eq!(base.nodes[i].is_merge(), linked.nodes[i].is_merge());
        }
        assert_eq!(
            lineage_oids(&base, row_of(&base, f1)),
            lineage_oids(&linked, row_of(&linked, f1)),
            "the link must not alter the feature's lineage"
        );
    }

    #[test]
    fn squash_link_cells_are_never_traced() {
        let (commits, [s, f1, _t, _z]) = squash_link_fixture();
        let layout = build_graph(&commits, &[], &[], &[], None, None, &[(f1, s)]);

        // Tracing either the feature or the trunk must leave the grey link dim.
        for sel in [f1, s] {
            let lit = trace_lit_edges(&layout, &lineage_oids(&layout, row_of(&layout, sel)));
            for node in &layout.nodes {
                for (i, cell) in node.cells.iter().enumerate() {
                    if cell_color(*cell) == Some(SQUASH_LINK_COLOR_INDEX) {
                        assert!(
                            !cell_is_traced(node.cell_oids[i], &lit),
                            "a squash-link cell must never light under tracing"
                        );
                    }
                }
            }
        }
    }

    /// The single column carrying the grey connector's vertical / elbow cells
    /// (`Pipe`/`Branch*`/`Merge*` — not the horizontal crossing run). Asserts they
    /// all share one column (no dangling half in a second column, #110) and
    /// returns it.
    fn squash_connector_col(layout: &GraphLayout) -> usize {
        let mut col: Option<usize> = None;
        for node in &layout.nodes {
            for (c, cell) in node.cells.iter().enumerate() {
                let is_link_spine = cell_color(*cell) == Some(SQUASH_LINK_COLOR_INDEX)
                    && matches!(
                        cell,
                        CellType::Pipe(_)
                            | CellType::BranchLeft(_)
                            | CellType::BranchRight(_)
                            | CellType::MergeLeft(_)
                            | CellType::MergeRight(_)
                    );
                if is_link_spine {
                    match col {
                        None => col = Some(c),
                        Some(x) => assert_eq!(
                            x, c,
                            "the squash connector must occupy a single column, not a split/dangling second one (#110)"
                        ),
                    }
                }
            }
        }
        col.expect("a squash connector spine cell is present")
    }

    /// #110 repro shape: squash commit S on the trunk (lane 0, upper), feature tip
    /// F two lanes right (lane 2), a live branch H keeping the middle lane busy at
    /// the intermediate row. The connector must hug F's own lane, NOT detour out
    /// to a fresh lane right of the tip (the old "phantom curve into a void").
    #[test]
    fn squash_link_hugs_tip_lane_without_right_detour() {
        let (s, h, f, t, b) = (oid(1), oid(2), oid(3), oid(4), oid(5));
        let commits = vec![
            ci(s, vec![t]), // squash commit, trunk (lane 0)
            ci(h, vec![b]), // a live branch — keeps the middle lane busy
            ci(f, vec![b]), // squash-merged feature tip (lane 2)
            ci(t, vec![b]), // trunk continues
            ci(b, vec![]),  // shared base
        ];
        let base = build_graph(&commits, &[], &[], &[], None, None, &[]);
        let linked = build_graph(&commits, &[], &[], &[], None, None, &[(f, s)]);

        let sr = row_of(&linked, s);
        let fr = row_of(&linked, f);
        let flane = linked.nodes[fr].lane;
        let gcol = squash_connector_col(&linked);

        // The connector rides the tip's OWN column — no lane was added to detour
        // right of the tip (the #110 regression widened the graph by one lane).
        assert_eq!(
            linked.max_lane, base.max_lane,
            "the link must not widen the graph into a right-side detour lane"
        );
        assert_eq!(gcol, flane * 2, "connector rides the tip's own lane column");

        // No grey cell of ANY kind sits right of the tip's column — that column
        // was the phantom void the old geometry ended a half-curve in.
        for node in &linked.nodes {
            for (col, cell) in node.cells.iter().enumerate() {
                if cell_color(*cell) == Some(SQUASH_LINK_COLOR_INDEX) {
                    assert!(
                        col <= gcol,
                        "no squash-link cell may sit right of the tip column (was the #110 void)"
                    );
                }
            }
        }

        // Tip is dot-anchored: its own cell is the commit dot and the grey pipe
        // runs straight down the same column into it (a continuous connector, no
        // separate up-and-right elbow leaving the tip).
        assert!(matches!(linked.nodes[fr].cells[gcol], CellType::Commit(_)));
        assert!(
            matches!(linked.nodes[fr - 1].cells[gcol], CellType::Pipe(SQUASH_LINK_COLOR_INDEX)),
            "grey connector runs straight down into the tip dot"
        );

        // The squash commit is joined by a single grey elbow in that same column.
        assert!(
            matches!(
                linked.nodes[sr].cells[gcol],
                CellType::BranchLeft(SQUASH_LINK_COLOR_INDEX)
                    | CellType::BranchRight(SQUASH_LINK_COLOR_INDEX)
            ),
            "squash commit joins the connector with a grey elbow"
        );

        // Continuity: every intermediate row carries the grey pipe in that column.
        for i in (sr + 1)..fr {
            assert!(
                matches!(linked.nodes[i].cells[gcol], CellType::Pipe(SQUASH_LINK_COLOR_INDEX)),
                "intermediate row {i} carries the grey connector pipe"
            );
        }
    }

    #[test]
    fn squash_link_is_one_continuous_connector_between_endpoints() {
        // Adjacent-row case (original fixture): tip F1 on lane 1, squash S on lane
        // 0 one row above. The connector hugs F1's lane — S elbows in, F1 is
        // dot-anchored — forming ONE join between the two rows.
        //
        // (This replaces the pre-#110 expectation that BOTH endpoint rows carry a
        // curve cell: that assumed the connector always used a third lane distinct
        // from both endpoints, the very assumption that produced the right-side
        // detour. When the connector rides an endpoint's own lane, that endpoint
        // is anchored by its dot and carries no separate curve.)
        let (commits, [s, f1, _t, _z]) = squash_link_fixture();
        let layout = build_graph(&commits, &[], &[], &[], None, None, &[(f1, s)]);

        let sr = row_of(&layout, s);
        let fr = row_of(&layout, f1);
        let flane = layout.nodes[fr].lane;
        let ulane = layout.nodes[sr].lane;
        let gcol = squash_connector_col(&layout);

        assert_eq!(gcol, flane * 2, "connector rides the tip's own lane");
        // Neither endpoint sits right of the other — no detour.
        assert!(gcol <= ulane.max(flane) * 2);

        // Tip dot-anchored; squash commit joined by a grey elbow directly above it
        // (`Branch*` touches the bottom edge, so the tip dot below connects up).
        assert!(matches!(layout.nodes[fr].cells[gcol], CellType::Commit(_)));
        assert!(matches!(
            layout.nodes[sr].cells[gcol],
            CellType::BranchLeft(SQUASH_LINK_COLOR_INDEX)
                | CellType::BranchRight(SQUASH_LINK_COLOR_INDEX)
        ));
    }

    #[test]
    fn squash_link_absent_when_an_endpoint_is_unloaded() {
        // A link whose target isn't loaded draws nothing (both-endpoints guard).
        let (commits, [_s, f1, _t, _z]) = squash_link_fixture();
        let bogus = oid(200);
        let missing = build_graph(&commits, &[], &[], &[], None, None, &[(f1, bogus)]);
        assert!(
            !has_squash_grey(&missing),
            "no link is drawn when an endpoint isn't loaded"
        );
    }

    // ── merged_lane_oids: which commits the dim-merged setting greys (#108) ──

    /// Trunk A—B—C (A newest, C the loaded base whose parent Z is off-graph),
    /// with three branches off C:
    ///
    /// - F1—F2: merged into A via a merge commit (A's 2nd parent is F1);
    /// - S1: a squash-merged branch (its local ref survives but it is NOT an
    ///   ancestor of the trunk);
    /// - G1: an ordinary UNMERGED branch.
    ///
    /// Returns the commit list and the OIDs `[A, B, C, F1, F2, S1, G1, Z]`.
    fn merged_lane_fixture() -> (Vec<CommitInfo>, [Oid; 8]) {
        let (a, b, c, f1, f2, s1, g1, z) = (
            oid(1),
            oid(2),
            oid(3),
            oid(4),
            oid(5),
            oid(6),
            oid(7),
            oid(9),
        );
        let commits = vec![
            ci(a, vec![b, f1]), // trunk merge: first parent B, 2nd parent F1
            ci(b, vec![c]),
            ci(f1, vec![f2]),
            ci(f2, vec![c]),
            ci(s1, vec![c]),
            ci(g1, vec![c]),
            ci(c, vec![z]), // trunk base; Z is off-graph (not loaded)
        ];
        (commits, [a, b, c, f1, f2, s1, g1, z])
    }

    #[test]
    fn merged_lane_selects_everything_off_the_live_first_parent_chains() {
        let (commits, [a, b, c, f1, f2, s1, g1, _z]) = merged_lane_fixture();
        // Live lines: trunk A and the unmerged branch G. F (merged via merge
        // commit) and S (squash-merged) have no live ref — #111: the dim set is
        // the complement of the live first-parent chains, so their lanes dim
        // WITHOUT needing their tips to be known/classified.
        let merged = merged_lane_oids(&commits, &[a, g1]);

        assert!(merged.contains(&f1), "merge-commit branch tip dims");
        assert!(merged.contains(&f2), "merge-commit branch interior dims");
        assert!(merged.contains(&s1), "squash-merged branch commit dims");
        // Trunk first-parent commits never dim — even C, which the merged
        // branches also reach (it sits on the live chains).
        assert!(!merged.contains(&a), "trunk tip stays lit");
        assert!(!merged.contains(&b), "trunk interior stays lit");
        assert!(!merged.contains(&c), "shared fork-point commit stays lit");
        assert!(!merged.contains(&g1), "unmerged (live) branch stays lit");
    }

    #[test]
    fn merged_lane_dims_deleted_branch_side_history() {
        // #111 (the user's report): a merge-commit PR whose branch ref was
        // deleted on merge. No ref points at F1/F2, yet hide-merged removes
        // them (the first-parent walk needs no ref) — so dim must grey them.
        // This is exactly the fixture above with F's ref absent: same result
        // set, asserted separately so the no-ref case can't regress.
        let (commits, [a, _b, _c, f1, f2, _s1, g1, _z]) = merged_lane_fixture();
        let merged = merged_lane_oids(&commits, &[a, g1]);
        assert!(merged.contains(&f1) && merged.contains(&f2), "ref-less side lane dims");
    }

    #[test]
    fn merged_lane_spares_a_fast_forwarded_branch_on_the_trunk_line() {
        // A branch whose commits sit on the trunk's own first-parent chain (a
        // fast-forward merge left nothing exclusive) must NOT dim: hide-merged
        // would keep those commits, so the dim mirror keeps them too.
        let (a, b, c) = (oid(1), oid(2), oid(3));
        let commits = vec![ci(a, vec![b]), ci(b, vec![c]), ci(c, vec![])];
        // B's branch is "merged" (ff) — trunk tip A reaches it first-parent.
        let merged = merged_lane_oids(&commits, &[a]);
        assert!(merged.is_empty(), "no commit exclusive to a ff-merged branch");
    }

    #[test]
    fn merged_lane_is_empty_when_every_ref_is_live() {
        // All tips live → every loaded commit sits on some live first-parent
        // chain except merge side-parents… which here means F's lane dims only
        // if F isn't live. Make every branch live: nothing dims.
        let (commits, [a, _b, _c, f1, _f2, s1, g1, _z]) = merged_lane_fixture();
        assert!(merged_lane_oids(&commits, &[a, f1, s1, g1]).is_empty());
    }

    #[test]
    fn edge_and_cell_touch_merged_on_either_endpoint() {
        let merged: HashSet<Oid> = [oid(4)].into_iter().collect();
        // Either endpoint in the set makes the edge touch.
        assert!(edge_touches_merged(Some((oid(4), oid(3))), &merged));
        assert!(edge_touches_merged(Some((oid(1), oid(4))), &merged));
        assert!(!edge_touches_merged(Some((oid(1), oid(3))), &merged));
        assert!(!edge_touches_merged(None, &merged));
        // A cell touches when either of its two edges does.
        assert!(cell_touches_merged((None, Some((oid(4), oid(3)))), &merged));
        assert!(!cell_touches_merged((Some((oid(1), oid(2))), None), &merged));
    }
}
