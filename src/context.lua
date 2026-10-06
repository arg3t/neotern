-- neotern runs this once after attach. nvim-treesitter-context draws the enclosing line (a struct,
-- a function) in two floating windows over the first text row, which `ext_multigrid` would draw as
-- two cards. So hide those windows and send the line as a `neotern_context` redraw:
-- `{ { { text, fg, bold }, ... } }`, one list of highlight runs per context line, or `{}` when the
-- context goes.
-- ponytail: wraps the plugin's `render` module and its window flags; a rewrite breaks it, as with
-- `blink.lua` and `telescope.lua`.
local hooked

local function send(ev)
  local ui = vim.api.nvim_list_uis()[1]
  if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

--- The context windows, by the window-local flag the plugin sets on each (`render.lua`).
--- Returns the line-number window first, then the code window.
local function windows()
  local gutter, code
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    if vim.w[win].treesitter_context_line_number then gutter = win end
    if vim.w[win].treesitter_context then code = win end
  end
  return gutter, code
end

--- The runs of row `row` of `buf`: its extmark highlights as `{ text, fg, bold }`.
local function runs(buf, row, out)
  local line = vim.api.nvim_buf_get_lines(buf, row, row + 1, false)[1]
  if not line or line == '' then return end
  local marks = vim.api.nvim_buf_get_extmarks(buf, -1, { row, 0 }, { row, -1 }, { details = true })
  local at = 0
  local function push(to, group)
    if to <= at then return end
    local hl = group and vim.api.nvim_get_hl(0, { name = group, link = false }) or {}
    out[#out + 1] = { line:sub(at + 1, to), hl.fg or -1, hl.bold or false }
    at = to
  end
  for _, mark in ipairs(marks) do
    local col, details = mark[3], mark[4]
    local to = details.end_col or #line
    -- A gap between marks keeps the plain text, so nothing is dropped.
    push(col, nil)
    push(math.min(to, #line), details.hl_group)
  end
  push(#line, nil)
end

local function emit()
  local gutter, code = windows()
  if not (code and vim.api.nvim_win_is_valid(code)) then return send({ 'neotern_context', {} }) end
  for _, win in ipairs({ gutter, code }) do
    if win and vim.api.nvim_win_is_valid(win) then pcall(vim.api.nvim_win_set_config, win, { hide = true }) end
  end
  local code_buf = vim.api.nvim_win_get_buf(code)
  local gutter_buf = gutter and vim.api.nvim_win_is_valid(gutter) and vim.api.nvim_win_get_buf(gutter)
  local lines = {}
  for row = 0, vim.api.nvim_buf_line_count(code_buf) - 1 do
    local out = {}
    if gutter_buf then runs(gutter_buf, row, out) end
    runs(code_buf, row, out)
    if #out > 0 then lines[#lines + 1] = out end
  end
  send({ 'neotern_context', { lines } })
end

local function hook()
  if hooked or not package.loaded['treesitter-context.render'] then return end
  hooked = true
  local render = require('treesitter-context.render')
  local open, close, close_all = render.open, render.close, render.close_contexts
  render.open = function(...)
    open(...)
    emit()
  end
  -- The plugin closes its windows in a `vim.schedule`, so clear the row now instead of looking.
  render.close = function(...)
    close(...)
    send({ 'neotern_context', {} })
  end
  render.close_contexts = function(...)
    close_all(...)
    send({ 'neotern_context', {} })
  end
end

hook()
vim.api.nvim_create_autocmd('User', { pattern = 'LazyLoad', callback = hook })
vim.api.nvim_create_autocmd('CursorMoved', { once = true, callback = hook })
