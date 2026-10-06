-- neotern runs this once after attach. Telescope draws its prompt, results and preview in three
-- windows of cells; Tern has a native picker sheet. So hide Telescope's windows and send its state
-- as a `neotern_picker` redraw: `{ title, prompt, rows, selected, preview, filetype, total }`,
-- or `{}` when the picker closes. A hidden window stays valid, so Telescope keeps filling its
-- buffers (the previewer checks the window, not its visibility).
-- ponytail: wraps telescope internals (`pickers._Picker`, `picker.manager`, `previewer.state`); a
-- telescope rewrite breaks it, like `blink.lua` does for blink.
local Picker, hooked, dirty

local function send(ev)
  local ui = vim.api.nvim_list_uis()[1]
  if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

local function hide(p)
  for _, win in ipairs({ p.prompt_win, p.results_win, p.preview_win }) do
    if win and vim.api.nvim_win_is_valid(win) then pcall(vim.api.nvim_win_set_config, win, { hide = true }) end
  end
  for _, border in ipairs({ p.prompt_border, p.results_border, p.preview_border }) do
    local win = type(border) == 'table' and border.winid
    if win and vim.api.nvim_win_is_valid(win) then pcall(vim.api.nvim_win_set_config, win, { hide = true }) end
  end
end

--- The rows (`{ id, text }`) in the order Telescope draws them, the selected row, and the slice
--- of the preview around its cursor.
local function state(p)
  local resolve = require('telescope.pickers.entry_display').resolve
  -- `manager:iter()` yields the entry only, and a row is 0-based while an index is 1-based.
  local rows, sel, i = {}, 0, 0
  for entry in p.manager:iter() do
    i = i + 1
    local ok, text = pcall(resolve, p, entry)
    text = ok and text or tostring(entry.value or '')
    -- The ordinal is the text the sorter matched, so a row keeps its identity while you type.
    rows[i] = { tostring(entry.ordinal or text), text }
    if p:get_row(i) == p._selection_row then sel = i end
  end
  -- `move_selection` steps by row, so the sheet must follow the rows, not the match order: with
  -- 'descending' the best match is the last row, and <Down> walks towards the worse ones.
  if p.sorting_strategy == 'descending' then
    local flipped = {}
    for j = #rows, 1, -1 do
      flipped[#flipped + 1] = rows[j]
    end
    rows, sel = flipped, sel > 0 and #rows - sel + 1 or 0
  end
  local buf = p.previewer and p.previewer.state and p.previewer.state.bufnr
  if not (buf and vim.api.nvim_buf_is_valid(buf)) then return rows, sel, '', '', 1, 0 end
  -- The previewer put the match under the hidden preview window's cursor.
  local win = p.preview_win
  local at = win and vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == buf
    and vim.api.nvim_win_get_cursor(win)[1]
    or 1
  local last = vim.api.nvim_buf_line_count(buf)
  local from = math.max(1, math.min(at - 8, last - 48))
  local lines = vim.api.nvim_buf_get_lines(buf, from - 1, from + 47, false)
  -- The previewer leaves 'filetype' empty when it highlights with treesitter alone.
  local ft = vim.bo[buf].filetype
  if ft == '' then
    local entry = p._selection_entry or {}
    local name = entry.filename or entry.path or entry.value
    ft = type(name) == 'string' and (vim.filetype.match({ filename = name }) or '') or ''
  end
  return rows, sel, table.concat(lines, '\n'), ft, from, at
end

local function emit(p)
  if dirty or not (p and p.manager and p.prompt_bufnr) then return end
  dirty = true
  vim.schedule(function()
    dirty = false
    if not (p.manager and vim.api.nvim_buf_is_valid(p.prompt_bufnr)) then return end
    hide(p)
    local rows, sel, preview, ft, from, at = state(p)
    local ev = { p.prompt_title or 'Find', p:_get_prompt() or '', rows, sel, preview, ft,
      p.stats.processed or #rows, p.previewer ~= nil, from, at }
    send({ 'neotern_picker', ev })
  end)
end

local function hook()
  if hooked or not package.loaded['telescope.pickers'] then return end
  Picker = require('telescope.pickers')._Picker
  hooked = true
  -- Tern's sheet always has room for the preview, so drop Telescope's width cutoff. Sorting
  -- ascending puts the best match in the first row, where <Down> walks towards the worse ones.
  local values = require('telescope.config').values
  values.layout_config.preview_cutoff = 0
  values.sorting_strategy = 'ascending'
  local find, adder, select, preview = Picker.find, Picker.entry_adder, Picker.set_selection, Picker.refresh_previewer
  Picker.find = function(self, ...)
    find(self, ...)
    emit(self)
  end
  Picker.entry_adder = function(self, ...)
    adder(self, ...)
    emit(self)
  end
  Picker.set_selection = function(self, ...)
    select(self, ...)
    emit(self)
  end
  Picker.refresh_previewer = function(self, ...)
    preview(self, ...)
    emit(self)
  end
  -- `close_windows(status)` is a plain function, not a method.
  local close = Picker.close_windows
  Picker.close_windows = function(status)
    close(status)
    send({ 'neotern_picker', {} })
  end
end

-- A click on a Tern row: row `i` (1-based) is selected, and accepted on a double-click.
function _G.neotern_pick(i, accept)
  local st = require('telescope.state')
  for _, bufnr in ipairs(st.get_existing_prompt_bufnrs()) do
    local p = st.get_status(bufnr).picker
    if p and p.manager then
      p:set_selection(p:get_row(i))
      if accept then require('telescope.actions').select_default(bufnr) end
    end
  end
end

hook()
vim.api.nvim_create_autocmd('User', { pattern = 'TelescopeFindPre', callback = hook })
vim.api.nvim_create_autocmd('User', { pattern = 'LazyLoad', callback = hook })
