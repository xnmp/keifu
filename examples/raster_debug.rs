//! Temporary reproduction harness: renders real graph rows through the real
//! rasterizer + trace logic into a PNG, to reproduce pixel-mode bug reports.
//!
//! Usage: cargo run --example raster_debug -- <repo> <commit_prefix> <cw> <ch> <out.png> [rows]

use image::RgbaImage;
use keifu::git::graph::{build_graph, lineage_oids};
use keifu::git::GitRepository;
use keifu::ui::graph_pixels::{
    build_row_spec, rasterize_row, CellShape, PIXEL_LEFT_PAD_CELLS,
};
use keifu::ui::theme::Theme;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repo_path = &args[1];
    let prefix = &args[2];
    let cw: u32 = args[3].parse().unwrap();
    let ch: u32 = args[4].parse().unwrap();
    let out = &args[5];
    let n_rows: usize = args.get(6).map(|s| s.parse().unwrap()).unwrap_or(24);

    let mut repo = GitRepository::open(repo_path).unwrap();
    let branches = repo.get_branches().unwrap();
    let stashes = repo.get_stashes();
    let tags = repo.get_tags();
    let commits = repo.get_commits(200, &branches, &stashes, false).unwrap();
    let head = commits.iter().find(|c| {
        branches.iter().any(|b| b.is_head && b.tip_oid == c.oid)
    });
    let layout = build_graph(
        &commits,
        &branches,
        &tags,
        &stashes,
        None,
        head.map(|c| c.oid),
        &[],
    );

    // Find the target commit's row and lineage.
    let target_idx = layout
        .nodes
        .iter()
        .position(|n| {
            n.commit
                .as_ref()
                .is_some_and(|c| c.oid.to_string().starts_with(prefix.as_str()))
        })
        .expect("commit prefix not found in layout");
    let lineage = lineage_oids(&layout, target_idx);
    eprintln!(
        "target row {} of {}; lineage size {}",
        target_idx,
        layout.nodes.len(),
        lineage.len()
    );

    let theme = Theme::dark();

    // Optional folding: collapse standalone connector rows into the following
    // commit row's underlay, replicating the real pixel pipeline
    // (build_pixel_row_specs / fold_rows). Produces (node_idx, underlay) rows.
    let fold = std::env::var("FOLD").is_ok();
    type FoldedRow = (
        usize,
        Vec<keifu::git::graph::CellType>,
        Vec<keifu::git::graph::CellOids>,
    );
    let folded: Vec<FoldedRow> = if fold {
        let mut out = Vec::new();
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
                let mut underlay = vec![keifu::git::graph::CellType::Empty; width];
                // Fold edge identity alongside, mirroring merge_connector_cells,
                // so the underlay dims/recolors exactly like the app.
                let mut underlay_oids: Vec<keifu::git::graph::CellOids> =
                    vec![(None, None); width];
                for &p in &pending {
                    for (col, cell) in layout.nodes[p].cells.iter().enumerate() {
                        if *cell != keifu::git::graph::CellType::Empty {
                            underlay[col] = *cell;
                            underlay_oids[col] = layout.nodes[p]
                                .cell_oids
                                .get(col)
                                .copied()
                                .unwrap_or((None, None));
                        }
                    }
                }
                pending.clear();
                out.push((i, underlay, underlay_oids));
            }
        }
        out
    } else {
        (0..layout.nodes.len())
            .map(|i| (i, Vec::new(), Vec::new()))
            .collect()
    };
    let target_row = folded
        .iter()
        .position(|(i, _, _)| *i == target_idx)
        .unwrap_or(0);
    let start = target_row.saturating_sub(n_rows / 2);
    let end = (start + n_rows).min(folded.len());

    // Rasterize each row (unfolded: connector rows render as their own rows,
    // which matches stroke geometry; folding only merges images).
    let mut row_imgs: Vec<RgbaImage> = Vec::new();
    let mut max_w = 0u32;
    for i in start..end {
        let (node_idx, underlay, underlay_oids) = &folded[i];
        let node = &layout.nodes[*node_idx];
        // App semantics (graph_view::adjacent_cells): the underlay physically
        // between the dot and the neighbour wins per column — row i's own
        // underlay for the above view, row i+1's underlay for the below view.
        let merge_view = |underlay: &[keifu::git::graph::CellType],
                          cells: &[keifu::git::graph::CellType]| {
            let w = underlay.len().max(cells.len());
            (0..w)
                .map(|col| match underlay.get(col) {
                    Some(u) if *u != keifu::git::graph::CellType::Empty => *u,
                    _ => cells
                        .get(col)
                        .copied()
                        .unwrap_or(keifu::git::graph::CellType::Empty),
                })
                .collect::<Vec<_>>()
        };
        let above = if i > 0 {
            Some(merge_view(underlay, &layout.nodes[folded[i - 1].0].cells))
        } else {
            None
        };
        let below = folded
            .get(i + 1)
            .map(|(ni, u, _)| merge_view(u, &layout.nodes[*ni].cells));
        let neighbor = |j: usize| keifu::ui::graph_pixels::NeighborRow {
            underlay: &folded[j].1,
            cells: &layout.nodes[folded[j].0].cells,
        };
        let above_row = i.checked_sub(1).map(neighbor);
        let below_row = (i + 1 < folded.len()).then(|| neighbor(i + 1));
        let mut spec = build_row_spec(
            above.as_deref(),
            node,
            below.as_deref(),
            underlay,
            above_row,
            below_row,
            &theme,
        );
        if std::env::var("NODIM").is_ok() {
            let img = rasterize_row(&spec, cw, ch);
            max_w = max_w.max(img.width());
            row_imgs.push(img);
            continue;
        }
        // Replicate apply_trace_dim (private in graph_view), incl. recolor.
        let lit = keifu::git::graph::trace_lit_edges(&layout, &lineage);
        let lane_rgb: std::collections::HashMap<git2::Oid, [u8; 3]> = layout
            .nodes
            .iter()
            .filter_map(|n| {
                n.commit.as_ref().map(|c| {
                    let rgb = keifu::ui::graph_pixels::color_to_rgb(
                        theme.lane_color(n.color_index),
                    );
                    (c.oid, rgb)
                })
            })
            .collect();
        let is_lit = |edge: Option<keifu::git::graph::CellEdge>| {
            edge.is_some_and(|e| lit.contains_key(&e))
        };
        let color_of = |edge: Option<keifu::git::graph::CellEdge>| {
            edge.and_then(|e| lit.get(&e))
                .and_then(|oid| lane_rgb.get(oid))
                .copied()
        };
        // Dim/recolor both layers, as build_pixel_row_specs does.
        let layers: [(&mut Vec<_>, &[keifu::git::graph::CellOids]); 2] = [
            (&mut spec.cells, &node.cell_oids),
            (&mut spec.underlay, underlay_oids),
        ];
        for (cells, oids) in layers {
        for (ci, pc) in cells.iter_mut().enumerate() {
            let (primary, secondary) =
                oids.get(ci).copied().unwrap_or((None, None));
            if pc.shape == CellShape::HorizontalPipe {
                pc.dim_secondary = !is_lit(primary);
                pc.dim = !is_lit(secondary);
                if let Some(rgb) = color_of(primary) {
                    pc.secondary = rgb;
                }
                if let Some(rgb) = color_of(secondary) {
                    pc.color = rgb;
                }
            } else if matches!(pc.shape, CellShape::Commit { .. }) {
                pc.dim = !(is_lit(primary) || is_lit(secondary));
                pc.dim_secondary = pc.dim;
            } else {
                // Own stroke = primary edge only; a secondary edge is a
                // co-routed sibling drawn by its own curve (see graph_view).
                pc.dim = !is_lit(primary);
                pc.dim_secondary = pc.dim;
                if let Some(rgb) = color_of(primary) {
                    pc.color = rgb;
                }
            }
        }
        }
        if std::env::var("DUMP_OIDS").is_ok() {
            let sid = |o: git2::Oid| o.to_string()[..7].to_string();
            let node_label = node
                .commit
                .as_ref()
                .map(|c| sid(c.oid))
                .unwrap_or_else(|| "connector".into());
            eprintln!(
                "row {i} {node_label} lane={} color_index={}",
                node.lane, node.color_index
            );
            for (layer, oids_l) in [("cells", &node.cell_oids), ("underlay", underlay_oids)]
            {
                for (ci, (pe, se)) in oids_l.iter().enumerate() {
                    if pe.is_none() && se.is_none() {
                        continue;
                    }
                    let fmt = |e: &Option<keifu::git::graph::CellEdge>| {
                        e.map(|(c2, p2)| {
                            let hit = lit
                                .get(&(c2, p2))
                                .map(|v| format!(" LIT->{}", sid(*v)))
                                .unwrap_or_default();
                            format!("({}->{}{hit})", sid(c2), sid(p2))
                        })
                        .unwrap_or_else(|| "-".into())
                    };
                    eprintln!("  {layer} col {ci}: p={} s={}", fmt(pe), fmt(se));
                }
            }
        }
        let img = rasterize_row(&spec, cw, ch);
        if std::env::var("DUMP_CELLS").is_ok() {
            for (layer, cells) in [("cells", &spec.cells), ("underlay", &spec.underlay)] {
                for (ci, pc) in cells.iter().enumerate() {
                    if pc.shape == CellShape::Empty {
                        continue;
                    }
                    eprintln!(
                        "row {i} {layer} col {ci}: {:?} color={:?} sec={:?} dim={} dim_sec={}",
                        pc.shape, pc.color, pc.secondary, pc.dim, pc.dim_secondary
                    );
                }
            }
        }
        max_w = max_w.max(img.width());
        row_imgs.push(img);
    }

    // Composite rows over a dark background, marking the target row.
    let bg = [26u8, 27, 48];
    let total_h = row_imgs.len() as u32 * ch;
    let mut canvas = RgbaImage::from_pixel(max_w, total_h, image::Rgba([bg[0], bg[1], bg[2], 255]));
    for (ri, img) in row_imgs.iter().enumerate() {
        let is_target = folded[start + ri].0 == target_idx;
        for y in 0..img.height() {
            for x in 0..img.width() {
                let p = img.get_pixel(x, y);
                let a = p[3] as f32 / 255.0;
                let row_bg = if is_target { [60u8, 62, 80] } else { bg };
                let dst = canvas.get_pixel_mut(x, ri as u32 * ch + y);
                for k in 0..3 {
                    dst[k] = (p[k] as f32 * a + row_bg[k] as f32 * (1.0 - a)).round() as u8;
                }
                dst[3] = 255;
            }
        }
        if is_target {
            // Fill the selection band beyond the strokes too.
            for y in 0..ch {
                for x in 0..max_w {
                    let dst = canvas.get_pixel_mut(x, ri as u32 * ch + y);
                    if dst.0[..3] == bg {
                        dst.0 = [60, 62, 80, 255];
                    }
                }
            }
        }
    }
    let _ = PIXEL_LEFT_PAD_CELLS; // silence unused import if pad unused
    canvas.save(out).unwrap();
    eprintln!("wrote {out} ({max_w}x{total_h})");

    if std::env::var("DUMP_CELLS").is_ok() {
        for (ni, underlay, _) in &folded[start..end] {
            let n = &layout.nodes[*ni];
            let label = n
                .commit
                .as_ref()
                .map(|c| c.short_id.clone())
                .unwrap_or_else(|| "connector".into());
            eprintln!("row {ni} ({label}): {:?}", n.cells);
            if !underlay.is_empty() {
                eprintln!("     underlay: {underlay:?}");
            }
        }
    }
}
