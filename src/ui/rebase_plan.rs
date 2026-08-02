//! Interactive-rebase plan overlay.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};

use crate::git::short_hash;
use crate::rebase_plan::{RebaseAction, RebasePlan};

use super::theme::Theme;

pub struct RebasePlanWidget<'a> {
    plan: &'a RebasePlan,
    cursor: usize,
    theme: &'a Theme,
}

impl<'a> RebasePlanWidget<'a> {
    pub fn new(plan: &'a RebasePlan, cursor: usize, theme: &'a Theme) -> Self {
        Self {
            plan,
            cursor,
            theme,
        }
    }
}

impl Widget for RebasePlanWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = self.theme.popup_block(format!(
            " Interactive rebase onto {} ",
            short_hash(self.plan.base_oid)
        ));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }

        let rows = inner.height.saturating_sub(2) as usize;
        let offset = self.cursor.saturating_sub(rows.saturating_sub(1));
        for (visible, entry) in self.plan.entries.iter().skip(offset).take(rows).enumerate() {
            let index = offset + visible;
            let y = inner.y + visible as u16;
            let style = if index == self.cursor {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };
            let action_style = match entry.action {
                RebaseAction::Drop => style.fg(self.theme.file_deleted),
                RebaseAction::Squash | RebaseAction::Fixup => style.fg(self.theme.file_modified),
                RebaseAction::Reword => style.fg(self.theme.accent()),
                RebaseAction::Pick => style,
            };
            let line = Line::from(vec![
                Span::styled(format!("{:<7}", entry.action.label()), action_style),
                Span::styled(format!("{}  ", short_hash(entry.oid)), style),
                Span::styled(&entry.subject, style),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
        }

        if inner.height >= 2 {
            let footer_y = inner.y + inner.height - 1;
            let footer = Line::from(vec![
                Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" navigate  "),
                Span::styled("J/K", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" reorder  p/s/f/r/d action  Enter review  Esc cancel"),
            ]);
            buf.set_line(inner.x, footer_y, &footer, inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Oid;
    use ratatui::buffer::Buffer;

    #[test]
    fn rendered_plan_shows_range_actions_and_controls() {
        let oid = |n| Oid::from_bytes(&[n; 20]).unwrap();
        let mut second = crate::rebase_plan::PlanEntry::pick(oid(2), "second commit");
        second.action = RebaseAction::Squash;
        let plan = RebasePlan {
            base_oid: oid(9),
            source_branch_ref: "refs/heads/main".into(),
            source_head_oid: oid(2),
            entries: vec![
                second,
                crate::rebase_plan::PlanEntry::pick(oid(1), "first commit"),
            ],
        };
        let area = Rect::new(0, 0, 90, 8);
        let mut buffer = Buffer::empty(area);

        RebasePlanWidget::new(&plan, 0, &Theme::dark()).render(area, &mut buffer);

        let rendered = buffer
            .content
            .chunks(area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Interactive rebase onto 0909090"));
        assert!(rendered.contains("squash 0202020  second commit"));
        assert!(rendered.contains("pick   0101010  first commit"));
        assert!(rendered.contains("J/K reorder"));
        assert!(rendered.contains("Enter review"));
    }
}
