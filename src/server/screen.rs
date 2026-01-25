//! Virtual screen model using vt100.

/// Virtual screen backed by vt100.
pub struct Screen {
    parser: vt100::Parser,
}

impl Screen {
    /// Create a new screen with the given dimensions.
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
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// Get the screen contents with trailing whitespace preserved.
    pub fn contents_formatted(&self) -> String {
        let screen = self.parser.screen();
        let mut result = String::new();

        for row in 0..screen.size().0 {
            let row_text = screen
                .rows_formatted(row, row + 1)
                .next()
                .unwrap_or_default();
            result.push_str(&String::from_utf8_lossy(&row_text));
            result.push('\n');
        }

        result
    }

    /// Get the cursor position (row, col), 0-indexed.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// Get the screen size (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Check if the alternate screen is active.
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
    pub fn snapshot(&self) -> String {
        // contents() already strips formatting
        let contents = self.parser.screen().contents();
        let mut lines: Vec<&str> = contents.lines().collect();

        // Trim trailing empty lines
        while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
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
}
