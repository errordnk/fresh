//! Regression test for issue #1554: scrollbar track/thumb colors ignored theme settings.
//!
//! Before the fix, `scrollbar_track_fg` and `scrollbar_thumb_fg` from the theme
//! were ignored for the editor split scrollbar (both vertical and horizontal),
//! because the render path used hardcoded `Color::DarkGray` / `Color::Black` /
//! `Color::Gray` values.

use crate::common::harness::EditorTestHarness;
use fresh::config::Config;
use ratatui::style::Color;

fn long_content(num_lines: usize) -> String {
    (0..num_lines)
        .map(|i| format!("line {i:04}: lorem ipsum dolor sit amet"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn scan_column_bgs(harness: &EditorTestHarness, col: u16) -> Vec<Color> {
    let (first, last) = harness.content_area_rows();
    (first..=last)
        .filter_map(|row| {
            harness
                .get_cell_style(col, row as u16)
                .and_then(|s| s.bg)
                .filter(|c| *c != Color::Reset)
        })
        .collect()
}

#[test]
fn test_vertical_scrollbar_uses_theme_track_and_thumb_colors() {
    let config = Config {
        theme: "high-contrast".into(),
        ..Default::default()
    };

    let mut harness = EditorTestHarness::with_config(80, 24, config).unwrap();
    let (expected_track, expected_thumb, ground) = {
        let theme = harness.editor().theme();
        (
            theme.scrollbar_track_fg,
            theme.scrollbar_thumb_fg,
            theme.editor_bg,
        )
    };

    harness.load_buffer_from_text(&long_content(200)).unwrap();
    harness.render().unwrap();

    // The vertical bar is a right half block (`▐`) in the bar's colour, so
    // its colours are the cells' foregrounds, and the half it leaves is the
    // editor's ground on every row — the cursor row's highlight stops short
    // of the bar.
    let (first, last) = harness.content_area_rows();
    let mut fgs = Vec::new();
    for row in first as u16..=last as u16 {
        assert_eq!(
            harness.get_cell(79, row).as_deref(),
            Some("▐"),
            "row {row}: the bar is a half block"
        );
        let bar = harness.get_cell_style(79, row).unwrap();
        assert_eq!(
            bar.bg,
            Some(ground),
            "row {row}: the half the bar leaves is the editor's ground"
        );
        fgs.extend(bar.fg);
    }

    assert!(
        fgs.contains(&expected_track),
        "Scrollbar track should use theme.scrollbar_track_fg ({expected_track:?}); saw {fgs:?}"
    );
    assert!(
        fgs.contains(&expected_thumb),
        "Scrollbar thumb should use theme.scrollbar_thumb_fg ({expected_thumb:?}); saw {fgs:?}"
    );
}

#[test]
fn test_horizontal_scrollbar_uses_theme_track_color() {
    let mut config = Config {
        theme: "high-contrast".into(),
        ..Default::default()
    };
    config.editor.line_wrap = false;
    config.editor.show_horizontal_scrollbar = true;

    let mut harness = EditorTestHarness::with_config(80, 24, config).unwrap();
    let expected_track = harness.editor().theme().scrollbar_track_fg;

    let long_line_content: String = (0..50)
        .map(|i| format!("line {i}: {}", "X".repeat(200)))
        .collect::<Vec<_>>()
        .join("\n");
    harness.load_buffer_from_text(&long_line_content).unwrap();
    harness.render().unwrap();

    let (_, last_content_row) = harness.content_area_rows();
    let row = last_content_row as u16;
    let width = harness.buffer().area.width;

    let bgs: Vec<Color> = (0..width)
        .filter_map(|c| {
            harness
                .get_cell_style(c, row)
                .and_then(|s| s.bg)
                .filter(|c| *c != Color::Reset)
        })
        .collect();

    assert!(
        bgs.contains(&expected_track),
        "Horizontal scrollbar track should use theme.scrollbar_track_fg ({expected_track:?}); saw bgs {bgs:?}"
    );
}

/// A file whose lines all fit the window has nothing to scroll: no vertical
/// bar is drawn, and its column shows the editor's ground. A longer one does
/// get its bar.
#[test]
fn test_vertical_scrollbar_hidden_while_the_file_fits() {
    let mut harness = EditorTestHarness::new(80, 24).unwrap();
    let ground = harness.editor().theme().editor_bg;
    harness.load_buffer_from_text(&long_content(5)).unwrap();
    harness.render().unwrap();

    let (first, last) = harness.content_area_rows();
    for row in first as u16..=last as u16 {
        assert!(
            !harness.is_scrollbar_thumb_at(79, row) && !harness.is_scrollbar_track_at(79, row),
            "row {row}: no bar on a file that fits"
        );
        assert_eq!(harness.get_cell(79, row).as_deref(), Some(" "));
        assert_eq!(
            harness.get_cell_style(79, row).and_then(|s| s.bg),
            Some(ground),
            "row {row}: the bar's column shows the editor's ground"
        );
    }

    harness.load_buffer_from_text(&long_content(200)).unwrap();
    harness.render().unwrap();
    assert!(
        (first as u16..=last as u16).any(|row| harness.is_scrollbar_thumb_at(79, row)),
        "a file longer than the window gets its bar"
    );
}
