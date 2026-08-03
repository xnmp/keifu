//! Commit info structure

use chrono::{DateTime, Local, TimeZone};
use git2::Oid;

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub oid: Oid,
    pub short_id: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: DateTime<Local>,
    pub message: String,
    pub full_message: String,
    pub parent_oids: Vec<Oid>,
}

/// The 7-character abbreviated form of an OID, as shown throughout the UI
/// (graph rows, toasts, undo prompts, compare mode).
pub fn short_hash(oid: Oid) -> String {
    oid.to_string()[..7].to_string()
}

impl CommitInfo {
    pub fn from_git2_commit(commit: &git2::Commit) -> Self {
        let oid = commit.id();
        let short_id = short_hash(oid);

        let author = commit.author();
        let author_name = author.name().unwrap_or("Unknown").to_string();
        let author_email = author.email().unwrap_or("").to_string();

        let time = commit.time();
        let timestamp = Local.timestamp_opt(time.seconds(), 0).unwrap();

        let full_message = commit.message().unwrap_or("").to_string();
        let message = full_message.lines().next().unwrap_or("").to_string();

        let parent_oids: Vec<Oid> = commit.parent_ids().collect();

        Self {
            oid,
            short_id,
            author_name,
            author_email,
            timestamp,
            message,
            full_message,
            parent_oids,
        }
    }
}
