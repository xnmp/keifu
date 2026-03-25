//! Files pane widget - shows changed files with staging support

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, StatefulWidget, Widget},
};

use crate::app::{App, FilesPaneItem, FocusedPanel};
use crate::git::{FileChangeKind, StageStatus};

use super::{render_placeholder_block, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

pub struct FilesPaneWidget {
    items: Vec<FilesPaneItem>,
    is_focused: bool,
    is_uncommitted: bool,
    is_loading: bool,
}

impl FilesPaneWidget {
    pub fn new(app: &App) -> Self {
        Self {
            items: app.files_pane_items(),
            is_focused: app.focused_panel == FocusedPanel::Files,
            is_uncommitted: app.is_uncommitted_selected(),
            is_loading: app.is_diff_loading(),
            has_cached_diff: app.cached_diff().is_some(),
        }
    }
}

/// Custom list state for files pane
pub struct FilesPaneState {
    pub selected: Option<usize>,
    pub offset: usize,
}

impl StatefulWidget for FilesPaneWidget {
    type State = FilesPaneState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
            render_placeholder_block(area, buf);
            return;
        }

        let border_color = if self.is_focused {
            Color::Green
        } else {
            Color::DarkGray
        };

        let title = if self.is_uncommitted {
            " Changed Files (s: stage/unstage) "
        } else {
            " Changed Files "
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        if self.items.is_empty() {
            if self.is_loading {
                let loading = Paragraph::new(Line::from(Span::styled(
                    "Loading...",
                    Style::default().fg(Color::DarkGray),
                )));
                loading.render(inner, buf);
            }
            return;
        }

        // Scroll management
        let visible_height = inner.height as usize;
        let selected = state.selected.unwrap_or(0);

        // Ensure selected item is visible
        if selected < state.offset {
            state.offset = selected;
        } else if selected >= state.offset + visible_height {
            state.offset = selected.saturating_sub(visible_height - 1);
        }

        // Render visible items
        for (i, item) in self.items.iter().skip(state.offset).enumerate() {
            if i >= visible_height {
                break;
            }
            let y = inner.y + i as u16;
            let is_selected = self.is_focused && state.selected == Some(state.offset + i);

            match item {
                FilesPaneItem::Header(text) => {
                    let style = Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD);
                    let line = Line::from(Span::styled(format!("  {}", text), style));
                    buf.set_line(inner.x, y, &line, inner.width);
                }
                FilesPaneItem::File(file) => {
                    let (indicator, color) = match file.kind {
                        FileChangeKind::Added => ("A", Color::Green),
                        FileChangeKind::Modified => ("M", Color::Yellow),
                        FileChangeKind::Deleted => ("D", Color::Red),
                        FileChangeKind::Renamed => ("R", Color::Cyan),
                        FileChangeKind::Copied => ("C", Color::Cyan),
                    };

                    let path_str = file.path.to_string_lossy().to_string();
                    let mut spans = vec![
                        Span::styled(format!(" {} ", indicator), Style::default().fg(color)),
                        Span::raw(&path_str),
                    ];

                    // Show stage status indicator for uncommitted files
                    if let Some(status) = file.stage_status {
                        match status {
                            StageStatus::Untracked => {
                                spans.push(Span::styled(
                                    " (untracked)",
                                    Style::default().fg(Color::DarkGray),
                                ));
                            }
                            _ => {}
                        }
                    }

                    if file.is_binary {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            "(binary)",
                            Style::default().fg(Color::DarkGray),
                        ));
                    } else if file.insertions > 0 || file.deletions > 0 {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            format!("+{}", file.insertions),
                            Style::default().fg(Color::Green),
                        ));
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            format!("-{}", file.deletions),
                            Style::default().fg(Color::Red),
                        ));
                    } else if self.is_loading {
                        spans.push(Span::styled(
                            " ...",
                            Style::default().fg(Color::DarkGray),
                        ));
                    }

                    let line = Line::from(spans);

                    if is_selected {
                        // Highlight selected row
                        let highlight_style = Style::default()
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD);
                        buf.set_style(
                            Rect::new(inner.x, y, inner.width, 1),
                            highlight_style,
                        );
                    }

                    buf.set_line(inner.x, y, &line, inner.width);
                }
            }
        }
    }
}
