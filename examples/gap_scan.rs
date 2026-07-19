//! Temporary: scan a raster_debug PNG for hairline gaps — short background
//! runs flanked by stroke pixels on the same row or column.
//!
//! Usage: cargo run --example gap_scan -- <png> <max_gap_px>

fn is_bg(p: &image::Rgba<u8>) -> bool {
    // Backgrounds used by raster_debug: base and selection band.
    let c = [p[0] as i32, p[1] as i32, p[2] as i32];
    let close = |a: [i32; 3], b: [i32; 3]| {
        (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() < 24
    };
    close(c, [26, 27, 48]) || close(c, [60, 62, 80])
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let img = image::open(&args[1]).unwrap().to_rgba8();
    if args[2] == "crop" {
        let (x, y, w, h, scale): (u32, u32, u32, u32, u32) = (
            args[3].parse().unwrap(),
            args[4].parse().unwrap(),
            args[5].parse().unwrap(),
            args[6].parse().unwrap(),
            args[7].parse().unwrap(),
        );
        let mut out = image::RgbaImage::new(w * scale, h * scale);
        for oy in 0..h * scale {
            for ox in 0..w * scale {
                let sx = (x + ox / scale).min(img.width() - 1);
                let sy = (y + oy / scale).min(img.height() - 1);
                out.put_pixel(ox, oy, *img.get_pixel(sx, sy));
            }
        }
        out.save(&args[8]).unwrap();
        return;
    }
    let max_gap: u32 = args[2].parse().unwrap();
    let (w, h) = img.dimensions();
    // Horizontal scan: stroke, short bg run, stroke on the same row.
    for y in 0..h {
        let mut x = 0;
        while x < w {
            if !is_bg(img.get_pixel(x, y)) {
                let mut g = x + 1;
                while g < w && is_bg(img.get_pixel(g, y)) {
                    g += 1;
                }
                if g < w && g - x > 1 && g - x - 1 <= max_gap {
                    println!("H y={y} x={}..{} gap={}", x + 1, g - 1, g - x - 1);
                }
                x = g;
            } else {
                x += 1;
            }
        }
    }
    // Vertical scan: stroke, short bg run, stroke in the same column.
    for x in 0..w {
        let mut y = 0;
        while y < h {
            if !is_bg(img.get_pixel(x, y)) {
                let mut g = y + 1;
                while g < h && is_bg(img.get_pixel(x, g)) {
                    g += 1;
                }
                if g < h && g - y > 1 && g - y - 1 <= max_gap {
                    println!("V x={x} y={}..{} gap={}", y + 1, g - 1, g - y - 1);
                }
                y = g;
            } else {
                y += 1;
            }
        }
    }
}
// (appended) crop mode: <png> crop <x> <y> <w> <h> <scale> <out>
