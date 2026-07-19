//! Temporary reproduction harness: renders real graph rows through the real
//! rasterizer + trace logic into a PNG, to reproduce pixel-mode bug reports.
//!
//! Usage: cargo run --example raster_debug -- <repo> <commit_prefix> <cw> <ch> <out.png> [rows]

use image::RgbaImage;
use keifu::git::graph::{build_graph, edge_is_traced, lineage_oids};
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
    let commits = repo.get_commits(200, &branches, &stashes).unwrap();
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
    let start = target_idx.saturating_sub(n_rows / 2);
    let end = (start + n_rows).min(layout.nodes.len());

    // Rasterize each row (unfolded: connector rows render as their own rows,
    // which matches stroke geometry; folding only merges images).
    let mut row_imgs: Vec<RgbaImage> = Vec::new();
    let mut max_w = 0u32;
    for i in start..end {
        let node = &layout.nodes[i];
        let above = if i > 0 {
            Some(layout.nodes[i - 1].cells.clone())
        } else {
            None
        };
        let below = layout.nodes.get(i + 1).map(|n| n.cells.clone());
        let mut spec = build_row_spec(above.as_deref(), node, below.as_deref(), &[], &theme);
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
        for (ci, pc) in spec.cells.iter_mut().enumerate() {
            let (primary, secondary) =
                node.cell_oids.get(ci).copied().unwrap_or((None, None));
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
                pc.dim = !(is_lit(primary) || is_lit(secondary));
                pc.dim_secondary = pc.dim;
                if let Some(rgb) = color_of(primary).or_else(|| color_of(secondary)) {
                    pc.color = rgb;
                }
            }
        }
        let img = rasterize_row(&spec, cw, ch);
        if std::env::var("DUMP_CELLS").is_ok() {
            for (ci, pc) in spec.cells.iter().enumerate() {
                if pc.shape == CellShape::HorizontalPipe {
                    let cy = ch / 2;
                    let xs: Vec<u32> = ((ci as u32 + 1) * cw..(ci as u32 + 2) * cw)
                        .filter(|&x| img.get_pixel(x, cy)[3] > 0)
                        .collect();
                    eprintln!(
                        "row {i} col {ci}: {:?} dim={} dim_sec={} colored-x-at-cy={:?}",
                        pc.shape, pc.dim, pc.dim_secondary, xs
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
        let is_target = start + ri == target_idx;
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
        for i in start..end {
            let n = &layout.nodes[i];
            let label = n
                .commit
                .as_ref()
                .map(|c| c.short_id.clone())
                .unwrap_or_else(|| "connector".into());
            eprintln!("row {i} ({label}): {:?}", n.cells);
        }
    }
}
