-- neotern runs this once after attach. which-key draws the keys that can follow the one you pressed
-- in a floating window of cells, which `ext_multigrid` would show as a card of terminal rows. So
-- hide its windows and send the keys as a `neotern_keys` redraw: `{ title, { { key, desc, group },
-- ... } }`, or `{}` when it closes.
-- ponytail: wraps which-key's `view` module (`state`, `expand`, `item`); a rewrite breaks it, as
-- with `blink.lua` and `telescope.lua`.
local hooked

local function send(ev)
  local ui = vim.api.nvim_list_uis()[1]
  if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

local function hide(view)
  for _, win in ipairs({ view.view, view.footer }) do
    local id = type(win) == 'table' and win.win
    if id and vim.api.nvim_win_is_valid(id) then pcall(vim.api.nvim_win_set_config, id, { hide = true }) end
  end
end

--- The keys which-key would show: its own items, rebuilt from the node it is on.
local function keys(view)
  local state = require('which-key.state').state
  if not (state and state.node) then return nil end
  local yes = function() return true end
  local items = {}
  for _, node in ipairs(state.node:children()) do
    vim.list_extend(items, view.expand(state.node, node, yes, yes))
  end
  view.sort(items)
  local out = {}
  for _, item in ipairs(items) do
    out[#out + 1] = { tostring(item.key or ''), tostring(item.desc or ''), item.group == true }
  end
  return { tostring(state.node.keys or ''), out }
end

local function hook()
  if hooked or not package.loaded['which-key'] then return end
  local ok, view = pcall(require, 'which-key.view')
  if not (ok and view.show) then return end
  hooked = true
  local show, hide_all = view.show, view.hide
  view.show = function(...)
    show(...)
    hide(view)
    local ev = keys(view)
    send({ 'neotern_keys', ev or {} })
  end
  view.hide = function(...)
    hide_all(...)
    send({ 'neotern_keys', {} })
  end
end

-- which-key loads late and `view` later still, so keep trying until the wrap is in place.
local timer = vim.uv.new_timer()
timer:start(0, 500, vim.schedule_wrap(function()
  hook()
  if hooked then timer:stop() end
end))
