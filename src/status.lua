-- neotern runs this once after attach. It sends, as one `neotern_status` redraw:
-- 1. the current window's 'statusline' (lualine's, or any); nvim's own is off ('laststatus' 0);
-- 2. barbecue.nvim's breadcrumbs of the current window; barbecue's winbar is off;
-- 3. the nvim-web-devicons icon of each buffer's file name, for the tabs: `{ name, glyph, fg }`.
-- 1 and 2 are highlight runs `{ side, text, fg, bold }`: side 0 is left, 1 the middle (after a
-- first `%=`), 2 right (after the last `%=`).
-- ponytail: polls every 100ms and sends on change; lualine itself refreshes on a timer too.
local FILL = '⠀' -- U+2800: a fillchar no status line prints, so it marks the `%=` gaps.
local DEFAULT = '%<%f %h%m%r%=%-14.(%l,%c%V%) %P'
local last, barbecue

local function send(ev)
  local ui = vim.api.nvim_list_uis()[1]
  if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

local function runs(fmt, win, winbar)
  if fmt == '' then return {} end
  local opts = { winid = win, highlights = true, fillchar = FILL, use_winbar = winbar }
  local ok, r = pcall(vim.api.nvim_eval_statusline, fmt, opts)
  if not ok then return {} end
  local out, side, gap, hls = {}, 0, false, r.highlights
  for i, h in ipairs(hls) do
    local text = r.str:sub(h.start + 1, hls[i + 1] and hls[i + 1].start or #r.str)
    local hl = vim.api.nvim_get_hl(0, { name = h.group, link = false })
    -- A run of FILLs is one gap, and it can span highlight runs.
    for j, part in ipairs(vim.split(text, FILL, { plain = true })) do
      if j > 1 then gap = true end
      if part ~= '' then
        if gap then side, gap = side + 1, false end
        out[#out + 1] = { side, part, hl.fg or -1, hl.bold or false }
      end
    end
  end
  -- One gap: what follows it is the right side, not the middle.
  if side == 1 then
    for _, run in ipairs(out) do run[1] = run[1] * 2 end
  end
  return out
end

-- barbecue's own winbar string, built from its components. ponytail: wraps barbecue internals
-- (ui.components, theme.highlights) and skips its truncation; a barbecue rewrite breaks it.
local function crumbs(win)
  if not package.loaded.barbecue then return '' end
  local ok, s = pcall(function()
    local config, theme = require('barbecue.config'), require('barbecue.theme')
    local comp = require('barbecue.ui.components')
    if not barbecue then
      require('barbecue.ui').toggle(false)
      barbecue = true
    end
    local buf = vim.api.nvim_win_get_buf(win)
    if vim.api.nvim_buf_get_name(buf) == ''
      or not vim.tbl_contains(config.user.include_buftypes, vim.bo[buf].buftype)
      or vim.tbl_contains(config.user.exclude_filetypes, vim.bo[buf].filetype)
    then
      return ''
    end
    local entries = vim.list_extend(comp.dirname(buf), { comp.basename(win, buf) })
    local parts = vim.tbl_map(function(e) return e:to_string() end, vim.list_extend(entries, comp.context(win, buf)))
    local h = theme.highlights
    local sep = ('%%#%s# %%#%s#%s%%#%s# '):format(h.normal, h.separator, config.user.symbols.separator, h.normal)
    return table.concat(parts, sep)
  end)
  return ok and s or ''
end

local function icons()
  local ok, devicons = pcall(require, 'nvim-web-devicons')
  if not ok then return {} end
  local out, seen = {}, {}
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    local name = vim.fn.fnamemodify(vim.api.nvim_buf_get_name(b), ':t')
    if name ~= '' and not seen[name] then
      seen[name] = true
      local glyph, color = devicons.get_icon_color(name, nil, { default = true })
      if glyph then out[#out + 1] = { name, glyph, tonumber((color or ''):sub(2), 16) or -1 } end
    end
  end
  return out
end

local function tick()
  if vim.o.laststatus ~= 0 then vim.o.laststatus = 0 end
  local win = vim.api.nvim_get_current_win()
  local stl = vim.wo[win].statusline
  local ev = { runs(stl ~= '' and stl or DEFAULT, win, false), runs(crumbs(win), win, true), icons() }
  local key = vim.inspect(ev)
  if key ~= last then
    last = key
    send({ 'neotern_status', ev })
  end
end

vim.uv.new_timer():start(0, 100, vim.schedule_wrap(tick))
