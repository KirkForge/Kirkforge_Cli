//! Tool / ink setters: active tool, line / box style, color, brush,
//! text border. Switching tool cancels an in-progress draft but
//! deliberately leaves an active resize alone (the user can be
//! mid-drag and tab between tools).

use crate::types::{BoxStyle, DrawMode, InkColor, LineStyle, TextBorderMode};

impl super::DrawState {
    pub fn set_tool(&mut self, tool: DrawMode) {
        self.tool = tool;
        // Switching tool cancels any in-progress draft — the new tool
        // shouldn't inherit a half-drawn object from the old one. We
        // deliberately do NOT cancel an active resize; the user can be
        // mid-drag and tab between tools (or hit a hotkey) without
        // silently losing the gesture.
        self.cancel_draft();
    }

    /// Move to the next (or previous) tool in `DrawMode` order. Wraps
    /// at both ends so Tab from the last tool lands back on Select.
    /// Used by Tab / Shift+Tab so the user can cycle without knowing
    /// the single-letter hotkeys.
    pub fn cycle_tool(&mut self, forward: bool) {
        // ponytail: derive the order from the enum's discriminants so
        // adding a new tool in the middle automatically extends the
        // cycle.
        let order = [
            DrawMode::Select,
            DrawMode::Box,
            DrawMode::Line,
            DrawMode::Elbow,
            DrawMode::Paint,
            DrawMode::Text,
        ];
        let cur = order.iter().position(|m| *m == self.tool).unwrap_or(0);
        let next = if forward {
            (cur + 1) % order.len()
        } else {
            (cur + order.len() - 1) % order.len()
        };
        self.set_tool(order[next]);
    }

    pub fn set_color(&mut self, color: InkColor) {
        self.color = color;
    }

    pub fn set_line_style(&mut self, style: LineStyle) {
        self.line_style = style;
    }

    pub fn set_box_style(&mut self, style: BoxStyle) {
        self.box_style = style;
    }

    pub fn set_brush(&mut self, brush: impl Into<String>) {
        self.brush = brush.into();
    }

    pub fn set_text_border(&mut self, border: TextBorderMode) {
        self.text_border = border;
    }
}
