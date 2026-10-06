# neotern

Neovim as a native [Tern](https://docs.stencil.so/tern) surface, in the style of Neovide.

neotern runs `nvim --embed` and attaches as an `ext_linegrid` UI over msgpack-RPC. It does not
parse nvim's terminal output. It keeps the grid in memory and draws it as a Tern Surface
Protocol (TSP) `screen` surface, with one `ansi` node per grid row.

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
  `grid_clear`, `grid_destroy`, `grid_cursor_goto`, `hl_attr_define`, `default_colors_set`). It
  renders a row of any grid as truecolor SGR.
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
  evaluates the current window's 'statusline' (for example lualine's) and barbecue.nvim's
  breadcrumbs with `nvim_eval_statusline`, and sends the highlight runs when they change. The
  status line goes to a Tern status strip at the bottom ('laststatus' is set to 0). The middle
  part (after a first `%=`) is centered. The breadcrumbs go to one line under the tabs, and
  barbecue's own winbar is turned off. The colors are nvim's foreground colors.
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
- Windows (`ext_multigrid`): each window has its own grid. A split is composited onto the screen
  at the position of its `win_pos`, so the screen rows hold the splits, the separators and
  anything grid 1 draws. Each float (`win_float_pos`) is its own Tern card in the layer, anchored
  to the cell nvim placed it at (`screen_row`, `screen_col`), in `compindex` order. So hover,
  signature help and Telescope are real cards over the text, and `grid_destroy`, `win_hide` and
  `win_close` take them down.

## Limits

- Cursor: `cell_percentage`, blink and the cursor highlight are ignored.
- Messages: Tern shows one toast at a time for about 3 s and ignores `ttl`. 'showmode' and
  `recording @q` are toasts too, so they go after about 3 s. A message card shows only the last
  30 lines. 'showcmd' and 'ruler' are not drawn.
- The loop polls pane input every 4 ms, so a redraw can wait up to 4 ms.
- The grid is 8 rows shorter than the pty at 16 px cells: the screen surface loses 114 px to
  Tern's chrome and the bars (`CHROME_PX`, measured with `tern shot`).
- Bars: they show nvim's foreground colors only, not the background colors. Only the current
  window's status line shows. The breadcrumbs come only from barbecue.nvim, through its
  internals (`barbecue.ui.components`), and they are not shortened to fit. Two tabs with the
  same file name have the same label.
- Mouse input and undercurl colors are not supported.
- Floats: a float keeps its own border cells inside Tern's card, so a bordered float shows both.
  A float does not blend with the text under it, and a float wider than the surface is clipped by
  Tern, not re-positioned. `src/blink.lua` hides blink's menu scrollbar, which is two 1-cell
  floats.
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
