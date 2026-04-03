//! Files pane widget - shows changed files with staging support

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, StatefulWidget, Widget},
};

use crate::app::{App, FilesPaneItem, FocusedPanel};
use crate::git::StageStatus;

use super::{render_placeholder_block, theme::Theme, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

pub struct FilesPaneWidget<'a> {
    items: Vec<FilesPaneItem>,
    is_focused: bool,
    is_uncommitted: bool,
    is_loading: bool,
    theme: &'a Theme,
}

impl<'a> FilesPaneWidget<'a> {
    pub fn new(app: &App, theme: &'a Theme) -> Self {
        Self {
            items: app.files_pane_items(),
            is_focused: app.focused_panel == FocusedPanel::Files,
            is_uncommitted: app.is_uncommitted_selected(),
            is_loading: app.is_diff_loading(),
            theme,
        }
    }
}

/// Custom list state for files pane
pub struct FilesPaneState {
    pub selected: Option<usize>,
    pub offset: usize,
}

impl<'a> StatefulWidget for FilesPaneWidget<'a> {
    type State = FilesPaneState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
            render_placeholder_block(area, buf, self.theme);
            return;
        }

        let border_color = if self.is_focused {
            self.theme.border_focused
        } else {
            self.theme.border_unfocused
        };

        let title = if self.is_uncommitted {
            " Changed Files (s: stage/unstage) "
        } else {
            " Changed Files "
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .border_type(self.theme.border_type(self.is_focused));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        if self.items.is_empty() {
            if self.is_loading {
                let loading = Paragraph::new(Line::from(Span::styled(
                    "Loading...",
                    Style::default().fg(self.theme.text_muted),
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
                        .fg(self.theme.help_header)
                        .add_modifier(Modifier::BOLD);
                    let line = Line::from(Span::styled(format!("  {}", text), style));
                    buf.set_line(inner.x, y, &line, inner.width);
                }
                FilesPaneItem::File(file) => {
                    let (indicator, color) = self.theme.file_change_style(&file.kind);

                    let path_str = file.path.to_string_lossy().to_string();
                    let icon = super::file_icons::file_icon(&file.path);
                    let mut spans = vec![
                        Span::styled(format!(" {} ", indicator), Style::default().fg(color)),
                        Span::styled(format!("{} ", icon.icon), Style::default().fg(icon.color)),
                        Span::raw(&path_str),
                    ];

                    // Show stage status indicator for uncommitted files
                    if let Some(status) = file.stage_status {
                        match status {
                            StageStatus::Untracked => {
                                spans.push(Span::styled(
                                    " (untracked)",
                                    Style::default().fg(self.theme.text_muted),
                                ));
                            }
                            _ => {}
                        }
                    }

                    if file.is_binary {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            "(binary)",
                            Style::default().fg(self.theme.text_muted),
                        ));
                    } else if file.insertions > 0 || file.deletions > 0 {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            format!("+{}", file.insertions),
                            Style::default().fg(self.theme.file_added),
                        ));
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            format!("-{}", file.deletions),
                            Style::default().fg(self.theme.file_deleted),
                        ));
                    } else if self.is_loading {
                        spans.push(Span::styled(
                            " ...",
                            Style::default().fg(self.theme.text_muted),
                        ));
                    }

                    let line = Line::from(spans);

                    if is_selected {
                        // Highlight selected row
                        let highlight_style = self.theme.selection_style();
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
