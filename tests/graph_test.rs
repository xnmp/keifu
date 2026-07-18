//! Tests for the graph rendering algorithm

use chrono::Local;
use git2::Oid;
use keifu::git::{build_graph, graph::CellType, BranchInfo, CommitInfo, StashInfo, TagInfo};

fn make_oid(id: &str) -> Oid {
    // Convert id into a 40-char hex hash
    let hash = format!(
        "{:0>40x}",
        id.bytes()
            .fold(0u128, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u128))
    );
    Oid::from_str(&hash[..40]).unwrap()
}

fn make_commit(id: &str, parents: Vec<&str>) -> CommitInfo {
    CommitInfo {
        oid: make_oid(id),
        short_id: id.to_string(),
        author_name: "test".to_string(),
        author_email: "test@example.com".to_string(),
        timestamp: Local::now(),
        message: format!("Commit {}", id),
        full_message: format!("Commit {}", id),
        parent_oids: parents.into_iter().map(make_oid).collect(),
    }
}

fn make_branch(name: &str, tip: &str, is_head: bool) -> BranchInfo {
    BranchInfo {
        name: name.to_string(),
        tip_oid: make_oid(tip),
        is_head,
        is_remote: false,
        upstream: None,
        ahead: 0,
        behind: 0,
    }
}

fn render_cells(cells: &[CellType]) -> String {
    cells
        .iter()
        .map(|c| match c {
            CellType::Empty => ' ',
            CellType::Pipe(_) => '│',
            CellType::Commit(_) => '○',
            CellType::BranchRight(_) => '╭',
            CellType::BranchLeft(_) => '╮',
            CellType::MergeRight(_) => '╰',
            CellType::MergeLeft(_) => '╯',
            CellType::Horizontal(_) => '─',
            CellType::HorizontalPipe(_, _) => '┼',
            CellType::TeeRight(_) => '├',
            CellType::TeeLeft(_) => '┤',
            CellType::TeeUp(_) => '┴',
        })
        .collect()
}

fn get_short_id(node: &keifu::git::graph::GraphNode) -> String {
    node.commit
        .as_ref()
        .map(|c| c.short_id.clone())
        .unwrap_or_else(|| "(connector)".to_string())
}

#[test]
fn test_linear_history() {
    // C3 -> C2 -> C1
    let commits = vec![
        make_commit("c3", vec!["c2"]),
        make_commit("c2", vec!["c1"]),
        make_commit("c1", vec![]),
    ];
    let branches = vec![make_branch("main", "c3", true)];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("Linear history:");
    for node in &layout.nodes {
        println!("  {} -> {}", get_short_id(node), render_cells(&node.cells));
    }

    assert_eq!(layout.max_lane, 0);
    // All commits should be on lane 0
    for node in &layout.nodes {
        assert_eq!(node.lane, 0);
    }
}

#[test]
fn test_unborn_repo_shows_uncommitted_node() {
    let layout = build_graph(&[], &[], &[], &[], Some(Some(1)), None);

    assert_eq!(layout.max_lane, 0);
    assert_eq!(layout.nodes.len(), 1);
    assert!(layout.nodes[0].is_uncommitted);
    assert_eq!(layout.nodes[0].uncommitted_count, Some(1));
    assert_eq!(layout.nodes[0].lane, 0);
    assert!(matches!(
        layout.nodes[0].cells.first(),
        Some(CellType::Commit(_))
    ));
}

#[test]
fn test_simple_branch_merge() {
    // C4 (merge) -> C3, C2
    // C3 -> C1
    // C2 -> C1
    // C1 (root)
    let commits = vec![
        make_commit("c4", vec!["c3", "c2"]), // merge commit
        make_commit("c3", vec!["c1"]),       // main branch
        make_commit("c2", vec!["c1"]),       // feature branch
        make_commit("c1", vec![]),           // root
    ];
    let branches = vec![
        make_branch("main", "c4", true),
        make_branch("feature", "c2", false),
    ];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nSimple branch merge:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> {}",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    // Extract commit nodes only (exclude connector rows)
    let commit_nodes: Vec<_> = layout.nodes.iter().filter(|n| n.commit.is_some()).collect();

    // C4 should be in lane 0 with a branch to C2
    assert_eq!(commit_nodes[0].lane, 0); // C4
                                         // C3 should be in lane 0
    assert_eq!(commit_nodes[1].lane, 0); // C3
                                         // C2 should be in lane 1 (separate branch)
    assert_eq!(commit_nodes[2].lane, 1); // C2
                                         // C1 should be in lane 0
    assert_eq!(commit_nodes[3].lane, 0); // C1
}

#[test]
fn test_multiple_merges() {
    // C7 (merge) -> C6, C5
    // C6 -> C4
    // C5 -> C4
    // C4 (merge) -> C3, C2
    // C3 -> C1
    // C2 -> C1
    // C1 (root)
    let commits = vec![
        make_commit("c7", vec!["c6", "c5"]),
        make_commit("c6", vec!["c4"]),
        make_commit("c5", vec!["c4"]),
        make_commit("c4", vec!["c3", "c2"]),
        make_commit("c3", vec!["c1"]),
        make_commit("c2", vec!["c1"]),
        make_commit("c1", vec![]),
    ];
    let branches = vec![
        make_branch("main", "c7", true),
        make_branch("feature", "c5", false),
        make_branch("develop", "c2", false),
    ];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nMultiple merges:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> '{}'",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    // Actual topology (verified via render_cells):
    // c7 lane=0 -> '○─╮ '   (merge: c6 stays lane 0, c5 branches to lane 1)
    // c6 lane=0 -> '○ │ '   (main continues, c5's lane still active)
    // c5 lane=1 -> '│ ○ '
    // (connector) lane=0 -> '├─╯ '  (c4 is a fork point: c6 and c5 both point to it)
    // c4 lane=0 -> '○─╮ '   (merge: c3 stays lane 0, c2 branches to lane 1)
    // c3 lane=0 -> '○ │ '
    // c2 lane=1 -> '│ ○ '
    // (connector) lane=0 -> '├─╯ '  (c1 is a fork point: c3 and c2 both point to it)
    // c1 lane=0 -> '○   '   (root)
    let by_id = |id: &str| -> &keifu::git::graph::GraphNode {
        layout
            .nodes
            .iter()
            .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some(id))
            .unwrap_or_else(|| panic!("{id} not found"))
    };

    let c7 = by_id("c7");
    let c6 = by_id("c6");
    let c5 = by_id("c5");
    let c4 = by_id("c4");
    let c3 = by_id("c3");
    let c2 = by_id("c2");
    let c1 = by_id("c1");

    // c6 (first parent) stays on the merge's lane; c5 (second parent) branches off
    assert_eq!(c7.lane, 0);
    assert_eq!(c6.lane, 0);
    assert_eq!(c5.lane, 1);
    assert!(matches!(c7.cells[0], CellType::Commit(_)));
    assert!(
        matches!(c7.cells[2], CellType::BranchLeft(_)),
        "c7 should branch off to c5's lane: {:?}",
        c7.cells
    );

    // Same shape repeats one level down for c4/c3/c2
    assert_eq!(c4.lane, 0);
    assert_eq!(c3.lane, 0);
    assert_eq!(c2.lane, 1);
    assert!(matches!(c4.cells[0], CellType::Commit(_)));
    assert!(
        matches!(c4.cells[2], CellType::BranchLeft(_)),
        "c4 should branch off to c2's lane: {:?}",
        c4.cells
    );

    // c4 and c1 are fork points (2 children each), so each is preceded by a
    // connector row that merges lane 1 back into lane 0.
    let c4_idx = layout.nodes.iter().position(|n| std::ptr::eq(n, c4)).unwrap();
    let c1_idx = layout.nodes.iter().position(|n| std::ptr::eq(n, c1)).unwrap();
    assert!(c4_idx > 0, "expected a connector row before c4");
    assert!(c1_idx > 0, "expected a connector row before c1");
    let connector_before_c4 = &layout.nodes[c4_idx - 1];
    let connector_before_c1 = &layout.nodes[c1_idx - 1];
    assert!(connector_before_c4.commit.is_none());
    assert!(connector_before_c1.commit.is_none());
    assert!(matches!(connector_before_c4.cells[0], CellType::TeeRight(_)));
    assert!(matches!(connector_before_c4.cells[2], CellType::MergeLeft(_)));
    assert!(matches!(connector_before_c1.cells[0], CellType::TeeRight(_)));
    assert!(matches!(connector_before_c1.cells[2], CellType::MergeLeft(_)));

    // c1 is the root: no parents, no outgoing connectors
    assert_eq!(c1.lane, 0);
    assert!(matches!(c1.cells[0], CellType::Commit(_)));
    assert!(c1.cells[1..].iter().all(|c| *c == CellType::Empty));

    assert_eq!(layout.max_lane, 1);
}

#[test]
fn test_cell_structure() {
    // Inspect the cell structure of a simple merge in detail
    let commits = vec![
        make_commit("m1", vec!["a1", "b1"]), // merge
        make_commit("a1", vec!["r1"]),       // main
        make_commit("b1", vec!["r1"]),       // branch
        make_commit("r1", vec![]),           // root
    ];
    let branches = vec![make_branch("main", "m1", true)];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nCell structure analysis:");
    for node in &layout.nodes {
        println!("  {} cells: {:?}", get_short_id(node), node.cells);
    }

    // Check the cell structure for m1
    let m1_cells = &layout.nodes[0].cells;
    println!("  m1 rendered: '{}'", render_cells(m1_cells));

    // m1 is a commit on lane 0 with a branch line to lane 1
    // CellType stores color indices, so only validate the cell type
    assert!(
        matches!(m1_cells.first(), Some(CellType::Commit(_))),
        "m1 cell[0] should be Commit, got {:?}",
        m1_cells.first()
    );
}

#[test]
fn test_octopus_merge() {
    // Octopus merge (3+ parents)
    // M -> A, B, C
    // A -> R
    // B -> R
    // C -> R
    // R (root)
    let commits = vec![
        make_commit("M", vec!["A", "B", "C"]),
        make_commit("A", vec!["R"]),
        make_commit("B", vec!["R"]),
        make_commit("C", vec!["R"]),
        make_commit("R", vec![]),
    ];
    let branches = vec![
        make_branch("main", "M", true),
        make_branch("branch-b", "B", false),
        make_branch("branch-c", "C", false),
    ];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nOctopus merge:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> '{}'",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    // Verified topology:
    // M lane=0 -> '○─╮─╮ '  (fans out: A stays lane 0, B branches to lane 1, C to lane 2)
    // A lane=0 -> '○ │ │ '
    // B lane=1 -> '│ ○ │ '
    // C lane=2 -> '│ │ ○ '
    // (connector) lane=0 -> '├─┴─╯ '  (R is a fork point: A, B, C all point to it)
    // R lane=0 -> '○     '

    let m = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("M"))
        .expect("M not found");
    let a = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("A"))
        .expect("A not found");
    let b = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("B"))
        .expect("B not found");
    let c = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("C"))
        .expect("C not found");
    let r = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("R"))
        .expect("R not found");

    // The merge commit's three parents fan out to three distinct lanes
    assert_eq!(m.lane, 0);
    assert_eq!(a.lane, 0);
    assert_eq!(b.lane, 1);
    assert_eq!(c.lane, 2);
    assert_eq!(
        layout.max_lane, 2,
        "octopus merge with 3 parents should occupy 3 lanes (0,1,2)"
    );

    // M's row: commit on lane 0, then a branch-off connector to lane 1 (B) and lane 2 (C)
    assert!(matches!(m.cells[0], CellType::Commit(_)));
    assert!(
        matches!(m.cells[2], CellType::BranchLeft(_)),
        "M should branch to B's lane: {:?}",
        m.cells
    );
    assert!(
        matches!(m.cells[4], CellType::BranchLeft(_)),
        "M should branch to C's lane: {:?}",
        m.cells
    );

    // R is a fork point (A, B, C all point to it) so a connector row precedes it,
    // fanning all three lanes back into lane 0.
    let r_idx = layout.nodes.iter().position(|n| std::ptr::eq(n, r)).unwrap();
    assert!(r_idx > 0, "expected a connector row before R");
    let connector = &layout.nodes[r_idx - 1];
    assert!(connector.commit.is_none());
    assert!(matches!(connector.cells[0], CellType::TeeRight(_)));
    assert!(
        matches!(connector.cells[2], CellType::TeeUp(_)),
        "middle merging lane (B) should be a T-up junction: {:?}",
        connector.cells
    );
    assert!(
        matches!(connector.cells[4], CellType::MergeLeft(_)),
        "rightmost merging lane (C) should close with MergeLeft: {:?}",
        connector.cells
    );

    // R terminates the graph: no parents, no outgoing connectors
    assert!(matches!(r.cells[0], CellType::Commit(_)));
    assert!(r.cells[1..].iter().all(|cell| *cell == CellType::Empty));
}

#[test]
fn test_parallel_branches() {
    // Parallel branches
    // M2 (merge) -> A2, B2
    // A2 -> A1
    // B2 -> B1
    // A1 -> M1
    // B1 -> M1
    // M1 (merge) -> R, X
    // X -> R
    // R (root)
    let commits = vec![
        make_commit("M2", vec!["A2", "B2"]),
        make_commit("A2", vec!["A1"]),
        make_commit("B2", vec!["B1"]),
        make_commit("A1", vec!["M1"]),
        make_commit("B1", vec!["M1"]),
        make_commit("M1", vec!["R", "X"]),
        make_commit("X", vec!["R"]),
        make_commit("R", vec![]),
    ];
    let branches = vec![make_branch("main", "M2", true)];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nParallel branches:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> '{}'",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    // Verified topology:
    // M2 lane=0 -> '○─╮ '
    // A2 lane=0 -> '○ │ '
    // B2 lane=1 -> '│ ○ '
    // A1 lane=0 -> '○ │ '
    // B1 lane=1 -> '│ ○ '
    // (connector) lane=0 -> '├─╯ '  (M1 is a fork point: A1, B1 both point to it)
    // M1 lane=0 -> '○─╮ '
    // X  lane=1 -> '│ ○ '
    // (connector) lane=0 -> '├─╯ '  (R is a fork point: M1, X both point to it)
    // R  lane=0 -> '○   '

    let by_id = |id: &str| -> &keifu::git::graph::GraphNode {
        layout
            .nodes
            .iter()
            .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some(id))
            .unwrap_or_else(|| panic!("{id} not found"))
    };

    let m2 = by_id("M2");
    let a2 = by_id("A2");
    let b2 = by_id("B2");
    let a1 = by_id("A1");
    let b1 = by_id("B1");
    let m1 = by_id("M1");
    let x = by_id("X");
    let r = by_id("R");

    // The A-chain (main line: M2, A2, A1) stays on lane 0 throughout
    assert_eq!(m2.lane, 0);
    assert_eq!(a2.lane, 0);
    assert_eq!(a1.lane, 0);

    // The B-chain (parallel branch: B2, B1) occupies a distinct lane (1),
    // separate from the A-chain, for its entire length
    assert_eq!(b2.lane, 1);
    assert_eq!(b1.lane, 1);
    assert_ne!(
        a2.lane, b2.lane,
        "the two parallel branches must occupy distinct lanes"
    );

    // Both chains are continuous: the lane holding the "other" branch shows
    // an unbroken line (Pipe, or the branch's own Commit row) between the two
    // visible commits on the A-chain - no gaps (Empty).
    let a2_idx = layout.nodes.iter().position(|n| std::ptr::eq(n, a2)).unwrap();
    let a1_idx = layout.nodes.iter().position(|n| std::ptr::eq(n, a1)).unwrap();
    let b_cell_idx = b2.lane * 2;
    for node in &layout.nodes[(a2_idx + 1)..a1_idx] {
        assert!(
            matches!(
                node.cells.get(b_cell_idx),
                Some(CellType::Pipe(_)) | Some(CellType::Commit(_))
            ),
            "expected continuous line on B's lane between A2 and A1, got {:?}",
            node.cells
        );
    }

    // M1 is a fork point (A1 and B1 both point to it), so a connector row
    // merges B1's lane back into the main lane right before M1.
    let m1_idx = layout.nodes.iter().position(|n| std::ptr::eq(n, m1)).unwrap();
    assert!(m1_idx > 0);
    let connector1 = &layout.nodes[m1_idx - 1];
    assert!(connector1.commit.is_none());
    assert!(matches!(connector1.cells[0], CellType::TeeRight(_)));
    assert!(matches!(connector1.cells[2], CellType::MergeLeft(_)));

    // M1 is itself a merge (parents R, X): fans back out to a second lane
    assert_eq!(m1.lane, 0);
    assert_eq!(x.lane, 1);
    assert!(matches!(m1.cells[0], CellType::Commit(_)));
    assert!(
        matches!(m1.cells[2], CellType::BranchLeft(_)),
        "M1 should branch out to X's lane: {:?}",
        m1.cells
    );

    // R is a fork point (M1, X both point to it): another connector row merges
    // X's lane back into the main lane before R, which terminates the graph.
    let r_idx = layout.nodes.iter().position(|n| std::ptr::eq(n, r)).unwrap();
    assert!(r_idx > 0);
    let connector2 = &layout.nodes[r_idx - 1];
    assert!(connector2.commit.is_none());
    assert!(matches!(connector2.cells[0], CellType::TeeRight(_)));
    assert!(matches!(connector2.cells[2], CellType::MergeLeft(_)));
    assert_eq!(r.lane, 0);
    assert!(matches!(r.cells[0], CellType::Commit(_)));
    assert!(r.cells[1..].iter().all(|cell| *cell == CellType::Empty));

    assert_eq!(layout.max_lane, 1);
}

#[test]
fn test_many_active_lanes() {
    // Multiple lanes active at once
    // HEAD -> M
    // M (merge) -> A, B, C, D
    // A -> R
    // B -> R
    // C -> R
    // D -> R
    // R (root)
    let commits = vec![
        make_commit("HEAD", vec!["M"]),
        make_commit("M", vec!["A", "B", "C", "D"]),
        make_commit("A", vec!["R"]),
        make_commit("B", vec!["R"]),
        make_commit("C", vec!["R"]),
        make_commit("D", vec!["R"]),
        make_commit("R", vec![]),
    ];
    let branches = vec![
        make_branch("main", "HEAD", true),
        make_branch("b", "B", false),
        make_branch("c", "C", false),
        make_branch("d", "D", false),
    ];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nMany active lanes:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> '{}'",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    // max_lane should be at least 3 (4 branches merge)
    assert!(
        layout.max_lane >= 3,
        "Expected max_lane >= 3, got {}",
        layout.max_lane
    );
}

#[test]
fn test_chained_merges_different_branches() {
    // Simulates the keifu-demo structure where:
    // - cdd4866 (main) merges 0c8f4c0 and 41654ad
    // - 0c8f4c0 merges 0e9a974 and 713c464
    // - 334c592 (develop) merges 7e6637e and 41654ad
    //
    // The issue was that the line from cdd4866 to 0c8f4c0 was not drawn
    // because the lane was incorrectly released when processing cdd4866.
    //
    // Structure (topological order):
    // cdd4866 -> 0c8f4c0, 41654ad
    // 334c592 -> 7e6637e, 41654ad
    // 41654ad -> root
    // 7e6637e -> root
    // 0c8f4c0 -> root, 713c464
    // 713c464 -> root
    // root
    let commits = vec![
        make_commit("main-merge", vec!["feature-merge", "release"]), // cdd4866
        make_commit("develop-merge", vec!["develop", "release"]),    // 334c592
        make_commit("release", vec!["root"]),                        // 41654ad
        make_commit("develop", vec!["root"]),                        // 7e6637e
        make_commit("feature-merge", vec!["root", "hotfix"]),        // 0c8f4c0
        make_commit("hotfix", vec!["root"]),                         // 713c464
        make_commit("root", vec![]),                                 // 0e9a974
    ];
    let branches = vec![
        make_branch("main", "main-merge", false),
        make_branch("develop", "develop-merge", true),
    ];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nChained merges (keifu-demo structure):");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> '{}'",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    // Find the main-merge and feature-merge nodes
    let main_merge_idx = layout
        .nodes
        .iter()
        .position(|n| {
            n.commit
                .as_ref()
                .map(|c| c.short_id == "main-merge")
                .unwrap_or(false)
        })
        .expect("main-merge not found");
    let feature_merge_idx = layout
        .nodes
        .iter()
        .position(|n| {
            n.commit
                .as_ref()
                .map(|c| c.short_id == "feature-merge")
                .unwrap_or(false)
        })
        .expect("feature-merge not found");

    // Count the number of Pipe cells on the lane of main-merge between the two commits
    let main_merge_lane = layout.nodes[main_merge_idx].lane;
    let mut pipe_count = 0;
    for idx in (main_merge_idx + 1)..feature_merge_idx {
        let cell_idx = main_merge_lane * 2;
        if let Some(cell) = layout.nodes[idx].cells.get(cell_idx) {
            if matches!(cell, CellType::Pipe(_)) {
                pipe_count += 1;
            }
        }
    }

    // There should be at least one Pipe connecting main-merge to feature-merge
    // (This was the bug: the lane was released and no Pipe was drawn)
    assert!(
        pipe_count > 0 || main_merge_idx + 1 == feature_merge_idx,
        "Expected Pipe cells connecting main-merge to feature-merge, got {} pipes between {} nodes",
        pipe_count,
        feature_merge_idx - main_merge_idx - 1
    );
}

#[test]
fn test_hotfix_merged_into_multiple_branches() {
    // Simulates 713c464 scenario where a hotfix is merged into multiple branches:
    // - ad98589 (release merge) merges a4b5efb and 713c464
    // - 0c8f4c0 (main merge) merges 0e9a974 and 713c464
    // 713c464 is a fork point (has 2 children) via second parent relationship
    //
    // Structure:
    // release-merge -> version-bump, hotfix
    // main-merge -> base, hotfix
    // version-bump -> base
    // hotfix -> base
    // base (root)
    let commits = vec![
        make_commit("release-merge", vec!["version-bump", "hotfix"]), // ad98589
        make_commit("main-merge", vec!["base", "hotfix"]),            // 0c8f4c0
        make_commit("version-bump", vec!["base"]),                    // a4b5efb
        make_commit("hotfix", vec!["base"]),                          // 713c464
        make_commit("base", vec![]),                                  // root
    ];
    let branches = vec![
        make_branch("release", "release-merge", false),
        make_branch("main", "main-merge", true),
        make_branch("hotfix", "hotfix", false),
    ];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nHotfix merged into multiple branches:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> '{}'",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    // Find the hotfix node and main-merge node
    let hotfix_idx = layout
        .nodes
        .iter()
        .position(|n| {
            n.commit
                .as_ref()
                .map(|c| c.short_id == "hotfix")
                .unwrap_or(false)
        })
        .expect("hotfix not found");
    let main_merge_idx = layout
        .nodes
        .iter()
        .position(|n| {
            n.commit
                .as_ref()
                .map(|c| c.short_id == "main-merge")
                .unwrap_or(false)
        })
        .expect("main-merge not found");

    // Check that main-merge row has a direct connection to hotfix
    // The connection should be drawn directly on the commit row (TeeRight at the hotfix lane)
    let main_merge_cells = &layout.nodes[main_merge_idx].cells;
    let has_direct_connection = main_merge_cells
        .iter()
        .any(|c| matches!(c, CellType::TeeRight(_)));

    assert!(
        has_direct_connection,
        "Expected direct connection (TeeRight) in main-merge row to hotfix lane. Cells: {:?}",
        main_merge_cells
    );

    // Verify the line continues from main-merge to hotfix by checking for Pipe cells
    let hotfix_lane = layout.nodes[hotfix_idx].lane;
    let mut has_continuous_line = true;
    for idx in (main_merge_idx + 1)..hotfix_idx {
        let cell_idx = hotfix_lane * 2;
        if let Some(cell) = layout.nodes[idx].cells.get(cell_idx) {
            if !matches!(cell, CellType::Pipe(_) | CellType::Commit(_)) {
                has_continuous_line = false;
                break;
            }
        }
    }

    assert!(
        has_continuous_line,
        "Expected continuous Pipe line from main-merge to hotfix"
    );
}

#[test]
fn test_stash_node_renders_with_base_connection() {
    // A stash is a commit-like node whose base commit is also in the graph.
    // Real stash commits have 2-3 parents (base + index tree + untracked
    // tree) but `GitRepository::get_commits` truncates `parent_oids` to just
    // the base before this ever reaches `build_graph` - so from build_graph's
    // point of view a stash always has a single parent.
    //
    // main2 -> base   (main branch continues)
    // stash1 -> base  (stash, single-parent after truncation)
    // base -> root
    // root (root)
    //
    // `base` has two children (main2, stash1) so it's a fork point: the
    // stash gets its own lane, and a connector row merges it back into
    // main's lane right before `base` is rendered.
    let commits = vec![
        make_commit("main2", vec!["base"]),
        make_commit("stash1", vec!["base"]),
        make_commit("base", vec!["root"]),
        make_commit("root", vec![]),
    ];
    let branches = vec![make_branch("main", "main2", true)];
    let stashes = vec![StashInfo {
        index: 0,
        message: "WIP on main: test stash".to_string(),
        oid: make_oid("stash1"),
        base_oid: make_oid("base"),
    }];

    let layout = build_graph(&commits, &branches, &[], &stashes, None, None);

    println!("\nStash node:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} is_stash={} label={:?} -> '{}'",
            get_short_id(node),
            node.lane,
            node.is_stash,
            node.stash_label,
            render_cells(&node.cells)
        );
    }

    let stash_node = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("stash1"))
        .expect("stash1 not found");

    // is_stash and stash_label are set correctly
    assert!(stash_node.is_stash);
    assert_eq!(stash_node.stash_label, Some("stash@{0}".to_string()));

    // Single-parent rendering: only one parent, so the stash's row itself
    // doesn't draw any branch/merge glyph (the parent connection is drawn on
    // the *base's* row via a connector, not on the stash's own row).
    assert_eq!(
        stash_node
            .commit
            .as_ref()
            .map(|c| c.parent_oids.len())
            .unwrap(),
        1
    );
    assert!(
        !stash_node
            .cells
            .iter()
            .any(|c| matches!(
                c,
                CellType::BranchLeft(_)
                    | CellType::BranchRight(_)
                    | CellType::MergeLeft(_)
                    | CellType::MergeRight(_)
            )),
        "stash's own row should have no branch/merge glyphs: {:?}",
        stash_node.cells
    );

    // The non-stash commit nodes should never be marked as stash
    let main2 = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("main2"))
        .expect("main2 not found");
    assert!(!main2.is_stash);
    assert_eq!(main2.stash_label, None);

    // Connection to the base commit: `base` is a fork point (main2 and
    // stash1 both point to it), so a connector row precedes it that merges
    // the stash's lane back into main's lane.
    let base_node = layout
        .nodes
        .iter()
        .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some("base"))
        .expect("base not found");
    assert_ne!(
        stash_node.lane, base_node.lane,
        "stash should get its own lane distinct from main"
    );

    let base_idx = layout
        .nodes
        .iter()
        .position(|n| std::ptr::eq(n, base_node))
        .unwrap();
    assert!(base_idx > 0, "expected a connector row before base");
    let connector = &layout.nodes[base_idx - 1];
    assert!(
        connector.commit.is_none(),
        "expected a connector row merging the stash lane into base's lane"
    );
    assert!(matches!(connector.cells[base_node.lane * 2], CellType::TeeRight(_)));
    assert!(matches!(
        connector.cells[stash_node.lane * 2],
        CellType::MergeLeft(_)
    ));
}

#[test]
fn test_uncommitted_node_connects_to_head_on_lane_zero() {
    // HEAD is on the only branch (lane 0). The uncommitted node should be
    // inserted at the top, on the same lane as HEAD, with no horizontal
    // connector needed since the lanes already match.
    let commits = vec![make_commit("c2", vec!["c1"]), make_commit("c1", vec![])];
    let branches = vec![make_branch("main", "c2", true)];

    let layout = build_graph(&commits, &branches, &[], &[], Some(Some(3)), Some(make_oid("c2")));

    println!("\nUncommitted, HEAD on lane 0:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} is_uncommitted={} -> '{}'",
            get_short_id(node),
            node.lane,
            node.is_uncommitted,
            render_cells(&node.cells)
        );
    }

    // Uncommitted node is inserted at the top
    assert!(layout.nodes[0].is_uncommitted);
    assert_eq!(layout.nodes[0].uncommitted_count, Some(3));
    assert_eq!(layout.nodes[0].lane, 0);

    // HEAD commit (c2) is on lane 0, matching the uncommitted node's lane
    let head_node = &layout.nodes[1];
    assert_eq!(head_node.commit.as_ref().unwrap().short_id, "c2");
    assert!(head_node.is_head);
    assert_eq!(head_node.lane, 0);
    assert_eq!(head_node.lane, layout.nodes[0].lane);

    // Same lane => no horizontal connector glyphs on HEAD's row
    assert!(
        !head_node
            .cells
            .iter()
            .any(|c| matches!(c, CellType::MergeLeft(_) | CellType::MergeRight(_))),
        "expected no horizontal connector when uncommitted shares HEAD's lane: {:?}",
        head_node.cells
    );
}

#[test]
fn test_uncommitted_node_connects_to_head_on_nonzero_lane() {
    // HEAD (f1) is the second parent of a fork commit `m`, so it starts life
    // on lane 1. `m`'s own row already occupies lane 1's column (with a
    // BranchLeft glyph), so by the time the uncommitted-changes placement
    // logic runs, lane 1 is *not* available for every row above HEAD - it
    // must pick a fresh lane (2) and draw a horizontal connector back to
    // HEAD's lane.
    //
    // m -> main_child, f1   (fork: main_child lane 0, f1 lane 1)
    // main_child (root)
    // f1 (root, HEAD)
    let commits = vec![
        make_commit("m", vec!["main_child", "f1"]),
        make_commit("main_child", vec![]),
        make_commit("f1", vec![]),
    ];
    let branches = vec![make_branch("feature", "f1", true)];

    let layout = build_graph(
        &commits,
        &branches,
        &[],
        &[],
        Some(Some(2)),
        Some(make_oid("f1")),
    );

    println!("\nUncommitted, HEAD on non-zero lane:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} is_uncommitted={} is_head={} -> '{}'",
            get_short_id(node),
            node.lane,
            node.is_uncommitted,
            node.is_head,
            render_cells(&node.cells)
        );
    }

    // Uncommitted node inserted at the top
    assert!(layout.nodes[0].is_uncommitted);
    assert_eq!(layout.nodes[0].uncommitted_count, Some(2));

    let head_node = layout
        .nodes
        .iter()
        .find(|n| n.is_head)
        .expect("HEAD node not found");
    assert_eq!(head_node.commit.as_ref().unwrap().short_id, "f1");
    assert_eq!(head_node.lane, 1, "HEAD (f1) starts on lane 1 (fork sibling)");

    let uncommitted_lane = layout.nodes[0].lane;
    assert_ne!(
        uncommitted_lane, head_node.lane,
        "lane 1 should be blocked by m's own row, forcing a different lane"
    );

    // A horizontal connector (MergeLeft, since the uncommitted lane is to
    // the right of HEAD's lane) must appear on HEAD's row to visually join
    // the uncommitted lane back to HEAD's lane.
    assert!(
        head_node
            .cells
            .iter()
            .any(|c| matches!(c, CellType::MergeLeft(_) | CellType::MergeRight(_))),
        "expected a horizontal connector (MergeLeft/MergeRight) on HEAD's row: {:?}",
        head_node.cells
    );

    // Every row strictly above HEAD carries a Pipe in the uncommitted lane,
    // continuing the line down to HEAD's row.
    let head_idx = layout.nodes.iter().position(|n| n.is_head).unwrap();
    let uncommitted_cell_idx = uncommitted_lane * 2;
    for node in &layout.nodes[0..head_idx] {
        assert!(
            matches!(
                node.cells.get(uncommitted_cell_idx),
                Some(CellType::Pipe(_)) | Some(CellType::Commit(_))
            ),
            "expected a continuous line down to HEAD in the uncommitted lane: {:?}",
            node.cells
        );
    }
}

#[test]
fn test_uncommitted_node_dropped_when_head_oid_not_in_graph() {
    // documents current behavior: if head_commit_oid doesn't match any node
    // in the graph (e.g. stale/unknown HEAD), build_graph silently drops the
    // uncommitted-changes node entirely rather than falling back to a
    // default lane. A future fix might change this, but today it's silent.
    let commits = vec![make_commit("c2", vec!["c1"]), make_commit("c1", vec![])];
    let branches = vec![make_branch("main", "c2", true)];

    let layout = build_graph(
        &commits,
        &branches,
        &[],
        &[],
        Some(Some(1)),
        Some(make_oid("does-not-exist")),
    );

    assert!(
        layout.nodes.iter().all(|n| !n.is_uncommitted),
        "documents current behavior: an unmatched head_commit_oid causes the \
         uncommitted node to be silently dropped"
    );
    assert_eq!(layout.nodes.len(), commits.len());
}

#[test]
fn test_empty_graph_no_commits_no_uncommitted() {
    // No commits, no branches, no uncommitted changes => empty layout, no panic.
    let layout = build_graph(&[], &[], &[], &[], None, None);

    assert_eq!(layout.nodes.len(), 0);
    assert_eq!(layout.max_lane, 0);
}

#[test]
fn tags_attach_to_their_target_commit() {
    // c3 -> c2 -> c1; tags sit on c1 and c3, none on c2.
    let commits = vec![
        make_commit("c3", vec!["c2"]),
        make_commit("c2", vec!["c1"]),
        make_commit("c1", vec![]),
    ];
    let branches = vec![make_branch("main", "c3", true)];
    let tags = vec![
        TagInfo {
            name: "v1.0".to_string(),
            target_oid: make_oid("c1"),
        },
        TagInfo {
            name: "v3.0".to_string(),
            target_oid: make_oid("c3"),
        },
    ];

    let layout = build_graph(&commits, &branches, &tags, &[], None, None);

    let by_id = |id: &str| -> &keifu::git::graph::GraphNode {
        layout
            .nodes
            .iter()
            .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some(id))
            .unwrap_or_else(|| panic!("{id} not found"))
    };

    assert_eq!(by_id("c1").tag_names, vec!["v1.0".to_string()]);
    assert_eq!(by_id("c3").tag_names, vec!["v3.0".to_string()]);
    assert!(by_id("c2").tag_names.is_empty());
}

#[test]
fn multiple_tags_on_same_commit_all_appear() {
    let commits = vec![make_commit("c1", vec![])];
    let branches = vec![make_branch("main", "c1", true)];
    let tags = vec![
        TagInfo {
            name: "v1.0".to_string(),
            target_oid: make_oid("c1"),
        },
        TagInfo {
            name: "release".to_string(),
            target_oid: make_oid("c1"),
        },
    ];

    let layout = build_graph(&commits, &branches, &tags, &[], None, None);
    let node = layout.nodes.iter().find(|n| n.commit.is_some()).unwrap();

    assert_eq!(node.tag_names.len(), 2);
    assert!(node.tag_names.contains(&"v1.0".to_string()));
    assert!(node.tag_names.contains(&"release".to_string()));
}

#[test]
fn test_orphan_disconnected_roots() {
    // Two root commits with no common ancestor, each with its own branch.
    // They should render on separate lanes and each chain should terminate
    // cleanly (no dangling connectors) at its root.
    //
    // a2 -> a1 (root)   [branch-a]
    // b2 -> b1 (root)   [branch-b]
    let commits = vec![
        make_commit("a2", vec!["a1"]),
        make_commit("b2", vec!["b1"]),
        make_commit("a1", vec![]),
        make_commit("b1", vec![]),
    ];
    let branches = vec![
        make_branch("branch-a", "a2", true),
        make_branch("branch-b", "b2", false),
    ];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    println!("\nOrphan disconnected roots:");
    for node in &layout.nodes {
        println!(
            "  {} lane={} -> '{}'",
            get_short_id(node),
            node.lane,
            render_cells(&node.cells)
        );
    }

    let by_id = |id: &str| -> &keifu::git::graph::GraphNode {
        layout
            .nodes
            .iter()
            .find(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some(id))
            .unwrap_or_else(|| panic!("{id} not found"))
    };

    let a2 = by_id("a2");
    let a1 = by_id("a1");
    let b2 = by_id("b2");
    let b1 = by_id("b1");

    // Each chain stays internally on one lane
    assert_eq!(a2.lane, a1.lane);
    assert_eq!(b2.lane, b1.lane);
    // The two disconnected chains occupy distinct lanes
    assert_ne!(
        a2.lane, b2.lane,
        "disconnected root chains must render on separate lanes"
    );
    assert!(layout.max_lane >= 1);

    // Both roots terminate cleanly: their own commit cell is set, and they
    // draw no outgoing parent connectors (no parents to connect to).
    for root in [a1, b1] {
        let own_cell_idx = root.lane * 2;
        assert!(matches!(root.cells[own_cell_idx], CellType::Commit(_)));
        assert!(
            !root
                .cells
                .iter()
                .any(|c| matches!(
                    c,
                    CellType::BranchLeft(_)
                        | CellType::BranchRight(_)
                        | CellType::MergeLeft(_)
                        | CellType::MergeRight(_)
                        | CellType::TeeUp(_)
                )),
            "root commit should have no outgoing parent connectors: {:?}",
            root.cells
        );
    }
}

#[test]
fn test_single_root_commit() {
    // A single commit with no parents and no history beyond it.
    let commits = vec![make_commit("only", vec![])];
    let branches = vec![make_branch("main", "only", true)];

    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    assert_eq!(layout.nodes.len(), 1);
    assert_eq!(layout.max_lane, 0);

    let node = &layout.nodes[0];
    assert_eq!(node.commit.as_ref().unwrap().short_id, "only");
    assert_eq!(node.lane, 0);
    assert!(node.is_head);
    assert!(!node.is_uncommitted);
    assert!(!node.is_stash);
    assert!(matches!(node.cells[0], CellType::Commit(_)));
    assert!(node.cells[1..].iter().all(|c| *c == CellType::Empty));
}

// ─────────────────────────────────────────────────────────────────────
// Branch tracing: lineage_oids / cell_is_traced / graph_has_enough_lanes
// ─────────────────────────────────────────────────────────────────────

use keifu::git::graph::{
    cell_is_traced, edge_is_traced, graph_has_enough_lanes, lineage_oids, GraphLayout,
};
use std::collections::HashSet;

/// Row index of the commit with the given short id.
fn row_of(layout: &GraphLayout, id: &str) -> usize {
    layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().map(|c| c.short_id.as_str()) == Some(id))
        .unwrap_or_else(|| panic!("{id} not found"))
}

/// Lineage OID set for selecting commit `id`.
fn lineage_of(layout: &GraphLayout, id: &str) -> HashSet<Oid> {
    lineage_oids(layout, row_of(layout, id))
}

#[test]
fn trace_linear_covers_the_whole_line_and_gating_is_off() {
    // c3 -> c2 -> c1, one lane only.
    let commits = vec![
        make_commit("c3", vec!["c2"]),
        make_commit("c2", vec!["c1"]),
        make_commit("c1", vec![]),
    ];
    let branches = vec![make_branch("main", "c3", true)];
    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    // Selecting the middle commit lights the entire linear history: ancestor
    // (c1, down) and descendant (c3, up).
    let lin = lineage_of(&layout, "c2");
    assert!(lin.contains(&make_oid("c1")));
    assert!(lin.contains(&make_oid("c2")));
    assert!(lin.contains(&make_oid("c3")));

    // A single-lane graph is never worth tracing.
    assert_eq!(layout.max_lane, 0);
    assert!(!graph_has_enough_lanes(&layout));
}

#[test]
fn trace_single_merge_excludes_the_other_branch() {
    // c4 = merge(c3 main-first-parent, c2 feature); both c3 and c2 off c1.
    let commits = vec![
        make_commit("c4", vec!["c3", "c2"]),
        make_commit("c3", vec!["c1"]),
        make_commit("c2", vec!["c1"]),
        make_commit("c1", vec![]),
    ];
    let branches = vec![make_branch("main", "c4", true)];
    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    // Selecting the main line (c3): down to c1, up to the merge c4 — but NOT the
    // feature commit c2.
    let main_line = lineage_of(&layout, "c3");
    assert!(main_line.contains(&make_oid("c1")));
    assert!(main_line.contains(&make_oid("c3")));
    assert!(main_line.contains(&make_oid("c4")));
    assert!(
        !main_line.contains(&make_oid("c2")),
        "the feature commit must not be on the main line"
    );

    // Selecting the feature (c2): just c2 and its ancestor c1 — the merge and
    // main commits are excluded (c2 is the merge's *second* parent).
    let feature = lineage_of(&layout, "c2");
    assert!(feature.contains(&make_oid("c2")));
    assert!(feature.contains(&make_oid("c1")));
    assert!(!feature.contains(&make_oid("c3")));
    assert!(!feature.contains(&make_oid("c4")));

    // The merge row draws a curve down to the feature (c2). Under the edge
    // model, selecting the feature must NOT light that curve — its edge is
    // (c4 → c2) and the merge commit c4 is off the feature's first-parent line.
    // This is the lead-in-strokes fix: a single shared endpoint (c2) no longer
    // lights the stroke. Nothing on the merge row is traced by the feature.
    let merge = &layout.nodes[row_of(&layout, "c4")];
    assert!(
        !merge.cell_oids.iter().any(|o| cell_is_traced(*o, &feature)),
        "selecting the feature must not light the merge row's lead-in strokes"
    );

    // Selecting the main line lights the merge's own dot — both endpoints of the
    // self-edge (c4, c4) are on the main line — while still leaving the feature
    // curve dim (c2 ∉ main line).
    let dot_col = merge.lane * 2;
    assert!(
        cell_is_traced(merge.cell_oids[dot_col], &main_line),
        "the merge dot is on the main line"
    );
    assert!(
        !merge
            .cell_oids
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != dot_col)
            .any(|(_, o)| cell_is_traced(*o, &main_line)),
        "the main line does not light the merge's curve to the feature"
    );
}

#[test]
fn trace_does_not_leak_onto_a_reused_lane() {
    // Two independent feature branches off the main line at different times.
    // The second reuses the lane the first freed. Selecting one must never
    // trace the other, even where they share a lane column.
    let commits = vec![
        make_commit("m2", vec!["m1", "g1"]), // merge feature G
        make_commit("g1", vec!["mid"]),      // feature G
        make_commit("m1", vec!["mid"]),      // main between the two merges
        make_commit("mid", vec!["b0", "f1"]),// merge feature F
        make_commit("f1", vec!["b0"]),       // feature F
        make_commit("b0", vec![]),           // base
    ];
    let branches = vec![make_branch("main", "m2", true)];
    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    let feat_f = lineage_of(&layout, "f1");
    let feat_g = lineage_of(&layout, "g1");

    // The two feature branches are disjoint (bar the shared base, which each
    // reaches as an ancestor — assert the feature *tips* don't cross).
    assert!(feat_f.contains(&make_oid("f1")));
    assert!(!feat_f.contains(&make_oid("g1")));
    assert!(feat_g.contains(&make_oid("g1")));
    assert!(!feat_g.contains(&make_oid("f1")));

    // Prove the lanes were actually reused: f1 and g1 sit on the same lane.
    let f_lane = layout.nodes[row_of(&layout, "f1")].lane;
    let g_lane = layout.nodes[row_of(&layout, "g1")].lane;
    assert_eq!(f_lane, g_lane, "the fixture must reuse the feature lane");
    assert_ne!(f_lane, 0, "features are off the main lane");

    // The occupant of the shared lane — each feature's own dot — is traced only
    // by its own selection, never by the other feature that reused the lane.
    // (Shared-ancestor pipes on the *main* lane may legitimately appear in both,
    // so we check the feature lane column specifically.)
    let g_dot = layout.nodes[row_of(&layout, "g1")].cell_oids[g_lane * 2];
    assert!(!cell_is_traced(g_dot, &feat_f), "F must not light G's reused lane");
    let f_dot = layout.nodes[row_of(&layout, "f1")].cell_oids[f_lane * 2];
    assert!(!cell_is_traced(f_dot, &feat_g), "G must not light F's reused lane");
}

#[test]
fn trace_horizontal_pipe_crossing_uses_both_oids() {
    // A2 merges a feature (C1) whose horizontal sweep crosses branch B's
    // in-flight pipe, producing a `┼` HorizontalPipe. Its primary OID is the
    // merge edge (C1); its secondary is the crossed branch-B pipe (b1).
    let commits = vec![
        make_commit("A3", vec!["A2"]),
        make_commit("B2", vec!["B1"]),       // branch B tip (keeps lane in flight)
        make_commit("A2", vec!["A1", "C1"]), // merge crosses B's lane
        make_commit("C1", vec!["R"]),        // feature merged into A2
        make_commit("A1", vec!["R"]),
        make_commit("B1", vec!["R"]),
        make_commit("R", vec![]),
    ];
    let branches = vec![
        make_branch("main", "A3", true),
        make_branch("b", "B2", false),
    ];
    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    // Find the HorizontalPipe on A2's row (both edge OIDs populated).
    let a2 = &layout.nodes[row_of(&layout, "A2")];
    let cross = a2
        .cells
        .iter()
        .enumerate()
        .find(|(_, c)| matches!(c, CellType::HorizontalPipe(_, _)))
        .map(|(i, _)| i)
        .expect("expected a HorizontalPipe crossing on the merge row");
    let cross_oids = a2.cell_oids[cross];
    let (primary, secondary) = cross_oids;
    // Primary = the merge sweep edge (A2 → C1); secondary = the crossed branch-B
    // pipe edge (B2 → B1).
    assert!(primary.is_some() && secondary.is_some());

    let feat_c = lineage_of(&layout, "C1");
    let branch_b = lineage_of(&layout, "B2");
    let main_line = lineage_of(&layout, "A3");

    // The secondary edge — the crossed vertical pipe — lights under branch B's
    // selection, since both its endpoints B2, B1 are on branch B. The primary
    // merge sweep does not.
    assert!(edge_is_traced(secondary, &branch_b), "crossed pipe (secondary) lit by branch B");
    assert!(!edge_is_traced(primary, &branch_b));
    assert!(
        cell_is_traced(cross_oids, &branch_b),
        "so the cell is traced via its secondary edge"
    );

    // Neither the feature nor the main line lights the merge sweep: the merge
    // commit A2 is off the feature's first-parent line and C1 is off the main
    // line, so the (A2 → C1) edge is never fully on-lineage — the lead-in fix.
    assert!(!edge_is_traced(primary, &feat_c));
    assert!(!edge_is_traced(primary, &main_line));
    assert!(!cell_is_traced(cross_oids, &feat_c));
    assert!(!cell_is_traced(cross_oids, &main_line));

    // The primary merge edge lights only when BOTH its endpoints are on the
    // lineage — the defining property of the edge-pair identity.
    let both: HashSet<Oid> = [make_oid("A2"), make_oid("C1")].into_iter().collect();
    assert!(edge_is_traced(primary, &both), "merge edge lit when both endpoints on-lineage");
}

#[test]
fn cell_is_traced_requires_both_edge_endpoints() {
    let a = make_oid("a");
    let b = make_oid("b");
    let other = make_oid("z");
    let lin: HashSet<Oid> = [a, b].into_iter().collect();
    // An edge lights only when BOTH (child, parent) endpoints are on the lineage.
    assert!(edge_is_traced(Some((a, b)), &lin));
    assert!(cell_is_traced((Some((a, b)), None), &lin));
    assert!(cell_is_traced((None, Some((a, b))), &lin)); // secondary hit
    assert!(cell_is_traced((Some((a, other)), Some((a, b))), &lin)); // secondary hit
    // A single shared endpoint is not enough — this is the lead-in-strokes fix.
    assert!(!edge_is_traced(Some((a, other)), &lin));
    assert!(!edge_is_traced(Some((other, b)), &lin));
    assert!(!cell_is_traced((Some((a, other)), None), &lin));
    assert!(!cell_is_traced((Some((other, b)), None), &lin));
    assert!(!cell_is_traced((None, None), &lin));
}

#[test]
fn trace_dim_mask_has_few_distinct_variants_per_row_shape() {
    // As the selection moves over every commit, the per-row (shape, dim-mask)
    // combinations must stay small — the pixel protocol cache is keyed by them,
    // so an explosion here would mean re-rasterizing the whole graph on every
    // selection move. Most rows are all-traced or all-dimmed; only branch/merge
    // rows have a few partial masks.
    let commits = vec![
        make_commit("h", vec!["g", "e"]), // merge feature E
        make_commit("g", vec!["f", "d"]), // merge feature D
        make_commit("e", vec!["c"]),      // feature E
        make_commit("f", vec!["c"]),      // main
        make_commit("d", vec!["b"]),      // feature D
        make_commit("c", vec!["b"]),      // main
        make_commit("b", vec!["a"]),      // main
        make_commit("a", vec![]),         // root
    ];
    let branches = vec![make_branch("main", "h", true)];
    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    // Base shapes: distinct cell-rows, independent of any selection. The
    // rendered glyph string plus the lane color indices fully determine the
    // non-dim part of a RowSpec, so it's a faithful shape key.
    let shape = |n: &keifu::git::graph::GraphNode| -> String { render_cells(&n.cells) };
    let base_shapes: HashSet<String> = layout.nodes.iter().map(shape).collect();

    // (shape, mask) pairs seen across every commit selection.
    let mut variants: HashSet<(String, Vec<bool>)> = HashSet::new();
    for (sel, node) in layout.nodes.iter().enumerate() {
        if node.commit.is_none() {
            continue; // connectors aren't selectable
        }
        let lineage = lineage_oids(&layout, sel);
        for n in &layout.nodes {
            let mask: Vec<bool> = n
                .cell_oids
                .iter()
                .map(|o| cell_is_traced(*o, &lineage))
                .collect();
            variants.insert((shape(n), mask));
        }
    }

    // The cache stays small: well under 3x the distinct base shapes. (Observed
    // for this fixture: 20 variants vs 9 base shapes — a 2.2x factor.)
    assert!(
        variants.len() <= 3 * base_shapes.len(),
        "trace-mask variants ({}) exceeded 3x base shapes ({})",
        variants.len(),
        base_shapes.len()
    );
}
