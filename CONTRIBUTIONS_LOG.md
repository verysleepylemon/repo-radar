# GitHub Contributions Log

Sources surfaced by [repo-radar](http://localhost:8080) — monitoring HN, Reddit, GitHub trending, and leak feeds.

---

## 2026-04-06 — Batch 2: maaslalani/sheets (Go • TUI)

**Source:** HN Front Page — "Sheets Spreadsheets in Your Terminal" (75 points)  
**Repo:** https://github.com/maaslalani/sheets  
**Context:** New TUI spreadsheet app launched ~5 days ago via HN Show HN. Within 24 hours it accumulated 9 open feature-request issues from the initial user flood. Stack: Go + BubbleTea + lipgloss.

### Issues Helped

---

#### Issue #23 — Proposal: .sheetsrc config file for custom key mappings
- **URL:** https://github.com/maaslalani/sheets/issues/23#issuecomment-4191684229
- **Problem:** No way to customize key bindings. Maintainer asked about Helix-style config as alternative to vim-syntax.
- **Solution Provided:** Compared vim-syntax RC file (proposed by NSEvent) with Helix-style TOML alternative, provided full TOML schema, proposed action table, and BubbleTea key dispatch implementation path.

```toml
# Helix-style ~/.config/sheets/config.toml

[keys.normal]
"C-c" = "quit"
"Z" = { "Z" = "write_quit" }

[keys.insert]
"C-c" = "normal_mode"

[keys.normal."g"]
"g" = "goto_top"
"B9" = "goto_cell"
```

**Implementation path:**
- `internal/sheets/config.go` — `LoadConfig(path string) (Config, error)`, `DefaultConfig() Config`
- `Config.Bindings` is `map[string]map[string]string` matching TOML table nesting
- Key dispatch in `model.go Update()`: check `m.config.Bindings[currentMode][keyString]` before normal handling

---

#### Issue #11 — Feature request: A help screen to display the keystrokes
- **URL:** https://github.com/maaslalani/sheets/issues/11#issuecomment-4191684275
- **Problem:** Non-vim users can't discover keybindings; no in-app reference.
- **Solution Provided:** Pointed out `:help` / `:?` added in v0.2.0 (yesterday); provided BubbleTea overlay popup implementation using `lipgloss.Place` for centered modal.

```go
// model.go
type model struct {
    showHelp bool
    // ... existing fields
}

// Update() — press ? in normal mode
case "?":
    m.showHelp = !m.showHelp
    return m, nil

// View() — overlay on grid when active
func (m model) View() string {
    if m.showHelp {
        return m.helpOverlay()
    }
    return m.gridView()
}

func (m model) helpOverlay() string {
    style := lipgloss.NewStyle().
        Border(lipgloss.RoundedBorder()).Padding(1, 2).Width(60)
    content := lipgloss.JoinVertical(lipgloss.Left,
        lipgloss.NewStyle().Bold(true).Render("Sheets — Keybindings"),
        "  Navigation:  h j k l  •  gg G  •  ctrl+u ctrl+d",
        // ... more keybinding rows
    )
    return lipgloss.Place(m.width, m.height,
        lipgloss.Center, lipgloss.Center, style.Render(content))
}
```

---

#### Issue #19 — Feature request: Header Row / Column
- **URL:** https://github.com/maaslalani/sheets/issues/19#issuecomment-4191684338
- **Problem:** No way to freeze/visually distinguish header row/column.
- **Solution Provided:** Model field additions (`frozenRows`, `frozenCols`), split grid render into frozen band + scrollable band, lipgloss `Reverse(true)` + `Bold(true)` for header style, `:freeze N` command to toggle.

```go
// model.go additions
type model struct {
    frozenRows int
    frozenCols int
    // ...
}

// view.go split render
func (m model) gridView() string {
    var sb strings.Builder
    // 1. Frozen header rows (always rendered)
    for r := 0; r < m.frozenRows; r++ {
        sb.WriteString(m.renderRow(r, headerStyle))
    }
    // 2. Scrollable body (skips frozen rows)
    for r := m.rowOffset; r < m.rowOffset+m.visibleRows; r++ {
        if r < m.frozenRows { continue }
        sb.WriteString(m.renderRow(r, m.rowStyle(r)))
    }
    return sb.String()
}

var headerStyle = lipgloss.NewStyle().Bold(true).Reverse(true).Padding(0, 1)
```

---

#### Issue #20 — Feature request: Column width fitting
- **URL:** https://github.com/maaslalani/sheets/issues/20#issuecomment-4191684382
- **Problem:** All columns have the same default width; wide content gets truncated.
- **Solution Provided:** `fitColumn(col int)` scans all rows for max rune width using `lipgloss.Width()` (handles ANSI/wide chars), key binding `=` (current col) / `==` (all cols), `cellWidth(col)` fallback helper.

```go
func (m *model) fitColumn(col int) {
    max := defaultColWidth
    for row := 0; row < m.rowCount; row++ {
        v := m.cells[cellKey{row, col}]
        w := lipgloss.Width(v) + 2  // +2 for padding
        if w > max { max = w }
    }
    if m.colWidths == nil { m.colWidths = make(map[int]int) }
    m.colWidths[col] = max
}

// Normal mode: = fits current, == fits all
case "=":
    if m.equalPending {
        m.fitAllColumns(); m.equalPending = false
    } else {
        m.equalPending = true
    }
```

---

#### Issue #21 — Feature request: Fit to screen width
- **URL:** https://github.com/maaslalani/sheets/issues/21#issuecomment-4191684434
- **Problem:** Columns don't adapt to terminal width; wide terminals waste space.
- **Solution Provided:** `fitToScreenWidth(termWidth int)` divides available width evenly across visible columns, `autoFitWidth` flag for resize auto-reflow via `tea.WindowSizeMsg`, `|` key to toggle, unified UX with #20.

```go
const minColWidth = 4

func (m *model) fitToScreenWidth(termWidth int) {
    usable := termWidth - m.rowLabelWidth - 1
    perCol := usable / m.visibleCols
    if perCol < minColWidth { perCol = minColWidth }
    for col := m.colOffset; col < m.colOffset+m.visibleCols; col++ {
        m.colWidths[col] = perCol
    }
}

// In Update():
case tea.WindowSizeMsg:
    m.width, m.height = msg.Width, msg.Height
    if m.autoFitWidth {
        m.fitToScreenWidth(msg.Width)
    }
```

---

## 2026-04-05 — Batch 1: Rust Repos (DytallixHQ + hyperlight-dev)

**Source:** GitHub API — `is:open label:help-wanted language:Rust`

| Issue | Repo | Comment URL |
|-------|------|-------------|
| #5 — Error messages | DytallixHQ/dytallix-sdk | (from post-to-github.ps1 run) |
| #4 — ML-DSA-65 benchmarks | DytallixHQ/dytallix-sdk | (from post-to-github.ps1 run) |
| #2 — Isochronous rejection sampling docs | DytallixHQ/dytallix-sdk | (from post-to-github.ps1 run) |
| #3 — Integration test for dytallix init | DytallixHQ/dytallix-sdk | (from post-to-github.ps1 run) |
| #706 — Microbenchmarks | hyperlight-dev/hyperlight | (from post-to-github.ps1 run) |

---

## Bug Fix: PowerShell UTF-8 Encoding

**Problem:** `Invoke-RestMethod -Body (ConvertTo-Json ...)` returns HTTP 400 for long comment bodies containing Unicode characters (bullet points •, em-dashes —, etc.)

**Root Cause:** PowerShell's `Invoke-RestMethod` defaults to a legacy Windows-1252 encoding for the request body even when the string is Unicode-correct.

**Fix:** Use `[System.Net.WebClient]::new()` with explicit `UTF-8.GetBytes()`:

```powershell
$json  = (@{ body = $Body } | ConvertTo-Json -Compress)
$bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
$wc    = [System.Net.WebClient]::new()
foreach ($k in $Headers.Keys) { $wc.Headers.Add($k, $Headers[$k]) }
$wc.Headers["Content-Type"] = "application/json; charset=utf-8"
$resp  = $wc.UploadData($url, "POST", $bytes)
```

Applied to both `post-to-github.ps1` and `post-sheets-comments.ps1`.

---

## Scripts

| File | Purpose |
|------|---------|
| `post-to-github.ps1` | Rust repos batch (5 comments, DytallixHQ + hyperlight) |
| `post-sheets-comments.ps1` | Go TUI sheets batch (5 comments, maaslalani/sheets) |
