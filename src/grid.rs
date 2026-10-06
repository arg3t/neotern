//! The `ext_linegrid` model of grid 1, rendered as one ANSI string per row.

use std::{
	collections::HashMap,
	fmt::Write,
	ops::Range,
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
	/// The hover or signature help text, from `float.lua`.
	pub doc:      Option<String>,
	/// The Telescope picker while one is open, from `telescope.lua`.
	pub pick:     Option<Pick>,
	/// The lines nvim-treesitter-context shows, from `context.lua`.
	pub context:  Vec<Vec<Chunk>>,
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
			doc:      None,
			pick:     None,
			context:  Vec::new(),
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
#[derive(Clone, Copy)]
pub struct Win {
	pub row:   usize,
	pub col:   usize,
	pub w:     usize,
	pub h:     usize,
	/// A float's drawing order (`compindex`); `None` for a split, which the screen holds.
	pub float: Option<i64>,
}

/// A Telescope picker (`:h telescope`), from `telescope.lua`.
pub struct Pick {
	pub title:   String,
	/// The text typed in the prompt.
	pub prompt:  String,
	/// The visible entries in order, each its id (the sorted-on text) and its display text.
	pub rows:    Vec<(String, String)>,
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
	/// Where the menu starts: grid `grid` (grid 1 for blink's), at `row`, `col`.
	pub grid:     u64,
	pub row:      usize,
	pub col:      usize,
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
							grid:     int(&a[4]).max(0) as u64,
							row:      int(&a[2]) as usize,
							col:      int(&a[3]) as usize,
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
						self.pick = (a.len() >= 10).then(|| Pick {
							title:   a[0].as_str().unwrap_or_default().into(),
							prompt:  a[1].as_str().unwrap_or_default().into(),
							rows:    a[2].as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(|r| {
								let [id, text] = r.as_array()?.as_slice() else { return None };
								Some((id.as_str()?.to_owned(), text.as_str()?.to_owned()))
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
					// The lines treesitter-context shows; each is runs of `{ text, fg, bold }`.
					"neotern_context" => {
						let lines = a.first().and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
						self.context = lines
							.iter()
							.map(|line| {
								let runs = line.as_array().map_or(&[][..], Vec::as_slice);
								runs.iter()
									.filter_map(|r| {
										let [text, fg, bold] = r.as_array()?.as_slice() else { return None };
										let fg = u32::try_from(int(fg)).ok();
										Some(Chunk { side: 0, text: text.as_str()?.into(), fg, bold: bold.as_bool()? })
									})
									.collect()
							})
							.collect();
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
						self.wins.insert(int(&a[0]) as u64, Win { row: n(2), col: n(3), w: n(4), h: n(5), float: None });
					},
					// nvim places the float (`screen_row`, `screen_col`); its size is its grid's.
					"win_float_pos" => {
						let at = |i: usize| a[i].as_f64().or_else(|| a[i].as_i64().map(|v| v as f64)).unwrap_or(0.0).max(0.0) as usize;
						self.wins.insert(int(&a[0]) as u64, Win { row: at(9), col: at(10), w: 0, h: 0, float: Some(int(&a[8])) });
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

	/// The cursor as the view draws it: `(1, row, col)` on the screen, or `(grid, row, col)` in
	/// a float.
	pub fn cursor(&self) -> (u64, usize, usize) {
		let (g, r, c) = self.cursor;
		match self.wins.get(&g) {
			Some(w) if w.float.is_none() => (1, w.row + r, w.col + c),
			Some(_) => (g, r, c),
			None => (1, r, c),
		}
	}

	/// The screen position of `(r, c)` in grid `g`.
	pub fn to_screen(&self, g: u64, r: usize, c: usize) -> (usize, usize) {
		self.wins.get(&g).map_or((r, c), |w| (w.row + r, w.col + c))
	}

	/// The shown floats in drawing order, each with its grid's size.
	pub fn floats(&self) -> Vec<(u64, Win)> {
		let mut floats: Vec<(u64, Win)> = self
			.wins
			.iter()
			.filter(|(_, w)| w.float.is_some())
			.filter_map(|(g, w)| self.grids.get(g).map(|cells| (*g, Win { w: cells.w, h: cells.h, ..*w })))
			.collect();
		floats.sort_by_key(|(_, w)| w.float);
		floats
	}

	/// The cell at `(r, c)` of `src`: 1 is the screen (each split's grid over grid 1), another id a
	/// float's grid.
	fn cell(&self, src: u64, r: usize, c: usize) -> &Cell {
		let split = (src == 1).then(|| {
			self.wins.iter().find(|(_, w)| w.float.is_none() && (w.row..w.row + w.h).contains(&r) && (w.col..w.col + w.w).contains(&c))
		});
		let (g, r, c) = match split.flatten() {
			Some((g, w)) => (*g, r - w.row, c - w.col),
			None => (src, r, c),
		};
		self.grids.get(&g).and_then(|cells| cells.get(r, c)).unwrap_or(&self.blank)
	}

	/// Whether the cell at `(r, c)` of `src` is the left half of a double-width char.
	pub fn wide(&self, src: u64, r: usize, c: usize) -> bool {
		self.cell(src, r, c + 1).text.is_empty()
	}

	/// Columns `cols` of row `r` as text with truecolor SGR; a block cursor's cell is drawn
	/// reversed. The view draws bar cursors.
	pub fn row(&self, src: u64, r: usize, cols: Range<usize>) -> String {
		let mut out = String::new();
		let mut last = None;
		let cursor = self.cursor();
		for c in cols {
			let cell = self.cell(src, r, c);
			// The right half of a double-width char is "".
			if cell.text.is_empty() {
				continue;
			}
			let attr = self.hl.get(&cell.hl).copied().unwrap_or_default();
			let (mut fg, mut bg) = (attr.fg.unwrap_or(self.fg), attr.bg.unwrap_or(self.bg));
			// The cmdline has Tern's caret, so the grid's cursor hides while it shows.
			if attr.reverse != (self.shape() == Some(Shape::Block) && (src, r, c) == cursor) {
				(fg, bg) = (bg, fg);
			}
			let key = (fg, bg, attr.bold, attr.italic, attr.underline, attr.strike);
			if last != Some(key) {
				let _ = write!(out, "\x1b[0;38;2;{};{};{};48;2;{};{};{}", fg >> 16, (fg >> 8) & 255, fg & 255, bg >> 16, (bg >> 8) & 255, bg & 255);
				for (on, code) in [(attr.bold, ";1"), (attr.italic, ";3"), (attr.underline, ";4"), (attr.strike, ";9")] {
					if on {
						out.push_str(code);
					}
				}
				out.push('m');
				last = Some(key);
			}
			out.push_str(&cell.text);
		}
		out.push_str("\x1b[0m");
		out
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn plain(g: &Grid, src: u64, r: usize) -> String {
		(0..g.w).map(|c| g.cell(src, r, c).text.clone()).collect()
	}

	fn ev(name: &str, args: Vec<Value>) -> Value {
		Value::Array(vec![name.into(), Value::Array(args)])
	}

	#[test]
	fn line_repeat_and_scroll() {
		let mut g = Grid::default();
		let cells = |s: Vec<Value>| Value::Array(s);
		let flushed = g.apply(&[
			ev("grid_resize", vec![1.into(), 4.into(), 3.into()]),
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
		assert_eq!(plain(&g, 1, 0), "abxx");
		assert_eq!(g.cell(1, 0, 1).hl, 5);
		g.apply(&[ev("grid_scroll", vec![1.into(), 0.into(), 3.into(), 0.into(), 4.into(), 1.into()])]);
		assert_eq!((plain(&g, 1, 0), plain(&g, 1, 1)), ("1111".into(), "2222".into()));
		g.apply(&[ev("grid_scroll", vec![1.into(), 0.into(), 3.into(), 0.into(), 4.into(), (-2).into()])]);
		assert_eq!(plain(&g, 1, 2), "1111");
		g.apply(&[ev("grid_resize", vec![1.into(), 2.into(), 1.into()])]);
		assert_eq!(plain(&g, 1, 0), "11");
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
			line(1, 0, "...."),
			line(1, 1, "####"),
			// A split of 2x1 at column 2 of the screen, and a 2x1 float at row 1, column 0.
			ev("grid_resize", vec![2.into(), 2.into(), 1.into()]),
			ev("win_pos", vec![2.into(), Value::Nil, 0.into(), 2.into(), 2.into(), 1.into()]),
			line(2, 0, "ab"),
			ev("grid_resize", vec![3.into(), 2.into(), 1.into()]),
			ev("win_float_pos", vec![3.into(), Value::Nil, "NW".into(), 1.into(), 0.into(), 0.into(), true.into(), 50.into(), 1.into(), 1.into(), 0.into()]),
			line(3, 0, "cd"),
			ev("grid_cursor_goto", vec![2.into(), 0.into(), 1.into()]),
		]);
		// The split's cells sit at its screen columns; the float keeps its own grid.
		assert_eq!(plain(&g, 1, 0), "..ab");
		assert_eq!(plain(&g, 1, 1), "####");
		assert_eq!((0..2).map(|c| g.cell(3, 0, c).text.clone()).collect::<String>(), "cd");
		// The cursor of a split is a screen position; a float's stays in its grid.
		assert_eq!(g.cursor(), (1, 0, 3));
		assert_eq!(g.floats().iter().map(|(id, w)| (*id, w.row, w.col, w.w, w.h)).collect::<Vec<_>>(), [(3, 1, 0, 2, 1)]);
		// A closed window leaves grid 1's cells showing.
		g.apply(&[ev("win_close", vec![2.into()])]);
		assert_eq!(plain(&g, 1, 0), "....");
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
