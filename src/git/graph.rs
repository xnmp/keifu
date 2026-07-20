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
    trunk_tip: Option<Oid>,
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
    // Parallel to `lanes`: the child commit whose edge this lane's pipe carries,
    // i.e. the commit that placed (or continued) the lane toward `lanes[i]`. A
    // lane pipe's traced edge is `(lane_children[i], lanes[i])` — both endpoints
    // needed for the pair-identity trace. Kept in lock-step with every `lanes`
    // mutation below.
    let mut lane_children: Vec<Option<Oid>> = Vec::new();
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut max_lane: usize = 0;

    // Color management
    let mut color_assigner = ColorAssigner::new();
    // OID -> color index mapping
    let mut oid_color_index: HashMap<Oid, usize> = HashMap::new();
    // Lane -> color index mapping (keep colors during forks)
    let mut lane_color_index: HashMap<usize, usize> = HashMap::new();

    // Trunk pinning: when a trunk tip is known AND loaded, pre-seed lane 0 with
    // it so the trunk always occupies the leftmost lane — even when HEAD is a
    // different branch or the trunk tip isn't the newest commit. Newer commits
    // (processed first, revwalk is newest-first) then take lanes >= 1, because
    // the empty-lane search below skips the occupied lane 0. When the loop later
    // reaches the trunk tip, the "lane tracking this OID" scan finds it at 0.
    //
    // The main color is reserved on lane 0 up front (idempotent with the
    // per-commit assignment when the trunk row is processed) so any fork
    // connector emitted *before* the trunk row still reads in the main blue.
    let trunk_oid_in_graph: Option<Oid> = trunk_tip.filter(|oid| oid_to_row.contains_key(oid));
    if let Some(trunk_oid) = trunk_oid_in_graph {
        lanes.push(Some(trunk_oid));
        lane_children.push(None);
        color_assigner.assign_main_color(0);
        lane_color_index.insert(0, crate::graph::colors::MAIN_BRANCH_COLOR);
    }

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

        // Determine color index
        let commit_color_index = if commit_lane_opt.is_some() {
            // Continue existing branch. For the pre-seeded trunk lane this
            // returns the reserved main color (continue_lane special-cases the
            // main lane), so the trunk tip lands blue on lane 0.
            color_assigner.continue_lane(lane)
        } else if trunk_oid_in_graph == Some(commit.oid) {
            // Trunk tip reached on a fresh lane (not pre-tracked, e.g. nothing
            // newer descended from it): assign the reserved main color here.
            color_assigner.assign_main_color(lane)
        } else if nodes.is_empty() && trunk_oid_in_graph.is_none() {
            // No trunk pinning: original behavior — the first commit (HEAD's
            // line) is the main branch and reserves the main color.
            color_assigner.assign_main_color(lane)
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
                let empty = lanes.iter().position(|l| l.is_none());
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

    // Single O(N) pass builds both indices the walks need:
    //  - oid_row: oid -> row index (commit-carrying rows only).
    //  - lane_child: (first_parent_oid, lane) -> child oid. Keys the UP-walk's
    //    "child that continues this lane" lookup so each hop is O(1) instead of
    //    scanning every node — turning the UP walk from O(N × lineage) into
    //    O(lineage). First-parent inheritance keeps the lane number, so
    //    (first_parent, lane) uniquely identifies the branch-line continuation.
    let mut oid_row: HashMap<Oid, usize> = HashMap::with_capacity(layout.nodes.len());
    let mut lane_child: HashMap<(Oid, usize), Oid> = HashMap::new();
    for (i, n) in layout.nodes.iter().enumerate() {
        let Some(c) = n.commit.as_ref() else { continue };
        oid_row.insert(c.oid, i);
        if let Some(fp) = c.parent_oids.first() {
            // First writer wins: the topmost (newest) child on the lane is the
            // real continuation, matching the previous forward `find_map` scan.
            lane_child.entry((*fp, n.lane)).or_insert(c.oid);
        }
    }

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

    // UP: follow the child that continues this lane (O(1) per hop via lane_child).
    let mut cur = sel;
    loop {
        let Some(&cur_row) = oid_row.get(&cur) else {
            break;
        };
        let cur_lane = layout.nodes[cur_row].lane;
        match lane_child.get(&(cur, cur_lane)) {
            Some(&ch) if set.insert(ch) => cur = ch,
            _ => break,
        }
    }

    set
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
        let layout = build_graph(&commits, &[], &[], &[], None, None, None);
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

    // ── trunk pinning (issue #1) ─────────────────────────────────────────

    fn toid(b: u8) -> Oid {
        Oid::from_bytes(&[b; 20]).unwrap()
    }

    /// A commit with a concrete oid and parent oids (bytes).
    fn tc(id: u8, parents: &[u8]) -> CommitInfo {
        CommitInfo {
            oid: toid(id),
            short_id: format!("{id:07}"),
            author_name: "a".into(),
            author_email: "a@b".into(),
            timestamp: chrono::Local::now(),
            message: format!("c{id}"),
            full_message: format!("c{id}"),
            parent_oids: parents.iter().map(|p| toid(*p)).collect(),
        }
    }

    fn tb(name: &str, tip: u8, head: bool) -> BranchInfo {
        BranchInfo {
            name: name.into(),
            tip_oid: toid(tip),
            is_head: head,
            is_remote: false,
            upstream: None,
            ahead: 0,
            behind: 0,
        }
    }

    /// The lane + color of the row carrying `oid`.
    fn row_lane_color(layout: &GraphLayout, oid: Oid) -> (usize, usize) {
        let n = layout
            .nodes
            .iter()
            .find(|n| n.commit.as_ref().is_some_and(|c| c.oid == oid))
            .expect("commit present");
        (n.lane, n.color_index)
    }

    #[test]
    fn trunk_is_pinned_to_lane_zero_when_not_newest() {
        // A feature branch (tip c4) has commits NEWER than the trunk tip (c2):
        //   c4 -> c3 -> c2(trunk/main) -> c1
        // Revwalk is newest-first, so c4/c3 are processed before the trunk. With
        // trunk pinning, the trunk tip must still land on lane 0 in the main
        // color, and the newer feature commits take a lane > 0.
        let commits = vec![tc(4, &[3]), tc(3, &[2]), tc(2, &[1]), tc(1, &[])];
        let branches = vec![tb("main", 2, false), tb("feature", 4, true)];
        let layout = build_graph(&commits, &branches, &[], &[], None, None, Some(toid(2)));

        let (trunk_lane, trunk_color) = row_lane_color(&layout, toid(2));
        assert_eq!(trunk_lane, 0, "trunk pinned to leftmost lane");
        assert_eq!(
            trunk_color,
            crate::graph::colors::MAIN_BRANCH_COLOR,
            "trunk carries the reserved main color"
        );
        // The newer feature tip is NOT on lane 0.
        let (feat_lane, _) = row_lane_color(&layout, toid(4));
        assert_ne!(feat_lane, 0, "newer feature commit does not steal lane 0");
    }

    #[test]
    fn trunk_pinning_is_noop_when_trunk_is_newest() {
        // Linear history with trunk (main, c3) as the newest commit — the same
        // situation as no pinning. Pinning must produce the identical layout
        // (lane 0 + main color on the tip) as passing trunk_tip = None.
        let commits = vec![tc(3, &[2]), tc(2, &[1]), tc(1, &[])];
        let branches = vec![tb("main", 3, true)];
        let pinned = build_graph(&commits, &branches, &[], &[], None, None, Some(toid(3)));
        let unpinned = build_graph(&commits, &branches, &[], &[], None, None, None);

        for oid in [toid(3), toid(2), toid(1)] {
            assert_eq!(
                row_lane_color(&pinned, oid),
                row_lane_color(&unpinned, oid),
                "pinned trunk==newest matches unpinned layout for {oid}"
            );
        }
        assert_eq!(row_lane_color(&pinned, toid(3)).0, 0);
        assert_eq!(
            row_lane_color(&pinned, toid(3)).1,
            crate::graph::colors::MAIN_BRANCH_COLOR
        );
    }

    #[test]
    fn trunk_beyond_loaded_window_is_not_pinned() {
        // trunk_tip references a commit not in `commits` (beyond the load limit):
        // pinning is skipped and the layout matches the unpinned one.
        let commits = vec![tc(3, &[2]), tc(2, &[1])];
        let branches = vec![tb("feature", 3, true)];
        let pinned = build_graph(&commits, &branches, &[], &[], None, None, Some(toid(99)));
        let unpinned = build_graph(&commits, &branches, &[], &[], None, None, None);
        assert_eq!(
            row_lane_color(&pinned, toid(3)),
            row_lane_color(&unpinned, toid(3)),
            "unloaded trunk tip leaves layout unchanged"
        );
    }

    // ── lineage_oids performance (issue #6) ──────────────────────────────

    /// Build an N-node single-lane chain layout: node i is on lane 0, its first
    /// parent is node i+1 (older). Worst case for the UP-walk, which must climb
    /// the whole chain from the bottom.
    fn linear_chain_layout(n: usize) -> GraphLayout {
        let oid_of = |i: usize| {
            let mut bytes = [0u8; 20];
            bytes[0] = (i & 0xff) as u8;
            bytes[1] = ((i >> 8) & 0xff) as u8;
            bytes[2] = ((i >> 16) & 0xff) as u8;
            Oid::from_bytes(&bytes).unwrap()
        };
        let nodes = (0..n)
            .map(|i| {
                let mut c = commit_with_parents(0);
                c.oid = oid_of(i);
                if i + 1 < n {
                    c.parent_oids = vec![oid_of(i + 1)];
                }
                GraphNode {
                    commit: Some(c),
                    lane: 0,
                    color_index: 0,
                    branch_names: Vec::new(),
                    tag_names: Vec::new(),
                    is_head: false,
                    is_uncommitted: false,
                    is_stash: false,
                    stash_label: None,
                    uncommitted_count: None,
                    cells: Vec::new(),
                    cell_oids: Vec::new(),
                }
            })
            .collect();
        GraphLayout { nodes, max_lane: 0 }
    }

    #[test]
    fn lineage_oids_covers_full_single_lane_chain() {
        // Correctness at scale: tracing the newest commit (row 0) of a 2000-node
        // single-lane chain must include every commit (down-walk over the whole
        // first-parent chain), and tracing the oldest (last row) must climb the
        // whole chain via the UP-walk. Both exercise the O(N) index built once.
        let layout = linear_chain_layout(2000);
        assert_eq!(lineage_oids(&layout, 0).len(), 2000, "down-walk covers all");
        assert_eq!(lineage_oids(&layout, 1999).len(), 2000, "up-walk covers all");
    }

    #[test]
    #[ignore = "timing benchmark; run with --ignored"]
    fn lineage_oids_scales_linearly() {
        // The UP-walk previously scanned every node per hop (O(N^2)); tracing the
        // bottom of a large chain was the keystroke lag. With the (first_parent,
        // lane) index it is O(N). This isn't a hard threshold assert (machine-
        // dependent) — run with `--ignored --nocapture` to see the timing.
        use std::time::Instant;
        for n in [1000usize, 2000, 4000, 8000] {
            let layout = linear_chain_layout(n);
            let start = Instant::now();
            // Trace from the oldest commit: worst case for the UP-walk.
            let set = lineage_oids(&layout, n - 1);
            let elapsed = start.elapsed();
            assert_eq!(set.len(), n);
            println!("lineage_oids n={n}: {elapsed:?}");
        }
    }
}
