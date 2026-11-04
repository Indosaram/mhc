# Terminal Interface Visual QA and Verification

This reference defines verification protocols, capture tooling, and rendering checks for terminal user interfaces (TUIs), interactive console menus, and command-line text formatting.

---

## 1. Pseudoterminal Execution and Truecolor Capture

Terminal user interfaces and interactive console tools cannot be reliably verified using plain stdout redirects or simulated shell strings:
- **Real PTY Allocation**: Interactive TUIs must be rendered inside a genuine pseudoterminal (PTY) to ensure accurate ANSI escape sequence processing, cursor movement, and viewport dimensions.
- **Truecolor and ANSI Style Integrity**: The capture pipeline must preserve 24-bit truecolor color codes, background styling, bold text, and terminal dim attributes without downsampling or striping escapes.
- **Prohibition of Fragile Screen Scrapers**: Never use ad-hoc terminal scrapers (such as naive screen grabbers) that degrade escape streams or distort character grids.

---

## 2. Bundled Capture Utility Invocation

Terminal captures utilize the bundled capture script provided by the `visual-qa` skill at `assets/omo-latest/skills/visual-qa/scripts/` (`tui-capture.mjs`):
```bash
# visual-qa skill
bun assets/omo-latest/skills/visual-qa/*/tui-capture.mjs \
  --title "TUI Visual QA" \
  --command "<interactive-command>" \
  --wait-for "<ready-regex-marker>" \
  --cols 100 \
  --rows 30 \
  --evidence-dir .omo/evidence/<slug>/tui-terminal
```

### Parameter Specification
- `--command`: The exact command-line invocation to run within the allocated PTY.
- `--wait-for`: A regex predicate matching output text indicating the interface has settled and is ready for capture. Avoid fixed sleep timeouts.
- `--cols` / `--rows`: Viewport dimensions in characters (defaulting to 100 columns by 30 rows).
- `--evidence-dir`: Destination path where capture artifacts are stored for verification and reporting.
- `--input`: Keystrokes or interactive inputs injected into the running session (e.g., `{ArrowDown}`, `{Enter}`, `{Esc}`).

---

## 3. Capture Artifact Inspection

The capture utility produces a suite of structured artifacts within the designated evidence directory:
- `terminal.txt`: Viewport character grid representing the final terminal screen state in plain text.
- `terminal.ansi`: Raw ANSI byte stream containing all escape codes, styling attributes, and cursor movements.
- `cells.json`: Structured array detailing character contents, foreground colors, and background styles for every cell in the viewport.
- `terminal.png`: Rendered image capture showing the exact visual appearance of the terminal screen.
- `metadata.json`: Run metadata including binary hashes, command parameters, timestamps, and process exit codes.

---

## 4. Visual Layout and Glyph Alignment Checks

Inspect captured terminal artifacts against rigorous visual and structural criteria:
- **Box-Drawing Border Alignment**: Verify that horizontal and vertical box-drawing lines (`─`, `│`, `┌`, `┐`, `└`, `┘`, `├`, `┤`, `┬`, `┴`, `┼`) align seamlessly from row to row without jagged offsets or broken corners.
- **Wide Character (CJK) Precision**: Ensure wide characters (such as Korean, Japanese, and Chinese glyphs) and emojis are allocated two full display columns without causing following characters to drift or clip against right borders.
- **Viewport Bounds and Overflow**: Confirm that no lines exceed the expected terminal column width and that status bars remain anchored to the viewport bottom without accidental wrapping.
- **Interactive State Validation**: Verify that selection highlights, cursor indicators, and active modal dialogs render legibly in both light and dark terminal palettes.
