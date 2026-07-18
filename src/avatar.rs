//! Pure logic for author avatars: email normalization/hashing, avatar URL
//! resolution (GitHub noreply / Gravatar), on-disk cache path/TTL helpers,
//! and image compositing (circle-crop + solid-color fallback discs).
//!
//! This module is intentionally free of I/O, threading, and networking:
//! callers are responsible for actually fetching/caching bytes and wiring
//! the results into the app/UI layers.

use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Negative-cache retry window: how long a ".missing" marker is trusted
/// before a re-download is attempted.
pub const MISSING_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Small, pleasant fallback palette. Same email always maps to the same
/// entry (see [`fallback_color`]).
const FALLBACK_PALETTE: [[u8; 3]; 8] = [
    [231, 76, 60],   // red
    [230, 126, 34],  // orange
    [241, 196, 15],  // yellow
    [46, 204, 113],  // green
    [26, 188, 156],  // teal
    [52, 152, 219],  // blue
    [155, 89, 182],  // purple
    [231, 76, 145],  // pink
];

/// Lowercase + trim — the Gravatar/cache normalization.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// MD5 hex digest (lowercase, 32 chars) of the input bytes.
pub fn md5_hex(input: &str) -> String {
    let digest = md5::compute(input.as_bytes());
    format!("{digest:x}")
}

/// MD5 hex of the normalized email — used for both the Gravatar hash and
/// cache filenames.
pub fn email_hash(email: &str) -> String {
    md5_hex(&normalize_email(email))
}

/// If `email` is a GitHub noreply address, its avatar URL, else `None`.
///
/// Two recognized forms (host match is case-insensitive):
/// - `"{id}+{login}@users.noreply.github.com"` (id all-digits) →
///   `"https://avatars.githubusercontent.com/u/{id}?s=64"`
/// - `"{login}@users.noreply.github.com"` →
///   `"https://avatars.githubusercontent.com/{login}?s=64"`
pub fn github_noreply_url(email: &str) -> Option<String> {
    const HOST: &str = "@users.noreply.github.com";
    let email = email.trim();
    let lower = email.to_lowercase();
    let local = lower.strip_suffix(HOST)?;
    // Recover the original-case local part (same byte length as `local`
    // since suffix stripping and lowercasing here are ASCII-length-stable
    // for this fixed-width host suffix).
    let local_original = &email[..local.len()];

    if let Some((id, login)) = local_original.split_once('+') {
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !login.is_empty() {
            return Some(format!("https://avatars.githubusercontent.com/u/{id}?s=64"));
        }
    }
    if local_original.is_empty() {
        return None;
    }
    Some(format!(
        "https://avatars.githubusercontent.com/{local_original}?s=64"
    ))
}

/// Gravatar URL for `email` (normalized before hashing).
pub fn gravatar_url(email: &str) -> String {
    format!(
        "https://www.gravatar.com/avatar/{}?s=64&d=404",
        email_hash(email)
    )
}

/// The avatar URL to fetch for an email: the GitHub noreply URL if it is
/// one, otherwise the Gravatar URL. `None` for an empty/whitespace-only
/// email.
pub fn resolve_avatar_url(email: &str) -> Option<String> {
    if email.trim().is_empty() {
        return None;
    }
    Some(github_noreply_url(email).unwrap_or_else(|| gravatar_url(email)))
}

/// Cache directory: `dirs::cache_dir()?/keifu/avatars`. Does NOT create it.
pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("keifu").join("avatars"))
}

/// `{dir}/{email_hash}.png`
pub fn cache_png_path(dir: &Path, email: &str) -> PathBuf {
    dir.join(format!("{}.png", email_hash(email)))
}

/// `{dir}/{email_hash}.missing` (negative-result marker file).
pub fn cache_missing_path(dir: &Path, email: &str) -> PathBuf {
    dir.join(format!("{}.missing", email_hash(email)))
}

/// Whether a `.missing` marker with mtime `mtime` is stale relative to
/// `now` (older than `ttl`), so a re-download should be attempted.
///
/// `now` is injected for testability. Clock skew (`now < mtime`) is treated
/// as NOT expired.
pub fn missing_is_expired(mtime: SystemTime, now: SystemTime, ttl: Duration) -> bool {
    match now.duration_since(mtime) {
        Ok(age) => age > ttl,
        Err(_) => false,
    }
}

/// Deterministic fallback avatar color derived from the email, chosen from
/// a small fixed palette. The same email always yields the same color.
pub fn fallback_color(email: &str) -> [u8; 3] {
    let hash = email_hash(email);
    // Use the first hex byte pair of the hash to index the palette.
    let byte = u8::from_str_radix(&hash[0..2], 16).unwrap_or(0);
    FALLBACK_PALETTE[(byte as usize) % FALLBACK_PALETTE.len()]
}

/// Composite a `d`×`d` RGBA source, anti-aliased circle-masked, centered
/// into a transparent `w`×`h` canvas. Shared by [`circle_crop`] and
/// [`fallback_disc`].
fn composite_circle_masked(src: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let d = src.width().min(src.height());
    let mut canvas = RgbaImage::new(w, h);
    if d == 0 || w == 0 || h == 0 {
        return canvas;
    }

    let radius = d as f32 / 2.0;
    let center = d as f32 / 2.0;
    let off_x = (w.saturating_sub(d)) / 2;
    let off_y = (h.saturating_sub(d)) / 2;

    for y in 0..d {
        for x in 0..d {
            let cx = off_x + x;
            let cy = off_y + y;
            if cx >= w || cy >= h {
                continue;
            }
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);

            let mut px = *src.get_pixel(x, y);
            let a = px[3] as f32 * coverage;
            px[3] = a.round().clamp(0.0, 255.0) as u8;
            canvas.put_pixel(cx, cy, px);
        }
    }

    canvas
}

/// Circle-crop + downscale `img` into a transparent `w`×`h` RGBA canvas.
///
/// A centered, anti-aliased opaque circle of diameter
/// `d = min(w,h).saturating_sub(2)` (1px margin) is produced: the source
/// image is resized (Lanczos3) to `d`×`d`, placed centered, and masked with
/// an anti-aliased circular alpha (coverage = `clamp(radius + 0.5 - dist, 0, 1)`)
/// multiplied into the source alpha. Fully transparent outside the circle.
/// Output is exactly `w`×`h`.
pub fn circle_crop(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let d = w.min(h).saturating_sub(2);
    if d == 0 {
        return RgbaImage::new(w, h);
    }
    let resized = image::imageops::resize(img, d, d, image::imageops::FilterType::Lanczos3);
    composite_circle_masked(&resized, w, h)
}

/// A solid-`color` avatar disc: builds a `d`×`d` solid opaque image and runs
/// it through the same circle mask as [`circle_crop`] to produce a `w`×`h`
/// transparent-cornered colored disc.
pub fn fallback_disc(color: [u8; 3], w: u32, h: u32) -> RgbaImage {
    let d = w.min(h).saturating_sub(2);
    if d == 0 {
        return RgbaImage::new(w, h);
    }
    let mut solid = RgbaImage::new(d, d);
    for px in solid.pixels_mut() {
        *px = image::Rgba([color[0], color[1], color[2], 255]);
    }
    composite_circle_masked(&solid, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- md5_hex --------------------------------------------------------

    #[test]
    fn md5_hex_known_vectors() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    // -- normalize_email --------------------------------------------------

    #[test]
    fn normalize_email_trims_and_lowercases() {
        assert_eq!(normalize_email("  Foo@Bar.COM "), "foo@bar.com");
    }

    // -- email_hash ---------------------------------------------------------

    #[test]
    fn email_hash_matches_md5_of_normalized() {
        assert_eq!(email_hash("Foo@Bar.com"), md5_hex("foo@bar.com"));
        assert_eq!(email_hash("  Foo@Bar.com  "), md5_hex("foo@bar.com"));
    }

    // -- github_noreply_url -------------------------------------------------

    #[test]
    fn github_noreply_url_id_form() {
        assert_eq!(
            github_noreply_url("12345+octocat@users.noreply.github.com"),
            Some("https://avatars.githubusercontent.com/u/12345?s=64".to_string())
        );
    }

    #[test]
    fn github_noreply_url_login_form() {
        assert_eq!(
            github_noreply_url("octocat@users.noreply.github.com"),
            Some("https://avatars.githubusercontent.com/octocat?s=64".to_string())
        );
    }

    #[test]
    fn github_noreply_url_ordinary_email_is_none() {
        assert_eq!(github_noreply_url("someone@example.com"), None);
    }

    #[test]
    fn github_noreply_url_case_insensitive_host() {
        assert_eq!(
            github_noreply_url("octocat@USERS.NOREPLY.GITHUB.COM"),
            Some("https://avatars.githubusercontent.com/octocat?s=64".to_string())
        );
        assert_eq!(
            github_noreply_url("12345+octocat@Users.NoReply.GitHub.Com"),
            Some("https://avatars.githubusercontent.com/u/12345?s=64".to_string())
        );
    }

    #[test]
    fn github_noreply_url_non_digit_id_falls_back_to_login_form() {
        // "{login}" containing a '+' where the prefix isn't all-digits is not
        // the id form; the whole local part is used as the login.
        assert_eq!(
            github_noreply_url("abc+octocat@users.noreply.github.com"),
            Some("https://avatars.githubusercontent.com/abc+octocat?s=64".to_string())
        );
    }

    // -- gravatar_url ---------------------------------------------------------

    #[test]
    fn gravatar_url_contains_hash_and_params() {
        let url = gravatar_url("Foo@Bar.COM");
        assert!(url.starts_with("https://www.gravatar.com/avatar/"));
        assert!(url.contains(&email_hash("Foo@Bar.COM")));
        assert!(url.ends_with("?s=64&d=404"));
    }

    #[test]
    fn gravatar_url_normalizes_before_hashing() {
        assert_eq!(
            gravatar_url("  FOO@BAR.com  "),
            gravatar_url("foo@bar.com")
        );
    }

    // -- resolve_avatar_url -----------------------------------------------

    #[test]
    fn resolve_avatar_url_noreply_uses_github() {
        let email = "octocat@users.noreply.github.com";
        assert_eq!(resolve_avatar_url(email), github_noreply_url(email));
    }

    #[test]
    fn resolve_avatar_url_ordinary_uses_gravatar() {
        let email = "someone@example.com";
        assert_eq!(resolve_avatar_url(email), Some(gravatar_url(email)));
    }

    #[test]
    fn resolve_avatar_url_empty_is_none() {
        assert_eq!(resolve_avatar_url(""), None);
        assert_eq!(resolve_avatar_url("   "), None);
    }

    // -- cache paths ----------------------------------------------------

    #[test]
    fn cache_png_path_ends_with_hash_and_extension() {
        let dir = Path::new("/tmp/whatever");
        let path = cache_png_path(dir, "foo@bar.com");
        assert_eq!(
            path,
            dir.join(format!("{}.png", email_hash("foo@bar.com")))
        );
        assert!(path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(&format!("{}.png", email_hash("foo@bar.com"))));
    }

    #[test]
    fn cache_missing_path_ends_with_hash_and_extension() {
        let dir = Path::new("/tmp/whatever");
        let path = cache_missing_path(dir, "foo@bar.com");
        assert_eq!(
            path,
            dir.join(format!("{}.missing", email_hash("foo@bar.com")))
        );
    }

    // -- missing_is_expired -------------------------------------------------

    #[test]
    fn missing_is_expired_past_ttl() {
        let now = SystemTime::now();
        let mtime = now - Duration::from_secs(8 * 24 * 60 * 60);
        assert!(missing_is_expired(mtime, now, MISSING_TTL));
    }

    #[test]
    fn missing_is_expired_within_ttl() {
        let now = SystemTime::now();
        let mtime = now - Duration::from_secs(24 * 60 * 60);
        assert!(!missing_is_expired(mtime, now, MISSING_TTL));
    }

    #[test]
    fn missing_is_expired_clock_skew_not_expired() {
        let now = SystemTime::now();
        let mtime = now + Duration::from_secs(1);
        assert!(!missing_is_expired(mtime, now, MISSING_TTL));
    }

    // -- fallback_color -------------------------------------------------

    #[test]
    fn fallback_color_is_deterministic_and_in_palette() {
        let a = fallback_color("someone@example.com");
        let b = fallback_color("someone@example.com");
        assert_eq!(a, b);
        assert!(FALLBACK_PALETTE.contains(&a));
    }

    #[test]
    fn fallback_color_varies_across_emails() {
        // Not a strict requirement, but sanity-check we're not returning a
        // constant regardless of input.
        let colors: std::collections::HashSet<_> = [
            "a@example.com",
            "b@example.com",
            "c@example.com",
            "d@example.com",
            "e@example.com",
        ]
        .iter()
        .map(|e| fallback_color(e))
        .collect();
        assert!(colors.len() > 1);
    }

    // -- circle_crop ------------------------------------------------------

    #[test]
    fn circle_crop_dimensions_and_masking() {
        let mut src = RgbaImage::new(64, 64);
        for px in src.pixels_mut() {
            *px = image::Rgba([255, 0, 0, 255]);
        }
        let out = circle_crop(&src, 20, 20);
        assert_eq!(out.dimensions(), (20, 20));

        // Corners fully transparent.
        assert_eq!(out.get_pixel(0, 0)[3], 0);
        assert_eq!(out.get_pixel(19, 0)[3], 0);
        assert_eq!(out.get_pixel(0, 19)[3], 0);
        assert_eq!(out.get_pixel(19, 19)[3], 0);

        // Center opaque and red-ish.
        let center = out.get_pixel(10, 10);
        assert_eq!(center[3], 255);
        assert!(center[0] > center[1] && center[0] > center[2]);
    }

    #[test]
    fn circle_crop_handles_tiny_dimensions() {
        // d = min(w,h).saturating_sub(2) == 0 when w or h <= 2.
        let src = RgbaImage::new(4, 4);
        let out = circle_crop(&src, 2, 2);
        assert_eq!(out.dimensions(), (2, 2));
        for px in out.pixels() {
            assert_eq!(px[3], 0);
        }
    }

    // -- fallback_disc ----------------------------------------------------

    #[test]
    fn fallback_disc_dimensions_and_masking() {
        let color = fallback_color("someone@example.com");
        let out = fallback_disc(color, 16, 16);
        assert_eq!(out.dimensions(), (16, 16));

        assert_eq!(out.get_pixel(0, 0)[3], 0);
        assert_eq!(out.get_pixel(15, 0)[3], 0);
        assert_eq!(out.get_pixel(0, 15)[3], 0);
        assert_eq!(out.get_pixel(15, 15)[3], 0);

        let center = out.get_pixel(8, 8);
        assert_eq!(center[3], 255);
        assert_eq!([center[0], center[1], center[2]], color);
    }
}
