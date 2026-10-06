-- neotern runs this once after attach. nvim draws hover (`K`), signature help and the diagnostic
-- float as a window of cells; Tern can draw the same markdown natively. So wrap
-- `vim.lsp.util.open_floating_preview`: hide the window it made and send its text as a
-- `neotern_doc` redraw, `{ markdown, syntax }`, or `{}` when the window closes.
-- ponytail: only this one entry point; a plugin that opens its own float still draws cells.
local shown

local function send(ev)
  local ui = vim.api.nvim_list_uis()[1]
  if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

local function clear(win)
  if shown == win then
    shown = nil
    send({ 'neotern_doc', {} })
  end
end

local open = vim.lsp.util.open_floating_preview
vim.lsp.util.open_floating_preview = function(contents, syntax, opts)
  local buf, win = open(contents, syntax, opts)
  -- The caller keeps the real buffer and window (its close events and `_update_win` still work).
  if not (win and vim.api.nvim_win_is_valid(win)) then return buf, win end
  pcall(vim.api.nvim_win_set_config, win, { hide = true })
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  shown = win
  send({ 'neotern_doc', { table.concat(lines, '\n'), syntax or '' } })
  vim.api.nvim_create_autocmd('WinClosed', {
    pattern = tostring(win),
    once = true,
    callback = function() clear(win) end,
  })
  return buf, win
end
