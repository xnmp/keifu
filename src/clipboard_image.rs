//! Capture an image from the system clipboard into a short-lived local file.
//!
//! Keifu deliberately uses platform clipboard commands instead of linking a
//! clipboard crate (see `docs/architecture.md`). The captured file is deleted
//! when it is dropped unless ownership of its path is transferred to the issue
//! action runner.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(1);
static IMAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ClipboardImage {
    path: Option<PathBuf>,
}

impl ClipboardImage {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub fn path(&self) -> &Path {
        self.path.as_deref().expect("clipboard image path")
    }

    /// Make a private copy for the background upload worker. The original
    /// remains attached to the draft so a failed upload can be retried.
    pub fn copy_for_upload(&self) -> Result<PathBuf, String> {
        let source = self.path();
        let parent = source
            .parent()
            .ok_or_else(|| "Clipboard image has no parent directory".to_string())?;
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "Clipboard image has no file extension".to_string())?;
        let (path, mut destination) = create_image_file(parent, extension)
            .ok_or_else(|| "Could not create clipboard image upload file".to_string())?;
        let result = fs::File::open(source)
            .and_then(|mut source| std::io::copy(&mut source, &mut destination))
            .map_err(|error| error.to_string());
        if let Err(error) = result {
            drop(destination);
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        drop(destination);
        Ok(path)
    }
}

impl Drop for ClipboardImage {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

/// Capture the first supported clipboard image, returning `None` when the
/// clipboard contains text/files only or the platform clipboard tool is absent.
pub fn capture() -> Option<ClipboardImage> {
    let dir = std::env::temp_dir().join("keifu");
    fs::create_dir_all(&dir).ok()?;

    capture_wayland(&dir)
        .or_else(|| capture_x11(&dir))
        .or_else(|| capture_macos(&dir))
        .or_else(|| capture_windows(&dir))
}

fn capture_wayland(dir: &Path) -> Option<ClipboardImage> {
    let types = command_text("wl-paste", &["--list-types"])?;
    let mime = preferred_mime(&types)?;
    let bytes = command_bytes("wl-paste", &["--no-newline", "--type", mime])?;
    persist_valid_image(dir, mime, &bytes)
}

fn capture_x11(dir: &Path) -> Option<ClipboardImage> {
    let types = command_text("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"])?;
    let mime = preferred_mime(&types)?;
    let bytes = command_bytes("xclip", &["-selection", "clipboard", "-t", mime, "-o"])?;
    persist_valid_image(dir, mime, &bytes)
}

fn capture_macos(dir: &Path) -> Option<ClipboardImage> {
    let (png, placeholder) = create_image_file(dir, "png")?;
    drop(placeholder);
    let mut pngpaste = Command::new("pngpaste");
    pngpaste
        .arg(&png)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if command_status(pngpaste) && valid_file(&png) {
        return Some(ClipboardImage::new(png));
    }

    const SCRIPT: &str = r#"
on run argv
  try
    set imageData to the clipboard as «class PNGf»
    set outputFile to open for access (POSIX file (item 1 of argv)) with write permission
    set eof outputFile to 0
    write imageData to outputFile
    close access outputFile
    return "ok"
  on error
    try
      close access outputFile
    end try
    return "no-image"
  end try
end run
"#;
    let mut osascript = Command::new("osascript");
    osascript
        .args(["-e", SCRIPT])
        .arg(&png)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if command_status(osascript) && valid_file(&png) {
        Some(ClipboardImage::new(png))
    } else {
        let _ = fs::remove_file(&png);
        None
    }
}

fn capture_windows(dir: &Path) -> Option<ClipboardImage> {
    let (png, placeholder) = create_image_file(dir, "png")?;
    drop(placeholder);
    let escaped = png.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         if ([Windows.Forms.Clipboard]::ContainsImage()) {{ \
         [Windows.Forms.Clipboard]::GetImage().Save('{escaped}', \
         [Drawing.Imaging.ImageFormat]::Png); exit 0 }}; exit 1"
    );
    let mut powershell = Command::new("powershell.exe");
    powershell
        .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if command_status(powershell) && valid_file(&png) {
        Some(ClipboardImage::new(png))
    } else {
        let _ = fs::remove_file(&png);
        None
    }
}

fn preferred_mime(types: &str) -> Option<&'static str> {
    ["image/png", "image/jpeg", "image/gif"]
        .into_iter()
        .find(|mime| types.lines().any(|line| line.trim() == *mime))
}

fn command_text(program: &str, args: &[&str]) -> Option<String> {
    String::from_utf8(command_bytes(program, args)?).ok()
}

fn command_bytes(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
            .ok()
    });
    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    };
    let bytes = reader.join().ok()??;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    if !status.success() {
        return None;
    }
    Some(bytes)
}

fn command_status(mut command: Command) -> bool {
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn persist_valid_image(dir: &Path, mime: &str, bytes: &[u8]) -> Option<ClipboardImage> {
    let extension = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        _ => return None,
    };
    let (path, mut file) = create_image_file(dir, extension)?;
    if file.write_all(bytes).is_err() {
        let _ = fs::remove_file(path);
        return None;
    }
    drop(file);
    if valid_file(&path) {
        Some(ClipboardImage::new(path))
    } else {
        let _ = fs::remove_file(path);
        None
    }
}

fn create_image_file(dir: &Path, extension: &str) -> Option<(PathBuf, fs::File)> {
    for _ in 0..100 {
        let sequence = IMAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "clipboard-image-{}-{sequence}.{extension}",
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => {
                secure_permissions(&path);
                return Some((path, file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

fn valid_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return false;
    }
    secure_permissions(path);
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    matches!(
        image::guess_format(&bytes),
        Ok(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Gif)
    )
}

#[cfg(unix)]
fn secure_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn secure_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_preference_is_stable_and_ignores_text() {
        assert_eq!(
            preferred_mime("text/plain\nimage/jpeg\nimage/png\n"),
            Some("image/png")
        );
        assert_eq!(preferred_mime("text/plain\ntext/html\n"), None);
    }

    #[test]
    fn invalid_or_oversized_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let invalid = dir.path().join("invalid.png");
        fs::write(&invalid, b"not an image").unwrap();
        assert!(!valid_file(&invalid));

        let oversized = dir.path().join("huge.png");
        let file = fs::File::create(&oversized).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        assert!(!valid_file(&oversized));
    }

    #[test]
    fn capture_paths_are_unique_private_files() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = create_image_file(dir.path(), "png").unwrap();
        let (second, _) = create_image_file(dir.path(), "png").unwrap();
        assert_ne!(first, second);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(first).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn upload_copy_does_not_consume_the_draft_capture() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut file) = create_image_file(dir.path(), "png").unwrap();
        file.write_all(b"private screenshot").unwrap();
        drop(file);
        let image = ClipboardImage::new(path.clone());

        let upload = image.copy_for_upload().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"private screenshot");
        assert_eq!(fs::read(&upload).unwrap(), b"private screenshot");

        drop(image);
        assert!(!path.exists(), "draft capture cleans up when dropped");
        assert!(upload.exists(), "worker copy has independent ownership");
        fs::remove_file(upload).unwrap();
    }
}
