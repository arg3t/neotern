//! The `ext_linegrid` model of grid 1, rendered as one ANSI string per row.

use std::{
	collections::HashMap,
	time::{Duration, Instant},
};

use rmpv::Value;

#[derive(Clone, Copy, Default)]
struct Attr {
	fg:        Option<u32>,
	bg:        Option<u32>,
	reverse:   bool,
	bold:      bool,
	italic:    bool,
	underline: bool,
	strike:    bool,
}

#[derive(Clone)]
struct Cell {
	text: String,
	hl:   u64,
}

impl Default for Cell {
	fn default() -> Self {
		Self { text: " ".into(), hl: 0 }
	}
}

pub struct Grid {
	/// The screen size: grid 1's.
	pub w:   usize,
	pub h:   usize,
	/// The `ext_cmdline` line while one is shown.
	pub cmd:    Option<Cmdline>,
	/// The `ext_popupmenu` while one is shown.
	pub pum:    Option<Pum>,
	/// The cursor: `(grid, row, col)`.
	cursor: (u64, usize, usize),
	/// The `ext_messages` shown as toasts, oldest first.
	pub msgs:   Vec<Msg>,
	/// The 'showmode' text (`-- INSERT --`, `recording @q`), or empty.
	pub showmode: String,
	/// The `ext_tabline` strip: the tab pages when there are several, else the listed buffers.
	/// Each is its nvim handle and its name.
	pub tabs:     Vec<(Value, String)>,
	/// Whether `tabs` holds tab pages, not buffers.
	pub pages:    bool,
	/// The current one in `tabs`.
	pub tab:      Option<usize>,
	/// The current window's 'statusline' and barbecue breadcrumbs, from `status.lua`.
	pub status:   Vec<Chunk>,
	pub crumbs:   Vec<Chunk>,
	/// The devicon glyph and color per file name (tail), from `status.lua`.
	pub icons:    HashMap<String, (String, Option<u32>)>,
	/// nvim's `Visual` background, for the cells a window marks.
	pub sel:      Option<u32>,
	/// The hover or signature help text, from `float.lua`.
	pub doc:      Option<String>,
	/// The Telescope picker while one is open, from `telescope.lua`.
	pub pick:     Option<Pick>,
	/// The sidebar picker (Snacks' explorer) while one is open, from `snacks.lua`.
	pub tree:     Option<Tree>,
	/// which-key's follow-up keys while it shows, from `keys.lua`: the keys pressed so far, then
	/// `(key, description, is a group)` per row.
	pub keys:     Option<(String, Vec<(String, String, bool)>)>,
	/// Each grid by id (`ext_multigrid`): grid 1 holds what is outside the windows (separators,
	/// status lines), and each window has its own.
	grids:   HashMap<u64, Cells>,
	/// The shown windows, by grid id.
	wins:    HashMap<u64, Win>,
	blank:   Cell,
	hl:      HashMap<u64, Attr>,
	fg:      u32,
	bg:      u32,
	/// The cursor shape per `mode_info_set` index, and the current index.
	shapes:  Vec<Shape>,
	mode:    usize,
	seq:     u64,
}

impl Default for Grid {
	fn default() -> Self {
		Self {
			w:        0,
			h:        0,
			cmd:      None,
			pum:      None,
			cursor:   (1, 0, 0),
			msgs:     Vec::new(),
			showmode: String::new(),
			tabs:     Vec::new(),
			pages:    false,
			tab:      None,
			status:   Vec::new(),
			crumbs:   Vec::new(),
			icons:    HashMap::new(),
			sel:      None,
			doc:      None,
			pick:     None,
			tree:     None,
			keys:     None,
			grids:    HashMap::new(),
			wins:     HashMap::new(),
			blank:    Cell::default(),
			hl:       HashMap::new(),
			fg:       0xffffff,
			bg:       0,
			shapes:   Vec::new(),
			mode:     0,
			seq:      0,
		}
	}
}

/// The cells of one grid.
#[derive(Default)]
struct Cells {
	w:     usize,
	h:     usize,
	cells: Vec<Cell>,
}

impl Cells {
	fn get(&self, r: usize, c: usize) -> Option<&Cell> {
		(r < self.h && c < self.w).then(|| &self.cells[r * self.w + c])
	}

	fn resize(&mut self, w: usize, h: usize) {
		let mut cells = vec![Cell::default(); w * h];
		for r in 0..h.min(self.h) {
			for c in 0..w.min(self.w) {
				cells[r * w + c] = self.cells[r * self.w + c].clone();
			}
		}
		(self.w, self.h, self.cells) = (w, h, cells);
	}

	/// `cells` is `[text, hl?, repeat?]...`; a missing hl repeats the previous one.
	fn line(&mut self, row: usize, mut col: usize, cells: &Value) {
		let mut hl = 0;
		for cell in cells.as_array().map_or(&[][..], Vec::as_slice) {
			let Some(cell) = cell.as_array() else { continue };
			let text = cell[0].as_str().unwrap_or_default();
			if let Some(id) = cell.get(1) {
				hl = int(id) as u64;
			}
			let repeat = cell.get(2).map_or(1, |n| int(n) as usize);
			for _ in 0..repeat {
				if row < self.h && col < self.w {
					self.cells[row * self.w + col] = Cell { text: text.into(), hl };
				}
				col += 1;
			}
		}
	}

	/// Moves `[top, bot) x [left, right)` up by `rows` (down when negative); vacated rows keep stale
	/// cells, which nvim redraws with `grid_line` before the next flush.
	fn scroll(&mut self, top: usize, bot: usize, left: usize, right: usize, rows: i64) {
		let (bot, right) = (bot.min(self.h), right.min(self.w));
		let shift = rows.unsigned_abs() as usize;
		if shift >= bot.saturating_sub(top) {
			return;
		}
		let dsts: Vec<usize> = if rows > 0 { (top..bot - shift).collect() } else { (top + shift..bot).rev().collect() };
		for dst in dsts {
			let src = if rows > 0 { dst + shift } else { dst - shift };
			for c in left..right {
				self.cells[dst * self.w + c] = self.cells[src * self.w + c].clone();
			}
		}
	}
}

/// Where a shown window sits on the screen.
#[derive(Clone)]
pub struct Win {
	pub row:    usize,
	pub col:    usize,
	pub w:      usize,
	pub h:      usize,
	/// A float's drawing order (`compindex`); `None` for a split, which the screen holds.
	pub float:  Option<i64>,
	/// nvim's window handle, for `nvim_set_current_win`.
	pub handle: Value,
}

/// One colored run of a window's text, in the offsets Tern counts (UTF-16 units).
pub struct Run {
	pub from:   usize,
	pub to:     usize,
	pub fg:     u32,
	pub bold:   bool,
	pub italic: bool,
	/// The cells carry a background of their own (a selection, a match, a diff).
	pub marked: bool,
}

/// A Telescope picker (`:h telescope`), from `telescope.lua`.
pub struct Pick {
	pub title:   String,
	/// The text typed in the prompt.
	pub prompt:  String,
	/// The visible entries in order, each its id (the sorted-on text) and its display text.
	pub rows:    Vec<(String, String)>,
	/// The rows that come colored (Snacks'), by id: each run is its text, color and weight.
	pub styled:  HashMap<String, Vec<(String, Option<u32>, bool)>>,
	/// The selected row, 1-based; 0 when there is none.
	pub sel:     usize,
	/// The previewed text and its filetype.
	pub preview: String,
	pub ft:      String,
	/// How many entries the finder looked at.
	pub total:   u64,
	/// Whether the picker has a previewer at all, so the pane stays while its text loads.
	pub pane:    bool,
	/// The buffer line `preview` starts at, and the line the previewer put the match on.
	pub first:   i64,
	pub at:      i64,
}

/// A sidebar picker drawn as a tree in the split nvim keeps for it, from `snacks.lua`.
pub struct Tree {
	/// The nvim window handle of that split.
	pub window:   i64,
	pub rows:     Vec<TreeRow>,
	/// The id of the row the list's cursor is on.
	pub selected: String,
}

/// One row of a [`Tree`].
pub struct TreeRow {
	/// The row's file, unique in the tree.
	pub id:    String,
	pub label: String,
	/// Its place in the picker's list, which a click sends back.
	pub index: i64,
	pub depth: usize,
	/// The icon Snacks draws for it, and that icon's color.
	pub glyph: String,
	pub color: Option<u32>,
}

/// The cursor shape of a mode (`:h guicursor`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shape {
	Block,
	Horizontal,
	Vertical,
}

/// One `msg_show` (`:h ui-messages`).
pub struct Msg {
	/// A local counter, for the toast key.
	pub key:  u64,
	pub kind: String,
	pub text: String,
	/// nvim's id: a message with the same id replaces this one.
	id:       String,
	/// When the model drops it; `None` (a `confirm` prompt or a multi-line output) stays until
	/// the next key or `msg_clear`.
	pub until: Option<Instant>,
}

/// One highlight run of a status line or winbar.
pub struct Chunk {
	/// 0 left, 1 middle (after a first `%=`), 2 right (after the last `%=`).
	pub side: u8,
	pub text: String,
	pub fg:   Option<u32>,
	pub bold: bool,
}

/// The command line nvim is editing (`:h ui-cmdline`).
pub struct Cmdline {
	/// `firstc` (`:`, `/`, ...) then an `input()` prompt.
	pub prompt: String,
	pub text:   String,
	/// The caret, a byte offset into `text`.
	pub pos:    usize,
}

/// The completion menu (`:h ui-popupmenu`).
pub struct Pum {
	/// `(word, kind, menu, info)` per item.
	pub items:    Vec<[String; 4]>,
	pub selected: Option<usize>,
	/// Completes the cmdline (grid -1), not the buffer.
	pub cmdline:  bool,
}

impl Cmdline {
	/// The caret in UTF-16 units, as Tern counts.
	pub fn caret(&self) -> usize {
		self.text.get(..self.pos).unwrap_or(&self.text).encode_utf16().count()
	}

	/// Applies a Tern `edit` (UTF-16 `[from, to)` becomes `text`, caret at `cursor`); false when
	/// `len` shows Tern saw another text.
	pub fn edit(&mut self, from: usize, to: usize, text: &str, cursor: usize, len: usize) -> bool {
		if self.text.encode_utf16().count() != len {
			return false;
		}
		let (a, b) = (byte(&self.text, from), byte(&self.text, to.max(from)));
		self.text.replace_range(a..b, text);
		self.pos = byte(&self.text, cursor);
		true
	}
}

/// The byte offset of UTF-16 offset `units`; inside a surrogate pair it rounds down.
fn byte(s: &str, units: usize) -> usize {
	let mut n = 0;
	for (i, c) in s.char_indices() {
		n += c.len_utf16();
		if n > units {
			return i;
		}
	}
	s.len()
}

fn int(v: &Value) -> i64 {
	v.as_i64().unwrap_or(0)
}

/// The value of key `k` in a msgpack map.
fn field<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
	v.as_map()?.iter().find(|(key, _)| key.as_str() == Some(k)).map(|(_, v)| v)
}

/// nvim sends -1 for "unset" default colors.
fn color(v: &Value, fallback: u32) -> u32 {
	v.as_i64().filter(|c| *c >= 0).map_or(fallback, |c| c as u32)
}

/// The text of a `[attr, text, hl]...` message content.
fn chunks(v: &Value) -> String {
	v.as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(|c| c.as_array()?.get(1)?.as_str()).collect()
}

/// An nvim window handle as a number: msgpack carries it as an ext value that holds an integer.
pub fn handle_id(v: &Value) -> Option<i64> {
	match v {
		Value::Ext(_, bytes) => rmpv::decode::read_value(&mut bytes.as_slice()).ok()?.as_i64(),
		other => other.as_i64(),
	}
}

/// How long a message stays in the model: a bit more than the ~3s Tern shows a toast.
const SHOWN: Duration = Duration::from_secs(4);

impl Grid {
	/// Applies one `redraw` batch; returns whether it ended with `flush`.
	pub fn apply(&mut self, batch: &[Value]) -> bool {
		let mut flush = false;
		for event in batch {
			let Some([name, calls @ ..]) = event.as_array().map(Vec::as_slice) else { continue };
			for call in calls {
				let a = call.as_array().map_or(&[][..], Vec::as_slice);
				match name.as_str().unwrap_or_default() {
					// ponytail: one level; a nested cmdline (<C-r>=) replaces the outer until nvim re-shows it.
					"cmdline_show" => {
						let prompt = format!("{}{}", a[2].as_str().unwrap_or_default(), a[3].as_str().unwrap_or_default());
						self.cmd = Some(Cmdline { prompt, text: chunks(&a[0]), pos: int(&a[1]) as usize });
					},
					"cmdline_pos" => {
						if let Some(cmd) = &mut self.cmd {
							cmd.pos = int(&a[0]) as usize;
						}
					},
					"cmdline_hide" => self.cmd = None,
					"popupmenu_show" => {
						let text = |v: &Value, i: usize| v.as_array().and_then(|f| f.get(i)?.as_str()).unwrap_or_default().to_owned();
						let items = a[0].as_array().map_or(&[][..], Vec::as_slice);
						self.pum = Some(Pum {
							items:    items.iter().map(|it| std::array::from_fn(|i| text(it, i))).collect(),
							selected: usize::try_from(int(&a[1])).ok(),
							cmdline:  int(&a[4]) == -1,
						});
					},
					"popupmenu_select" => {
						if let Some(pum) = &mut self.pum {
							pum.selected = usize::try_from(int(&a[0])).ok();
						}
					},
					"popupmenu_hide" => self.pum = None,
					"tabline_update" => {
						let list = |v: &Value, handle: &str| -> Vec<(Value, String)> {
							let entries = v.as_array().map_or(&[][..], Vec::as_slice);
							entries.iter().map(|e| {
								let name = field(e, "name").and_then(Value::as_str).unwrap_or_default();
								(field(e, handle).cloned().unwrap_or(Value::Nil), name.to_owned())
							}).collect()
						};
						let pages = list(&a[1], "tab");
						self.pages = pages.len() > 1;
						let (cur, tabs) = if self.pages { (&a[0], pages) } else { (&a[2], list(&a[3], "buffer")) };
						self.tab = tabs.iter().position(|(h, _)| h == cur);
						self.tabs = tabs;
					},
					// ponytail: the text is markdown, or plain in a fence when nvim named another syntax.
					"neotern_doc" => {
						self.doc = a.first().and_then(Value::as_str).filter(|t| !t.trim().is_empty()).map(|text| {
							match a[1].as_str().unwrap_or_default() {
								"markdown" | "" => text.to_owned(),
								syntax => format!("```{syntax}\n{text}\n```"),
							}
						});
					},
					"neotern_picker" => {
						let rows = a.get(2).and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
						self.pick = (a.len() >= 10).then(|| Pick {
							title:   a[0].as_str().unwrap_or_default().into(),
							prompt:  a[1].as_str().unwrap_or_default().into(),
							rows:    rows.iter().filter_map(|r| {
								let [id, text, ..] = r.as_array()?.as_slice() else { return None };
								Some((id.as_str()?.to_owned(), text.as_str()?.to_owned()))
							}).collect(),
							styled:  rows.iter().filter_map(|r| {
								let [id, _, runs] = r.as_array()?.as_slice() else { return None };
								let runs = runs.as_array()?.iter().filter_map(|run| {
									let [text, fg, bold] = run.as_array()?.as_slice() else { return None };
									Some((text.as_str()?.to_owned(), u32::try_from(int(fg)).ok(), bold.as_bool()?))
								}).collect::<Vec<_>>();
								Some((id.as_str()?.to_owned(), runs))
							}).collect(),
							sel:     int(&a[3]).max(0) as usize,
							preview: a[4].as_str().unwrap_or_default().into(),
							ft:      a[5].as_str().unwrap_or_default().into(),
							total:   int(&a[6]).max(0) as u64,
							pane:    a[7].as_bool().unwrap_or(false),
							first:   int(&a[8]).max(1),
							at:      int(&a[9]),
						});
					},
					"neotern_tree" => {
						self.tree = (a.len() >= 3).then(|| Tree {
							window:   int(&a[0]),
							rows:     a[1].as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(|r| {
								let [id, label, index, depth, glyph, color] = r.as_array()?.as_slice() else { return None };
								Some(TreeRow {
									id:    id.as_str()?.to_owned(),
									label: label.as_str()?.to_owned(),
									index: int(index),
									depth: int(depth).max(0) as usize,
									glyph: glyph.as_str()?.to_owned(),
									color: u32::try_from(int(color)).ok(),
								})
							}).collect(),
							selected: a[2].as_str().unwrap_or_default().into(),
						});
					},
					"neotern_keys" => {
						self.keys = (a.len() >= 2).then(|| {
							let rows = a[1].as_array().map_or(&[][..], Vec::as_slice);
							let rows = rows
								.iter()
								.filter_map(|r| {
									let [key, desc, group] = r.as_array()?.as_slice() else { return None };
									Some((key.as_str()?.into(), desc.as_str()?.into(), group.as_bool()?))
								})
								.collect();
							(a[0].as_str().unwrap_or_default().to_owned(), rows)
						});
					},
					"neotern_status" => {
						let runs = |v: &Value| -> Vec<Chunk> {
							let runs = v.as_array().map_or(&[][..], Vec::as_slice);
							runs.iter().filter_map(|r| {
								let [side, text, fg, bold] = r.as_array()?.as_slice() else { return None };
								let (side, fg) = (int(side) as u8, u32::try_from(int(fg)).ok());
								Some(Chunk { side, text: text.as_str()?.into(), fg, bold: bold.as_bool()? })
							}).collect()
						};
						self.status = runs(&a[0]);
						self.crumbs = runs(&a[1]);
						let icons = a[2].as_array().map_or(&[][..], Vec::as_slice);
						self.icons = icons.iter().filter_map(|i| {
							let [name, glyph, fg] = i.as_array()?.as_slice() else { return None };
							Some((name.as_str()?.into(), (glyph.as_str()?.into(), u32::try_from(int(fg)).ok())))
						}).collect();
						self.sel = a.get(3).map(|v| int(v)).and_then(|c| u32::try_from(c).ok());
					},
					"mode_info_set" => {
						let on = a[0].as_bool().unwrap_or(false);
						let modes = a[1].as_array().map_or(&[][..], Vec::as_slice);
						self.shapes = modes
							.iter()
							.map(|m| {
								let kv = m.as_map().map_or(&[][..], Vec::as_slice);
								match kv.iter().find(|(k, _)| k.as_str() == Some("cursor_shape")).and_then(|(_, v)| v.as_str()) {
									Some("horizontal") if on => Shape::Horizontal,
									Some("vertical") if on => Shape::Vertical,
									_ => Shape::Block,
								}
							})
							.collect();
					},
					"mode_change" => self.mode = int(&a[1]) as usize,
					"msg_show" => {
						let flag = |i: usize| a.get(i).and_then(Value::as_bool).unwrap_or(false);
						let id = a.get(5).filter(|v| !v.is_nil()).map(Value::to_string).unwrap_or_default();
						self.msg(a[0].as_str().unwrap_or_default(), chunks(&a[1]), flag(2), flag(4), id);
					},
					"msg_clear" => _ = self.key(),
					"msg_showmode" => self.showmode = chunks(&a[0]),
					"msg_history_show" => {
						let entries = a[0].as_array().map_or(&[][..], Vec::as_slice);
						let text: Vec<String> = entries.iter().filter_map(|e| Some(chunks(e.as_array()?.get(1)?))).collect();
						self.msg("history", text.join("\n"), false, false, String::new());
					},
					"grid_resize" => {
						let (g, w, h) = (int(&a[0]) as u64, int(&a[1]) as usize, int(&a[2]) as usize);
						self.grids.entry(g).or_default().resize(w, h);
						if g == 1 {
							(self.w, self.h) = (w, h);
						}
					},
					"grid_clear" => {
						if let Some(g) = self.grids.get_mut(&(int(&a[0]) as u64)) {
							g.cells.fill(Cell::default());
						}
					},
					"grid_destroy" => {
						let g = int(&a[0]) as u64;
						self.grids.remove(&g);
						self.wins.remove(&g);
					},
					"grid_cursor_goto" => self.cursor = (int(&a[0]) as u64, int(&a[1]) as usize, int(&a[2]) as usize),
					"grid_line" => {
						if let Some(g) = self.grids.get_mut(&(int(&a[0]) as u64)) {
							g.line(int(&a[1]) as usize, int(&a[2]) as usize, &a[3]);
						}
					},
					"grid_scroll" => {
						if let Some(g) = self.grids.get_mut(&(int(&a[0]) as u64)) {
							let n = |i: usize| int(&a[i]) as usize;
							g.scroll(n(1), n(2), n(3), n(4), int(&a[5]));
						}
					},
					"win_pos" => {
						let n = |i: usize| int(&a[i]) as usize;
						let win = Win { row: n(2), col: n(3), w: n(4), h: n(5), float: None, handle: a[1].clone() };
						self.wins.insert(int(&a[0]) as u64, win);
					},
					// nvim places the float (`screen_row`, `screen_col`); its size is its grid's.
					"win_float_pos" => {
						let at = |i: usize| a[i].as_f64().or_else(|| a[i].as_i64().map(|v| v as f64)).unwrap_or(0.0).max(0.0) as usize;
						let win = Win { row: at(9), col: at(10), w: 0, h: 0, float: Some(int(&a[8])), handle: a[1].clone() };
						self.wins.insert(int(&a[0]) as u64, win);
					},
					"win_hide" | "win_close" => _ = self.wins.remove(&(int(&a[0]) as u64)),
					"default_colors_set" => {
						self.fg = color(&a[0], 0xffffff);
						self.bg = color(&a[1], 0);
					},
					"hl_attr_define" => {
						let mut attr = Attr::default();
						for (k, v) in a[1].as_map().map_or(&[][..], Vec::as_slice) {
							let on = v.as_bool().unwrap_or(false);
							match k.as_str().unwrap_or_default() {
								"foreground" => attr.fg = v.as_u64().map(|c| c as u32),
								"background" => attr.bg = v.as_u64().map(|c| c as u32),
								"reverse" => attr.reverse = on,
								"bold" => attr.bold = on,
								"italic" => attr.italic = on,
								"strikethrough" => attr.strike = on,
								"underline" | "undercurl" | "underdouble" | "underdotted" | "underdashed" => {
									attr.underline |= on;
								},
								_ => {},
							}
						}
						self.hl.insert(int(&a[0]) as u64, attr);
					},
					"flush" => flush = true,
					_ => {},
				}
			}
		}
		flush
	}

	/// Adds a `msg_show`: it replaces the one with the same nvim `id`, or the last one on
	/// `replace`; `append` (`:echon`) extends the last one.
	fn msg(&mut self, kind: &str, text: String, replace: bool, append: bool, id: String) {
		// `:echo ""`: plugins clear the message area with it.
		if kind == "empty" {
			self.msgs.retain(|m| m.until.is_none());
			return;
		}
		let at = self.msgs.iter().position(|m| !id.is_empty() && m.id == id);
		let last = (replace || append).then(|| self.msgs.len().checked_sub(1)).flatten();
		let text = match at.or(last).map(|i| self.msgs.remove(i)) {
			Some(old) if append => old.text + &text,
			_ => text,
		};
		// A new key each time: Tern shows a toast once, when its key first appears.
		self.seq += 1;
		let line = !text.trim_matches('\n').contains('\n');
		let until = (kind != "confirm" && line).then(|| Instant::now() + SHOWN);
		self.msgs.push(Msg { key: self.seq, kind: kind.into(), text, id, until });
	}

	/// Drops the messages that wait for a key; returns whether any went.
	pub fn key(&mut self) -> bool {
		let n = self.msgs.len();
		self.msgs.retain(|m| m.until.is_some());
		self.msgs.len() != n
	}

	/// Drops the toasts whose time is up; returns whether any went.
	pub fn expire(&mut self) -> bool {
		let (n, now) = (self.msgs.len(), Instant::now());
		self.msgs.retain(|m| m.until.is_none_or(|t| t > now));
		self.msgs.len() != n
	}

	/// The cursor shape now; `None` while the cmdline (with Tern's caret) shows.
	pub fn shape(&self) -> Option<Shape> {
		self.cmd.is_none().then(|| self.shapes.get(self.mode).copied().unwrap_or(Shape::Block))
	}

	/// The shown floats in drawing order, each with its grid's size.
	pub fn floats(&self) -> Vec<(u64, Win)> {
		let mut floats: Vec<(u64, Win)> = self
			.wins
			.iter()
			.filter(|(_, w)| w.float.is_some())
			.filter_map(|(g, w)| self.grids.get(g).map(|cells| (*g, Win { w: cells.w, h: cells.h, ..w.clone() })))
			.collect();
		floats.sort_by_key(|(_, w)| w.float);
		floats
	}

	/// The split windows, each with its grid's size, in a stable order (by grid id).
	pub fn splits(&self) -> Vec<(u64, Win)> {
		let mut wins: Vec<(u64, Win)> = self
			.wins
			.iter()
			.filter(|(_, w)| w.float.is_none())
			.filter_map(|(g, w)| self.grids.get(g).map(|c| (*g, Win { w: w.w.min(c.w), h: w.h.min(c.h), ..w.clone() })))
			.collect();
		wins.sort_unstable_by_key(|(g, _)| *g);
		wins
	}

	/// The text of grid `g` for Tern's editor, with one colored run per cell run and the offsets
	/// Tern counts (UTF-16 units over the whole text). A run whose cells carry a background of
	/// their own is marked: nvim's selection, search matches and diffs show as a highlight.
	pub fn text(&self, g: u64) -> (String, Vec<Run>) {
		let Some(cells) = self.grids.get(&g) else { return (String::new(), Vec::new()) };
		let (mut text, mut runs) = (String::new(), Vec::<Run>::new());
		let mut at = 0;
		for r in 0..cells.h {
			if r > 0 {
				text.push('\n');
				at += 1;
			}
			for c in 0..cells.w {
				let cell = cells.get(r, c).unwrap_or(&self.blank);
				// The right half of a double-width char is "".
				if cell.text.is_empty() {
					continue;
				}
				let attr = self.hl.get(&cell.hl).copied().unwrap_or_default();
				let (fg, len) = (attr.fg.unwrap_or(self.fg), cell.text.encode_utf16().count());
				let marked = attr.bg.is_some_and(|bg| bg != self.bg) || attr.reverse;
				match runs.last_mut() {
					Some(run)
						if run.to == at
							&& run.fg == fg
							&& run.bold == attr.bold
							&& run.italic == attr.italic
							&& run.marked == marked =>
					{
						run.to += len;
					},
					_ => runs.push(Run { from: at, to: at + len, fg, bold: attr.bold, italic: attr.italic, marked }),
				}
				text.push_str(&cell.text);
				at += len;
			}
		}
		(text, runs)
	}

	/// The offset Tern counts for cell `(r, c)` of grid `g`.
	pub fn offset(&self, g: u64, r: usize, c: usize) -> usize {
		let Some(cells) = self.grids.get(&g) else { return 0 };
		let mut at = 0;
		for row in 0..cells.h.min(r + 1) {
			if row > 0 {
				at += 1;
			}
			let last = if row == r { c } else { cells.w };
			for col in 0..last.min(cells.w) {
				at += cells.get(row, col).map_or(1, |cell| cell.text.encode_utf16().count());
			}
		}
		at
	}

	/// The cell of grid `g` at the offset Tern counts, for a click.
	pub fn at_offset(&self, g: u64, offset: usize) -> (usize, usize) {
		let Some(cells) = self.grids.get(&g) else { return (0, 0) };
		let mut at = 0;
		for row in 0..cells.h {
			if row > 0 {
				at += 1;
			}
			for col in 0..cells.w {
				if at >= offset {
					return (row, col);
				}
				at += cells.get(row, col).map_or(1, |cell| cell.text.encode_utf16().count());
			}
			if at >= offset {
				return (row, cells.w.saturating_sub(1));
			}
		}
		(cells.h.saturating_sub(1), 0)
	}

	/// The cursor as nvim sends it: `(grid, row, col)`.
	pub fn caret(&self) -> (u64, usize, usize) {
		self.cursor
	}

	/// Every foreground color a cell can take, for the sheet that names them.
	pub fn palette(&self) -> Vec<u32> {
		let mut out: Vec<u32> = self.hl.values().filter_map(|a| a.fg).collect();
		out.push(self.fg);
		out
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The text of grid `src`, as the editor shows it.
	fn plain(g: &Grid, src: u64) -> String {
		g.text(src).0
	}

	fn ev(name: &str, args: Vec<Value>) -> Value {
		Value::Array(vec![name.into(), Value::Array(args)])
	}

	#[test]
	fn line_repeat_and_scroll() {
		let mut g = Grid::default();
		let cells = |s: Vec<Value>| Value::Array(s);
		let attr = Value::Map(vec![("foreground".into(), 0xff0000.into())]);
		let flushed = g.apply(&[
			ev("grid_resize", vec![1.into(), 4.into(), 3.into()]),
			ev("hl_attr_define", vec![5.into(), attr, Value::Map(vec![]), Value::Array(vec![])]),
			// "ab" with hl 5, then "x" repeated twice keeping hl 5.
			ev("grid_line", vec![1.into(), 0.into(), 0.into(), cells(vec![
				Value::Array(vec!["a".into(), 5.into()]),
				Value::Array(vec!["b".into()]),
				Value::Array(vec!["x".into(), 5.into(), 2.into()]),
			])]),
			ev("grid_line", vec![1.into(), 1.into(), 0.into(), cells(vec![Value::Array(vec!["1".into(), 0.into(), 4.into()])])]),
			ev("grid_line", vec![1.into(), 2.into(), 0.into(), cells(vec![Value::Array(vec!["2".into(), 0.into(), 4.into()])])]),
			Value::Array(vec!["flush".into(), Value::Array(vec![])]),
		]);
		assert!(flushed);
		let (text, runs) = g.text(1);
		assert_eq!(text, "abxx\n1111\n2222");
		// One run per color per row: the first row is hl 5, the rows below take the default.
		let want = [(0, 4, 0xff0000), (5, 9, 0xffffff), (10, 14, 0xffffff)];
		assert_eq!(runs.iter().map(|r| (r.from, r.to, r.fg)).collect::<Vec<_>>(), want);
		g.apply(&[ev("grid_scroll", vec![1.into(), 0.into(), 3.into(), 0.into(), 4.into(), 1.into()])]);
		assert_eq!(plain(&g, 1), "1111\n2222\n2222");
		g.apply(&[ev("grid_resize", vec![1.into(), 2.into(), 1.into()])]);
		assert_eq!(plain(&g, 1), "11");
	}

	#[test]
	fn multigrid_layout() {
		let mut g = Grid::default();
		let line = |grid: i64, row: i64, text: &str| {
			let cells = text.chars().map(|ch| Value::Array(vec![ch.to_string().into(), 0.into()])).collect();
			ev("grid_line", vec![grid.into(), row.into(), 0.into(), Value::Array(cells)])
		};
		g.apply(&[
			ev("grid_resize", vec![1.into(), 4.into(), 2.into()]),
			// A split of 2x2 at column 2 of the screen, and a 2x1 float at row 1, column 0.
			ev("grid_resize", vec![2.into(), 2.into(), 2.into()]),
			ev("win_pos", vec![2.into(), Value::Nil, 0.into(), 2.into(), 2.into(), 2.into()]),
			line(2, 0, "ab"),
			line(2, 1, "cd"),
			ev("grid_resize", vec![3.into(), 2.into(), 1.into()]),
			ev("win_float_pos", vec![3.into(), Value::Nil, "NW".into(), 1.into(), 0.into(), 0.into(), true.into(), 50.into(), 1.into(), 1.into(), 0.into()]),
			line(3, 0, "ef"),
			ev("grid_cursor_goto", vec![2.into(), 1.into(), 1.into()]),
		]);
		// Each window draws its own grid, at the cells `win_pos` gave it.
		assert_eq!(g.splits().iter().map(|(id, w)| (*id, w.row, w.col, w.w, w.h)).collect::<Vec<_>>(), [(2, 0, 2, 2, 2)]);
		assert_eq!(plain(&g, 2), "ab\ncd");
		assert_eq!(plain(&g, 3), "ef");
		assert_eq!(g.floats().iter().map(|(id, w)| (*id, w.row, w.col, w.w, w.h)).collect::<Vec<_>>(), [(3, 1, 0, 2, 1)]);
		// The caret of the second row's second cell is after "ab\nc"; a click there comes back.
		assert_eq!(g.caret(), (2, 1, 1));
		assert_eq!(g.offset(2, 1, 1), 4);
		assert_eq!(g.at_offset(2, 4), (1, 1));
		// A closed window leaves no box behind.
		g.apply(&[ev("win_close", vec![2.into()])]);
		assert!(g.splits().is_empty());
	}

	#[test]
	fn cmdline_edit_offsets() {
		// "é" is 2 bytes and 1 UTF-16 unit, "😀" is 4 bytes and 2 units.
		let mut cmd = Cmdline { prompt: ":".into(), text: "é😀x".into(), pos: 6 };
		assert_eq!(cmd.caret(), 3);
		// A stale len is ignored.
		assert!(!cmd.edit(0, 0, "a", 1, 3));
		// Replace the emoji with "ab", caret after it.
		assert!(cmd.edit(1, 3, "ab", 3, 4));
		assert_eq!((cmd.text.as_str(), cmd.pos), ("éabx", 4));
		// An offset inside a surrogate pair rounds down.
		assert_eq!(byte("😀", 1), 0);
	}

	#[test]
	fn messages() {
		let mut g = Grid::default();
		// nvim gives each message a new id.
		let show = |kind: &str, text: &str, append: bool, id: i64| {
			let content = Value::Array(vec![Value::Array(vec![0.into(), text.into(), 0.into()])]);
			ev("msg_show", vec![kind.into(), content, false.into(), true.into(), append.into(), id.into(), "".into()])
		};
		g.apply(&[show("echo", "ab", false, 1), show("echo", "cd", true, 2)]);
		// `:echon` joins the text under a new key, so Tern shows the toast again.
		assert_eq!(g.msgs.iter().map(|m| (m.key, m.text.as_str())).collect::<Vec<_>>(), [(2, "abcd")]);
		g.apply(&[show("list_cmd", "\n1\n2", false, 3), show("confirm", "Save?", false, 4)]);
		// `:echo ""` drops the toasts; the output and the question wait for a key.
		g.apply(&[show("empty", "", false, 5)]);
		assert_eq!(g.msgs.iter().map(|m| m.kind.as_str()).collect::<Vec<_>>(), ["list_cmd", "confirm"]);
		assert!(g.key() && g.msgs.is_empty());
	}
}
