//! Centralized theme definitions for UI rendering

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

/// All color and style definitions used by the TUI.
///
/// Colors are grouped semantically so that dark/light variants
/// can swap entire palettes without touching widget code.
pub struct Theme {
    // Panel borders
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub border_filter_active: Color,

    // Selection (graph list highlight, file list highlight)
    pub selection_bg: Color,
    pub selection_modifier: Modifier,

    // List selection (popups: branch filter, search dropdown, commit menu)
    pub list_selection_fg: Color,
    pub list_selection_bg: Color,

    // Text
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,

    // File change indicators
    pub file_added: Color,
    pub file_modified: Color,
    pub file_deleted: Color,
    pub file_renamed: Color,

    // Diff view
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    pub diff_add_emph_bg: Color,
    pub diff_del_emph_bg: Color,
    pub diff_hunk_fg: Color,
    pub diff_hunk_bg: Color,

    // Git metadata (graph right-side columns)
    pub hash_color: Color,
    pub author_color: Color,
    pub date_color: Color,

    // Status bar
    pub status_repo_fg: Color,
    pub status_repo_bg: Color,
    pub status_branch_fg: Color,
    pub status_branch_bg: Color,
    pub status_detached_fg: Color,
    pub status_detached_bg: Color,
    pub status_key_fg: Color,
    pub status_key_bg: Color,
    pub status_mode_fg: Color,
    pub status_mode_bg: Color,
    pub status_busy_bg: Color,
    pub status_success_bg: Color,
    pub status_error_fg: Color,
    pub status_error_bg: Color,

    // Dialogs/popups
    pub popup_bg: Color,
    pub popup_border: Color,
    pub input_border: Color,
    pub confirm_border: Color,
    pub button_yes: Color,
    pub button_no: Color,

    // Search
    pub search_match: Color,
    pub search_cursor: Color,

    // Branch labels in graph
    pub branch_head: Color,
    // Tag labels in graph (distinct from branch labels)
    pub tag_label: Color,
    // Open-PR badge in graph (distinct from branch/tag labels). `pr_badge` is
    // the default (no CI); the `pr_ci_*` set colors the badge by check status.
    pub pr_badge: Color,
    pub pr_ci_pass: Color,
    pub pr_ci_pending: Color,
    pub pr_ci_fail: Color,
    // HEAD marker gold (pixel star fill + Unicode ◉ fallback), matching the ⭐.
    pub head_star: Color,

    // Editor selection
    pub editor_selection_fg: Color,
    pub editor_selection_bg: Color,

    // Help popup
    pub help_key: Color,
    pub help_header: Color,

    // Syntect theme name for file diff view
    pub syntect_theme: &'static str,

    // Graph lane colors (indexed by color_index from graph layout)
    pub lane_colors: [Color; 11],
    pub uncommitted_color: Color,

    // Whether syntax highlighting should use dark ANSI colors (for light backgrounds)
    pub syntax_use_dark_colors: bool,
}

impl Theme {
    /// Dark theme (current default colors).
    pub fn dark() -> Self {
        Self {
            // Panel borders
            border_focused: Color::Green,
            // Fallback when the terminal background can't be detected;
            // `adapt_to_background` derives a visible value from the real bg.
            border_unfocused: Color::DarkGray,
            border_filter_active: Color::Yellow,

            // Selection
            selection_bg: Color::DarkGray,
            selection_modifier: Modifier::BOLD,

            // List selection (popups)
            list_selection_fg: Color::Black,
            list_selection_bg: Color::Cyan,

            // Text
            text_primary: Color::White,
            text_secondary: Color::DarkGray,
            text_muted: Color::DarkGray,

            // File change indicators
            file_added: Color::Green,
            file_modified: Color::Yellow,
            file_deleted: Color::Red,
            file_renamed: Color::Cyan,

            // Diff view
            diff_add_bg: Color::Rgb(0, 50, 0),
            diff_del_bg: Color::Rgb(65, 0, 0),
            diff_add_emph_bg: Color::Rgb(0, 90, 0),
            diff_del_emph_bg: Color::Rgb(110, 0, 0),
            diff_hunk_fg: Color::LightCyan,
            diff_hunk_bg: Color::Rgb(30, 40, 55),

            // Git metadata — secondary columns that recede below the graph and
            // message. Calm muted-gray fallbacks (author brightest, hash most
            // muted); `adapt_to_background` derives the real tinted ladder.
            hash_color: Color::DarkGray,
            author_color: Color::Gray,
            date_color: Color::DarkGray,

            // Status bar
            status_repo_fg: Color::Black,
            status_repo_bg: Color::Magenta,
            status_branch_fg: Color::Black,
            status_branch_bg: Color::Green,
            status_detached_fg: Color::White,
            status_detached_bg: Color::Red,
            status_key_fg: Color::Black,
            status_key_bg: Color::Cyan,
            status_mode_fg: Color::Black,
            status_mode_bg: Color::Yellow,
            status_busy_bg: Color::Yellow,
            status_success_bg: Color::Cyan,
            status_error_fg: Color::White,
            status_error_bg: Color::Red,

            // Dialogs/popups
            popup_bg: Color::Black,
            popup_border: Color::Cyan,
            input_border: Color::Cyan,
            confirm_border: Color::Yellow,
            button_yes: Color::Green,
            button_no: Color::Red,

            // Search
            search_match: Color::Yellow,
            search_cursor: Color::Cyan,

            // Branch labels — bright ANSI green follows the terminal palette
            // while staying legible for the checked-out HEAD label and node.
            branch_head: Color::LightGreen,
            // Tag labels — gold, the git convention for tag refs.
            tag_label: Color::LightYellow,
            // Open-PR badge — GitHub link blue, distinct from green/gold labels.
            pr_badge: Color::Rgb(88, 166, 255),
            pr_ci_pass: Color::Rgb(63, 185, 80),
            pr_ci_pending: Color::Rgb(210, 153, 34),
            pr_ci_fail: Color::Rgb(248, 81, 73),
            head_star: Color::Rgb(255, 200, 50),

            // Editor selection
            editor_selection_fg: Color::White,
            editor_selection_bg: Color::Blue,

            // Help popup
            help_key: Color::Cyan,
            help_header: Color::Yellow,

            // Syntect theme
            syntect_theme: "base16-eighties.dark",

            // Graph lanes: bright colors for dark backgrounds
            lane_colors: [
                Color::Cyan,
                Color::Green,
                Color::Magenta,
                Color::Yellow,
                Color::Red,
                Color::LightCyan,
                Color::LightGreen,
                Color::LightMagenta,
                Color::LightYellow,
                Color::LightBlue, // main branch
                Color::LightRed,
            ],
            uncommitted_color: Color::DarkGray,
            syntax_use_dark_colors: false,
        }
    }

    /// Light theme for use on light terminal backgrounds.
    pub fn light() -> Self {
        Self {
            // Panel borders
            border_focused: Color::Blue,
            border_unfocused: Color::DarkGray,
            border_filter_active: Color::Yellow,

            // Selection
            selection_bg: Color::LightBlue,
            selection_modifier: Modifier::BOLD,

            // List selection (popups)
            list_selection_fg: Color::White,
            list_selection_bg: Color::Blue,

            // Text
            text_primary: Color::Black,
            text_secondary: Color::DarkGray,
            text_muted: Color::Gray,

            // File change indicators
            file_added: Color::Green,
            file_modified: Color::Yellow,
            file_deleted: Color::Red,
            file_renamed: Color::Cyan,

            // Diff view
            diff_add_bg: Color::Rgb(200, 255, 200),
            diff_del_bg: Color::Rgb(255, 200, 200),
            diff_add_emph_bg: Color::Rgb(150, 240, 150),
            diff_del_emph_bg: Color::Rgb(240, 150, 150),
            diff_hunk_fg: Color::Blue,
            diff_hunk_bg: Color::Rgb(220, 230, 245),

            // Git metadata — secondary columns, muted to recede (author darkest
            // so it reads first, hash lightest/most muted). `adapt_to_background`
            // derives the real tinted ladder from the terminal bg.
            hash_color: Color::Gray,
            author_color: Color::DarkGray,
            date_color: Color::Gray,

            // Status bar
            status_repo_fg: Color::White,
            status_repo_bg: Color::Magenta,
            status_branch_fg: Color::White,
            status_branch_bg: Color::Green,
            status_detached_fg: Color::White,
            status_detached_bg: Color::Red,
            status_key_fg: Color::White,
            status_key_bg: Color::Blue,
            status_mode_fg: Color::Black,
            status_mode_bg: Color::Yellow,
            status_busy_bg: Color::Yellow,
            status_success_bg: Color::Blue,
            status_error_fg: Color::White,
            status_error_bg: Color::Red,

            // Dialogs/popups
            popup_bg: Color::White,
            popup_border: Color::Blue,
            input_border: Color::Blue,
            confirm_border: Color::Yellow,
            button_yes: Color::Green,
            button_no: Color::Red,

            // Search
            search_match: Color::Blue,
            search_cursor: Color::Blue,

            // Branch labels
            branch_head: Color::Green,
            // Tag labels — dark gold, legible on light backgrounds.
            tag_label: Color::Rgb(150, 110, 0),
            // Open-PR badge — GitHub link blue, darkened for light backgrounds.
            pr_badge: Color::Rgb(9, 105, 218),
            pr_ci_pass: Color::Rgb(26, 127, 55),
            pr_ci_pending: Color::Rgb(154, 103, 0),
            pr_ci_fail: Color::Rgb(207, 34, 46),
            head_star: Color::Rgb(184, 134, 11),

            // Editor selection
            editor_selection_fg: Color::White,
            editor_selection_bg: Color::Blue,

            // Help popup
            help_key: Color::Blue,
            help_header: Color::DarkGray,

            // Syntect theme
            syntect_theme: "base16-ocean.light",

            // Graph lanes: darker colors for light backgrounds
            lane_colors: [
                Color::DarkGray,
                Color::Rgb(0, 130, 0),    // dark green
                Color::Rgb(150, 0, 150),  // dark magenta
                Color::Rgb(160, 120, 0),  // dark yellow/gold
                Color::Red,
                Color::Rgb(0, 140, 140),  // dark cyan
                Color::Rgb(0, 100, 0),    // darker green
                Color::Rgb(130, 0, 130),  // darker magenta
                Color::Rgb(140, 100, 0),  // darker gold
                Color::Blue,              // main branch
                Color::Rgb(180, 0, 0),    // dark red
            ],
            uncommitted_color: Color::Gray,
            syntax_use_dark_colors: true,
        }
    }

    /// Derive background-relative structural colors from the terminal's actual
    /// background color, so inactive borders, dates and muted text keep a
    /// consistent, visible contrast on *any* terminal theme (light or dark,
    /// custom palettes included) instead of relying on the dim `DarkGray` ANSI
    /// slot — which many themes render nearly invisible.
    ///
    /// Each color is a blend from the real background toward its contrast
    /// (white on dark backgrounds, black on light ones), so the result is
    /// tinted by the terminal theme while guaranteeing legibility.
    pub fn adapt_to_background(mut self, bg: (u8, u8, u8)) -> Self {
        let contrast = if luma(bg) < 0.5 {
            (255, 255, 255)
        } else {
            (0, 0, 0)
        };
        // Selected row band: a clear step off the background so the highlighted
        // graph/file row stands out, while keeping foreground text readable.
        self.selection_bg = mix(bg, contrast, 0.32);
        // Inactive borders: present but clearly dimmer than the focused accent.
        self.border_unfocused = mix(bg, contrast, 0.30);
        // Muted text and the uncommitted node: low-key but readable.
        self.text_secondary = mix(bg, contrast, 0.45);
        self.text_muted = mix(bg, contrast, 0.40);
        self.uncommitted_color = mix(bg, contrast, 0.40);
        // Metadata columns: one muted-gray family tinted by the terminal bg, so
        // they recede below the graph and message and never clash with lane
        // colors. A subtle brightness ladder keeps the columns distinguishable —
        // author reads first, hash (least-read) is the most muted.
        self.author_color = mix(bg, contrast, 0.55);
        self.date_color = mix(bg, contrast, 0.47);
        self.hash_color = mix(bg, contrast, 0.38);
        self
    }

    // -- convenience style constructors --

    pub fn selection_style(&self) -> Style {
        Style::default()
            .bg(self.selection_bg)
            .add_modifier(self.selection_modifier)
    }

    pub fn list_selection_style(&self) -> Style {
        Style::default()
            .fg(self.list_selection_fg)
            .bg(self.list_selection_bg)
            .add_modifier(Modifier::BOLD)
    }

    /// The single app-wide accent color: focused panel borders and titles,
    /// status-bar keys and the repo chip all key off this one hue so "active"
    /// reads consistently everywhere.
    pub fn accent(&self) -> Color {
        self.border_focused
    }

    /// Panel title emphasis: the accent (bold) when focused, muted when not, so
    /// the focused panel's border *and* title stand out via the one accent.
    pub fn title_style(&self, focused: bool) -> Style {
        if focused {
            Style::default()
                .fg(self.accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.text_muted)
        }
    }

    pub fn focused_border_style(&self) -> Style {
        Style::default().fg(self.border_focused)
    }

    pub fn unfocused_border_style(&self) -> Style {
        Style::default().fg(self.border_unfocused)
    }

    pub fn border_style(&self, focused: bool) -> Style {
        if focused {
            self.focused_border_style()
        } else {
            self.unfocused_border_style()
        }
    }

    /// Rounded borders on every panel, focused or not — focus is signalled by
    /// border/title *color* (the accent), not by changing the border shape, so
    /// the frame stays visually stable as focus moves.
    pub fn border_type(&self) -> BorderType {
        BorderType::Rounded
    }

    /// Get a lane color by index (replaces graph::colors::get_color_by_index).
    pub fn lane_color(&self, color_index: usize) -> Color {
        if color_index == usize::MAX {
            return self.uncommitted_color;
        }
        self.lane_colors[color_index % self.lane_colors.len()]
    }

    pub fn file_change_style(&self, kind: &crate::git::FileChangeKind) -> (& 'static str, Color) {
        use crate::git::FileChangeKind;
        match kind {
            FileChangeKind::Added => ("A", self.file_added),
            FileChangeKind::Modified => ("M", self.file_modified),
            FileChangeKind::Deleted => ("D", self.file_deleted),
            FileChangeKind::Renamed => ("R", self.file_renamed),
            FileChangeKind::Copied => ("C", self.file_renamed),
        }
    }
}

/// Relative luminance of an sRGB color, 0.0 (black) – 1.0 (white).
fn luma((r, g, b): (u8, u8, u8)) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

/// Linearly interpolate between two colors; `t` in 0.0..=1.0 (0 = `from`).
fn mix(from: (u8, u8, u8), to: (u8, u8, u8), t: f32) -> Color {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color::Rgb(lerp(from.0, to.0), lerp(from.1, to.1), lerp(from.2, to.2))
}
