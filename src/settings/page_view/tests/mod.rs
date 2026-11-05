//! The page-view tests, one file per subject.
//!
//! The shared helpers live here; each subject file holds the tests that name it.

use super::super::*;
use super::*;
use crate::settings::page_view::render::{
    dedicated_selector, row_placement_boxes, separator_frame,
};
use objc2::sel;

/// The index just past the block that starts at the first `{` at or after `start`.
///
/// The source-reading tests cut a function or a block out of a file. Anchoring that cut on the
/// NEXT item's text made the cut depend on what happened to follow it in the same file: when the
/// two moved to different files the anchor vanished, and while they shared a file a match from
/// the neighbour could satisfy it. Brace matching asks the item only about itself.
fn braced_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0i32;
    let mut seen = false;
    for (offset, byte) in bytes.iter().enumerate().skip(start) {
        match byte {
            b'{' => {
                depth += 1;
                seen = true;
            }
            b'}' => {
                depth -= 1;
                if seen && depth == 0 {
                    return offset + 1;
                }
            }
            _ => {}
        }
    }
    panic!("the block starting at byte {start} never closes");
}
fn input() -> PageInput {
    PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current())
}
fn switcher() -> PageView {
    switcher_page_view(&input())
}
fn general() -> PageView {
    general_page_view(&input())
}
fn fields(view: &PageView) -> Vec<ControlField> {
    view.rows.iter().filter_map(|row| row.field()).collect()
}
fn view_measured(view: &PageView, index: usize, caption_h: f64) -> MeasuredText {
    let mut text = MeasuredText::base();
    text.subtitle_heights = (0..view.rows.len())
        .map(|row| if row == index { caption_h } else { 0.0 })
        .collect();
    text
}

mod description;
mod identity;
mod layout;
mod render_build;
