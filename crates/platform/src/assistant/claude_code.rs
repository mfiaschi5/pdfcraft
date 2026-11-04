//! Claude Code as the assistant's way to Claude: the user's own, unmodified `claude` program,
//! signed in with their Claude subscription, run once per request in headless mode. What it is
//! given and how its output is read live in `pdfcraft_ai::claude_code` (no I/O); this runs it.
//!
//! - **Locked down.** The arguments turn off its tools, MCP servers, hooks, plugins, CLAUDE.md,
//!   memory and saved sessions. It runs in an empty folder of its own, without API keys in its
//!   environment, so the request goes on the subscription.
//! - **Bounded.** Its output is capped ([`MAX_OUTPUT_BYTES`], [`MAX_LINE_BYTES`]), and it is
//!   stopped when it goes quiet ([`IDLE`]), runs too long ([`TOTAL`]) or the user stops it.
//! - **Its sign-in stays its own.** PdfCraft never reads the program's credentials; it asks the
//!   program (`claude auth status`) whether it is signed in.

use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pdfcraft_ai::claude_code::{self, CLEARED_ENV, SignIn};
use pdfcraft_ai::transport::{HttpRequest, Transport, TransportError};

/// Most output read from one run.
pub const MAX_OUTPUT_BYTES: usize = 16 << 20;
/// Longest line of output.
pub const MAX_LINE_BYTES: usize = 4 << 20;
/// Stopped after this long with no output.
pub const IDLE: Duration = Duration::from_secs(180);
/// Stopped after this long in all.
pub const TOTAL: Duration = Duration::from_secs(15 * 60);
/// How long `claude auth status` may take.
const STATUS_TIME: Duration = Duration::from_secs(20);
/// What is kept of the program's stderr, to say why it failed.
const MAX_STDERR_BYTES: usize = 8 * 1024;

/// The `claude` program, run for each request.
#[derive(Clone, Debug)]
pub struct ClaudeCode {
    program: PathBuf,
    idle: Duration,
}

/// Kills the program if the run ends early (an error, a cap, the user stopping it).
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl ClaudeCode {
    pub fn new(program: PathBuf) -> Self {
        Self { program, idle: IDLE }
    }

    #[cfg(test)]
    fn quiet_after(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// The program found where Claude Code installs it ([`find`]).
    pub fn find() -> Option<Self> {
        find(std::env::var_os("HOME").map(PathBuf::from).as_deref(), std::env::var_os("PATH").as_deref()).map(Self::new)
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The empty folder it runs in, so no project's files or settings are near it.
    fn workdir() -> PathBuf {
        let dir = std::env::temp_dir().join("pdfcraft-claude-code");
        match std::fs::create_dir_all(&dir) {
            Ok(()) => dir,
            Err(_) => std::env::temp_dir(),
        }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.current_dir(Self::workdir()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for var in CLEARED_ENV {
            c.env_remove(var);
        }
        c
    }

    /// Ask the program how it is signed in.
    pub fn sign_in(&self) -> Result<SignIn, String> {
        let mut c = self.command();
        c.args(["auth", "status"]).stdin(Stdio::null());
        let child = c.spawn().map_err(|e| format!("couldn't start Claude Code: {e}"))?;
        let mut child = Running(child);
        let stdout = child.0.stdout.take();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(s) = stdout {
                let _ = s.take(64 * 1024).read_to_end(&mut out);
            }
            let _ = tx.send(out);
        });
        let out = rx.recv_timeout(STATUS_TIME).map_err(|_| "Claude Code didn't answer in time".to_owned())?;
        claude_code::sign_in(&out)
    }
}

/// Where Claude Code's program is: `PDFCRAFT_CLAUDE`, where its installers put it, the `PATH`,
/// and last the copy inside the Claude desktop app.
pub fn find(home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let name = if cfg!(windows) { "claude.exe" } else { "claude" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(own) = std::env::var_os("PDFCRAFT_CLAUDE") {
        candidates.push(PathBuf::from(own));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/bin").join(name));
        candidates.push(home.join(".claude/local").join(name));
    }
    if !cfg!(windows) {
        candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
        candidates.push(PathBuf::from("/usr/local/bin/claude"));
    }
    if let Some(path) = path {
        candidates.extend(std::env::split_paths(path).take(256).map(|d| d.join(name)));
    }
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    home.and_then(desktop_copy)
}

/// The newest `claude` inside the Claude desktop app (macOS).
fn desktop_copy(home: &Path) -> Option<PathBuf> {
    let root = home.join("Library/Application Support/Claude/claude-code");
    let version = |name: &str| -> Option<Vec<u32>> { name.split('.').map(|p| p.parse().ok()).collect() };
    let mut versions: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(Result::ok)
        .take(256)
        .filter_map(|e| Some((version(&e.file_name().to_string_lossy())?, e.path())))
        .collect();
    versions.sort();
    versions.into_iter().rev().find_map(|(_, dir)| {
        std::fs::read_dir(dir).ok()?.filter_map(Result::ok).take(64).map(|e| e.path().join("claude.app/Contents/MacOS/claude")).find(|p| p.is_file())
    })
}

/// Lines out of the program's output as it arrives, each one capped.
struct Lines {
    pending: Vec<u8>,
    total: usize,
}

impl Lines {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, TransportError> {
        self.total = self.total.saturating_add(chunk.len());
        if self.total > MAX_OUTPUT_BYTES {
            return Err(TransportError::TooLarge(MAX_OUTPUT_BYTES));
        }
        self.pending.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            out.push(String::from_utf8_lossy(&line).into_owned());
        }
        if self.pending.len() > MAX_LINE_BYTES {
            return Err(TransportError::TooLarge(MAX_LINE_BYTES));
        }
        Ok(out)
    }
}

impl Transport for ClaudeCode {
    fn post_stream(&self, req: &HttpRequest, on_bytes: &mut dyn FnMut(&[u8]) -> ControlFlow<()>) -> Result<u16, TransportError> {
        let run = claude_code::invocation(&req.body).map_err(TransportError::Refused)?;
        if let Some(reply) = &run.reply {
            let _ = on_bytes(reply);
            return Ok(200);
        }
        let mut c = self.command();
        c.args(&run.args);
        let child = c.spawn().map_err(|e| TransportError::Connect(format!("couldn't start Claude Code ({}): {e}", self.program.display())))?;
        let mut child = Running(child);
        // Written from a thread: a long input could fill the pipe before the program reads it.
        if let Some(mut stdin) = child.0.stdin.take() {
            let input = run.stdin.clone();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
        }
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        if let Some(mut stdout) = child.0.stdout.take() {
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(buf.get(..n).unwrap_or_default().to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
        let (etx, erx) = mpsc::channel::<Vec<u8>>();
        if let Some(stderr) = child.0.stderr.take() {
            std::thread::spawn(move || {
                let mut kept = Vec::new();
                let mut rest = stderr;
                let _ = (&mut rest).take(MAX_STDERR_BYTES as u64).read_to_end(&mut kept);
                // Drained, so a chatty program never blocks on a full pipe.
                let _ = std::io::copy(&mut rest, &mut std::io::sink());
                let _ = etx.send(kept);
            });
        }
        let mut t = run.translator();
        let mut lines = Lines { pending: Vec::new(), total: 0 };
        let started = Instant::now();
        let mut heard = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => {
                    heard = Instant::now();
                    for line in lines.push(&chunk)? {
                        let out = t.line(&line);
                        if !out.is_empty() && on_bytes(&out).is_break() {
                            return Err(TransportError::Cancelled);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if heard.elapsed() > self.idle || started.elapsed() > TOTAL {
                        return Err(TransportError::Timeout);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let last = String::from_utf8_lossy(&lines.pending).into_owned();
        let mut out = t.line(&last);
        let status = child.0.wait().ok();
        let stderr = erx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
        let mut why = pdfcraft_ai::text::capped(String::from_utf8_lossy(&stderr).trim(), 2_000);
        if why.is_empty()
            && let Some(code) = status.filter(|s| !s.success()).and_then(|s| s.code())
        {
            why = format!("exit status {code}");
        }
        out.extend(t.finish(&why));
        if !out.is_empty() {
            let _ = on_bytes(&out);
        }
        Ok(200)
    }
}

#[cfg(test)]
mod tests;
