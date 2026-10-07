//! neotern: Neovim drawn as a native Tern surface. Runs `nvim --embed`, keeps the `ext_linegrid`
//! grid, and renders it as one `ansi` node per row in a `screen` surface.

mod grid;
mod nvim;

use std::{
	collections::BTreeSet,
	fmt::Write as _,
	env,
	error::Error,
	process::{Command, ExitCode},
	sync::mpsc,
	time::Duration,
};

use rmpv::Value;
use tern_sdk::{
	Input, Options, Session, SurfaceOptions,
	keys::Key,
	ui::{self, View},
	wire::{Event, feature},
};

use crate::{
	grid::{Grid, Shape},
	nvim::Nvim,
};

/// How long one wait for pane input lasts before nvim's redraws are drained.
// ponytail: polling, so redraws wait up to POLL; a wake pipe in a custom `term::Terminal` removes it.
const POLL: Duration = Duration::from_millis(4);

/// The node id of the cmdline field: `q` in the overlay keyed `cmd` of the layer region.
const CMD_ID: &str = "layer.cmd.q";

/// The node id of the tab strip, keyed `tabs` above the grid rows.
const TABS_ID: &str = "main.0.tabs";

/// The node id of the Telescope picker sheet, keyed `pick` in the layer region.
const PICK_ID: &str = "layer.pick";

/// The cmdline card takes the look of Tern's command palette (`.cmdk`): 640px wide, 16px
/// corners, a flat 56px query row over a rule. The buffer menu is a compact 260px list with a
/// 380px docs column at its right. ponytail: Tern's inner classes may change.
const PALETTE_CSS: &str = "
[data-id='layer.cmd'].sf-overlay.sz-lg > .sf-ov-card { width: 640px; border-radius: 16px; }
[data-id='layer.cmd'] .sf-ov-body { padding: 0; gap: 0; }
[data-id='layer.cmd'] .sf-input { height: 56px; padding: 0 16px; background: none; box-shadow: none; font-size: 15px; }
[data-id='layer.cmd'] .sf-list { padding: 6px; border-top: 1px solid var(--l1); }
[data-id='layer.cmd'] [data-role='ask'] { padding: 14px 16px 0; }
[data-id='layer.pum'].sf-overlay > .sf-ov-card { width: auto; border-radius: 10px; }
[data-id='layer.pum'] .sf-ov-body { padding: 4px; gap: 0; }
[data-id='layer.pum'] .sf-row { align-items: flex-start; }
[data-id='layer.pum'] .sf-list { width: 260px; flex: none; font-size: 13px; }
[data-id='layer.pum'] [data-role='doc'] { width: 380px; max-height: 240px; overflow: auto; margin-left: 4px; padding: 2px 10px; border-left: 1px solid var(--l1); font-size: 12px; }
[data-id='layer.pum'].sf-overlay .sf-md .md .code-head { display: none; }
[data-role^='cur-']::after { content: ''; position: absolute; left: 0; bottom: 0; background: currentColor; }
[data-role='cur-v']::after { top: 0; width: 2px; }
[data-role='cur-h']::after { right: 0; height: 2px; }
.sf-rows-mark { display: none; }
.sf-status .sf-st-row { position: relative; }
[data-role='mid'] { position: absolute; left: 50%; transform: translateX(-50%); }
[data-role='float'].sf-overlay > .sf-ov-card { width: auto; padding: 0; border-radius: 8px; margin-top: -4px; }
[data-role='float'].sf-overlay > .sf-ov-card.nohead > .sf-ov-body { padding: 0; gap: 0; }
[data-role='doc-float'].sf-overlay > .sf-ov-card.nohead > .sf-ov-body { max-height: 320px; overflow: auto; overscroll-behavior: contain; padding: 10px 14px; font-size: 12px; }
[data-id='layer.pick'] .pk-main { min-width: 0; }
[data-id='layer.pick'] .pk-pv { width: 56%; flex: none; }
[data-id='layer.pick'] .pk-pv-slot { max-height: 100%; overflow: auto; overscroll-behavior: contain; }
[data-role^='w'].sf-editor { position: absolute; padding: 0; background: none; border-radius: 0; box-shadow: inset -1px 0 0 var(--l1); }
/* `contain` keeps the wheel inside the window: without it the pane scrolls at the ends and takes
   the tab strip and the breadcrumbs with it. */
[data-role^='w'].sf-editor .sf-ed-scroll { overflow-x: hidden; overflow-y: auto; scrollbar-width: none; overscroll-behavior: contain; }
.sf-region.sf-main, .sf-region.sf-layer { overscroll-behavior: contain; }
/* The caret keeps its box when the block cursor stands for it, so Tern still scrolls to it.
   `visibility` and not `opacity`, which Tern's own blink animates. */
.sf-editor .sf-caret { visibility: hidden; }
[data-role='screen-v'] .sf-editor .sf-caret { visibility: visible; }
[data-role^='w'] .sf-t-mark { background: var(--nt-sel, var(--l2)); color: inherit; border-radius: 0; }
[data-role^='w'] .sf-t-code { background: var(--tv-cur, var(--accent)); color: var(--page); border: 0; box-shadow: none; border-radius: 0; padding: 0; font: inherit; }
[data-role='float-text'] { padding: 0; background: none; box-shadow: none; }
[data-role='float-text'] .sf-caret { display: none; }
";

fn main() -> Result<ExitCode, Box<dyn Error>> {
	let args: Vec<String> = env::args().skip(1).collect();
	// `edit`: Tern keeps the cmdline's selection (Shift+arrows, Cmd+A/C) and sends edits.
	// ponytail: no `undo` feature; nvim's cmdline has no undo, so Ctrl+Z stays a key.
	let Some(mut session) = Session::<()>::connect(Options::new().app("neotern").feature(feature::EDIT))? else {
		// Not in Tern: plain nvim does the job.
		let code = Command::new("nvim").args(&args).status()?.code().unwrap_or(1);
		return Ok(ExitCode::from(code as u8));
	};
	let sf = session.open(SurfaceOptions::screen().title("nvim").role("neotern"))?;
	session.stylesheet(sf, "palette", Some(PALETTE_CSS))?;
	let (tx, rx) = mpsc::channel();
	let mut nvim = Nvim::spawn(&args, tx)?;
	let mut size = winsize();
	let opts = Value::Map(vec![
		("rgb".into(), true.into()),
		("ext_linegrid".into(), true.into()),
		("ext_cmdline".into(), true.into()),
		("ext_popupmenu".into(), true.into()),
		("ext_messages".into(), true.into()),
		("ext_tabline".into(), true.into()),
		("ext_multigrid".into(), true.into()),
	]);
	nvim.notify("nvim_ui_attach", vec![size.0.into(), size.1.into(), opts])?;
	nvim.notify("nvim_exec_lua", vec![include_str!("blink.lua").into(), Value::Array(vec![])])?;
	nvim.notify("nvim_exec_lua", vec![include_str!("status.lua").into(), Value::Array(vec![])])?;
	nvim.notify("nvim_exec_lua", vec![include_str!("float.lua").into(), Value::Array(vec![])])?;
	nvim.notify("nvim_exec_lua", vec![include_str!("telescope.lua").into(), Value::Array(vec![])])?;
	nvim.notify("nvim_exec_lua", vec![include_str!("keys.lua").into(), Value::Array(vec![])])?;
	let mut grid = Grid::default();
	// The field Tern's caret sits in, the cell box Tern draws with, and the sheet that places the
	// window boxes. The CSS engine has no `ch` unit, so the boxes are pixels of that cell.
	let mut focus: Option<String> = None;
	let mut cell = (8.0, 16.0);
	let mut layout = String::new();
	// The grid size asked of nvim per window, so a resize is requested once.
	let mut grids: std::collections::HashMap<u64, (usize, usize)> = std::collections::HashMap::new();
	// The status colors sent in the `colors` sheet; nvim's colors only add up, so it only grows.
	let mut colors = BTreeSet::new();
	let mut sel = None;
	'run: loop {
		match session.next(Some(POLL))? {
			Some(Input::Key(key)) if key.name == "paste" => {
				let text = key.text.unwrap_or_default();
				nvim.notify("nvim_paste", vec![text.into(), true.into(), (-1).into()])?;
			},
			Some(Input::Key(key)) => {
				nvim.notify("nvim_input", vec![notation(&key).into()])?;
				// Like vim's hit-enter prompt, but the key also goes to nvim.
				if grid.key() {
					session.render(sf, view(&grid))?;
				}
			},
			Some(Input::Event(Event::Edit(ed))) if ed.id == CMD_ID => {
				if let Some(cmd) = grid.cmd.as_mut()
					&& cmd.edit(ed.from, ed.to, &ed.text, ed.cursor, ed.len)
				{
					// setcmdline's pos is 1-based bytes; nvim echoes the line back with cmdline_show.
					let args = vec![cmd.text.as_str().into(), (cmd.pos + 1).into()];
					nvim.notify("nvim_call_function", vec!["setcmdline".into(), Value::Array(args)])?;
					session.render(sf, view(&grid))?;
				}
			},
			// Tern's own cell size: the window boxes are multiples of it.
			Some(Input::Event(Event::Resize(r))) => {
				if let Some(c) = r.cell.filter(|c| c.w > 0.0 && c.h > 0.0) {
					(cell, layout) = ((c.w, c.h), String::new());
				}
			},
			// A click in a window: nvim maps the cell to a buffer position, folds and all.
			Some(Input::Event(Event::Edit(ed))) if ed.from == ed.to && ed.text.is_empty() => {
				if let Some((g, _)) = grid.splits().into_iter().find(|(g, _)| win_id(*g) == ed.id) {
					let (row, col) = grid.at_offset(g, ed.cursor);
					for action in ["press", "release"] {
						let args = vec!["left".into(), action.into(), "".into(), (g as i64).into(), (row as i64).into(), (col as i64).into()];
						nvim.notify("nvim_input_mouse", args)?;
					}
				}
			},
			// A click in a window that is not the current one asks for the keys.
			Some(Input::Event(Event::Focus(f))) => {
				if let Some((_, win)) = grid.splits().into_iter().find(|(g, _)| win_id(*g) == f.id) {
					nvim.notify("nvim_set_current_win", vec![win.handle.clone()])?;
				}
			},
			// A click on a tab: go to that tab page or buffer.
			Some(Input::Event(Event::Select(s))) if s.id == TABS_ID => {
				if let Some((handle, _)) = s.item.parse::<usize>().ok().and_then(|i| grid.tabs.get(i)) {
					let set = if grid.pages { "nvim_set_current_tabpage" } else { "nvim_set_current_buf" };
					nvim.notify(set, vec![handle.clone()])?;
				}
			},
			// A click on a picker row: select that entry, and open it on a double-click.
			Some(Input::Event(ev @ (Event::Select(_) | Event::Activate(_)))) if matches!(&ev, Event::Select(s) | Event::Activate(s) if s.id == PICK_ID) => {
				let (Event::Select(s) | Event::Activate(s)) = &ev else { unreachable!() };
				let row = grid.pick.as_ref().and_then(|p| p.rows.iter().position(|(id, _)| *id == s.item));
				if let Some(i) = row {
					let args = vec![(i as i64 + 1).into(), matches!(ev, Event::Activate(_)).into()];
					nvim.notify("nvim_exec_lua", vec!["neotern_pick(...)".into(), Value::Array(args)])?;
				}
			},
			// A click on the picker's backdrop or its `esc` button closes it, as Escape does.
			Some(Input::Event(Event::Action(act))) if act.id == PICK_ID => {
				nvim.notify("nvim_input", vec!["<Esc>".into()])?;
			},
			// A click on a menu row: select it (insert its word); a double-click also accepts it.
			Some(Input::Event(ev @ (Event::Select(_) | Event::Activate(_)))) => {
				let (Event::Select(s) | Event::Activate(s)) = &ev else { unreachable!() };
				if let Some(i) = s.item.rsplit('.').next().and_then(|i| i.parse::<i64>().ok()) {
					let done = matches!(ev, Event::Activate(_));
					let args = vec![i.into(), done.into()];
					nvim.notify("nvim_exec_lua", vec!["neotern_select(...)".into(), Value::Array(args)])?;
				}
			},
			_ => {},
		}
		let now = winsize();
		if grid.expire() {
			session.render(sf, view(&grid))?;
		}
		if now != size {
			size = now;
			nvim.notify("nvim_ui_try_resize", vec![size.0.into(), size.1.into()])?;
		}
		while let Ok(event) = rx.try_recv() {
			match event {
				nvim::Event::Redraw(batch) => {
					if grid.apply(&batch) {
						let n = colors.len();
						let icons = grid.icons.values().filter_map(|(_, fg)| *fg);
						colors.extend(grid.status.iter().chain(&grid.crumbs).filter_map(|c| c.fg).chain(icons));
						colors.extend(grid.palette().into_iter().chain(grid.sel));
						if colors.len() != n || sel != grid.sel {
							sel = grid.sel;
							session.stylesheet(sf, "colors", Some(&colors_css(&colors, grid.sel)))?;
						}
						// The window boxes move only when nvim lays the screen out again.
						let splits = grid.splits();
						let css = screen_css(&splits, grid.h, cell);
						if css != layout {
							layout = css;
							session.stylesheet(sf, "layout", Some(&layout))?;
						}
						// `g:neotern_overscan` screens of text per window: nvim renders them into a
						// grid taller than the box, and Tern scrolls them. 'scroll' keeps <C-d> at
						// half of what you see, not half of the grid.
						for (g, win) in &splits {
							// Never more rows than the file has: the last line is then really the end.
							let cap = grid::win_number(&win.handle).and_then(|w| grid.lines.get(&w)).map_or(usize::MAX, |n| n + 1);
							let want = (win.w, (win.h * grid.overscan).min(cap.max(win.h)));
							if grids.get(g) == Some(&want) {
								continue;
							}
							grids.insert(*g, want);
							let (w, h) = (want.0 as i64, (grid.overscan > 1).then_some(want.1 as i64).unwrap_or(0));
							nvim.notify("nvim_ui_try_resize_grid", vec![(*g as i64).into(), w.into(), h.into()])?;
							let opts = || Value::Map(vec![("win".into(), win.handle.clone())]);
							let scroll = (win.h / 2).max(1) as i64;
							nvim.notify("nvim_set_option_value", vec!["scroll".into(), scroll.into(), opts()])?;
							// A tall grid only holds text around the cursor, so nvim must keep the
							// cursor in its middle: the wheel then has room above it and below it.
							if grid.overscan > 1 {
								let off = (want.1 / 2) as i64;
								nvim.notify("nvim_set_option_value", vec!["scrolloff".into(), off.into(), opts()])?;
							}
						}
						session.render(sf, view(&grid))?;
						// The caret is the cmdline's while one is open, else the window's that nvim
						// has: Tern blinks it there and sends that field's clicks.
						let cur = win_id(grid.caret().0);
						let want = match grid.cmd.is_some() {
							true => Some(CMD_ID.to_owned()),
							false => splits.iter().any(|(g, _)| win_id(*g) == cur).then_some(cur),
						};
						if want != focus {
							focus = want;
							session.focus(sf, focus.as_deref())?;
						}
					}
				},
				nvim::Event::Exit => break 'run,
			}
		}
	}
	session.close()?;
	let code = nvim.wait()?.code().unwrap_or(1);
	Ok(ExitCode::from(code as u8))
}

fn view(grid: &Grid) -> View {
	let pum = grid.pum.as_ref();
	let splits = grid.splits();
	// Every window is a Tern `editor` of its grid's text: a real caret, native selection and a
	// click that lands on a character. `screen_css` puts each one at its cell position.
	let (cg, cr, cc) = grid.caret();
	let shape = match grid.shape() {
		Some(Shape::Vertical) => "screen-v",
		Some(Shape::Horizontal) => "screen-h",
		_ => "screen-block",
	};
	// A block or underline cursor is a mark over the cell, as nvim draws it; an insert bar is
	// Tern's own caret, which sits between two characters.
	let block = grid.shape() != Some(Shape::Vertical);
	let windows: Vec<ui::Node> = splits
		.iter()
		.map(|(g, win)| {
			let cursor = (*g == cg).then(|| (grid.offset(*g, cr, cc), grid.offset(*g, cr, cc + 1)));
			editor(grid, *g, win.h, cursor, block, &format!("w{g}")).into()
		})
		.collect();
	// A card hangs under the caret of the focused window; a float of its own goes there too, as
	// most of them (hover, a plugin menu) open at the cursor.
	let caret = splits.iter().find(|(g, _)| *g == cg).map(|(g, _)| ui::Anchor::Caret(win_id(*g)));
	let at = |fallback: ui::Anchor| caret.clone().unwrap_or(fallback);
	let pum_at = pum.filter(|p| !p.cmdline).map(|_| at(ui::Anchor::Center));
	let floats: Vec<(u64, grid::Win, ui::Anchor)> =
		grid.floats().into_iter().map(|(g, w)| (g, w, at(ui::Anchor::Center))).collect();
	let doc_at = grid.doc.as_ref().map(|_| at(ui::Anchor::Center));
	// The tab pages, or the buffers while there is one tab page (like barbar), with devicons.
	let mut tabs = ui::tabs().key("tabs");
	for (i, (_, name)) in grid.tabs.iter().enumerate() {
		let tail = name.rsplit('/').next().filter(|t| !t.is_empty()).unwrap_or("[No Name]");
		let mut label = match grid.icons.get(tail) {
			Some((glyph, fg)) => spans(&format!("{glyph} "), *fg, false),
			None => Vec::new(),
		};
		label.push(ui::span(tail));
		tabs = tabs.tab(i.to_string(), label);
	}
	if let Some(i) = grid.tab {
		tabs = tabs.active(i.to_string());
	}
	// barbecue's breadcrumbs, one line; a blank one keeps the grid's height when there are none.
	let mut crumbs: Vec<ui::Span> = grid.crumbs.iter().flat_map(|c| spans(&c.text, c.fg, c.bold)).collect();
	if crumbs.is_empty() {
		crumbs.push(ui::span(" "));
	}
	let crumbs = ui::text(crumbs).key("crumbs").role("crumbs");
	// main is a region with Tern's block gap, which a program sheet can't reach; an inner col has
	// no gap. The windows sit in a `screen` box that `screen_css` sizes and positions.
	let screen = ui::col().key("screen").role(shape).gap(ui::Gap::None).children(windows);
	let grid_col = ui::col().gap(ui::Gap::None).wrap(true).child(tabs).child(crumbs).child(screen);
	let view = View::new().main(ui::col().child(grid_col));
	let menu = |p: &grid::Pum, id: &str| {
		let items = p.items.iter().enumerate().map(|(i, [word, kind, menu, _])| {
			ui::item(word.as_str()).icon(kind_icon(kind)).value(menu.as_str()).key(i)
		});
		// ponytail: virtual rows sit at a 44px pitch for 22px rows here; a plain list is fine for pum sizes.
		let list = ui::list().key("l").max_lines(10).children(items);
		match p.selected {
			Some(i) => list.selected(format!("{id}.l.{i}")),
			None => list,
		}
	};
	let mut layer = ui::col();
	if let Some(cmd) = &grid.cmd {
		// The command palette look: a glass card at the top center with the field, then the
		// cmdline completions. No `mode`: a mode label turns Tern's native editing off.
		let field = ui::input().key("q").prompt(cmd.prompt.as_str()).text(cmd.text.as_str()).cursor(cmd.caret());
		let mut card = ui::overlay().key("cmd").anchor(ui::Anchor::Top).size(ui::OverlaySize::Lg);
		// A `confirm()` question sits above its `[Y]es, (N)o` field; a toast would go after ~3s.
		if let Some(m) = grid.msgs.iter().rfind(|m| m.kind == "confirm") {
			card = card.child(ui::text(m.text.as_str()).key("ask").role("ask"));
		}
		card = card.child(field);
		if let Some(p) = pum.filter(|p| p.cmdline) {
			card = card.child(menu(p, "layer.cmd"));
		}
		layer = layer.child(card);
	}
	// A float (a plugin menu, a border) is the same editor in a card over the caret.
	for (g, win, at) in floats {
		let size = ui::Bound::wh(ui::Extent::Ch(win.w as f64), ui::Extent::Lines(win.h as f64));
		let body = editor(grid, g, win.h, None, false, "float-text").min(size);
		layer = layer.child(ui::overlay().key(g).role("float").anchor(at).size(ui::OverlaySize::Sm).child(body));
	}
	if let Some((md, at)) = grid.doc.as_deref().zip(doc_at) {
		let body = ui::md(md).key("d");
		layer = layer.child(ui::overlay().key("doc").role("doc-float").anchor(at).size(ui::OverlaySize::Md).child(body));
	}
	// which-key as a card at the bottom: a row per key that can follow, with its keycap.
	if let Some((title, rows)) = &grid.keys {
		let items = rows.iter().enumerate().map(|(i, (key, desc, group))| {
			let item = ui::item(desc.as_str()).key(i).hint(vec![key.clone()]);
			if *group { item.icon("folder") } else { item }
		});
		let list = ui::list().key("l").max_lines(14).children(items);
		let head = if title.is_empty() { "Keys".to_owned() } else { format!("Keys after {title}") };
		let card = ui::overlay().key("keys").role("keys").anchor(ui::Anchor::Bottom).size(ui::OverlaySize::Lg).head(head).child(list);
		layer = layer.child(card);
	}
	// Telescope as Tern's picker sheet: a search head, the entries, and the preview as code.
	if let Some(p) = &grid.pick {
		let items: Vec<ui::PickerItem> = p
			.rows
			.iter()
			.map(|(id, text)| {
				// A row's text starts with its devicon glyph, which Tern has no name for. `mono`
				// splits what is left on the last `/`, so the directory dims and the name stands out.
				let text = text.trim_start_matches(|c| matches!(c as u32, 0xe000..=0xf8ff | 0xf0000..)).trim_start();
				ui::PickerItem { mono: Some(true), ..ui::PickerItem::new(id.as_str(), text) }
			})
			.collect();
		let order: Vec<ui::OrderEntry> = p.rows.iter().map(|(id, _)| ui::OrderEntry::Item(id.clone())).collect();
		let mut sheet = ui::picker()
			.key("pick")
			.title(p.title.as_str())
			.query(p.prompt.as_str())
			.noun("results")
			.items(items)
			.order(order)
			.total(p.total)
			.preview(if p.pane { ui::PickerPreview::Side } else { ui::PickerPreview::None });
		if let Some((id, _)) = p.sel.checked_sub(1).and_then(|i| p.rows.get(i)) {
			sheet = sheet.selected(id.as_str());
		}
		if !p.preview.is_empty() {
			// Line numbers from the buffer, and a mark on the line the previewer centred.
			let mut code = ui::code(p.preview.as_str()).key("pv").lang(p.ft.as_str()).numbers(true).start(p.first);
			if p.at > 0 {
				code = code.marks(vec![ui::CodeMark { line: p.at as u64, tone: Some(ui::Tone::Accent), ranges: Vec::new() }]);
			}
			sheet = sheet.child(code);
		}
		layer = layer.child(sheet);
	}
	if let Some(at) = pum_at {
		let p = pum.unwrap();
		// The docs of the selected item (blink's docs, or nvim's `info`) sit right of the list.
		let mut body = ui::row().key("r").gap(ui::Gap::None).child(menu(p, "layer.pum.r"));
		if let Some(info) = p.selected.and_then(|i| p.items.get(i)).map(|it| &it[3]).filter(|s| !s.is_empty()) {
			body = body.child(ui::col().key("d").role("doc").child(ui::md(info.as_str())));
		}
		layer = layer.child(ui::overlay().key("pum").anchor(at).size(ui::OverlaySize::Sm).child(body));
	}
	for m in grid.msgs.iter().filter(|m| m.kind != "confirm") {
		layer = layer.child(message(m));
	}
	// ponytail: 'showmode' flashes like a message, so `recording @q` goes after ~3s; a status seg if it matters.
	if !grid.showmode.is_empty() {
		layer = layer.child(ui::toast(grid.showmode.as_str()).key(format!("mode {}", grid.showmode)));
	}
	// The status line in Tern's status strip: a segment per highlight run of 'statusline'. Tern's
	// strip has no middle group, so the middle runs are one segment that `PALETTE_CSS` centers.
	let seg = |c: &grid::Chunk| ui::seg(spans(c.text.trim(), c.fg, c.bold));
	let shown = |side: u8| grid.status.iter().enumerate().filter(move |(_, c)| c.side == side && !c.text.trim().is_empty());
	let mut status = ui::status().key("st").children(shown(0).map(|(i, c)| seg(c).key(i)));
	let mid: Vec<ui::Span> = shown(1).flat_map(|(_, c)| spans(&c.text, c.fg, c.bold)).collect();
	if !mid.is_empty() {
		status = status.child(ui::seg(mid).key("mid").role("mid"));
	}
	status = status.children(shown(2).map(|(i, c)| seg(c).key(i).side(ui::Side::Right)));
	// A region root draws no element of its own, so the strip sits in a col.
	view.layer(layer).dock(ui::col().child(status))
}

/// Grid `g` as a Tern `editor`: the cells' text, one decoration per colored run, and the caret
/// at `caret` when nvim's cursor is in it. nvim owns the wrapping and the scrolling, so the text
/// is exactly what the window shows and the node never wraps or follows the caret.
fn editor(grid: &Grid, g: u64, h: usize, cursor: Option<(usize, usize)>, block: bool, role: &str) -> ui::Editor {
	// With more than one screen in the grid, Tern scrolls and keeps the caret in view; with one the
	// text is the box and nvim owns every scroll.
	let follow = grid.overscan > 1;
	let (text, runs) = grid.text(g);
	let mut decor = runs
		.iter()
		.map(|r| ui::Decor { from: r.from, to: r.to, s: token(r.fg, r.bold, r.italic, r.marked), fx: None })
		.collect::<Vec<_>>();
	if let Some((from, to)) = cursor.filter(|_| block) {
		// `code` is a token with a background of its own, restyled as nvim's block cursor.
		decor.push(ui::Decor { from, to: to.max(from + 1), s: "code".into(), fx: None });
	}
	ui::editor()
		.key(format!("w{g}"))
		.role(role)
		.text(text)
		.decor(decor)
		.max_lines(h as u32)
		.cursor(cursor.map_or(0, |(from, _)| from))
		.prop("nowrap", true)
		.prop("followCursor", follow)
}

/// The node id of the editor of grid `g`.
fn win_id(g: u64) -> String {
	format!("main.0.screen.w{g}")
}

/// The span tokens of a cell run: its color names a `--sf-p-` variable `colors_css` defines.
fn token(fg: u32, bold: bool, italic: bool, marked: bool) -> String {
	let mut s = format!("c{fg:06x} mono");
	if bold {
		s += " strong";
	}
	if italic {
		s += " em";
	}
	if marked {
		s += " mark";
	}
	s
}

/// The sheet that puts each window at its cell position: nvim lays the screen out, Tern draws it.
/// `ch` is the cell width and `--sf-lh` the line height, so the boxes land on nvim's grid.
fn screen_css(splits: &[(u64, grid::Win)], rows: usize, cell: (f64, f64)) -> String {
	let (cw, ch) = cell;
	let _ = cw;
	let mut css = format!("[data-role^='screen'] {{ position: relative; height: {}px; }}\n", rows as f64 * ch);
	for (g, win) in splits {
		let (left, top) = (win.col as f64 * cw, win.row as f64 * ch);
		let (w, h) = (win.w as f64 * cw, win.h as f64 * ch);
		let _ = writeln!(css, "[data-role='w{g}'] {{ left: {left}px; top: {top}px; width: {w}px; height: {h}px; }}");
	}
	css
}

/// nvim text as spans: its color is the token `c<rrggbb>`, which `colors_css` defines; Nerd Font
/// glyphs (Private Use Area) get the `icon` token. Tern draws a span with no known token as bare
/// text, which takes no color, so each also gets `mono` (these strips are mono already).
fn spans(text: &str, fg: Option<u32>, bold: bool) -> Vec<ui::Span> {
	let mut style = fg.map(|fg| format!("c{fg:06x} mono")).unwrap_or_default();
	if bold {
		style += " strong";
	}
	let mut out: Vec<(bool, String)> = Vec::new();
	for ch in text.chars() {
		let icon = matches!(ch as u32, 0xe000..=0xf8ff | 0xf0000..);
		match out.last_mut() {
			Some((i, t)) if *i == icon => t.push(ch),
			_ => out.push((icon, ch.into())),
		}
	}
	let span = |(icon, t): (bool, String)| ui::Span { t, s: Some(if icon { format!("{style} icon") } else { style.clone() }), ..Default::default() };
	out.into_iter().map(span).collect()
}

/// The sheet that gives each color token its color: `--sf-p-<token>` is the color Tern puts on a
/// span with an unknown token.
fn colors_css(colors: &BTreeSet<u32>, sel: Option<u32>) -> String {
	let mut vars: String = colors.iter().map(|c| format!("--sf-p-c{c:06x}: #{c:06x}; ")).collect();
	if let Some(sel) = sel {
		let _ = write!(vars, "--nt-sel: #{sel:06x}; ");
	}
	format!(".sf-status, .sf-tabs, [data-role='crumbs'], .sf-editor {{ {vars}}}")
}

/// A message: one line is a toast, more is a card at the bottom that stays until a key.
// ponytail: the card shows the last 30 lines; paging if long output (`:set all`) matters.
fn message(m: &grid::Msg) -> ui::Node {
	// `:ls` and other lists start with a newline.
	let text = m.text.trim_matches('\n');
	let tone = match m.kind.as_str() {
		"emsg" | "echoerr" | "lua_error" | "rpc_error" | "shell_err" => ui::Tone::Error,
		"wmsg" => ui::Tone::Warning,
		_ => ui::Tone::Neutral,
	};
	if m.until.is_some() {
		return ui::toast(text).key(m.key).tone(tone).into();
	}
	let lines: Vec<&str> = text.lines().collect();
	let tail = lines[lines.len().saturating_sub(30)..].join("\n");
	ui::overlay().key(m.key).anchor(ui::Anchor::Bottom).size(ui::OverlaySize::Lg).tone(tone).child(ui::ansi(tail)).into()
}

/// The icon for an LSP `CompletionItemKind` name, or a vim `kind` letter (`:h complete-items`).
fn kind_icon(kind: &str) -> &'static str {
	match kind {
		"Method" | "Function" | "Constructor" | "f" | "m" => "braces",
		"Field" | "Property" => "tag",
		"Variable" | "v" => "box",
		"Class" | "Struct" | "Interface" | "t" => "layers",
		"Module" => "stack",
		"Keyword" => "key",
		"Snippet" => "wand",
		"Constant" | "EnumMember" | "Value" | "d" => "hash",
		"Enum" => "list",
		"File" => "file",
		"Folder" => "folder",
		"Reference" => "link",
		"Color" => "palette",
		"Unit" => "scale",
		"Event" => "bolt",
		"Operator" => "slash",
		"TypeParameter" => "type",
		"Text" => "note",
		_ => "code",
	}
}

/// The screen surface's main is smaller than the pty: Tern's own pane chrome and main's padding
/// take both sides, and the tab strip (28px) and the breadcrumb line (16px) the height. Measured
/// in `tern shot`: main was 871 x 580 px where the pty gave 896 x 624 px.
// ponytail: fixed pixel overhead; a skin or Tern change that moves it clips or wastes a row.
const CHROME_PX: u16 = 88;
const CHROME_W: u16 = 28;

/// The grid size in cells that fits the screen surface, from the pty.
fn winsize() -> (u16, u16) {
	// SAFETY: TIOCGWINSZ writes one `winsize` into `ws`.
	let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
	// SAFETY: as above; stdout is the pane's pty.
	let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
	if !ok || ws.ws_col == 0 || ws.ws_row == 0 {
		return (80, 24);
	}
	let cell_h = if ws.ws_ypixel > 0 { (ws.ws_ypixel / ws.ws_row).max(1) } else { 16 };
	let cell_w = if ws.ws_xpixel > 0 { (ws.ws_xpixel / ws.ws_col).max(1) } else { 8 };
	let cols = ws.ws_col.saturating_sub(CHROME_W.div_ceil(cell_w)).max(20);
	(cols, ws.ws_row.saturating_sub(CHROME_PX.div_ceil(cell_h)).max(1))
}

/// A Tern key in `nvim_input` notation (`:h key-notation`).
fn notation(key: &Key) -> String {
	let named = match key.name.as_str() {
		"enter" => Some("CR"),
		"tab" => Some("Tab"),
		"backspace" => Some("BS"),
		"escape" => Some("Esc"),
		"space" => Some("Space"),
		"up" => Some("Up"),
		"down" => Some("Down"),
		"left" => Some("Left"),
		"right" => Some("Right"),
		"home" => Some("Home"),
		"end" => Some("End"),
		"page_up" => Some("PageUp"),
		"page_down" => Some("PageDown"),
		"delete" => Some("Del"),
		"insert" => Some("Insert"),
		_ => None,
	};
	let fkey = key.name.strip_prefix('f').filter(|n| n.parse::<u8>().is_ok()).map(|n| format!("F{n}"));
	let name = match (named, fkey) {
		(Some(n), _) => n.to_owned(),
		(None, Some(f)) => f,
		(None, None) => {
			if let Some(text) = key.typed() {
				return text.replace('<', "<lt>");
			}
			if key.name == "<" { "lt".to_owned() } else { key.name.clone() }
		},
	};
	let mut mods = String::new();
	for (on, m) in [(key.ctrl, "C-"), (key.alt, "M-"), (key.meta, "D-"), (key.shift && key.name.chars().count() > 1, "S-")] {
		if on {
			mods.push_str(m);
		}
	}
	if mods.is_empty() && name.chars().count() == 1 { name } else { format!("<{mods}{name}>") }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn key_notation() {
		let k = |name: &str, text: Option<&str>| Key { text: text.map(Into::into), ..Key::named(name) };
		assert_eq!(notation(&k("a", Some("A"))), "A");
		assert_eq!(notation(&k("<", Some("<"))), "<lt>");
		assert_eq!(notation(&Key { ctrl: true, ..k("w", None) }), "<C-w>");
		assert_eq!(notation(&Key { shift: true, ..k("tab", None) }), "<S-Tab>");
		assert_eq!(notation(&k("enter", Some("\r"))), "<CR>");
		assert_eq!(notation(&Key { alt: true, ..k("f5", None) }), "<M-F5>");
		assert_eq!(notation(&k("f", Some("f"))), "f");
	}
}
