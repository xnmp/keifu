//! Network operations: fetch/push, auto-refresh, fs-watcher polling.

use super::*;

impl App {
    pub fn update_fetch_status(&mut self) -> bool {
        let Some(result) = self.network.poll_fetch() else {
            return false;
        };
        match result {
            Ok(()) => {
                self.network.reset_timers();
                if matches!(self.mode, AppMode::FileDiff { .. }) {
                    self.pending_refresh = true;
                } else {
                    let prev_head = self.repo.head_oid();
                    let prev_branch_count = self.branches.len();
                    match self.refresh(true) {
                        Ok(()) => {
                            let new_head = self.repo.head_oid();
                            let new_branch_count = self.branches.len();
                            if prev_head != new_head || prev_branch_count != new_branch_count {
                                self.set_message("Fetched from origin");
                            }
                        }
                        Err(e) => self.show_error(format!("Refresh failed: {e}")),
                    }
                }
            }
            Err(e) => self.show_error(e),
        }
        true
    }

    pub fn update_push_status(&mut self) -> bool {
        let Some(result) = self.network.poll_push() else {
            return false;
        };
        match result {
            Ok(()) => {
                self.set_message("Pushed to origin");
                if let Err(e) = self.refresh(true) {
                    self.show_error(format!("Refresh failed: {e}"));
                }
            }
            Err(e) => self.show_error(e),
        }
        true
    }

    pub fn is_fetching(&self) -> bool {
        self.network.is_fetching()
    }

    pub fn is_pushing(&self) -> bool {
        self.network.is_pushing()
    }

    pub fn is_network_busy(&self) -> bool {
        self.network.is_busy()
    }

    pub fn check_auto_refresh(&mut self) -> bool {
        if matches!(self.mode, AppMode::FileDiff { .. }) {
            return false;
        }
        let events = self.network.check_auto_timers(&self.config.refresh);
        if events.should_auto_fetch {
            self.start_fetch(false, true);
            true
        } else if events.should_auto_refresh {
            if let Err(e) = self.refresh(false) {
                self.set_message(format!("Auto-refresh failed: {e}"));
            }
            self.network.mark_refreshed();
            true
        } else {
            false
        }
    }

    pub fn poll_fs_watcher(&mut self) -> bool {
        if let Some(pending) = self.pending_watcher.as_mut() {
            if let Some(watcher) = pending.try_take() {
                self.watcher = watcher;
                self.pending_watcher = None;
            }
        }
        if !self.config.refresh.auto_refresh {
            return false;
        }
        if matches!(self.mode, AppMode::FileDiff { .. }) {
            return false;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.poll() {
            crate::watcher::PollResult::Refresh => {
                if let Err(e) = self.refresh(false) {
                    self.set_message(format!("Watch refresh failed: {e}"));
                }
                self.network.mark_refreshed();
                true
            }
            crate::watcher::PollResult::Disconnected => {
                self.set_message("Filesystem watcher disconnected".to_string());
                self.watcher = None;
                true
            }
            crate::watcher::PollResult::Idle => false,
        }
    }

    pub(crate) fn start_fetch(&mut self, show_message: bool, silent: bool) {
        if let Some(msg) = self.network.start_fetch(&self.repo_path, show_message, silent) {
            self.set_message(msg);
        }
    }

    pub(crate) fn start_push(&mut self) {
        let msg = self.network.start_push(&self.repo_path);
        self.set_message(msg);
    }

    pub(crate) fn reset_timers(&mut self) {
        self.network.reset_timers();
    }
}
