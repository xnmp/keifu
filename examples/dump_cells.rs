//! Temporary: print folded-row CellType grids around a row range to diagnose
//! seam artifacts. Usage: cargo run --example dump_cells -- <repo> <start> <end>

use keifu::git::graph::{build_graph, CellType};
use keifu::git::GitRepository;

fn glyph(c: &CellType) -> String {
    match c {
        CellType::Empty => "  .".into(),
        CellType::Pipe(i) => format!(" |{i}"),
        CellType::Horizontal(i) => format!(" -{i}"),
        CellType::BranchRight(i) => format!("BR{i}"),
        CellType::BranchLeft(i) => format!("BL{i}"),
        CellType::MergeRight(i) => format!("MR{i}"),
        CellType::MergeLeft(i) => format!("ML{i}"),
        CellType::TeeRight(i) => format!("TR{i}"),
        CellType::TeeLeft(i) => format!("TL{i}"),
        CellType::TeeUp(i) => format!("TU{i}"),
        CellType::HorizontalPipe(h, p) => format!("H{h}P{p}"),
        CellType::Commit(i) => format!(" @{i}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repo_path = &args[1];
    let start: usize = args[2].parse().unwrap();
    let end: usize = args[3].parse().unwrap();

    let mut repo = GitRepository::open(repo_path).unwrap();
    let branches = repo.get_branches().unwrap();
    let stashes = repo.get_stashes();
    let tags = repo.get_tags();
    let commits = repo.get_commits(300, &branches, &stashes, false).unwrap();
    let head = commits
        .iter()
        .find(|c| branches.iter().any(|b| b.is_head && b.tip_oid == c.oid));
    let layout = build_graph(
        &commits,
        &branches,
        &tags,
        &stashes,
        None,
        head.map(|c| c.oid),
        &[],
    );

    // Fold connectors like the pixel pipeline: (node_idx, underlay).
    let mut folded: Vec<(usize, Vec<CellType>)> = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    for (i, node) in layout.nodes.iter().enumerate() {
        if node.is_connector() {
            pending.push(i);
        } else {
            let width = pending
                .iter()
                .map(|&p| layout.nodes[p].cells.len())
                .max()
                .unwrap_or(0);
            let mut underlay = vec![CellType::Empty; width];
            for &p in &pending {
                for (col, cell) in layout.nodes[p].cells.iter().enumerate() {
                    if *cell != CellType::Empty {
                        underlay[col] = *cell;
                    }
                }
            }
            pending.clear();
            folded.push((i, underlay));
        }
    }

    for (fi, (ni, underlay)) in folded.iter().enumerate().take(end.min(folded.len())).skip(start)
    {
        let node = &layout.nodes[*ni];
        let subj: String = node
            .commit
            .as_ref()
            .map(|c| c.message.chars().take(48).collect())
            .unwrap_or_default();
        if !underlay.is_empty() {
            let u: Vec<String> = underlay.iter().map(glyph).collect();
            println!("row {fi:3} UNDER  {}", u.join(" "));
        }
        let cells: Vec<String> = node.cells.iter().map(glyph).collect();
        println!("row {fi:3} CELLS  {}  {subj}", cells.join(" "));
    }
}
