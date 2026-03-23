//! Files pane state and behavior.

use ratatui::widgets::ListState;

use crate::app::AppMode;

/// Holds selection state for the "Changed Files" pane.
#[derive(Debug, Clone, Default)]
pub struct FilesPane {
    pub list_state: ListState,
    pub selected_file_index: Option<usize>,
}

impl FilesPane {
    pub fn enter(&mut self, mode: &mut AppMode, file_count: usize) -> Option<String> {
        let len = file_count;
        if len == 0 {
            return Some("No files changed".to_string());
        }

        let selected = self.selected_file_index.unwrap_or(0).min(len.saturating_sub(1));
        self.list_state.select(Some(selected));
        *mode = AppMode::Files;
        None
    }

    pub fn exit(&mut self, mode: &mut AppMode) {
        *mode = AppMode::Normal;
        self.list_state.select(None);
    }

    pub fn move_selection(&mut self, file_count: usize, delta: isize) {
        let len = file_count;
        if len == 0 {
            self.list_state.select(None);
            return;
        }

        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, (len - 1) as isize) as usize;
        self.list_state.select(Some(next));
    }

    pub fn select_current(&mut self) {
        self.selected_file_index = self.list_state.selected();
    }
}
