//! Virtual screen model using vt100.

/// Virtual screen backed by vt100.
pub struct Screen {
    parser: vt100::Parser,
}

impl Screen {
    /// Create a new screen with the given dimensions.
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
        }
    }

    /// Process output bytes through the terminal parser.
    pub fn process(&mut self, data: &[u8]) {
        self.parser.process(data);
    }

    /// Get the current screen contents as a string.
    /// Each row is separated by a newline.
    #[must_use]
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// Get the screen contents with ANSI formatting codes preserved.
    /// This returns the screen text with color/style escape codes but without
    /// cursor positioning or screen-clearing sequences.
    #[must_use]
    #[allow(clippy::similar_names)] // fg/bg are intentionally similar
    #[allow(clippy::too_many_lines)] // Complex function, splitting would reduce clarity
    pub fn contents_formatted(&self) -> String {
        use std::fmt::Write;
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let mut result = String::new();
        let mut current_fg: Option<vt100::Color> = None;
        let mut current_bg: Option<vt100::Color> = None;
        let mut current_bold = false;
        let mut current_dim = false;
        let mut current_italic = false;
        let mut current_underline = false;
        let mut current_inverse = false;
        let mut trailing_empty_rows = 0;

        for row in 0..rows {
            let mut row_text = String::new();
            let mut row_has_content = false;
            let mut trailing_spaces = 0;

            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    // Skip wide character continuations
                    if cell.is_wide_continuation() {
                        continue;
                    }

                    let contents = cell.contents();

                    // Track if we need to emit formatting changes
                    let fg = cell.fgcolor();
                    let bg = cell.bgcolor();
                    let bold = cell.bold();
                    let dim = cell.dim();
                    let italic = cell.italic();
                    let underline = cell.underline();
                    let inverse = cell.inverse();

                    // Check if attributes changed
                    let attrs_changed = current_fg != Some(fg)
                        || current_bg != Some(bg)
                        || current_bold != bold
                        || current_dim != dim
                        || current_italic != italic
                        || current_underline != underline
                        || current_inverse != inverse;

                    if attrs_changed && cell.has_contents() {
                        // Emit reset and new attributes
                        let mut sgr = vec!["0".to_string()]; // Reset

                        // Foreground color
                        match fg {
                            vt100::Color::Default => {}
                            vt100::Color::Idx(n) => {
                                if n < 8 {
                                    sgr.push(format!("{}", 30 + n));
                                } else if n < 16 {
                                    sgr.push(format!("{}", 90 + n - 8));
                                } else {
                                    sgr.push(format!("38;5;{n}"));
                                }
                            }
                            vt100::Color::Rgb(r, g, b) => {
                                sgr.push(format!("38;2;{r};{g};{b}"));
                            }
                        }

                        // Background color
                        match bg {
                            vt100::Color::Default => {}
                            vt100::Color::Idx(n) => {
                                if n < 8 {
                                    sgr.push(format!("{}", 40 + n));
                                } else if n < 16 {
                                    sgr.push(format!("{}", 100 + n - 8));
                                } else {
                                    sgr.push(format!("48;5;{n}"));
                                }
                            }
                            vt100::Color::Rgb(r, g, b) => {
                                sgr.push(format!("48;2;{r};{g};{b}"));
                            }
                        }

                        if bold {
                            sgr.push("1".to_string());
                        }
                        if dim {
                            sgr.push("2".to_string());
                        }
                        if italic {
                            sgr.push("3".to_string());
                        }
                        if underline {
                            sgr.push("4".to_string());
                        }
                        if inverse {
                            sgr.push("7".to_string());
                        }

                        // Only emit if we have non-default attributes
                        if sgr.len() > 1 || current_fg.is_some() {
                            // First flush any trailing spaces before the escape
                            row_text.push_str(&" ".repeat(trailing_spaces));
                            trailing_spaces = 0;
                            let _ = write!(row_text, "\x1b[{}m", sgr.join(";"));
                        }

                        current_fg = Some(fg);
                        current_bg = Some(bg);
                        current_bold = bold;
                        current_dim = dim;
                        current_italic = italic;
                        current_underline = underline;
                        current_inverse = inverse;
                    }

                    if contents.is_empty() || contents == " " {
                        trailing_spaces += 1;
                    } else {
                        // Flush trailing spaces
                        row_text.push_str(&" ".repeat(trailing_spaces));
                        trailing_spaces = 0;
                        row_text.push_str(contents);
                        row_has_content = true;
                    }
                }
            }

            // Don't include trailing spaces on lines
            if row_has_content {
                // Flush any pending empty rows
                for _ in 0..trailing_empty_rows {
                    result.push('\n');
                }
                trailing_empty_rows = 0;
                result.push_str(&row_text);
                result.push('\n');
            } else {
                trailing_empty_rows += 1;
            }
        }

        // Remove trailing newline if present
        if result.ends_with('\n') {
            result.pop();
        }

        // Reset attributes at the end if we changed any
        if current_fg.is_some()
            && (current_fg != Some(vt100::Color::Default)
                || current_bg != Some(vt100::Color::Default)
                || current_bold
                || current_dim
                || current_italic
                || current_underline
                || current_inverse)
        {
            result.push_str("\x1b[0m");
        }

        result
    }

    /// Get the cursor position (row, col), 0-indexed.
    #[must_use]
    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// Get the screen size (rows, cols).
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Check if the alternate screen is active.
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// Resize the screen.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        // vt100::Parser doesn't have a resize method, so we create a new parser
        // and copy the contents. This is a limitation we may need to work around.
        self.parser = vt100::Parser::new(rows, cols, 0);
    }

    /// Get a snapshot of the screen as normalized text.
    /// Strips ANSI codes and trailing whitespace.
    #[must_use]
    pub fn snapshot(&self) -> String {
        // contents() already strips formatting
        let contents = self.parser.screen().contents();
        let mut lines: Vec<&str> = contents.lines().collect();

        // Trim trailing empty lines
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }

        // Trim trailing whitespace from each line
        lines
            .iter()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_output() {
        let mut screen = Screen::new(24, 80);
        screen.process(b"Hello, World!");
        assert!(screen.contents().contains("Hello, World!"));
    }

    #[test]
    fn test_cursor_movement() {
        let mut screen = Screen::new(24, 80);
        screen.process(b"ABC\rX");
        // \r moves cursor to beginning of line, X overwrites A
        assert!(screen.contents().starts_with("XBC"));
    }

    #[test]
    fn test_newlines() {
        let mut screen = Screen::new(24, 80);
        screen.process(b"line1\nline2\nline3");
        let snapshot = screen.snapshot();
        assert!(snapshot.contains("line1"));
        assert!(snapshot.contains("line2"));
        assert!(snapshot.contains("line3"));
    }

    #[test]
    fn test_ansi_colors_stripped() {
        let mut screen = Screen::new(24, 80);
        // Red text: ESC[31m Hello ESC[0m
        screen.process(b"\x1b[31mHello\x1b[0m");
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.trim(), "Hello");
        assert!(!snapshot.contains("\x1b"));
    }

    #[test]
    fn test_contents_formatted_preserves_colors() {
        let mut screen = Screen::new(24, 80);
        // Red "RED", reset, space, green "GREEN"
        screen.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m");

        let formatted = screen.contents_formatted();
        eprintln!("formatted output: {:?}", formatted);

        // Should contain the text
        assert!(formatted.contains("RED"));
        assert!(formatted.contains("GREEN"));

        // Should contain ANSI codes
        assert!(formatted.contains("\x1b["));

        // Should have red color code (31)
        assert!(formatted.contains("31"));

        // Should have green color code (32)
        assert!(formatted.contains("32"));

        // Should NOT have cursor positioning (like ESC[H or ESC[J or ESC[row;colH)
        assert!(!formatted.contains("\x1b[H"));
        assert!(!formatted.contains("\x1b[J"));
        assert!(!formatted.contains("\x1b[?25h")); // show cursor

        // Text should be on one line (no spurious newlines in the middle)
        let lines: Vec<&str> = formatted.lines().collect();
        assert_eq!(lines.len(), 1, "Expected 1 line, got: {:?}", lines);
    }
}
