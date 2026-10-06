//! neotern: Neovim drawn as a native Tern surface. Runs `nvim --embed`, keeps the `ext_linegrid`
//! grid, and renders it as one `ansi` node per row in a `screen` surface.

mod grid;
mod nvim;

use std::{
	collections::BTreeSet,
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
[data-role='ctx'] { white-space: pre; overflow: hidden; box-shadow: inset 0 -1px 0 var(--l1); }
[data-role='mid'] { position: absolute; left: 50%; transform: translateX(-50%); }
[data-role='float'].sf-overlay > .sf-ov-card { width: auto; padding: 0; border-radius: 8px; margin-top: -4px; }
[data-role='float'].sf-overlay > .sf-ov-card.nohead > .sf-ov-body { padding: 0; gap: 0; }
[data-role='doc-float'].sf-overlay > .sf-ov-card.nohead > .sf-ov-body { max-height: 320px; overflow: auto; padding: 10px 14px; font-size: 12px; }
[data-id='layer.pick'] .pk-main { min-width: 0; }
[data-id='layer.pick'] .pk-pv { width: 56%; flex: none; }
[data-id='layer.pick'] .pk-pv-slot { max-height: 100%; overflow: auto; }
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
	nvim.notify("nvim_exec_lua", vec![include_str!("context.lua").into(), Value::Array(vec![])])?;
	let mut grid = Grid::default();
	let mut focused = false;
	// The status colors sent in the `colors` sheet; nvim's colors only add up, so it only grows.
	let mut colors = BTreeSet::new();
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
						if colors.len() != n {
							session.stylesheet(sf, "colors", Some(&colors_css(&colors)))?;
						}
						session.render(sf, view(&grid))?;
						if grid.cmd.is_some() != focused {
							focused = !focused;
							session.focus(sf, focused.then_some(CMD_ID))?;
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
	// A card hangs below a cell of the screen, whose row splits at that column so the card's left
	// edge lines up with it: the buffer menu under the completed word, a float at its own cell.
	let mut anchors: Vec<(usize, usize)> = Vec::new();
	let mut anchor = |row: usize, col: usize| -> ui::Anchor {
		let (above, row) = (row == 0, row.saturating_sub(1));
		let col = col.min(grid.w.saturating_sub(1));
		anchors.push((row, col));
		ui::Anchor::Node { id: format!("main.0.{row}.{col}"), above }
	};
	let pum_at = pum.filter(|p| !p.cmdline).map(|p| {
		let (row, col) = grid.to_screen(p.grid, p.row, p.col);
		anchor(row + 1, col)
	});
	let floats: Vec<(u64, grid::Win, ui::Anchor)> = grid.floats().into_iter().map(|(g, w)| (g, w, anchor(w.row, w.col))).collect();
	// Hover and signature help sit under the cursor's cell, as nvim's own float does.
	let doc_at = grid.doc.as_ref().map(|_| {
		let (_, row, col) = grid.cursor();
		anchor(row + 1, col)
	});
	let screen = cells(grid, 1, grid.w, grid.h, &anchors);
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
	// The treesitter context, in the row above the grid: the enclosing line, with nvim's colors.
	// ponytail: one line (the plugin's `max_lines`), so the row keeps the grid's height fixed.
	let mut ctx: Vec<ui::Span> = grid.context.first().map_or_else(Vec::new, |line| line.iter().flat_map(|c| spans(&c.text, c.fg, c.bold)).collect());
	if ctx.is_empty() {
		ctx.push(ui::span(" "));
	}
	let ctx = ui::text(ctx).key("ctx").role("ctx");
	// main is a region with Tern's block gap, which a program sheet can't reach; an inner col has
	// no gap. `wrap` keeps a tall grid from virtualizing.
	let grid_col = ui::col().gap(ui::Gap::None).wrap(true).child(tabs).child(crumbs).child(ctx).children(screen);
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
	// A float (hover, signature help, Telescope) is a card over the cell nvim placed it at. Its
	// rows carry no width of their own, so the body asks for the grid's size in cells.
	for (g, win, at) in floats {
		let size = ui::Bound::wh(ui::Extent::Ch(win.w as f64), ui::Extent::Lines(win.h as f64));
		let body = ui::col().key("c").gap(ui::Gap::None).min(size).children(cells(grid, g, win.w, win.h, &[]));
		layer = layer.child(ui::overlay().key(g).role("float").anchor(at).size(ui::OverlaySize::Sm).child(body));
	}
	if let Some((md, at)) = grid.doc.as_deref().zip(doc_at) {
		let body = ui::md(md).key("d");
		layer = layer.child(ui::overlay().key("doc").role("doc-float").anchor(at).size(ui::OverlaySize::Md).child(body));
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

/// Grid `src` as one node per row: one row node, or a row of parts when it is cut. A row splits
/// at a bar cursor and at each anchor of `anchors` (`(row, col)`), whose part a card anchors to.
/// A part's key is its first column, so its id is `<row id>.<col>`.
fn cells(grid: &Grid, src: u64, w: usize, h: usize, anchors: &[(usize, usize)]) -> Vec<ui::Node> {
	let (cg, cr, cc) = grid.cursor();
	let bar = grid.shape().filter(|s| *s != Shape::Block && cg == src && cc < w);
	(0..h)
		.map(|r| -> ui::Node {
			let mut cuts = vec![0, w];
			let at = anchors.iter().filter(|(row, _)| *row == r);
			cuts.extend(at.map(|(_, col)| *col));
			let anchored = cuts.len() > 2;
			if bar.is_some() && cr == r {
				cuts.extend([cc, (cc + 1 + grid.wide(src, r, cc) as usize).min(w)]);
			}
			cuts.sort_unstable();
			cuts.dedup();
			if cuts.len() == 2 && !anchored {
				// `rows` clips, while `ansi` wraps a row that fills its width into a second line.
				return ui::rows(vec![grid.row(src, r, 0..w)]).cols(w as u32).key(r).into();
			}
			// A row gives its parts no width of their own; grow in proportion to the cells keeps
			// every column where it was. `rows` clips instead of wrapping.
			let parts = cuts.windows(2).map(|p| {
				let (from, to) = (p[0], p[1]);
				let part = ui::rows(vec![grid.row(src, r, from..to)]).cols((to - from) as u32).grow((to - from) as f64).key(from);
				match bar {
					Some(shape) if r == cr && from == cc => part.role(if shape == Shape::Vertical { "cur-v" } else { "cur-h" }),
					_ => part,
				}
			});
			ui::row().gap(ui::Gap::None).key(r).children(parts).into()
		})
		.collect()
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
fn colors_css(colors: &BTreeSet<u32>) -> String {
	let vars: String = colors.iter().map(|c| format!("--sf-p-c{c:06x}: #{c:06x}; ")).collect();
	format!(".sf-status, .sf-tabs, [data-role='crumbs'] {{ {vars}}}")
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

/// The screen surface's main is shorter than the pty: the cover inset (10px), the dock (24px of
/// its own and the 24px status strip), main's padding (2 x 6px), the tab strip (28px) and the
/// breadcrumb and context lines (2 x 16px), measured in `tern shot`.
// ponytail: fixed pixel overhead; a skin or Tern change that moves it clips or wastes a row.
const CHROME_PX: u16 = 130;

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
	(ws.ws_col, ws.ws_row.saturating_sub(CHROME_PX.div_ceil(cell_h)).max(1))
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
