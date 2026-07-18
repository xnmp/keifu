//! Commit graph construction

use std::collections::{HashMap, HashSet};

use git2::Oid;

use super::{BranchInfo, CommitInfo};
use crate::graph::colors::{ColorAssigner, UNCOMMITTED_COLOR_INDEX};

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
    /// tracing. `.0` is the primary edge's target OID (the lane/curve/commit it
    /// draws); `.1` is a secondary OID for `HorizontalPipe` cells (the vertical
    /// lane crossed underneath the horizontal stroke). Either being in the
    /// selected commit's lineage marks the cell as traced. `None` = no lineage
    /// (e.g. the grey uncommitted connector).
    pub cell_oids: Vec<CellOids>,
}

/// Per-cell commit-edge identity: `(primary, secondary)` target OIDs. See
/// [`GraphNode::cell_oids`].
pub type CellOids = (Option<Oid>, Option<Oid>);

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

/// Build a graph from commit list
/// uncommitted_count: None if no uncommitted changes, Some(count) if there
/// are uncommitted changes.  The inner Option is None when the exact file
/// count is unavailable (e.g. collapsed untracked directories).
/// head_commit_oid: The OID of the commit that HEAD points to (for uncommitted changes)
pub fn build_graph(
    commits: &[CommitInfo],
    branches: &[BranchInfo],
    tags: &[super::repository::TagInfo],
    stashes: &[super::repository::StashInfo],
    uncommitted_count: Option<Option<usize>>,
    head_commit_oid: Option<Oid>,
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
    let mut head_oid: Option<Oid> = None;
    for branch in branches {
        oid_to_branches
            .entry(branch.tip_oid)
            .or_default()
            .push(branch.name.clone());
        if branch.is_head {
            head_oid = Some(branch.tip_oid);
        }
    }

    // OID -> row index mapping
    let oid_to_row: HashMap<Oid, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.oid, i))
        .collect();

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
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut max_lane: usize = 0;

    // Color management
    let mut color_assigner = ColorAssigner::new();
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
            // Find an empty lane or create one
            let empty = lanes.iter().position(|l| l.is_none());
            if let Some(l) = empty {
                l
            } else {
                lanes.push(None);
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
                    color_assigner.release_lane(l);
                    lane_color_index.remove(&l);
                }
            }
        }

        // Determine color index
        let commit_color_index = if commit_lane_opt.is_some() {
            // Continue existing branch
            color_assigner.continue_lane(lane)
        } else if nodes.is_empty() {
            // First commit (main branch) - reserve color so others cannot use it
            color_assigner.assign_main_color(lane)
        } else {
            // New branch start - assign a new color (exclude reserved)
            color_assigner.assign_color(lane)
        };
        oid_color_index.insert(commit.oid, commit_color_index);
        // Record lane color (to preserve colors during forks)
        lane_color_index.insert(lane, commit_color_index);

        // Clear this commit lane
        if lane < lanes.len() {
            lanes[lane] = None;
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
                    // Track the parent on this lane as well (same OID on multiple lanes)
                    lanes[lane] = Some(*parent_oid);
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
                // First parent uses the same lane - inherit color
                lanes[lane] = Some(*parent_oid);
                oid_color_index.insert(*parent_oid, commit_color_index);
                (lane, false, commit_color_index)
            } else {
                // Subsequent parents use new lanes - assign fork sibling colors
                let empty = lanes.iter().position(|l| l.is_none());
                let new_lane = if let Some(l) = empty {
                    l
                } else {
                    lanes.push(None);
                    lanes.len() - 1
                };
                lanes[new_lane] = Some(*parent_oid);
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
            &oid_color_index,
            &lane_color_index,
            max_lane,
        );

        // Get branch names
        let branch_names = oid_to_branches
            .get(&commit.oid)
            .cloned()
            .unwrap_or_default();

        let is_head = head_oid.map(|h| h == commit.oid).unwrap_or(false);

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
                    // Move the ending lane OID into the main lane
                    if let Some(oid) = lanes[ending_lane] {
                        if lanes.get(main_lane).map(|l| l.is_none()).unwrap_or(false) {
                            lanes[main_lane] = Some(oid);
                        }
                    }
                    lanes[ending_lane] = None;
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
                    // pipe keeps its OID so tracing still lights it up.
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

/// Build cells for one row - color index version
/// parent_lanes: (parent OID, lane, existing-tracked flag, color index, already-shown flag)
#[allow(clippy::too_many_arguments)] // cohesive lane/color/oid inputs; a struct adds indirection
fn build_row_cells_with_colors(
    commit_lane: usize,
    commit_color: usize,
    commit_oid: Oid,
    parent_lanes: &[(Oid, usize, bool, usize, bool)],
    active_lanes: &[Option<Oid>],
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
                    // This pipe carries the edge toward `oid`.
                    oids[cell_idx] = (Some(*oid), None);
                }
            }
        }
    }

    // Draw commit node
    let commit_cell_idx = commit_lane * 2;
    if commit_cell_idx < cells.len() {
        cells[commit_cell_idx] = CellType::Commit(commit_color);
        oids[commit_cell_idx] = (Some(commit_oid), None);
    }

    // Draw connections to parents
    for &(parent_oid, parent_lane, was_existing, parent_color, already_shown) in parent_lanes.iter()
    {
        if parent_lane == commit_lane {
            // Same lane - only a vertical line (drawn on next row)
            continue;
        }

        // Connection to a different lane
        if parent_lane > commit_lane {
            // Connection to a lane on the right
            // Horizontal line to the right from the commit position
            for col in (commit_lane * 2 + 1)..(parent_lane * 2) {
                if col < cells.len() {
                    let existing = cells[col];
                    if let CellType::Pipe(pl) = existing {
                        cells[col] = CellType::HorizontalPipe(parent_color, pl);
                        // Horizontal edge → parent; keep the crossed pipe's OID.
                        oids[col] = (Some(parent_oid), oids[col].0);
                    } else if existing == CellType::Empty {
                        cells[col] = CellType::Horizontal(parent_color);
                        oids[col] = (Some(parent_oid), None);
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
                oids[end_idx] = (Some(parent_oid), None);
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
                        oids[col] = (Some(parent_oid), oids[col].0);
                    } else if existing == CellType::Empty {
                        cells[col] = CellType::Horizontal(parent_color);
                        oids[col] = (Some(parent_oid), None);
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
                oids[start_idx] = (Some(parent_oid), None);
            }
        }
    }

    (cells, oids)
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
    oid_color_index: &HashMap<Oid, usize>,
    lane_color_index: &HashMap<usize, usize>,
    max_lane: usize,
) -> (Vec<CellType>, Vec<CellOids>) {
    let mut cells = vec![CellType::Empty; (max_lane + 1) * 2];
    // Every stroke on this connector belongs to the fork commit's lineage,
    // except the unrelated pass-through pipes, which carry their own OID.
    let mut oids: Vec<CellOids> = vec![(None, None); cells.len()];

    // Sorted list of merging lane numbers
    let mut merging_lane_nums: Vec<usize> = merging_lanes.iter().map(|(l, _)| *l).collect();
    merging_lane_nums.sort();

    // Draw a T junction on the main lane
    let main_cell_idx = main_lane * 2;
    if main_cell_idx < cells.len() {
        cells[main_cell_idx] = CellType::TeeRight(main_color);
        oids[main_cell_idx] = (Some(fork_oid), None);
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
                    oids[cell_idx] = (Some(*oid), None);
                }
            }
        }
    }

    // Rightmost merging lane
    let rightmost_lane = *merging_lane_nums.last().unwrap_or(&main_lane);

    // Draw connectors to merging lanes
    for &(merge_lane, merge_color) in merging_lanes {
        // Horizontal line from main lane to merging lane
        for col in (main_lane * 2 + 1)..(merge_lane * 2) {
            if col < cells.len() {
                let existing = cells[col];
                if let CellType::Pipe(pl) = existing {
                    cells[col] = CellType::HorizontalPipe(merge_color, pl);
                    // Fork stroke crossing an unrelated pipe: keep both OIDs.
                    oids[col] = (Some(fork_oid), oids[col].0);
                } else if matches!(existing, CellType::Empty | CellType::Horizontal(_)) {
                    cells[col] = CellType::Horizontal(merge_color);
                    oids[col] = (Some(fork_oid), None);
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
            oids[end_idx] = (Some(fork_oid), None);
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
    // as it's visible in the graph.
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

/// Whether a cell (by its `(primary, secondary)` edge OIDs) belongs to the
/// traced lineage. Either OID being in the lineage lights the cell — the
/// secondary covers a lineage pipe crossed underneath a `HorizontalPipe`.
pub fn cell_is_traced(oids: CellOids, lineage: &HashSet<Oid>) -> bool {
    oids.0.is_some_and(|o| lineage.contains(&o)) || oids.1.is_some_and(|o| lineage.contains(&o))
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
}
