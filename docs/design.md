# neotern: design and limits

How neotern draws Neovim as a Tern surface, and what it cannot draw. The overview and the feature
list are in the [README](../README.md).

neotern runs `nvim --embed` and attaches as an `ext_linegrid` UI over msgpack-RPC. It does not
parse nvim's terminal output. It keeps every grid in memory and draws them as a Tern Surface
Protocol (TSP) `screen` surface: each window as an `editor` node, each float as a card.

## Use

```sh
cargo build --release
target/release/neotern [nvim args...]   # e.g. neotern -O a.rs b.rs
```

All arguments go to `nvim`. Outside Tern (no TSP reply), neotern runs plain `nvim` with the same
arguments and exits with its status.

## Design

- `src/nvim.rs`: spawns `nvim --embed`. A reader thread sends each `redraw` notification to a
  channel. All API calls are notifications, so no request ids are tracked.
- `src/grid.rs`: one cell grid per nvim grid (`grid_resize`, `grid_line`, `grid_scroll`,
  `grid_clear`, `grid_destroy`, `grid_cursor_goto`, `hl_attr_define`, `default_colors_set`) and
  where each window sits (`win_pos`, `win_float_pos`). It gives a grid's cells as text with one
  colored run per cell run, in the offsets Tern counts.
- `src/main.rs`: the TSP session ([tern-sdk](https://github.com/stencil-hq/tern-sdk)), key to
  `nvim_input` notation, bracketed paste to `nvim_paste`, and pty resize to `nvim_ui_try_resize`.
  It renders on each `flush`. The SDK sends only the changed rows and respects frame credits.
- Command line (`ext_cmdline`): while nvim edits a `:`, `/` or `input()` line, it shows as a
  focused Tern `input` field in a top-center overlay styled like Tern's command palette (640 px,
  16 px corners, a stylesheet named `palette`). The overlay is not modal, so the grid stays live
  (incsearch). The hello lists the `edit` feature, so
  Tern keeps a native selection in the field (Shift+arrows, Cmd+A, Cmd+C, drag, click to place the
  caret). Each `edit` event is applied to the local copy (UTF-16 offsets to bytes; a stale `len` is
  dropped) and sent to nvim with `setcmdline()`. Typed keys still go to `nvim_input`. Turn on
  Settings › Terminal › Native composer editing for the selection.
- Popup menu (`ext_popupmenu`): command-line completion is a list under the palette input.
  Insert-mode completion is a small overlay anchored at the menu column of the cursor row (Tern
  flips it above near the bottom). A click selects an item; a double-click also accepts it.
- blink.cmp: blink draws its menu in its own float, not the built-in popup menu. After attach,
  neotern runs `src/blink.lua`. When blink loads, the script hides blink's menu float and sends
  the items and the selection to neotern as `popupmenu_*` redraws, so they show in the same Tern
  list (insert mode and command line). It also hides blink's documentation float and puts the
  resolved docs in the item `info` field. Each item shows a kind icon, the label and the source.
  The `info` of the selected item (blink docs or a native `info`) shows as markdown at the right
  of the list.
- Cursor (`mode_info_set`): the block cursor is a reversed cell. For a `vertical` or
  `horizontal` shape, the cursor cell is a separate row part with a 2 px CSS bar in the theme
  foreground color.
- Messages (`ext_messages`): a one-line message is a Tern toast (red for errors, yellow for
  warnings). A multi-line output (`:ls`, `:messages`, a Lua error) is a card at the bottom. The
  card stays until the next key, and the key also goes to nvim. A `confirm()` question shows
  above its prompt in the command-line card. With `g:neotern`, set `cmdheight=0`, because a
  config that sets it after attach leaves empty rows.
- Tabs (`ext_tabline`): a Tern tab strip over the grid shows the tab pages, or the listed
  buffers while there is one tab page (like barbar). Each tab has its nvim-web-devicons icon. A
  click goes to that tab page or buffer.
- Status line and breadcrumbs: after attach, neotern runs `src/status.lua`. Every 100 ms it
  renders the focused window's 'statusline' (`lualine.statusline(true)`, or the window's own) and
  barbecue.nvim's breadcrumbs with `nvim_eval_statusline`, and sends the highlight runs when they
  change. The status line goes to a Tern status strip at the bottom ('laststatus' is 0). The
  middle part (after a first `%=`) is centered. The breadcrumbs go to one line under the tabs, and
  barbecue's own winbar is turned off. The colors are nvim's foreground colors.
- Windows stacked over each other still get one status row from nvim, whatever 'laststatus' says.
  So `src/status.lua` calls `lualine.hide`, sets 'statusline' to `%=` (which draws nothing), fills
  it with `─` ('fillchars' `stl` and `stlnc`) and colors `StatusLine` like `WinSeparator`: the row
  is a hairline, as between windows side by side.
- Hover and signature help: after attach, neotern runs `src/float.lua`, which wraps
  `vim.lsp.util.open_floating_preview`. It hides the window nvim made and sends the text, so `K`,
  signature help and the diagnostic float show as a Tern markdown card under the cursor, with
  highlighted code blocks. The buffer and window stay, so nvim's own close events still work.
- Telescope: after attach, neotern runs `src/telescope.lua`. It hides Telescope's prompt, results
  and preview windows (a hidden window stays valid, so Telescope keeps filling their buffers) and
  sends the state on each update: the title, the prompt, the entries, the selection and the
  preview. Tern draws it as a `picker` sheet with a search head, rows (the directory dims) and a
  preview pane of highlighted code with line numbers and a mark on the matched line. A click
  selects a row and a double click opens it. It also sets `sorting_strategy = 'ascending'`, so the
  best match is the first row and `<Down>` walks down, and `preview_cutoff = 0`, because the sheet
  always has room.
- `g:neotern` is 1 before your config runs (`--cmd`), like `g:neovide`. Use it to skip plugins
  that also take over the command line or popup menu. For example, noice.nvim stops with an
  error when a UI uses `ext_cmdline`, so set `cond = not vim.g.neotern` on its lazy.nvim spec.
  nvim-treesitter-context is off as well: its context float drew over the first text row, and the
  breadcrumbs already name the cursor's scope.
- Windows (`ext_multigrid`): each window has its own grid, and each one is a Tern `editor` of that
  grid's text, so the text is real text: Tern draws the caret, keeps a selection and lands a click
  on a character. The `layout` sheet puts every window box at its cell position (pixels of the cell
  size the `resize` event gives), so nvim still lays the screen out. `nowrap` and `followCursor`
  are off, because the text is what the window already shows: nvim owns the wrapping and the
  scrolling. Each float (`win_float_pos`) is the same editor in a card over the caret, in
  `compindex` order, and `grid_destroy`, `win_hide` and `win_close` take them down.
- The cursor: a block or replace cursor is a decoration over its cell, in the terminal theme's
  cursor color, as nvim draws it. In insert mode it is Tern's own caret, a bar between two
  characters. A cell that carries a background of its own (the visual selection, a search match, a
  diff) is marked with nvim's `Visual` background.
- Mouse: a click in a window arrives as an `edit` event with the caret's offset. neotern turns the
  offset back into a cell and sends `nvim_input_mouse` for that window's grid, so nvim resolves the
  buffer position itself, folds, signs and wrapping included. A click in another window asks for
  the keys (a `focus` event), which becomes `nvim_set_current_win`.
- which-key: after attach, neotern runs `src/keys.lua`. It hides which-key's windows and sends the
  keys that can follow as a `neotern_keys` redraw, which shows as a card at the bottom: one row per
  key, with its keycap and description, and a folder icon for a group.

## Limits

- Cursor: `cell_percentage`, blink and the cursor highlight are ignored.
- Messages: Tern shows one toast at a time for about 3 s and ignores `ttl`. 'showmode' and
  `recording @q` are toasts too, so they go after about 3 s. A message card shows only the last
  30 lines. 'showcmd' and 'ruler' are not drawn.
- The loop polls pane input every 4 ms, so a redraw can wait up to 4 ms.
- The grid is 6 rows and 4 columns smaller than the pty at 8 x 16 px cells: the screen surface
  loses 88 px of height and 28 px of width to Tern's chrome and the bars (`CHROME_PX` and
  `CHROME_W`, measured with `tern shot`).
- Bars: they show nvim's foreground colors only, not the background colors. Only the current
  window's status line shows. The breadcrumbs come only from barbecue.nvim, through its
  internals (`barbecue.ui.components`), and they are not shortened to fit. Two tabs with the
  same file name have the same label.
- Windows: a cell's background becomes one mark, so a search match looks like the selection, and
  `reverse` cells are marked too. Tern's native editing owns a few keys in a focused window
  (shift with the arrows, ⌘A, ⌘C), which therefore never reach nvim; the user setting is
  Settings › Terminal › Native composer editing. A window wider or taller than the surface is
  clipped, not scrolled.
- Underline, undercurl and strikethrough cells lose their line: a run keeps its color, its weight
  and its italics only.
- Floats: a float keeps its own border cells inside Tern's card, so a bordered float shows both,
  and the card hangs under the caret instead of the cell nvim placed it at. `src/blink.lua` hides
  blink's menu scrollbar, which is two 1-cell floats.
- Command line: Ctrl+Z is a plain key (nvim's command line has no undo). Only the innermost
  command line shows (`<C-r>=` replaces the outer one). Multi-line blocks (`cmdline_block_*`,
  for example `:function`) and the `<C-v>` marker (`cmdline_special_char`) are not drawn.
- Popup menu: the palette and menu styles use Tern's inner classes, which can change. The list is
  not virtualized, because Tern's virtual rows got a 44 px pitch for 22 px rows. The blink.cmp
  bridge wraps blink v1 internals (`completion.windows.menu`, `windows.documentation`), so a
  blink rewrite can break it. blink's `scroll_documentation_*` keys do nothing.

Planned: `ext_messages` (toasts),
`ext_tabline` (tabs), `ext_multigrid` floats (overlays), and a Tern plugin with a
"New Neovim pane" command.
