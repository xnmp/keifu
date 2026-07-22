//! Cross-session cache of the merged-branch classification result.
//!
//! Classifying merged branches is O(branches × ancestry + bounded patch-id
//! scans + three-way merges) — over a second on branchy repos, and it grew more
//! expensive once remote branches became eligible (#100) and a second trunk tip
//! was added (#103). With `hide_merged_branches` on, that cost used to be paid
//! *synchronously* before the first frame (to avoid flashing branches that would
//! immediately vanish), so startup blocked "forever" (#104).
//!
//! This cache lets startup serve the *last* result instantly and revalidate in
//! the background:
//!
//!  - The result is stored under a **signature** — the fingerprint of the exact
//!    classification inputs ([`crate::merged_branch_fetch::ClassifyInput::signature`]:
//!    all branch (name, tip) pairs, the base name + tip, and the gh-merged set).
//!  - At startup, if the signature still matches the live inputs, the cached
//!    result is correct and is used synchronously (instant, no flash).
//!  - If it does not match (branches moved, a new gh signal, or no cache), the
//!    stale result — or empty — paints the first frame *without blocking*, and
//!    the async classifier reconciles a moment later. The brief flash of a few
//!    soon-to-hide branches is the accepted tradeoff over a frozen startup.
//!
//! Purely a persistence concern: [`crate::git::merged`] stays a pure classifier,
//! and this module never classifies — it only reads and writes the result the
//! app layer produces.
//!
//! Storage: one JSON file per repository under the same config directory as
//! `state.toml` (`~/.config/keifu/merged_cache/<repo-hash>.json`), keyed by a
//! hash of the repository path. A corrupt, missing, or version-mismatched file
//! is silently ignored (returns `None`) — the cache is an optimization, never a
//! source of truth, so a bad entry degrades to the async path, never a crash.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use git2::Oid;
use serde::{Deserialize, Serialize};

/// On-disk format version. Bump when the serialized shape changes so old entries
/// are ignored rather than mis-parsed.
const CACHE_VERSION: u32 = 1;

/// A cached classification result plus the signature of the inputs that produced
/// it. Public fields are the in-memory form (sets/maps + real `Oid`s); the
/// on-disk form ([`OnDisk`]) uses sorted vectors and hex strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedCache {
    /// Fingerprint of the classification inputs (see
    /// [`crate::merged_branch_fetch::ClassifyInput::signature`]). The cached
    /// result is trusted only when this equals the live inputs' signature.
    pub signature: u64,
    /// The gh-merged head-branch set that was part of those inputs. Kept so a
    /// startup — which cannot observe the live gh set before its first fetch —
    /// can reconstruct the input identity from what the cache recorded.
    pub gh_merged: HashSet<String>,
    /// Names of branches classified as merged.
    pub merged: HashSet<String>,
    /// `branch name → squash landing commit` for the squash-merged subset.
    pub squash_targets: HashMap<String, Oid>,
}

/// Serialized shape. Vectors are sorted on write for a stable, diff-friendly
/// file; `Oid`s are stored as hex strings.
#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    signature: u64,
    #[serde(default)]
    gh_merged: Vec<String>,
    #[serde(default)]
    merged: Vec<String>,
    /// `(branch name, squash-commit hex)` pairs.
    #[serde(default)]
    squash_targets: Vec<(String, String)>,
}

impl MergedCache {
    /// The cache directory: `<config>/keifu/merged_cache`. `None` when the
    /// platform has no config dir. Does not create it.
    fn cache_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("keifu").join("merged_cache"))
    }

    /// The per-repo cache file path. Keyed by a stable hash of the repository
    /// path (canonicalized when possible so equivalent paths collapse), mirroring
    /// the avatar cache's hash-named files.
    fn cache_path(repo_path: &str) -> Option<PathBuf> {
        Some(Self::cache_dir()?.join(format!("{}.json", repo_key(repo_path))))
    }

    /// Load the cached result for `repo_path`, or `None` when there is none, it is
    /// unreadable, malformed, or written by a different version. Never fails: a
    /// bad cache is indistinguishable from a cold one.
    pub fn load(repo_path: &str) -> Option<Self> {
        Self::load_from(&Self::cache_path(repo_path)?)
    }

    /// Best-effort persist. IO / serialization errors are swallowed: failing to
    /// warm the cache must never disturb a working session.
    pub fn save(&self, repo_path: &str) {
        let Some(path) = Self::cache_path(repo_path) else {
            return;
        };
        self.save_to(&path);
    }

    /// [`Self::load`] against an explicit file path (the shared core; testable
    /// without touching the real config dir).
    fn load_from(path: &std::path::Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let disk: OnDisk = serde_json::from_str(&content).ok()?;
        if disk.version != CACHE_VERSION {
            return None;
        }
        // Drop any target whose oid no longer parses (corruption / a truncated
        // write) rather than discarding the whole entry — the merged set is
        // still usable, and a missing squash target only drops a link line.
        let squash_targets = disk
            .squash_targets
            .into_iter()
            .filter_map(|(name, hex)| Oid::from_str(&hex).ok().map(|oid| (name, oid)))
            .collect();
        Some(Self {
            signature: disk.signature,
            gh_merged: disk.gh_merged.into_iter().collect(),
            merged: disk.merged.into_iter().collect(),
            squash_targets,
        })
    }

    /// [`Self::save`] to an explicit file path (the shared core). Creates the
    /// parent directory; swallows IO / serialization errors.
    fn save_to(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut gh_merged: Vec<String> = self.gh_merged.iter().cloned().collect();
        gh_merged.sort();
        let mut merged: Vec<String> = self.merged.iter().cloned().collect();
        merged.sort();
        let mut squash_targets: Vec<(String, String)> = self
            .squash_targets
            .iter()
            .map(|(name, oid)| (name.clone(), oid.to_string()))
            .collect();
        squash_targets.sort();
        let disk = OnDisk {
            version: CACHE_VERSION,
            signature: self.signature,
            gh_merged,
            merged,
            squash_targets,
        };
        if let Ok(json) = serde_json::to_string(&disk) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// A stable, filesystem-safe key for a repository path: 16 hex digits of a hash
/// of the canonicalized path (falling back to the raw string when canonicalize
/// fails, e.g. a path that no longer exists in a test).
fn repo_key(repo_path: &str) -> String {
    let canonical = std::fs::canonicalize(repo_path)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| repo_path.to_string());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MergedCache {
        let oid = Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
        MergedCache {
            signature: 0xdead_beef_1234_5678,
            gh_merged: ["feat/x".to_string()].into_iter().collect(),
            merged: ["feat/x".to_string(), "fix/y".to_string()]
                .into_iter()
                .collect(),
            squash_targets: [("feat/x".to_string(), oid)].into_iter().collect(),
        }
    }

    #[test]
    fn save_then_load_round_trips_every_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("entry.json");
        assert!(MergedCache::load_from(&path).is_none(), "cold cache is a miss");
        let original = sample();
        original.save_to(&path);
        let loaded = MergedCache::load_from(&path).expect("saved entry loads back");
        assert_eq!(loaded, original);
    }

    #[test]
    fn distinct_repo_paths_key_to_distinct_files() {
        // Per-repo scoping: two paths must not collide on one cache file.
        assert_ne!(repo_key("/repo/one"), repo_key("/repo/two"));
        // And the key is stable for the same input.
        assert_eq!(repo_key("/repo/one"), repo_key("/repo/one"));
    }

    #[test]
    fn version_mismatch_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.json");
        // A future/foreign version must be treated as absent.
        std::fs::write(
            &path,
            r#"{"version":999,"signature":1,"gh_merged":[],"merged":["x"],"squash_targets":[]}"#,
        )
        .unwrap();
        assert!(MergedCache::load_from(&path).is_none(), "wrong version → miss");
    }

    #[test]
    fn corrupt_json_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(&path, "{ not valid json ]").unwrap();
        assert!(MergedCache::load_from(&path).is_none(), "corrupt file → miss");
    }

    #[test]
    fn missing_file_is_a_miss() {
        assert!(
            MergedCache::load_from(std::path::Path::new("/no/such/cache.json")).is_none(),
            "absent file → miss, not a panic"
        );
    }

    #[test]
    fn bad_oid_target_is_dropped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("partial.json");
        // One valid target, one un-parseable oid → keep the merged set, drop the
        // bad target only.
        std::fs::write(
            &path,
            r#"{"version":1,"signature":7,"gh_merged":[],"merged":["a","b"],
               "squash_targets":[["a","0123456789abcdef0123456789abcdef01234567"],["b","zzzz"]]}"#,
        )
        .unwrap();
        let loaded = MergedCache::load_from(&path).expect("entry with a bad target still loads");
        assert_eq!(loaded.merged.len(), 2);
        assert_eq!(loaded.squash_targets.len(), 1, "only the valid target survives");
        assert!(loaded.squash_targets.contains_key("a"));
    }

    #[test]
    fn public_save_load_resolve_a_per_repo_path() {
        // The public entry points must round-trip too (exercises `cache_path`).
        // Uses a real temp git-repo-shaped dir so `canonicalize` succeeds.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_str().unwrap();
        // Only run when the platform has a config dir (always true in CI/dev).
        if MergedCache::cache_path(repo).is_none() {
            return;
        }
        // Clean any stale entry from a previous run of this exact temp path
        // (tempdir names are unique, so this is belt-and-suspenders).
        let original = sample();
        original.save(repo);
        let loaded = MergedCache::load(repo).expect("public round-trip");
        assert_eq!(loaded, original);
        // Clean up the entry we wrote into the real config dir.
        if let Some(p) = MergedCache::cache_path(repo) {
            let _ = std::fs::remove_file(p);
        }
    }
}
