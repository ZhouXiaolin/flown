use iodilos::prelude::Color;
use std::borrow::Cow;
use syntect::highlighting::{Theme, ThemeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppTheme {
    pub(crate) syntax_theme_name: Cow<'static, str>,
    pub(crate) markdown: MarkdownTheme,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownTheme {
    pub(crate) search_highlight_bg: Color,
    pub(crate) search_match_bg: Color,
    pub(crate) code_gutter: Color,
    pub(crate) blockquote_marker: Color,
    pub(crate) list_level_1: Color,
    pub(crate) list_level_2: Color,
    pub(crate) list_level_3: Color,
    pub(crate) ordered_list: Color,
    pub(crate) table_border: Color,
    pub(crate) table_separator: Color,
    pub(crate) table_header: Color,
    pub(crate) table_cell: Color,
    pub(crate) heading_1: Color,
    pub(crate) heading_2: Color,
    pub(crate) heading_3: Color,
    pub(crate) heading_4: Color,
    pub(crate) heading_other: Color,
    pub(crate) heading_underline: Color,
    pub(crate) code_frame: Color,
    pub(crate) code_label: Color,
    pub(crate) inline_code_fg: Color,
    pub(crate) inline_code_bg: Color,
    pub(crate) rule: Color,
    pub(crate) link_icon: Color,
    pub(crate) link_text: Color,
    pub(crate) link_hover: Color,
    pub(crate) blockquote_text: Color,
    pub(crate) text: Color,
    pub(crate) strong_text: Color,
    pub(crate) latex_inline_fg: Color,
    pub(crate) latex_inline_bg: Color,
    pub(crate) latex_block_fg: Color,
    pub(crate) mermaid_keyword: Color,
    pub(crate) mermaid_arrow: Color,
    pub(crate) mermaid_label: Color,
    pub(crate) mermaid_block_fg: Color,
    pub(crate) mark_fg: Color,
    pub(crate) mark_bg: Color,
    pub(crate) task_checked: Color,
    pub(crate) task_unchecked: Color,
    pub(crate) alert_note: Color,
    pub(crate) alert_tip: Color,
    pub(crate) alert_important: Color,
    pub(crate) alert_warning: Color,
    pub(crate) alert_caution: Color,
}

pub(crate) const OCEAN_DARK_MARKDOWN: MarkdownTheme = MarkdownTheme {
    search_highlight_bg: Color::Rgb(72, 62, 16),
    search_match_bg: Color::Rgb(140, 120, 30),
    code_gutter: Color::Rgb(40, 48, 68),
    blockquote_marker: Color::Rgb(75, 80, 148),
    list_level_1: Color::Rgb(95, 200, 148),
    list_level_2: Color::Rgb(138, 155, 200),
    list_level_3: Color::Rgb(168, 168, 185),
    ordered_list: Color::Rgb(95, 200, 148),
    table_border: Color::Rgb(65, 75, 108),
    table_separator: Color::Rgb(55, 65, 95),
    table_header: Color::Rgb(140, 190, 255),
    table_cell: Color::Rgb(205, 208, 218),
    heading_1: Color::Rgb(140, 190, 255),
    heading_2: Color::Rgb(120, 210, 170),
    heading_3: Color::Rgb(210, 180, 120),
    heading_4: Color::Rgb(162, 192, 222),
    heading_other: Color::Rgb(180, 180, 190),
    heading_underline: Color::Rgb(40, 50, 75),
    code_frame: Color::Rgb(40, 48, 68),
    code_label: Color::Rgb(95, 110, 145),
    inline_code_fg: Color::Rgb(220, 150, 118),
    inline_code_bg: Color::Rgb(38, 32, 31),
    rule: Color::Rgb(48, 56, 76),
    link_icon: Color::Rgb(85, 148, 235),
    link_text: Color::Rgb(88, 152, 238),
    link_hover: Color::Rgb(130, 190, 255),
    blockquote_text: Color::Rgb(148, 148, 195),
    text: Color::Rgb(208, 210, 218),
    strong_text: Color::Rgb(245, 245, 255),
    latex_inline_fg: Color::Rgb(200, 160, 225),
    latex_inline_bg: Color::Rgb(38, 28, 48),
    latex_block_fg: Color::Rgb(195, 155, 220),
    mermaid_keyword: Color::Rgb(80, 200, 200),
    mermaid_arrow: Color::Rgb(120, 160, 200),
    mermaid_label: Color::Rgb(100, 210, 180),
    mermaid_block_fg: Color::Rgb(160, 190, 200),
    mark_fg: Color::Rgb(208, 210, 218),
    mark_bg: Color::Rgb(80, 68, 20),
    task_checked: Color::Rgb(95, 200, 148),
    task_unchecked: Color::Rgb(100, 100, 110),
    alert_note: Color::Rgb(88, 152, 238),
    alert_tip: Color::Rgb(95, 200, 148),
    alert_important: Color::Rgb(200, 160, 225),
    alert_warning: Color::Rgb(210, 180, 120),
    alert_caution: Color::Rgb(218, 95, 95),
};

pub(crate) const OCEAN_DARK_THEME: AppTheme = AppTheme {
    syntax_theme_name: Cow::Borrowed("base16-ocean.dark"),
    markdown: OCEAN_DARK_MARKDOWN,
};

pub(crate) fn app_theme() -> AppTheme {
    OCEAN_DARK_THEME.clone()
}

pub(crate) fn current_syntect_theme(themes: &ThemeSet) -> &Theme {
    themes
        .themes
        .get(OCEAN_DARK_THEME.syntax_theme_name.as_ref())
        .or_else(|| themes.themes.values().next())
        .expect("syntect theme set is empty")
}
