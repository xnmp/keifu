//! Temporary: rasterize folded rows through the real pixel pipeline to a PNG,
//! to diagnose line artifacts the text dump can't show.
//! Usage: KEIFU_UNCOMMITTED=<n> cargo run --example render_rows -- <repo> <start> <end> <out.png>

use image::RgbaImage;
use keifu::git::graph::{build_graph, CellType, GraphNode};
use keifu::git::GitRepository;
use keifu::ui::graph_pixels::{build_row_spec, rasterize_row, NeighborRow};
use keifu::ui::theme::Theme;

fn merge(underlay: &[CellType], cells: &[CellType]) -> Vec<CellType> {
    let w = underlay.len().max(cells.len());
    (0..w)
        .map(|col| match underlay.get(col) {
            Some(u) if *u != CellType::Empty => *u,
            _ => cells.get(col).copied().unwrap_or(CellType::Empty),
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repo_path = &args[1];
    let start: usize = args[2].parse().unwrap();
    let end: usize = args[3].parse().unwrap();
    let out = &args[4];

    let mut repo = GitRepository::open(repo_path).unwrap();
    let branches = repo.get_branches().unwrap();
    let stashes = repo.get_stashes();
    let tags = repo.get_tags();
    let commits = repo.get_commits(300, &branches, &stashes, false).unwrap();
    let head_oid = repo.head_oid();
    let uncommitted = std::env::var("KEIFU_UNCOMMITTED")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(Some);
    // Real squash links, like the app's dim-mode classifier produces.
    let squash_links: Vec<(git2::Oid, git2::Oid)> = keifu::git::merged::base_branch(&branches)
        .map(|base| {
            let (_, targets) = keifu::git::merged::classify_merged_branches_with_targets(
                repo.repo(),
                &branches,
                base.tip_oid,
                &base.name.clone(),
                &Default::default(),
            );
            targets
                .iter()
                .filter_map(|(name, &target)| {
                    let tip = branches.iter().find(|b| &b.name == name)?.tip_oid;
                    Some((tip, target))
                })
                .collect()
        })
        .unwrap_or_default();
    eprintln!("squash links: {}", squash_links.len());
    let layout = build_graph(
        &commits,
        &branches,
        &tags,
        &stashes,
        uncommitted,
        head_oid,
        &squash_links,
    );

    // Fold connectors into the following commit row, like `fold_rows`.
    let mut rows: Vec<(&GraphNode, Vec<CellType>)> = Vec::new();
    let mut pending: Vec<&GraphNode> = Vec::new();
    for node in &layout.nodes {
        if node.is_connector() {
            pending.push(node);
            continue;
        }
        let width = pending.iter().map(|n| n.cells.len()).max().unwrap_or(0);
        let mut under = vec![CellType::Empty; width];
        for n in &pending {
            for (col, cell) in n.cells.iter().enumerate() {
                if *cell != CellType::Empty {
                    under[col] = *cell;
                }
            }
        }
        pending.clear();
        rows.push((node, under));
    }

    // adjacent_cells: folded underlay between i and the neighbour wins per column.
    let adjacent = |i: usize, above: bool| -> Option<Vec<CellType>> {
        let (underlay, neighbour): (&[CellType], Option<&[CellType]>) = if above {
            (
                &rows[i].1,
                i.checked_sub(1).map(|p| rows[p].0.cells.as_slice()),
            )
        } else {
            match rows.get(i + 1) {
                Some(next) => (&next.1, Some(next.0.cells.as_slice())),
                None => (&[], None),
            }
        };
        if underlay.is_empty() && neighbour.is_none() {
            return None;
        }
        Some(merge(
            underlay,
            neighbour.map(|c| c.to_vec()).unwrap_or_default().as_slice(),
        ))
    };

    let theme = Theme::dark();
    let (cw, ch) = (20u32, 40u32);
    let end = end.min(rows.len());
    let max_cells = (start..end).map(|i| rows[i].0.cells.len()).max().unwrap_or(1);
    let mut canvas = RgbaImage::from_pixel(
        max_cells as u32 * cw,
        (end - start) as u32 * ch,
        image::Rgba([30, 34, 42, 255]),
    );

    for i in start..end {
        let above = adjacent(i, true);
        let below = adjacent(i, false);
        let nb = |j: usize| NeighborRow {
            underlay: &rows[j].1,
            cells: &rows[j].0.cells,
        };
        let spec = build_row_spec(
            above.as_deref(),
            rows[i].0,
            below.as_deref(),
            &rows[i].1,
            i.checked_sub(1).map(nb),
            (i + 1 < rows.len()).then(|| nb(i + 1)),
            &theme,
        );
        let img = rasterize_row(&spec, cw, ch);
        let y0 = (i - start) as u32 * ch;
        for (x, y, px) in img.enumerate_pixels() {
            if px.0[3] > 0 && x < canvas.width() {
                let dst = canvas.get_pixel_mut(x, y0 + y);
                let a = px.0[3] as u32;
                for k in 0..3 {
                    dst.0[k] = ((px.0[k] as u32 * a + dst.0[k] as u32 * (255 - a)) / 255) as u8;
                }
            }
        }
    }
    canvas.save(out).unwrap();
    eprintln!("wrote {out} rows {start}..{end}");
}
