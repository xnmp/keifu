//! Non-git workspace operations (filesystem, config file manipulation)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};

/// Add a pattern to .gitignore at the repository root.
/// Returns Ok(false) if the pattern already exists, Ok(true) if it was added.
pub fn add_to_gitignore(repo_path: &str, pattern: &str) -> Result<bool> {
    let gitignore_path = Path::new(repo_path).join(".gitignore");

    // Check if pattern already exists
    if gitignore_path.exists() {
        let contents = std::fs::read_to_string(&gitignore_path)
            .context("Failed to read .gitignore")?;
        if contents.lines().any(|line| line.trim() == pattern.trim()) {
            return Ok(false);
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore_path)
        .context("Failed to open .gitignore")?;

    // Ensure we start on a new line if file doesn't end with one
    if gitignore_path.exists() {
        let contents = std::fs::read_to_string(&gitignore_path)
            .context("Failed to read .gitignore")?;
        if !contents.is_empty() && !contents.ends_with('\n') {
            writeln!(file)?;
        }
    }

    writeln!(file, "{}", pattern).context("Failed to write to .gitignore")?;

    Ok(true)
}

/// Remove a pattern from .gitignore at the repository root.
/// Returns Ok(false) if the pattern was not found, Ok(true) if removed.
pub fn remove_from_gitignore(repo_path: &str, pattern: &str) -> Result<bool> {
    let gitignore_path = Path::new(repo_path).join(".gitignore");

    if !gitignore_path.exists() {
        return Ok(false);
    }

    let contents =
        fs::read_to_string(&gitignore_path).context("Failed to read .gitignore")?;

    let trimmed = pattern.trim();
    let filtered: Vec<&str> = contents
        .lines()
        .filter(|line| line.trim() != trimmed)
        .collect();

    if filtered.len() == contents.lines().count() {
        return Ok(false);
    }

    let mut new_contents = filtered.join("\n");
    if !new_contents.is_empty() {
        new_contents.push('\n');
    }

    fs::write(&gitignore_path, new_contents).context("Failed to write .gitignore")?;

    Ok(true)
}

/// Move a file or folder to `.archive/` at the repository root.
/// Creates the `.archive/` directory if it doesn't exist.
/// Preserves the relative path structure inside `.archive/`.
pub fn archive_path(repo_path: &str, relative_path: &str) -> Result<()> {
    let repo = Path::new(repo_path);
    let source = repo.join(relative_path);

    if !source.exists() {
        bail!("Path does not exist: {}", relative_path);
    }

    let dest = repo.join(".archive").join(relative_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .context("Failed to create .archive directory structure")?;
    }

    fs::rename(&source, &dest).context(format!(
        "Failed to move '{}' to '.archive/{}'",
        relative_path, relative_path
    ))?;

    Ok(())
}

/// Move a file or folder from `.archive/` back to its original location.
pub fn unarchive_path(repo_path: &str, relative_path: &str) -> Result<()> {
    let repo = Path::new(repo_path);
    let source = repo.join(".archive").join(relative_path);

    if !source.exists() {
        bail!(
            "Archived path does not exist: .archive/{}",
            relative_path
        );
    }

    let dest = repo.join(relative_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("Failed to create directory structure")?;
    }

    fs::rename(&source, &dest).context(format!(
        "Failed to move '.archive/{}' back to '{}'",
        relative_path, relative_path
    ))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn add_to_gitignore_creates_file_if_missing() {
        let dir = setup_temp_dir();
        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n");
    }

    #[test]
    fn add_to_gitignore_appends_to_existing() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "node_modules/\ntarget/\n");
    }

    #[test]
    fn add_to_gitignore_appends_newline_if_missing() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "node_modules/").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "node_modules/\ntarget/\n");
    }

    #[test]
    fn add_to_gitignore_skips_duplicate() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(!result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n");
    }

    #[test]
    fn add_to_gitignore_handles_empty_file() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "*.log").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "*.log\n");
    }

    #[test]
    fn archive_path_moves_file() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join("old.txt"), "content").unwrap();

        archive_path(dir.path().to_str().unwrap(), "old.txt").unwrap();

        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/old.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn archive_path_preserves_directory_structure() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join("src/utils")).unwrap();
        fs::write(dir.path().join("src/utils/helper.rs"), "fn help() {}").unwrap();

        archive_path(dir.path().to_str().unwrap(), "src/utils/helper.rs").unwrap();

        assert!(!dir.path().join("src/utils/helper.rs").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/src/utils/helper.rs")).unwrap(),
            "fn help() {}"
        );
    }

    #[test]
    fn archive_path_moves_folder() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join("src/old")).unwrap();
        fs::write(dir.path().join("src/old/a.rs"), "a").unwrap();
        fs::write(dir.path().join("src/old/b.rs"), "b").unwrap();

        archive_path(dir.path().to_str().unwrap(), "src/old").unwrap();

        assert!(!dir.path().join("src/old").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/src/old/a.rs")).unwrap(),
            "a"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/src/old/b.rs")).unwrap(),
            "b"
        );
    }

    #[test]
    fn archive_path_errors_on_missing_source() {
        let dir = setup_temp_dir();
        let result = archive_path(dir.path().to_str().unwrap(), "nonexistent.txt");
        assert!(result.is_err());
    }

    #[test]
    fn remove_from_gitignore_removes_matching_line() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "target/\nnode_modules/\n*.log\n").unwrap();

        let result = remove_from_gitignore(dir.path().to_str().unwrap(), "node_modules/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n*.log\n");
    }

    #[test]
    fn remove_from_gitignore_returns_false_if_not_found() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        let result = remove_from_gitignore(dir.path().to_str().unwrap(), "missing").unwrap();
        assert!(!result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n");
    }

    #[test]
    fn remove_from_gitignore_handles_missing_file() {
        let dir = setup_temp_dir();
        let result = remove_from_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(!result);
    }

    #[test]
    fn unarchive_path_moves_file_back() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join(".archive")).unwrap();
        fs::write(dir.path().join(".archive/old.txt"), "content").unwrap();

        unarchive_path(dir.path().to_str().unwrap(), "old.txt").unwrap();

        assert!(!dir.path().join(".archive/old.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("old.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn unarchive_path_preserves_directory_structure() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join(".archive/src/utils")).unwrap();
        fs::write(dir.path().join(".archive/src/utils/helper.rs"), "fn help() {}").unwrap();

        unarchive_path(dir.path().to_str().unwrap(), "src/utils/helper.rs").unwrap();

        assert!(!dir.path().join(".archive/src/utils/helper.rs").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("src/utils/helper.rs")).unwrap(),
            "fn help() {}"
        );
    }

    #[test]
    fn unarchive_path_errors_on_missing_source() {
        let dir = setup_temp_dir();
        let result = unarchive_path(dir.path().to_str().unwrap(), "nonexistent.txt");
        assert!(result.is_err());
    }
}
