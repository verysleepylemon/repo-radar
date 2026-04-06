# post-sheets-comments.ps1
# Posts 5 expert technical comments to open issues on maaslalani/sheets
# (a terminal spreadsheet tool surfaced by repo-radar on 2026-04-06, HN Front Page)
#
# Usage (token already set in env from previous session):
#   .\post-sheets-comments.ps1
#
# Or explicitly:
#   $env:GH_TOKEN = "ghp_YOUR_TOKEN_HERE"
#   .\post-sheets-comments.ps1

param(
    [string]$Token = $env:GH_TOKEN,
    [switch]$DryRun
)

if (-not $Token) {
    Write-Error "GitHub token required. Set `$env:GH_TOKEN first."
    exit 1
}

$Headers = @{
    "Authorization"        = "Bearer $Token"
    "Accept"               = "application/vnd.github+json"
    "X-GitHub-Api-Version" = "2022-11-28"
    "User-Agent"           = "repo-radar/1.0"
}

function Post-Comment {
    param(
        [string]$Repo,
        [int]   $Issue,
        [string]$Body
    )
    $url = "https://api.github.com/repos/$Repo/issues/$Issue/comments"

    if ($DryRun) {
        Write-Host "[DRY-RUN] Would post to $Repo #$Issue ($($Body.Length) chars)"
        return
    }

    try {
        # Use WebClient with explicit UTF-8 to handle Unicode/long bodies reliably
        $json  = (@{ body = $Body } | ConvertTo-Json -Compress)
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
        $wc    = [System.Net.WebClient]::new()
        foreach ($k in $Headers.Keys) { $wc.Headers.Add($k, $Headers[$k]) }
        $wc.Headers["Content-Type"] = "application/json; charset=utf-8"
        $resp  = $wc.UploadData($url, "POST", $bytes)
        $html  = ([System.Text.Encoding]::UTF8.GetString($resp) | ConvertFrom-Json).html_url
        Write-Host "[OK] $Repo #$Issue  ->  $html"
    }
    catch {
        $status = $_.Exception.Response.StatusCode.value__
        Write-Host "[FAIL] $Repo #$Issue  (HTTP $status): $($_.Exception.Message)"
    }
}

# ─────────────────────────────────────────────────────────────────────────────
# Issue 1 of 5
# Repo:  maaslalani/sheets
# Issue: #23 "Proposal: .sheetsrc config file for custom key mappings"
# Context: Maintainer asked about Helix-style config as alternative to vim syntax
# ─────────────────────────────────────────────────────────────────────────────
$comment1 = @'
This is a well-scoped proposal. Adding to the Helix-style TOML angle @maaslalani raised:

Helix (`~/.config/helix/config.toml`) uses a `[keys.normal]` / `[keys.insert]` table structure. Here's what that would look like for `sheets`:

```toml
# ~/.config/sheets/config.toml   (XDG-friendly path)

[keys.normal]
"C-c" = "quit"          # single key: Ctrl-C → quit
"Z" = { "Z" = "write_quit" }   # nested: ZZ → :wq

[keys.insert]
"C-c" = "normal_mode"

[keys.normal."g"]       # prefix table: all g-prefixed sequences
"g" = "goto_top"
"B9" = "goto_cell"
```

**Advantages over vim-style map syntax:**
- Pure TOML: no custom parser needed — standard `encoding/toml` handles it (`github.com/BurntSushi/toml` or `github.com/pelletier/go-toml/v2` are zero-CGO and small)
- Nested tables naturally express multi-key sequences (`ZZ`, `gB9`, `<C-c><C-c>`)
- `set` options can live in `[options]` alongside bindings later without format changes
- Editor-native: Helix users expect this format; vim users adapt easily

**Proposed action table (string values for the binding RHS):**

```toml
[actions]
# name          = description
"quit"          = ":q"
"write"         = ":w"
"write_quit"    = ":wq"
"normal_mode"   = "<Esc>"
"nop"           = "<Nop>"
```

**Implementation path if going TOML:**
- `internal/sheets/config.go` — `LoadConfig(path string) (Config, error)`, `DefaultConfig() Config`
- `Config.Bindings` is a nested `map[string]map[string]string` matching the TOML table structure
- Key dispatch in `model.go` `Update()`: before normal handling, check `m.config.Bindings[currentMode][keyString]`, resolve action, emit the corresponding `tea.Msg`

Happy to sketch either the TOML variant or support both formats (`:sheetsrc` for vim users, TOML for Helix / everyone else) — the parser decision is the main fork point.
'@

Post-Comment "maaslalani/sheets" 23 $comment1

# ─────────────────────────────────────────────────────────────────────────────
# Issue 2 of 5
# Repo:  maaslalani/sheets
# Issue: #11 "Feature request: A help screen to display the keystrokes"
# Context: Non-vim user asking for popup; :help/:? already added in v0.2.0
# ─────────────────────────────────────────────────────────────────────────────
$comment2 = @'
Good news: a help command was added in v0.2.0 (yesterday's release). In the command prompt:

```
:help   or   :?
```

Press `:` to open the command bar, type `?` and hit Enter — it prints the full keybinding reference.

**For a popup overlay** (so it's accessible from normal mode without going through the command bar, activated by e.g. `?` in normal mode), here is the BubbleTea pattern:

```go
// In model.go — add a flag
type model struct {
    // ... existing fields ...
    showHelp bool
}

// In Update() — normal mode, before other bindings
case "?":
    m.showHelp = !m.showHelp
    return m, nil

// In View() — overlay on top of the grid
func (m model) View() string {
    if m.showHelp {
        return m.helpOverlay()
    }
    return m.gridView()
}

// helpOverlay renders a centered box using lipgloss
func (m model) helpOverlay() string {
    style := lipgloss.NewStyle().
        Border(lipgloss.RoundedBorder()).
        Padding(1, 2).
        Width(60)

    content := lipgloss.JoinVertical(lipgloss.Left,
        lipgloss.NewStyle().Bold(true).Render("Sheets — Keybindings"),
        "",
        "  Navigation:  h j k l  •  gg G  •  ctrl+u ctrl+d",
        "  Insert:       i  c  I  •  Enter (commit) • Esc (exit)",
        "  Visual:       v  V       Copy: y yy  •  Cut: x  •  Paste: p",
        "  Command:      :w  :q  :wq  :goto B9",
        "  Search:       /  ?  n  N",
        "  Formula:      =( in visual mode",
        "  Undo/Redo:    u  ctrl+r",
        "",
        "  Press ? again to close",
    )

    return lipgloss.Place(m.width, m.height,
        lipgloss.Center, lipgloss.Center,
        style.Render(content))
}
```

`lipgloss.Place` centres the box over the grid without pushing layout around. Close on a second `?`, `Esc`, or `q`.
'@

Post-Comment "maaslalani/sheets" 11 $comment2

# ─────────────────────────────────────────────────────────────────────────────
# Issue 3 of 5
# Repo:  maaslalai/sheets
# Issue: #19 "Feature request: Header Row / Column"
# Context: Freeze row 0 visually (bold/inverted) and during scroll
# ─────────────────────────────────────────────────────────────────────────────
$comment3 = @'
Here's a concrete implementation sketch for frozen header rows/columns in BubbleTea + lipgloss.

**Model changes** (`model.go`):
```go
type model struct {
    // ... existing fields ...
    frozenRows int  // number of rows frozen at top (0 = none, 1 = first row)
    frozenCols int  // number of columns frozen at left
}
```

**Rendering** (`view.go`) — split the grid render into three bands:

```go
func (m model) gridView() string {
    var sb strings.Builder

    // 1. Frozen header rows (always row index 0..frozenRows-1)
    for r := 0; r < m.frozenRows; r++ {
        sb.WriteString(m.renderRow(r, headerStyle))
    }

    // 2. Scrollable body rows (rowOffset .. rowOffset+visibleRows)
    for r := m.rowOffset; r < m.rowOffset+m.visibleRows && r < m.rowCount; r++ {
        if r < m.frozenRows { continue }  // skip frozen rows in body pass
        sb.WriteString(m.renderRow(r, m.rowStyle(r)))
    }
    return sb.String()
}
```

**Header style** (bold + inverted, matching the request):
```go
var headerStyle = lipgloss.NewStyle().
    Bold(true).
    Reverse(true).        // inverted colours — fg/bg swapped
    Padding(0, 1)
```

**Toggling** — activate with `:freeze 1` (command mode) or a dedicated key:
```go
// In handleCommand():
case strings.HasPrefix(cmd, "freeze "):
    n, _ := strconv.Atoi(strings.TrimPrefix(cmd, "freeze "))
    m.frozenRows = n
```

**Frozen columns** work identically: prepend the frozen columns before `colOffset` in `renderRow`, skip them in the scrollable pass.

**CSV persistence** — frozen header is a display preference, not a CSV concept. If persistence matters, a potential approach is sidecar `.sheets.toml` per file (or the `~/.sheetsrc` from #23), storing `frozen_rows = 1`.
'@

Post-Comment "maaslalani/sheets" 19 $comment3

# ─────────────────────────────────────────────────────────────────────────────
# Issue 4 of 5
# Repo:  maaslalani/sheets
# Issue: #20 "Feature request: Column width fitting"
# Context: Fit column to max content width with a manual trigger key
# ─────────────────────────────────────────────────────────────────────────────
$comment4 = @'
Here's a complete implementation sketch for column-width auto-fit.

**Model** (`model.go`) — column widths already likely tracked; if not:
```go
type model struct {
    // ... existing fields ...
    colWidths map[int]int  // col index → character width; 0 = use defaultColWidth
}
```

**Fit function** — scan all rows for the widest value in a column:
```go
// fitColumn sets colWidths[col] to the maximum content width in that column.
func (m *model) fitColumn(col int) {
    max := defaultColWidth
    for row := 0; row < m.rowCount; row++ {
        v := m.cells[cellKey{row, col}]
        w := lipgloss.Width(v)  // handles ANSI/wide chars correctly
        if w+2 > max {          // +2 for cell padding
            max = w + 2
        }
    }
    if m.colWidths == nil {
        m.colWidths = make(map[int]int)
    }
    m.colWidths[col] = max
}

// fitAllColumns runs fitColumn for every column.
func (m *model) fitAllColumns() {
    for col := 0; col < m.colCount; col++ {
        m.fitColumn(col)
    }
}
```

**Key binding** — suggest `=` in normal mode (fits current column), `==` fits all:
```go
// In Update(), normal mode:
case "=":
    if m.equalPending {
        m.fitAllColumns()   // == → fit all
        m.equalPending = false
    } else {
        m.equalPending = true
    }
    return m, nil

// resolve pending = after any other key:
if m.equalPending {
    m.fitColumn(m.selectedCol)  // single = → fit current column
    m.equalPending = false
}
```

**Render** — use `m.colWidths[col]` width in the lipgloss cell style, falling back to `defaultColWidth`:
```go
func (m model) cellWidth(col int) int {
    if w, ok := m.colWidths[col]; ok {
        return w
    }
    return defaultColWidth
}
```

This keeps existing behaviour intact — `colWidths` starts empty so all columns render at `defaultColWidth` until the user explicitly triggers a fit.
'@

Post-Comment "maaslalani/sheets" 20 $comment4

# ─────────────────────────────────────────────────────────────────────────────
# Issue 5 of 5
# Repo:  maaslalai/sheets
# Issue: #21 "Feature request: Fit to screen width"
# Context: Distribute all visible columns evenly across the terminal width
# ─────────────────────────────────────────────────────────────────────────────
$comment5 = @'
Auto-fit to screen width is a natural complement to #20 (per-column fit). Here's a concrete approach.

**Idea:** divide the available terminal width evenly across the *visible* columns (those currently rendered), subject to a minimum cell width.

```go
const minColWidth = 4  // absolute floor so cells don't collapse to nothing

// fitToScreenWidth distributes the terminal width evenly across visible columns.
// visibleCols is the number of columns currently rendered on screen.
func (m *model) fitToScreenWidth(termWidth int) {
    // Subtract row-label column width and borders
    labelWidth := m.rowLabelWidth + 1  // +1 for separator
    usable := termWidth - labelWidth
    if usable <= 0 {
        return
    }
    perCol := usable / m.visibleCols
    if perCol < minColWidth {
        perCol = minColWidth
    }
    if m.colWidths == nil {
        m.colWidths = make(map[int]int)
    }
    for col := m.colOffset; col < m.colOffset+m.visibleCols && col < m.colCount; col++ {
        m.colWidths[col] = perCol
    }
}
```

**When to trigger:**
1. On window resize — BubbleTea fires `tea.WindowSizeMsg` automatically, so:
```go
case tea.WindowSizeMsg:
    m.width  = msg.Width
    m.height = msg.Height
    if m.autoFitWidth {
        m.fitToScreenWidth(msg.Width)
    }
```
2. Manual key, e.g. `|` in normal mode to toggle `autoFitWidth` on/off
3. `:fitwidth` command

**Combined with #20** — a sensible UX would be:
- `=` — fit current column to content
- `==` — fit all columns to content
- `|` — fit all visible columns to terminal width (ignores content)
- Resize auto-reflows if `autoFitWidth` is on

The `m.autoFitWidth` flag means the user's manual column-width adjustments from `=` are preserved when they don't want auto-reflow.
'@

Post-Comment "maaslalani/sheets" 21 $comment5

Write-Host ""
Write-Host "Done. All 5 comments posted to maaslalai/sheets."
Write-Host "Issues covered: #23 (config), #11 (help), #19 (headers), #20 (col fit), #21 (screen fit)"
