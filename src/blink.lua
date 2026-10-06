-- neotern runs this once after attach. blink.cmp draws its menu and its documentation in
-- floats, which ext_popupmenu never sees: hide the menu float, never open the docs float, and
-- send both as fake popupmenu_* redraws (the docs go in the selected item's `info`).
-- ponytail: wraps blink v1 internals (windows.menu, windows.documentation, completion.list); a
-- blink rewrite breaks it.
local menu
--- The resolved docs: `{ idx = menu index, label = item label, md = markdown }`.
local doc

local function send(ev)
	local ui = vim.api.nvim_list_uis()[1]
	if ui and ui.chan > 0 then vim.rpcnotify(ui.chan, 'redraw', ev, { 'flush', {} }) end
end

local function show()
	if not menu.win:is_open() then return end
	local kinds = require('blink.cmp.types').CompletionItemKind
	local items = {}
	for i, it in ipairs(menu.items or {}) do
		items[i] = { it.label, kinds[it.kind] or '', it.source_name or '', doc and doc.idx == i and doc.label == it.label and doc.md or '' }
	end
	local sel = (menu.selected_item_idx or 0) - 1
	if vim.fn.mode() == 'c' then
		send({ 'popupmenu_show', { items, sel, 0, 0, -1 } })
	else
		local b = menu.context.bounds
		local pos = vim.fn.screenpos(0, b.line_number, b.start_col)
		send({ 'popupmenu_show', { items, sel, pos.row - 1, pos.col - 1, 1 } })
	end
end

local function hook()
	if menu or not package.loaded['blink.cmp'] then return end
	menu = require('blink.cmp.completion.windows.menu')
	local open, close, items, select = menu.open, menu.close, menu.open_with_items, menu.set_selected_item_idx
	-- The menu's own scrollbar is two 1-cell floats, which `ext_multigrid` would draw as cards.
	local bar = menu.win.scrollbar
	if bar then bar.update = function(self) self.win:hide() end end
	menu.open = function()
		open()
		local win = menu.win:get_win()
		if win then vim.api.nvim_win_set_config(win, { hide = true }) end
		show()
	end
	menu.open_with_items = function(...)
		items(...)
		show()
	end
	menu.set_selected_item_idx = function(idx)
		select(idx)
		show()
	end
	menu.close = function()
		local was = menu.win:is_open()
		close()
		if was then send({ 'popupmenu_hide', {} }) end
	end
	local docs = require('blink.cmp.completion.windows.documentation')
	local docs_close = docs.close
	docs.show_item = function(context, item)
		docs.auto_show_timer:stop()
		if item == nil or not menu.win:is_open() then return docs.close() end
		-- blink hands over a copy of the menu item, so match it by its index and label.
		local idx = menu.selected_item_idx
		require('blink.cmp.sources.lib').resolve(context, item):map(function(it)
			local md, d = {}, it.documentation
			if it.detail and it.detail ~= '' then md[1] = '```' .. vim.bo.filetype .. '\n' .. it.detail .. '\n```' end
			if type(d) == 'table' then d = d.value end
			if d and d ~= '' then md[#md + 1] = d end
			doc = #md > 0 and { idx = idx, label = item.label, md = table.concat(md, '\n\n') } or nil
			show()
		end)
	end
	docs.close = function()
		docs_close()
		if doc then
			doc = nil
			show()
		end
	end
end

-- A click on a Tern menu row: item i (0-based) is selected, and accepted on a double-click.
function _G.neotern_select(i, accept)
	if menu and menu.win:is_open() then
		local list = require('blink.cmp.completion.list')
		if accept then list.accept({ index = i + 1 }) else list.select(i + 1) end
	else
		vim.api.nvim_select_popupmenu_item(i, true, accept, {})
	end
end

hook()
vim.api.nvim_create_autocmd('VimEnter', { callback = hook })
vim.api.nvim_create_autocmd('User', { pattern = 'LazyLoad', callback = hook })
