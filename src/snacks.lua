-- neotern runs this once after attach. Snacks draws a picker in floats of cells (a backdrop box, the
-- input, the list and the preview); Tern has native elements for that. A hidden window stays valid,
-- so the floats are hidden and Snacks keeps filling its buffers. Two shapes:
--  * A floating layout becomes Tern's picker sheet, as `telescope.lua` does: a `neotern_picker`
--    redraw, `{ title, prompt, rows, selected, preview, filetype, total, pane, first, at }`, or `{}`
--    when the picker closes.
--  * A sidebar layout (the explorer) lives in a split that nvim keeps in the layout, so Tern draws
--    its list as a native list in that split: a `neotern_tree` redraw, `{ window, rows, selected }`
--    with a row `{ id, label, index, depth, glyph, color }`, or `{}` when it closes.
-- ponytail: wraps snacks internals (`snacks.picker.core.{picker,list,preview}`, `list:_move`,
-- `list:format`); a Snacks rewrite breaks it, like `blink.lua` does for blink.
local MAX_ROWS = 100
local MAX_TREE = 2000

local hooked, dirty, window

local function send(ev)
  local ui = vim.api.nvim_list_uis()[1]
  if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

--- 'sheet' for a floating picker, 'tree' for a sidebar one, nil when it is closed or not shown.
local function kind(p)
  if not (p and p.layout and not p.closed and p.shown and p.list and p.list.win) then return nil end
  local root = p.layout.root
  local position = root and root.opts and root.opts.position
  if position == 'float' then return 'sheet' end
  if position == 'left' or position == 'right' then return 'tree' end
end

--- Hides every float of the picker, and the backdrop each window dims what is behind it with. A
--- split stays: nvim keeps it in the layout and Tern draws it.
local function hide(p)
  local wins = { p.layout.root }
  vim.list_extend(wins, vim.tbl_values(p.layout.box_wins))
  vim.list_extend(wins, vim.tbl_values(p.layout.wins))
  for _, win in ipairs(wins) do
    for _, w in ipairs({ win, win.backdrop or false }) do
      local id = w and w.win
      if id and vim.api.nvim_win_is_valid(id) and vim.api.nvim_win_get_config(id).relative ~= '' then
        pcall(vim.api.nvim_win_set_config, id, { hide = true })
      end
    end
  end
end

local hl_cache = {}
vim.api.nvim_create_autocmd('ColorScheme', { callback = function() hl_cache = {} end })

-- A run's foreground is an RGB value, -1 for none, or one of these: a name for a Tern token, which
-- reads on the sheet's own background in either theme, where nvim's colors assume nvim's.
local DIM, MATCH = 0x1000001, 0x1000002
local TOKENS = {
  SnacksPickerDir = DIM,
  SnacksPickerDimmed = DIM,
  SnacksPickerPathHidden = DIM,
  SnacksPickerPathIgnored = DIM,
  SnacksPickerMatch = MATCH,
}

--- A highlight group's foreground (-1 for none, or a token) and weight.
local function style(group)
  local cached = hl_cache[group]
  if not cached then
    local hl = vim.api.nvim_get_hl(0, { name = group, link = false })
    cached = { TOKENS[group] or hl.fg or -1, hl.bold or false }
    hl_cache[group] = cached
  end
  return cached
end

--- The formatted row as colored runs `{ text, foreground, bold }`, from the text Snacks puts in its
--- list and the extmarks that color it: a group over a byte range, and the icon as virtual text
--- laid over the spaces that hold its place. A later extmark wins, so a match shows over its file.
local function chunks_of(text, extmarks)
  local fg, bold, overlay = {}, {}, {}
  for _, mark in ipairs(extmarks) do
    local col = mark.col or 0
    if mark.virt_text and mark.virt_text_pos == 'overlay' then
      local run, group = mark.virt_text[1][1], mark.virt_text[1][2]
      group = type(group) == 'table' and group[#group] or group
      local st = group and style(group) or { -1, false }
      overlay[col + 1] = { run, st[1], st[2], vim.fn.strdisplaywidth(run) }
    elseif mark.hl_group and mark.end_col then
      local groups = type(mark.hl_group) == 'table' and mark.hl_group or { mark.hl_group }
      for _, group in ipairs(groups) do
        local st = style(group)
        for i = col + 1, math.min(mark.end_col, #text) do
          if st[1] ~= -1 then fg[i] = st[1] end
          if st[2] then bold[i] = true end
        end
      end
    end
  end
  local runs, buf, cur_fg, cur_bold = {}, {}, -1, false
  local function flush()
    if #buf > 0 then runs[#runs + 1] = { table.concat(buf), cur_fg, cur_bold } end
    buf = {}
  end
  local i = 1
  while i <= #text do
    local icon = overlay[i]
    if icon then
      flush()
      runs[#runs + 1] = { icon[1], icon[2], icon[3] }
      i = i + icon[4]
    else
      local f, b = fg[i] or -1, bold[i] or false
      if f ~= cur_fg or b ~= cur_bold then
        flush()
        cur_fg, cur_bold = f, b
      end
      buf[#buf + 1] = text:sub(i, i)
      i = i + 1
    end
  end
  flush()
  -- The first column holds the selection mark, and the end is padding.
  if runs[1] then runs[1][1] = runs[1][1]:gsub('^%s+', '') end
  if runs[#runs] then runs[#runs][1] = runs[#runs][1]:gsub('%s+$', '') end
  return vim.tbl_filter(function(run) return run[1] ~= '' end, runs)
end

--- The rows (`{ id, text, runs }`) around the cursor, the selected one, and where they start in the list.
local function rows_of(p)
  local list = p.list
  local count = list:count()
  local first = math.max(1, math.min(list.cursor - math.floor(MAX_ROWS / 3), count - MAX_ROWS + 1))
  local rows, seen = {}, {}
  for idx = first, math.min(count, first + MAX_ROWS - 1) do
    local item = list:get(idx)
    if item then
      local ok, text, extmarks = pcall(list.format, list, item)
      local runs = ok and chunks_of(text, extmarks) or {}
      text = ok and text or tostring(item.text or '')
      -- An id names a row for Tern, so two rows with the same text get a suffix.
      local id = tostring(item.text or text)
      seen[id] = (seen[id] or 0) + 1
      if seen[id] > 1 then id = id .. '\0' .. seen[id] end
      rows[#rows + 1] = { id, text, runs }
    end
  end
  return rows, first, count > 0 and list.cursor - first + 1 or 0
end

--- The slice of the preview around the matched line, with its filetype.
local function preview_of(p)
  local pv = p.preview
  local win = pv and pv.win
  local buf = win and win.buf
  if not (buf and vim.api.nvim_buf_is_valid(buf) and pv.item) then return '', '', 1, 0 end
  local at = win:win_valid() and vim.api.nvim_win_get_cursor(win.win)[1] or 1
  local last = vim.api.nvim_buf_line_count(buf)
  local from = math.max(1, math.min(at - 8, last - 48))
  local lines = vim.api.nvim_buf_get_lines(buf, from - 1, from + 47, false)
  -- Snacks highlights the buffer with treesitter and leaves its own 'filetype' on it, so the
  -- item's file names the language.
  local name = pv.item.file
  local ft = type(name) == 'string' and vim.filetype.match({ filename = name }) or ''
  if ft == '' and not vim.bo[buf].filetype:find('^snacks_') then ft = vim.bo[buf].filetype end
  return table.concat(lines, '\n'), ft, from, at
end

--- The list as rows `{ id, label, index, depth, glyph, color }`: the id is the row's file, `index`
--- is its place in the list, and the glyph is the icon Snacks draws, with its foreground color (-1
--- for none).
local function tree_of(p)
  local list = p.list
  local icons = p.opts.icons.files
  local rows = {}
  for idx = 1, math.min(list:count(), MAX_TREE) do
    local item = list:get(idx)
    if item then
      Snacks.picker.util.resolve(item)
      local file = item.file or item.text or ''
      local depth, parent = 0, item.parent
      while parent do
        depth, parent = depth + 1, parent.parent
      end
      local glyph, hl = icons.dir_open, 'Directory'
      if not (item.dir and item.open) then
        glyph, hl = Snacks.util.icon(file, item.dir and 'directory' or 'file', { fallback = icons })
      end
      local fg = hl and vim.api.nvim_get_hl(0, { name = hl, link = false }).fg or -1
      local label = vim.fn.fnamemodify(file, ':t')
      rows[#rows + 1] = { file, label ~= '' and label or file, idx, depth, vim.trim(glyph or ''), fg }
    end
  end
  local current = list:current()
  return rows, current and (current.file or current.text) or ''
end

local function emit(p)
  if dirty then return end
  dirty = true
  vim.schedule(function()
    dirty = false
    local shape = kind(p)
    if not shape then return end
    hide(p)
    window = { picker = p, first = 1, kind = shape }
    if shape == 'tree' then
      local rows, selected = tree_of(p)
      send({ 'neotern_tree', { p.layout.root.win, rows, selected } })
      return
    end
    local rows, first, sel = rows_of(p)
    local preview, ft, from, at = preview_of(p)
    window.first = first
    send({ 'neotern_picker', { p.title or 'Find', p.input:get() or '', rows, sel, preview, ft,
      p:count(), p.preview ~= nil and p.preview.win ~= nil and p.preview.win:valid() or false, from, at } })
  end)
end

local function hook()
  if hooked or not _G.Snacks then return end
  local ok, Picker = pcall(require, 'snacks.picker.core.picker')
  local _, List = pcall(require, 'snacks.picker.core.list')
  local _, Preview = pcall(require, 'snacks.picker.core.preview')
  if not (ok and Picker and List and Preview) then return end
  hooked = true
  local wrap = function(class, name, after)
    local orig = class[name]
    class[name] = function(self, ...)
      local ret = orig(self, ...)
      after(self, ...)
      return ret
    end
  end
  wrap(Picker, 'show', function(self)
    if kind(self) then hide(self) end
    emit(self)
  end)
  wrap(Picker, 'update', function(self) emit(self) end)
  wrap(Picker, 'update_titles', function(self) emit(self) end)
  wrap(List, 'render', function(self) emit(self.picker) end)
  wrap(Preview, 'show', function(_, picker) emit(picker) end)
  local close = Picker.close
  Picker.close = function(self, ...)
    local was = window and window.picker == self and window.kind
    close(self, ...)
    if was then
      window = nil
      send({ was == 'tree' and 'neotern_tree' or 'neotern_picker', {} })
    end
  end
  vim.api.nvim_create_autocmd({ 'VimResized', 'WinResized' }, {
    callback = function() if window then emit(window.picker) end end,
  })
end

-- A click on a Tern row: row `i` (1-based) is selected, and accepted on a double-click. Rows are
-- shared with Telescope's sheet, so whatever else answers (`telescope.lua`) keeps the call.
local other = _G.neotern_pick
function _G.neotern_pick(i, accept)
  local p = window and window.picker
  if not (p and not p.closed and p.list and window.kind == 'sheet') then
    if other then return other(i, accept) end
    return
  end
  p.list:_move(window.first + i - 1, true, true)
  if accept then p:action('confirm') end
end

-- A click on a tree row: the row at list index `i` is selected and confirmed, which opens a file
-- and toggles a directory.
function _G.neotern_tree_pick(i)
  local p = window and window.picker
  if not (p and not p.closed and p.list and window.kind == 'tree') then return end
  p.list:_move(i, true, true)
  p:action('confirm')
end

hook()
vim.api.nvim_create_autocmd('User', { pattern = { 'VeryLazy', 'LazyDone' }, callback = hook })
vim.api.nvim_create_autocmd('VimEnter', { callback = hook })
