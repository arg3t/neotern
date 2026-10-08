-- neotern runs this once after attach. Snacks draws its picker in four floats of cells (a backdrop
-- box, the input, the list and the preview); Tern has a native picker sheet. So hide Snacks'
-- windows and send its state as the same `neotern_picker` redraw `telescope.lua` sends:
-- `{ title, prompt, rows, selected, preview, filetype, total, pane, first, at }`, or `{}` when the
-- picker closes. A hidden window stays valid, so Snacks keeps filling its buffers.
-- Only floating layouts become a sheet. A sidebar picker (the explorer) lives in a split.
-- ponytail: wraps snacks internals (`snacks.picker.core.{picker,list,preview}`, `list:_move`,
-- `list:format`); a Snacks rewrite breaks it, like `blink.lua` does for blink.
local MAX_ROWS = 100

local hooked, dirty, window

local function send(ev)
  local ui = vim.api.nvim_list_uis()[1]
  if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

--- A picker that is a float and not yet closed, the only kind the sheet draws.
local function sheet(p)
  if not (p and p.layout and not p.closed and p.shown and p.list and p.list.win) then return false end
  local root = p.layout.root
  return root ~= nil and root.opts ~= nil and root.opts.position == 'float'
end

local function hide(p)
  local wins = { p.layout.root }
  vim.list_extend(wins, vim.tbl_values(p.layout.box_wins))
  vim.list_extend(wins, vim.tbl_values(p.layout.wins))
  for _, win in ipairs(wins) do
    local id = win.win
    if id and vim.api.nvim_win_is_valid(id) then pcall(vim.api.nvim_win_set_config, id, { hide = true }) end
  end
end

--- The rows (`{ id, text }`) around the cursor, the selected one, and where they start in the list.
local function rows_of(p)
  local list = p.list
  local count = list:count()
  local first = math.max(1, math.min(list.cursor - math.floor(MAX_ROWS / 3), count - MAX_ROWS + 1))
  local rows, seen = {}, {}
  for idx = first, math.min(count, first + MAX_ROWS - 1) do
    local item = list:get(idx)
    if item then
      local ok, text = pcall(function() return (list:format(item)) end)
      text = ok and text or tostring(item.text or '')
      -- An id names a row for Tern, so two rows with the same text get a suffix.
      local id = tostring(item.text or text)
      seen[id] = (seen[id] or 0) + 1
      if seen[id] > 1 then id = id .. '\0' .. seen[id] end
      rows[#rows + 1] = { id, text }
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

local function emit(p)
  if dirty then return end
  dirty = true
  vim.schedule(function()
    dirty = false
    if not sheet(p) then return end
    hide(p)
    local rows, first, sel = rows_of(p)
    local preview, ft, from, at = preview_of(p)
    window = { picker = p, first = first }
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
    if sheet(self) then hide(self) end
    emit(self)
  end)
  wrap(Picker, 'update', function(self) emit(self) end)
  wrap(Picker, 'update_titles', function(self) emit(self) end)
  wrap(List, 'render', function(self) emit(self.picker) end)
  wrap(Preview, 'show', function(_, picker) emit(picker) end)
  local close = Picker.close
  Picker.close = function(self, ...)
    local was = window and window.picker == self
    close(self, ...)
    if was then
      window = nil
      send({ 'neotern_picker', {} })
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
  if not (p and not p.closed and p.list) then
    if other then return other(i, accept) end
    return
  end
  p.list:_move(window.first + i - 1, true, true)
  if accept then p:action('confirm') end
end

hook()
vim.api.nvim_create_autocmd('User', { pattern = { 'VeryLazy', 'LazyDone' }, callback = hook })
vim.api.nvim_create_autocmd('VimEnter', { callback = hook })
