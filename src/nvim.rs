//! `nvim --embed` over msgpack-RPC on its stdio.

use std::{
	io::{self, BufReader, Write},
	process::{Child, ChildStdin, Command, ExitStatus, Stdio},
	sync::mpsc::Sender,
	thread,
};

use rmpv::Value;

/// What the reader thread reports.
pub enum Event {
	/// The params of one `redraw` notification: a list of `[name, args...]`.
	Redraw(Vec<Value>),
	/// nvim closed its output (it quit).
	Exit,
}

pub struct Nvim {
	child: Child,
	stdin: ChildStdin,
}

impl Nvim {
	/// Starts `nvim --embed args...`; redraws and the exit arrive on `tx`. `g:neotern` is set
	/// before the user config runs, so it can skip plugins that clash with neotern (like `g:neovide`).
	pub fn spawn(args: &[String], tx: Sender<Event>) -> io::Result<Self> {
		let mut child = Command::new("nvim")
			.args(["--embed", "--cmd", "let g:neotern = 1"])
			.args(args)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			// Our stdout is the pane: nvim's stderr must not reach it.
			.stderr(Stdio::null())
			.spawn()?;
		let stdin = child.stdin.take().expect("piped stdin");
		let stdout = child.stdout.take().expect("piped stdout");
		thread::spawn(move || {
			let mut r = BufReader::new(stdout);
			// [2, method, params] is a notification; responses and requests are ignored.
			while let Ok(Value::Array(msg)) = rmpv::decode::read_value(&mut r) {
				if let [Value::Integer(t), Value::String(m), Value::Array(params)] = msg.as_slice()
					&& t.as_u64() == Some(2)
					&& m.as_str() == Some("redraw")
					&& tx.send(Event::Redraw(params.clone())).is_err()
				{
					return;
				}
			}
			let _ = tx.send(Event::Exit);
		});
		Ok(Self { child, stdin })
	}

	/// Calls `method` as a notification: errors come back as `nvim_error_event`, which we ignore.
	pub fn notify(&mut self, method: &str, args: Vec<Value>) -> io::Result<()> {
		let msg = Value::Array(vec![2.into(), method.into(), Value::Array(args)]);
		let mut buf = Vec::new();
		rmpv::encode::write_value(&mut buf, &msg)?;
		self.stdin.write_all(&buf)?;
		self.stdin.flush()
	}

	pub fn wait(mut self) -> io::Result<ExitStatus> {
		drop(self.stdin);
		self.child.wait()
	}
}
