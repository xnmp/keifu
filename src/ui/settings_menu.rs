//! Settings menu popup widget (Ctrl+,).
//!
//! Stateless: it renders a snapshot of each setting's value (ordered to match
//! `settings::descriptors()`) plus the current cursor and the (optional) numeric
//! edit buffer. All settings logic lives in `crate::settings`;
//! this widget only draws. Rows are grouped under dim section headers; the
//! selected row is highlighted, and the right column shows each setting's value
//! (or the live edit buffer when a numeric value is being typed).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;
use crate::settings::{descriptors, format_value, SettingGroup, SettingValue};

/// One rendered line: a non-selectable section header, or a setting row keyed by
/// its descriptor index.
enum Row {
    Header(SettingGroup),
    Item(usize),
}

pub struct SettingsMenuWidget<'a> {
    /// Current value of each setting, indexed to match `descriptors()`.
    values: &'a [SettingValue],
    /// Index into `settings::descriptors()` of the highlighted setting.
    selected: usize,
    /// When the user is typing a numeric value, the in-progress buffer.
    editing: Option<&'a str>,
    theme: &'a Theme,
}

impl<'a> SettingsMenuWidget<'a> {
    pub fn new(
        values: &'a [SettingValue],
        selected: usize,
        editing: Option<&'a str>,
        theme: &'a Theme,
    ) -> Self {
        Self {
            values,
            selected,
            editing,
            theme,
        }
    }

    /// Build the display rows (headers interleaved before each group's first
    /// setting). Order follows `descriptors()`, which is grouped contiguously.
    fn rows(count: usize, groups: &[SettingGroup]) -> Vec<Row> {
        let mut rows = Vec::with_capacity(count + SettingGroup::ALL.len());
        let mut current: Option<SettingGroup> = None;
        for (i, g) in groups.iter().enumerate().take(count) {
            if current != Some(*g) {
                rows.push(Row::Header(*g));
                current = Some(*g);
            }
            rows.push(Row::Item(i));
        }
        rows
    }
}

impl<'a> Widget for SettingsMenuWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self.theme.popup_block(" Settings ");
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.width < 4 || inner.height < 2 {
            return;
        }
        let width = inner.width as usize;

        let ds = descriptors();
        let groups: Vec<SettingGroup> = ds.iter().map(|d| d.group).collect();
        let rows = Self::rows(ds.len(), &groups);

        // Display-row index of the selected setting, for scroll bookkeeping.
        let selected_row = rows
            .iter()
            .position(|r| matches!(r, Row::Item(i) if *i == self.selected))
            .unwrap_or(0);

        // Reserve the last inner line for the footer.
        let list_height = inner.height.saturating_sub(1) as usize;
        let scroll = if selected_row >= list_height {
            selected_row - list_height + 1
        } else {
            0
        };

        let dim = Style::default()
            .fg(self.theme.text_muted)
            .add_modifier(Modifier::DIM);
        let header_style = Style::default()
            .fg(self.theme.help_key)
            .add_modifier(Modifier::BOLD);

        for (vis, row) in rows.iter().skip(scroll).take(list_height).enumerate() {
            let y = inner.y + vis as u16;
            match row {
                Row::Header(g) => {
                    let line = Line::from(Span::styled(g.label(), header_style));
                    buf.set_line(inner.x, y, &line, inner.width);
                }
                Row::Item(idx) => {
                    let d = &ds[*idx];
                    let is_selected = *idx == self.selected;
                    let base = if is_selected {
                        self.theme.list_selection_style()
                    } else {
                        Style::default().fg(self.theme.text_primary)
                    };

                    // Value column: the live edit buffer (with cursor) when this
                    // row is being typed into, else the formatted value.
                    let value = match (is_selected, self.editing) {
                        (true, Some(buf_str)) => format!("{buf_str}▏"),
                        _ => format_value(d.kind, self.values[*idx]),
                    };
                    let note = d.note.map(|n| format!(" ({n})")).unwrap_or_default();

                    let prefix = if is_selected { "▸ " } else { "  " };
                    let label = format!("{prefix}{}", d.label);

                    // Right-align: [label] … [gap] [value][note]
                    let value_w = value.width() + note.width();
                    let label_w = label.width();
                    let gap = width.saturating_sub(label_w + value_w).max(1);

                    let value_style = if is_selected {
                        base
                    } else {
                        Style::default().fg(self.theme.text_primary)
                    };
                    let spans = vec![
                        Span::styled(label, base),
                        Span::styled(" ".repeat(gap), base),
                        Span::styled(value, value_style.add_modifier(Modifier::BOLD)),
                        Span::styled(note, dim),
                    ];
                    buf.set_line(inner.x, y, &Line::from(spans), inner.width);
                    if is_selected {
                        buf.set_style(Rect::new(inner.x, y, inner.width, 1), base);
                    }
                }
            }
        }

        // Footer.
        let footer_y = inner.y + inner.height.saturating_sub(1);
        let footer = Line::from(Span::styled(
            "j/k move  Space toggle  0-9 set num  Esc close",
            dim,
        ));
        buf.set_line(inner.x, footer_y, &footer, inner.width);
    }
}
