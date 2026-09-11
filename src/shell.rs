//! Persistent Ctrl-O subshell, Midnight-Commander style.
//!
//! A single shell process is kept alive in a pseudo-terminal for the life of
//! the app. Ctrl-O *toggles* into it (forwarding the real terminal to the PTY)
//! and Ctrl-O again toggles back to the panels — the shell keeps running, so
//! its working directory, environment, history and jobs are preserved between
//! visits.

use crate::app::event::AppEvent;
use crate::util::async_bridge::AppSender;
use crate::util::{Error, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Environment variable set in the Ctrl-O subshell so a Rat Commander launched
/// from within it can tell it is nested (and disable its own subshell). Mirrors
/// Midnight Commander's `MC_SID` marker.
pub const SUBSHELL_ENV: &str = "RC_SUBSHELL";

/// Whether *this* process was started inside a Rat Commander Ctrl-O subshell
/// (i.e. the marker env var is present). Read once at startup.
pub fn in_subshell() -> bool {
    std::env::var_os(SUBSHELL_ENV).is_some_and(|v| !v.is_empty())
}

/// Byte sent by Ctrl-O in the legacy (raw) keyboard encoding.
const CTRL_O: u8 = 0x0F;
/// Unicode key code of the toggle key (`o`) in the kitty/xterm CSI encodings.
const CTRL_O_KEYCODE: u16 = b'o' as u16;
/// Ctrl bit in the kitty/xterm modifier encoding (parameter value minus one).
const CTRL_MOD: u16 = 4;
/// Longest unfinished escape sequence held back while waiting for the read
/// that completes it; anything longer is treated as garbage and forwarded so
/// a malformed flood can't stall input.
const MAX_HOLD: usize = 24;

/// What a scan of buffered input found.
enum Scan {
    /// Ctrl-O found: forward the bytes before `start`, swallow the toggle
    /// sequence itself, and return to the panels.
    Toggle { start: usize },
    /// No toggle. `hold` trailing bytes look like an unfinished escape
    /// sequence and should be kept back until the next read completes it.
    None { hold: usize },
}

/// Scan `buf` for a Ctrl-O keypress in any encoding a terminal may use while
/// the subshell owns the screen. The shell running inside the PTY can switch
/// the *real* terminal's keyboard encoding out from under us — fish 4.x, for
/// example, enables the kitty keyboard protocol (`CSI = 5 u`) at every prompt,
/// after which Ctrl-O arrives as `ESC[111;5u` rather than the raw 0x0F byte.
/// Recognized encodings:
///   - raw byte 0x0F (legacy)
///   - kitty CSI-u: `ESC [ 111 <:alternates>? ; <mods> <:event>? u`
///   - xterm modifyOtherKeys: `ESC [ 27 ; <mods> ; 111 ~`
fn scan_for_ctrl_o(buf: &[u8]) -> Scan {
    let mut i = 0;
    while i < buf.len() {
        match buf[i] {
            CTRL_O => return Scan::Toggle { start: i },
            0x1B if i + 1 < buf.len() && buf[i + 1] == b'[' => {
                // A CSI sequence: params are 0x30..=0x3F bytes, then an
                // optional intermediate, then a final byte in 0x40..=0x7E.
                let params_start = i + 2;
                let mut j = params_start;
                while j < buf.len() && (0x20..=0x3F).contains(&buf[j]) {
                    j += 1;
                }
                if j >= buf.len() {
                    // Unfinished sequence at the end of the chunk: hold it back.
                    let len = buf.len() - i;
                    return Scan::None { hold: if len <= MAX_HOLD { len } else { 0 } };
                }
                let terminator = buf[j];
                let params = &buf[params_start..j];
                let is_toggle = match terminator {
                    b'u' => csi_u_is_ctrl_o(params),
                    b'~' => modify_other_keys_is_ctrl_o(params),
                    _ => false,
                };
                if is_toggle {
                    return Scan::Toggle { start: i };
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    Scan::None { hold: 0 }
}

/// Modifier bitmask from a kitty/xterm `<mods>` parameter (encoded value minus
/// one), with the lock bits masked off so Caps/Num Lock don't break the match.
fn decoded_mods(field: &str) -> Option<u16> {
    const CAPS_LOCK: u16 = 64;
    const NUM_LOCK: u16 = 128;
    let raw: u16 = field.parse().ok()?;
    Some(raw.saturating_sub(1) & !(CAPS_LOCK | NUM_LOCK))
}

/// `ESC [ <key>[:alt] ; <mods>[:event] u` — is it a Ctrl-O press/repeat?
fn csi_u_is_ctrl_o(params: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(params) else {
        return false;
    };
    let mut fields = s.split(';');
    // `split` always yields a first item; the key field may carry
    // shifted/base-layout alternate codes after colons.
    let key_field = fields.next().expect("split yields a first item");
    let key = key_field.split(':').next().expect("split yields a first item");
    if key.parse() != Ok(CTRL_O_KEYCODE) {
        return false;
    }
    // Modifier field defaults to "1" (no modifiers) and may carry an event
    // type after a colon: 1 = press, 2 = repeat, 3 = release.
    let mut sub = fields.next().unwrap_or("1").split(':');
    let mods_field = sub.next().expect("split yields a first item");
    let Some(mods) = decoded_mods(mods_field) else {
        return false;
    };
    let event = sub.next().unwrap_or("1");
    mods == CTRL_MOD && (event == "1" || event == "2")
}

/// `ESC [ 27 ; <mods> ; <key> ~` (xterm modifyOtherKeys) — is it Ctrl-O?
fn modify_other_keys_is_ctrl_o(params: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(params) else {
        return false;
    };
    let mut fields = s.split(';');
    if fields.next() != Some("27") {
        return false;
    }
    let Some(mods) = fields.next().and_then(decoded_mods) else {
        return false;
    };
    let key = fields.next().and_then(|k| k.parse().ok());
    mods == CTRL_MOD && key == Some(CTRL_O_KEYCODE)
}

pub struct Subshell {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// When true, the reader thread mirrors PTY output to the real stdout.
    active: Arc<AtomicBool>,
    pid: Option<u32>,
    /// The emulator this shell feeds; handed to the backdrop via `set_current`.
    feed: crate::console::ConsoleFeed,
}

impl Subshell {
    /// Spawn the shell in `cwd` attached to a fresh PTY of the given size.
    ///
    /// The reader thread mirrors output to the real stdout only while toggled in
    /// (`Ctrl-O`), but *always* feeds the shared console emulator (`parser`) so
    /// the backdrop stays live, raises `used` on the first byte, and — while not
    /// toggled in — nudges the render loop (`tx`) to repaint.
    pub fn spawn(
        cwd: &Path,
        rows: u16,
        cols: u16,
        feed: crate::console::ConsoleFeed,
        tx: AppSender,
    ) -> Result<Subshell> {
        let parser = feed.parser.clone();
        let used = feed.used.clone();
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| Error::other(format!("openpty failed: {e}")))?;

        // The user's shell (config `shell`, else `$SHELL` / the shell we were
        // launched from), plus whatever flags that shell needs.
        let mut argv = interactive_argv().into_iter();
        let program = argv.next().expect("interactive_argv always yields a program");
        let mut cmd = CommandBuilder::new(program);
        cmd.args(argv);
        cmd.cwd(cwd);
        // Mark the shell's environment so a nested Rat Commander started from it
        // detects the nesting and disables its own (unsupported) subshell.
        cmd.env(SUBSHELL_ENV, "1");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::other(format!("failed to start shell: {e}")))?;
        let pid = child.process_id();
        // Close the slave in the parent so the PTY reports EOF when the shell exits.
        drop(pair.slave);

        let reader =
            pair.master.try_clone_reader().map_err(|e| Error::other(format!("pty reader: {e}")))?;
        let writer =
            pair.master.take_writer().map_err(|e| Error::other(format!("pty writer: {e}")))?;

        let active = Arc::new(AtomicBool::new(false));
        // Reader thread: drain the PTY for the shell's lifetime. Every chunk feeds
        // the shared console emulator (the backdrop); it is additionally mirrored
        // to the real stdout while toggled in (Ctrl-O), or — while not toggled in
        // — signals the render loop to repaint the backdrop.
        {
            let active = active.clone();
            let mut reader = reader;
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let mut out = std::io::stdout();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                            used.store(true, Ordering::Relaxed);
                            if active.load(Ordering::Relaxed) {
                                let _ = out.write_all(&buf[..n]);
                                let _ = out.flush();
                            } else {
                                // Coalesced nudge; a full channel just means a
                                // repaint is already pending.
                                let _ = tx.try_send(AppEvent::ConsoleOutput);
                            }
                        }
                    }
                }
            });
        }

        Ok(Subshell { master: pair.master, writer, child, active, pid, feed })
    }

    /// The emulator this shell feeds, for `Console::set_current`.
    pub fn console(&self) -> crate::console::ConsoleFeed {
        self.feed.clone()
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    /// Write a command line to the shell (as if typed), followed by Enter, so it
    /// runs in this persistent session. Used by the command line so its commands
    /// and the `Ctrl-O` shell are one and the same session.
    pub fn send_line(&mut self, line: &str) {
        let _ = self.writer.write_all(line.as_bytes());
        let _ = self.writer.write_all(b"\n");
        let _ = self.writer.flush();
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Forward the real terminal to the shell until Ctrl-O is pressed (or the
    /// shell exits). The terminal must already be in raw mode and on the
    /// primary screen.
    pub fn run_until_toggle(&mut self) {
        self.active.store(true, Ordering::Relaxed);
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        let mut buf = [0u8; 1024];
        // Bytes held back from the previous read: the tail of a possibly
        // unfinished escape sequence that could turn out to be Ctrl-O.
        let mut pending: Vec<u8> = Vec::new();
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            let n = match handle.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            pending.extend_from_slice(&buf[..n]);
            match scan_for_ctrl_o(&pending) {
                Scan::Toggle { start } => {
                    // Forward everything before the toggle sequence, swallow
                    // the sequence itself, then return to the panels.
                    let _ = self.writer.write_all(&pending[..start]);
                    let _ = self.writer.flush();
                    break;
                }
                Scan::None { hold } => {
                    let forward = pending.len() - hold;
                    if self.writer.write_all(&pending[..forward]).is_err() {
                        break;
                    }
                    let _ = self.writer.flush();
                    pending.drain(..forward);
                }
            }
        }
        self.active.store(false, Ordering::Relaxed);
    }

    /// The shell's current working directory, if it can be determined (Linux).
    pub fn child_cwd(&self) -> Option<std::path::PathBuf> {
        #[cfg(target_os = "linux")]
        {
            self.pid.and_then(|pid| std::fs::read_link(format!("/proc/{pid}/cwd")).ok())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = self.pid;
            None
        }
    }
}

impl Drop for Subshell {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

// ---------------------------------------------------------------------------
// Remote shell (SSH): Ctrl-O / command line on an SFTP/SCP panel run on the
// remote host, over the *same* SSH connection the file transfers use.
// ---------------------------------------------------------------------------

/// An interactive shell on a remote SSH host, presented exactly like the local
/// [`Subshell`]: it feeds a console emulator (the backdrop), mirrors to stdout
/// while toggled in (`Ctrl-O`), and takes typed input / command lines. The russh
/// channel is pumped by a background async task; the blocking-stdin
/// [`run_until_toggle`](RemoteShell::run_until_toggle) forwards keystrokes to it
/// through an input queue.
pub struct RemoteShell {
    /// Bytes to write to the remote shell (drained by the pump task).
    input_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// Latest requested PTY size, applied by the pump on the next loop turn.
    resize: Arc<Mutex<Option<(u16, u16)>>>,
    resize_notify: Arc<tokio::sync::Notify>,
    /// When true, the pump mirrors remote output to the real stdout.
    active: Arc<AtomicBool>,
    /// Set by the pump when the channel closes (shell exited / disconnected).
    closed: Arc<AtomicBool>,
    feed: crate::console::ConsoleFeed,
    /// The remote directory last `cd`'d to, so the shell follows the panel without
    /// re-`cd`ing on every command.
    last_cd: Option<String>,
    task: tokio::task::JoinHandle<()>,
}

impl RemoteShell {
    /// Wrap an opened remote shell channel, spawning the pump that feeds `feed`.
    pub fn spawn(
        ch: crate::vfs::remote::RemoteShellChannel,
        feed: crate::console::ConsoleFeed,
        tx: AppSender,
    ) -> RemoteShell {
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let resize = Arc::new(Mutex::new(None));
        let resize_notify = Arc::new(tokio::sync::Notify::new());
        let active = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(remote_pump(
            ch.channel,
            feed.clone(),
            tx,
            active.clone(),
            closed.clone(),
            input_rx,
            resize.clone(),
            resize_notify.clone(),
        ));
        RemoteShell { input_tx, resize, resize_notify, active, closed, feed, last_cd: None, task }
    }

    /// The emulator this shell feeds, for `Console::set_current`.
    pub fn console(&self) -> crate::console::ConsoleFeed {
        self.feed.clone()
    }

    /// `cd` the remote shell into `dir` (a POSIX path), unless it is already
    /// there — so the shell follows the active panel like the local one does.
    pub fn cd_to(&mut self, dir: &str) {
        if self.last_cd.as_deref() != Some(dir) {
            self.send_line(&format!("cd -- {}", crate::vfs::remote::shell_quote(dir)));
            self.last_cd = Some(dir.to_string());
        }
    }

    /// Whether the channel is still open.
    pub fn is_alive(&self) -> bool {
        !self.closed.load(Ordering::Relaxed)
    }

    /// Write a command line to the remote shell (as if typed), followed by Enter.
    pub fn send_line(&mut self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        let _ = self.input_tx.send(bytes);
    }

    /// Request a PTY resize; applied by the pump on its next turn.
    pub fn resize(&self, rows: u16, cols: u16) {
        if let Ok(mut g) = self.resize.lock() {
            *g = Some((rows, cols));
        }
        self.resize_notify.notify_one();
    }

    /// Forward the real terminal to the remote shell until Ctrl-O is pressed (or
    /// the shell closes). Mirrors [`Subshell::run_until_toggle`]; the "writer" is
    /// the input queue the pump drains onto the channel.
    pub fn run_until_toggle(&mut self) {
        self.active.store(true, Ordering::Relaxed);
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        let mut buf = [0u8; 1024];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            if self.closed.load(Ordering::Relaxed) {
                break;
            }
            let n = match handle.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            pending.extend_from_slice(&buf[..n]);
            match scan_for_ctrl_o(&pending) {
                Scan::Toggle { start } => {
                    if start > 0 {
                        let _ = self.input_tx.send(pending[..start].to_vec());
                    }
                    break;
                }
                Scan::None { hold } => {
                    let forward = pending.len() - hold;
                    if forward > 0 {
                        let _ = self.input_tx.send(pending[..forward].to_vec());
                    }
                    pending.drain(..forward);
                }
            }
        }
        self.active.store(false, Ordering::Relaxed);
    }
}

impl Drop for RemoteShell {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Pump a remote shell channel: feed its output into the console emulator (and
/// stdout while toggled in), drain queued input onto the channel, and apply
/// resizes. Runs until the channel closes.
#[allow(clippy::too_many_arguments)]
async fn remote_pump(
    mut channel: russh::Channel<russh::client::Msg>,
    feed: crate::console::ConsoleFeed,
    tx: AppSender,
    active: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    mut input_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    resize: Arc<Mutex<Option<(u16, u16)>>>,
    resize_notify: Arc<tokio::sync::Notify>,
) {
    use tokio::io::AsyncWriteExt;
    let mut writer = channel.make_writer();
    let mut out = std::io::stdout();
    loop {
        // Apply a pending resize while the channel isn't otherwise borrowed.
        let pending = resize.lock().ok().and_then(|mut g| g.take());
        if let Some((rows, cols)) = pending {
            let _ = channel.window_change(cols as u32, rows as u32, 0, 0).await;
        }
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(russh::ChannelMsg::Data { data }) => {
                    feed_output(&data, &feed, &active, &mut out, &tx);
                }
                Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                    feed_output(&data, &feed, &active, &mut out, &tx);
                }
                Some(russh::ChannelMsg::Eof) | None => break,
                Some(_) => {}
            },
            Some(bytes) = input_rx.recv() => {
                let _ = writer.write_all(&bytes).await;
                let _ = writer.flush().await;
            }
            _ = resize_notify.notified() => { /* loops to apply the resize above */ }
        }
    }
    closed.store(true, Ordering::Relaxed);
    // Wake the render loop so the closed shell is noticed.
    let _ = tx.try_send(AppEvent::ConsoleOutput);
}

/// Feed a chunk of remote output into the emulator, mirroring it to stdout while
/// toggled in or nudging a repaint otherwise (mirrors the local reader thread).
fn feed_output(
    data: &[u8],
    feed: &crate::console::ConsoleFeed,
    active: &Arc<AtomicBool>,
    out: &mut std::io::Stdout,
    tx: &AppSender,
) {
    if let Ok(mut p) = feed.parser.lock() {
        p.process(data);
    }
    feed.used.store(true, Ordering::Relaxed);
    if active.load(Ordering::Relaxed) {
        let _ = out.write_all(data);
        let _ = out.flush();
    } else {
        let _ = tx.try_send(AppEvent::ConsoleOutput);
    }
}

// ---------------------------------------------------------------------------
// Which shell to run
// ---------------------------------------------------------------------------

/// The `shell` setting from `config.toml`, applied at startup and whenever the
/// setting changes. Empty means "detect it" — see [`preferred`].
static PREFERRED: RwLock<String> = RwLock::new(String::new());

/// Serializes the tests that swap the process-global [`PREFERRED`] shell, so a
/// test running in parallel never observes a value another test set. Shared
/// with `crate::app`'s command-line test.
#[cfg(test)]
pub(crate) static PREFERRED_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Set the configured shell program (the `shell` setting); empty = auto-detect.
pub fn set_preferred(program: &str) {
    if let Ok(mut p) = PREFERRED.write() {
        p.clear();
        p.push_str(program.trim());
    }
}

/// The command-line dialect a shell speaks: which flag runs a single command,
/// and whether an interactive one-shot (`-i`) makes sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// `sh`, `bash`, `zsh`, `fish`, `nu`, Git-Bash … — `-c <command>`.
    Posix,
    /// `cmd.exe` — `/C <command>`.
    Cmd,
    /// `powershell.exe` / `pwsh.exe` — `-Command <command>`.
    PowerShell,
}

/// A program path reduced to its lower-cased base name without an extension —
/// `file_stem`, except that both `/` and `\` separate, so a Windows shell path
/// is still recognized by a Unix build (and vice versa).
fn program_stem(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let stem = match name.rsplit_once('.') {
        Some((base, _)) if !base.is_empty() => base,
        _ => name,
    };
    stem.to_ascii_lowercase()
}

/// The dialect `program` speaks, from its file name. Unknown shells are assumed
/// POSIX on Unix and `cmd`-like on Windows, so a shell we've never heard of
/// still gets the flag its platform's shells conventionally use.
pub fn kind_of(program: &str) -> ShellKind {
    match program_stem(program).as_str() {
        "cmd" => ShellKind::Cmd,
        "powershell" | "pwsh" => ShellKind::PowerShell,
        // Git-Bash, MSYS2, WSL's `bash.exe`, Nushell and friends all take `-c`.
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "mksh" | "tcsh" | "csh" | "ash"
        | "busybox" | "nu" | "elvish" | "xonsh" | "yash" => ShellKind::Posix,
        _ if cfg!(windows) => ShellKind::Cmd,
        _ => ShellKind::Posix,
    }
}

/// The shell program to run: the `shell` setting when set, else the platform's
/// idea of the user's shell.
///
/// On Unix that is `$SHELL`. On Windows there is no such variable — `%COMSPEC%`
/// names `cmd.exe` no matter which shell the user actually lives in — so the
/// process tree is walked first to find the shell that launched us (see
/// [`parent_shell`]), and `%COMSPEC%` is only the fallback.
pub fn preferred() -> String {
    if let Some(p) = PREFERRED.read().ok().map(|p| p.clone())
        && !p.is_empty()
    {
        return p;
    }
    detected().to_string()
}

/// The auto-detected shell, resolved once for the life of the process: our
/// ancestry can't change under us, and on Windows walking it costs a system
/// call per ancestor — too much to repeat for every command run.
fn detected() -> &'static str {
    static DETECTED: OnceLock<String> = OnceLock::new();
    DETECTED.get_or_init(|| {
        if cfg!(windows) {
            parent_shell()
                .unwrap_or_else(|| std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()))
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
        }
    })
}

/// Walk up the process tree looking for the shell Rat Commander was started
/// from, so `Ctrl-O` and the command line land in the same shell the user typed
/// `rc` into (PowerShell, `pwsh`, Git-Bash, …) rather than always `cmd.exe`.
///
/// Only ancestors whose *own* name says "shell" count; anything else (a
/// terminal emulator, an IDE, `explorer.exe`, a nested Rat Commander) is
/// skipped. The walk is bounded, and a candidate whose start time is later than
/// ours is rejected — on Windows a recorded parent PID outlives the parent and
/// may have been recycled by an unrelated process.
#[cfg(windows)]
fn parent_shell() -> Option<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    /// How many ancestors to inspect. A shell is normally the direct parent;
    /// the slack covers launcher shims (`rc.cmd`, `cargo run`, a `.lnk`).
    const MAX_DEPTH: usize = 8;

    let mut sys = System::new();
    let exe_only = ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet);
    let mut pid = sysinfo::get_current_pid().ok()?;
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, exe_only);
    let mut started = sys.process(pid)?.start_time();

    for _ in 0..MAX_DEPTH {
        let parent = sys.process(pid)?.parent()?;
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[parent]), true, exe_only);
        let p = sys.process(parent)?;
        // A parent that started after its child is a recycled PID, not our
        // ancestor: the chain is broken, so stop rather than trust it.
        if p.start_time() > started {
            return None;
        }
        let name = p.name().to_string_lossy().into_owned();
        if is_shell_name(&name) {
            // Prefer the full path so we launch exactly this binary (there can
            // be several `pwsh.exe` on a machine); fall back to the bare name,
            // which PATH resolves.
            return Some(p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or(name));
        }
        started = p.start_time();
        pid = parent;
    }
    None
}

#[cfg(not(windows))]
fn parent_shell() -> Option<String> {
    None
}

/// Whether an executable's file name is one of the shells we recognize. Used to
/// pick an ancestor process out of the tree, so it must not match the terminals
/// and launchers that also sit above us.
#[cfg(windows)]
fn is_shell_name(name: &str) -> bool {
    matches!(
        program_stem(name).as_str(),
        "powershell" | "pwsh" | "cmd" | "bash" | "sh" | "zsh" | "fish" | "nu" | "elvish" | "xonsh"
    )
}

/// Argv for an interactive shell session (`Ctrl-O`): the shell program followed
/// by any flags it needs, with no command to run.
pub fn interactive_argv() -> Vec<String> {
    interactive_argv_for(&preferred())
}

/// Argv that runs `cmd` once through the user's shell, as typed at Rat
/// Commander's own command line or in an F2 user-menu entry.
///
/// A POSIX shell is run **interactively** (`-i -c …`) so the command sees the
/// same aliases, shell functions and rc-file environment as the user's normal
/// prompt: a non-interactive shell never sources `~/.bashrc`/`~/.zshrc`, and
/// bash disables alias expansion outright when non-interactive, so an alias
/// typed here would silently expand to nothing ("command not found"). `cmd.exe`
/// and PowerShell have no rc-file aliases to bring in, so they run the plain
/// one-shot form.
pub fn command_argv(cmd: &str) -> Vec<String> {
    let program = preferred();
    match kind_of(&program) {
        ShellKind::Posix => {
            let mut argv = vec![program, "-i".to_string(), "-c".to_string()];
            argv.push(cmd.to_string());
            argv
        }
        _ => one_shot_argv(&program, cmd),
    }
}

/// Argv that runs `cmd` once, non-interactively, for commands Rat Commander
/// composes itself rather than ones the user typed: launching an external
/// editor/viewer, and the `rc.ext` filter pipelines.
///
/// On Unix that is plain POSIX `sh` — `rc.ext` entries and extfs helpers are
/// written in `sh` syntax, which a user's `fish` or `nu` login shell would not
/// understand. Windows has no such lingua franca, so it uses the preferred
/// shell, the one those commands were written for.
pub fn script_argv(cmd: &str) -> Vec<String> {
    if cfg!(windows) {
        one_shot_argv(&preferred(), cmd)
    } else {
        vec!["sh".to_string(), "-c".to_string(), cmd.to_string()]
    }
}

/// A `tokio` command built from one of the argv helpers above (element 0 is the
/// program, the rest its arguments). Callers that need a PTY instead hand the
/// argv straight to `portable_pty`.
pub fn command_from(argv: Vec<String>) -> tokio::process::Command {
    let mut argv = argv.into_iter();
    let program = argv.next().expect("a shell argv always starts with its program");
    let mut c = tokio::process::Command::new(program);
    c.args(argv);
    c
}

/// [`interactive_argv`] for an explicit shell program (the testable half).
fn interactive_argv_for(program: &str) -> Vec<String> {
    let mut argv = vec![program.to_string()];
    if kind_of(program) == ShellKind::PowerShell {
        // Without this every Ctrl-O reprints the PowerShell copyright banner.
        argv.push("-NoLogo".to_string());
    }
    argv
}

/// Argv running `cmd` as a plain, non-interactive one-shot through `program`,
/// with whichever "run this command" flag its dialect uses.
fn one_shot_argv(program: &str, cmd: &str) -> Vec<String> {
    let mut argv = vec![program.to_string()];
    match kind_of(program) {
        ShellKind::Cmd => argv.push("/C".to_string()),
        ShellKind::PowerShell => {
            argv.push("-NoLogo".to_string());
            argv.push("-Command".to_string());
        }
        ShellKind::Posix => argv.push("-c".to_string()),
    }
    argv.push(cmd.to_string());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toggle_at(buf: &[u8]) -> Option<usize> {
        match scan_for_ctrl_o(buf) {
            Scan::Toggle { start } => Some(start),
            Scan::None { .. } => None,
        }
    }

    #[test]
    fn raw_ctrl_o() {
        assert_eq!(toggle_at(b"\x0f"), Some(0));
        assert_eq!(toggle_at(b"ls\x0fmore"), Some(2));
    }

    #[test]
    fn kitty_csi_u_ctrl_o() {
        // Plain press, press with explicit event type, and repeat.
        assert_eq!(toggle_at(b"\x1b[111;5u"), Some(0));
        assert_eq!(toggle_at(b"\x1b[111;5:1u"), Some(0));
        assert_eq!(toggle_at(b"\x1b[111;5:2u"), Some(0));
        // Caps Lock / Num Lock bits don't break the match.
        assert_eq!(toggle_at(b"\x1b[111;69u"), Some(0));
        assert_eq!(toggle_at(b"\x1b[111;197u"), Some(0));
    }

    #[test]
    fn kitty_csi_u_rejects_non_toggles() {
        // Release must not toggle (fish enables report-event-types).
        assert!(toggle_at(b"\x1b[111;5:3u").is_none());
        // Wrong key, missing ctrl, extra modifiers.
        assert!(toggle_at(b"\x1b[112;5u").is_none());
        assert!(toggle_at(b"\x1b[111u").is_none());
        assert!(toggle_at(b"\x1b[111;1u").is_none());
        assert!(toggle_at(b"\x1b[111;7u").is_none()); // ctrl+shift+alt
        // Kitty protocol *push/query* sequences, not keys at all.
        assert!(toggle_at(b"\x1b[=5u").is_none());
        assert!(toggle_at(b"\x1b[?0u").is_none());
        assert!(toggle_at(b"\x1b[>1u").is_none());
    }

    #[test]
    fn modify_other_keys_ctrl_o() {
        assert_eq!(toggle_at(b"\x1b[27;5;111~"), Some(0));
        assert!(toggle_at(b"\x1b[27;5;112~").is_none());
        assert!(toggle_at(b"\x1b[27;2;111~").is_none());
        // Ordinary special keys (e.g. Delete) pass through.
        assert!(toggle_at(b"\x1b[3~").is_none());
    }

    #[test]
    fn alternate_key_reports() {
        // Key field may carry shifted/base-layout alternates after a colon.
        assert_eq!(toggle_at(b"\x1b[111:79;5u"), Some(0));
    }

    #[test]
    fn embedded_in_stream() {
        let buf = b"abc\x1b[A\x1b[111;5uxyz";
        assert_eq!(toggle_at(buf), Some(6));
    }

    #[test]
    fn unfinished_sequence_is_held() {
        match scan_for_ctrl_o(b"ls\x1b[111;5") {
            Scan::None { hold } => assert_eq!(hold, 7),
            Scan::Toggle { .. } => panic!("must not toggle on a prefix"),
        }
        // A bare trailing ESC is forwarded immediately (vi-mode Esc must not lag).
        match scan_for_ctrl_o(b"ls\x1b") {
            Scan::None { hold } => assert_eq!(hold, 0),
            Scan::Toggle { .. } => panic!(),
        }
        // Over-long garbage "sequences" are not held back forever.
        match scan_for_ctrl_o(b"\x1b[0123456789012345678901234567") {
            Scan::None { hold } => assert_eq!(hold, 0),
            Scan::Toggle { .. } => panic!(),
        }
    }

    // --- Remote shell round-trip over a real (in-process) SSH connection ---

    /// A throwaway ed25519 host key for the test SSH server.
    const TEST_HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n\
QyNTUxOQAAACBhtSAp308g5/FxsHPUCHBLm2jW2k9S/rE+TqPjPHBVlAAAAJB9CQOFfQkD\n\
hQAAAAtzc2gtZWQyNTUxOQAAACBhtSAp308g5/FxsHPUCHBLm2jW2k9S/rE+TqPjPHBVlA\n\
AAAEBuA4oTbyADSU6M0oRqvoIzRfsXXZ2ESA5/JFHtNMzhKGG1ICnfTyDn8XGwc9QIcEub\n\
aNbaT1L+sT5Oo+M8cFWUAAAAB3JjLXRlc3QBAgMEBQY=\n\
-----END OPENSSH PRIVATE KEY-----\n";

    /// A minimal SSH server: accepts any password, accepts a session channel, and
    /// echoes back whatever the client sends (standing in for a remote shell).
    #[derive(Clone)]
    struct EchoServer;

    impl russh::server::Server for EchoServer {
        type Handler = EchoServer;
        fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> EchoServer {
            EchoServer
        }
    }

    impl russh::server::Handler for EchoServer {
        type Error = russh::Error;

        async fn auth_password(
            &mut self,
            _user: &str,
            _password: &str,
        ) -> std::result::Result<russh::server::Auth, Self::Error> {
            Ok(russh::server::Auth::Accept)
        }

        async fn channel_open_session(
            &mut self,
            _channel: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _session: &mut russh::server::Session,
        ) -> std::result::Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn data(
            &mut self,
            channel: russh::ChannelId,
            data: &[u8],
            session: &mut russh::server::Session,
        ) -> std::result::Result<(), Self::Error> {
            let _ = session.data(channel, data.to_vec());
            Ok(())
        }
    }

    #[tokio::test]
    async fn remote_shell_round_trips_over_ssh() {
        use russh::server::Server as _; // brings `run_on_socket` into scope

        let key = russh::keys::PrivateKey::from_openssh(TEST_HOST_KEY).expect("host key");
        let config =
            std::sync::Arc::new(russh::server::Config { keys: vec![key], ..Default::default() });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut server = EchoServer;
            let _ = server.run_on_socket(config, &listener).await;
        });

        // Connect the client and open an interactive shell channel — the exact
        // path Ctrl-O / the command line takes on an SFTP/SCP panel.
        let creds = crate::vfs::remote::RemoteCreds {
            protocol: crate::vfs::remote::Protocol::Sftp,
            host: "127.0.0.1".to_string(),
            port,
            user: "u".to_string(),
            password: "p".to_string(),
            path: String::new(),
            passive: true,
            key_file: String::new(),
            key_passphrase: String::new(),
        };
        let handle = crate::vfs::remote::ssh_connect(&creds).await.expect("ssh connect");
        let ch = crate::vfs::remote::open_shell_channel(&handle, 24, 80).await.expect("shell");

        let (tx, _rx) = crate::util::async_bridge::channel();
        let feed = crate::console::ConsoleFeed::new(24, 80);
        let mut shell = RemoteShell::spawn(ch, feed.clone(), tx);
        assert!(shell.is_alive());

        // Send a command; the echo server sends it straight back, which the pump
        // feeds into the console emulator (the backdrop).
        shell.send_line("echo hello world");
        let mut found = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            let text = {
                let p = feed.parser.lock().unwrap();
                let s = p.screen();
                let (rows, cols) = s.size();
                let mut out = String::new();
                for r in 0..rows {
                    for c in 0..cols {
                        if let Some(cell) = s.cell(r, c) {
                            out.push_str(cell.contents());
                        }
                    }
                }
                out
            };
            if text.contains("echo hello world") {
                found = true;
                break;
            }
        }
        assert!(found, "the remote shell echoed the command back to the console backdrop");
    }

    // -- Which shell to run -------------------------------------------------

    #[test]
    fn shell_kind_from_program_name() {
        // Windows shells are recognized by name whichever platform we build on,
        // so a config pointing at one is honoured under a cross-compile too.
        assert_eq!(kind_of("cmd"), ShellKind::Cmd);
        assert_eq!(kind_of(r"C:\Windows\System32\cmd.exe"), ShellKind::Cmd);
        assert_eq!(kind_of("PowerShell.EXE"), ShellKind::PowerShell);
        assert_eq!(kind_of(r"C:\Program Files\PowerShell\7\pwsh.exe"), ShellKind::PowerShell);
        assert_eq!(kind_of("/bin/bash"), ShellKind::Posix);
        assert_eq!(kind_of("fish"), ShellKind::Posix);
        assert_eq!(kind_of(r"C:\Program Files\Git\bin\bash.exe"), ShellKind::Posix);
        // An unknown shell falls back to its platform's convention.
        let unknown = if cfg!(windows) { ShellKind::Cmd } else { ShellKind::Posix };
        assert_eq!(kind_of("some-new-shell"), unknown);
    }

    #[test]
    fn interactive_argv_matches_the_shell_dialect() {
        // A POSIX shell and cmd.exe need no flags to sit at a prompt; PowerShell
        // would otherwise reprint its banner on every Ctrl-O.
        assert_eq!(interactive_argv_for("/bin/zsh"), ["/bin/zsh"]);
        assert_eq!(interactive_argv_for("cmd.exe"), ["cmd.exe"]);
        assert_eq!(interactive_argv_for("pwsh"), ["pwsh", "-NoLogo"]);
    }

    #[test]
    fn one_shot_argv_uses_each_shell_s_run_command_flag() {
        assert_eq!(one_shot_argv("/bin/sh", "ls -l"), ["/bin/sh", "-c", "ls -l"]);
        assert_eq!(one_shot_argv("cmd.exe", "dir"), ["cmd.exe", "/C", "dir"]);
        assert_eq!(
            one_shot_argv("pwsh.exe", "Get-ChildItem"),
            ["pwsh.exe", "-NoLogo", "-Command", "Get-ChildItem"]
        );
    }

    /// The configured `shell` setting overrides detection, and clearing it hands
    /// the choice back to the platform. Both halves are asserted in one test:
    /// `PREFERRED` is process-global, so a second test racing this one could see
    /// a value it did not set.
    #[test]
    fn configured_shell_overrides_detection() {
        let _guard = PREFERRED_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let restore = preferred();
        set_preferred("  /usr/bin/fish  ");
        assert_eq!(preferred(), "/usr/bin/fish", "trimmed, and used verbatim");
        // The command line runs a POSIX shell interactively so aliases expand.
        assert_eq!(command_argv("ll"), ["/usr/bin/fish", "-i", "-c", "ll"]);

        set_preferred(r"C:\Program Files\PowerShell\7\pwsh.exe");
        assert_eq!(
            command_argv("gci"),
            [r"C:\Program Files\PowerShell\7\pwsh.exe", "-NoLogo", "-Command", "gci"],
            "a path with spaces stays one argument"
        );

        set_preferred("");
        // Nothing configured: Unix follows $SHELL (with a POSIX fallback), and
        // Windows falls back to %COMSPEC% when no shell ancestor is found.
        if cfg!(unix) {
            assert_eq!(preferred(), std::env::var("SHELL").unwrap_or("/bin/sh".into()));
        }
        set_preferred(&restore);
    }

    /// Commands Rat Commander composes itself (external editor/viewer, `rc.ext`
    /// filters) are `sh` scripts, so on Unix they must keep running under `sh`
    /// even when the user's shell is something that can't parse them.
    #[test]
    #[cfg(unix)]
    fn script_argv_stays_posix_sh_on_unix() {
        let _guard = PREFERRED_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let restore = preferred();
        set_preferred("/usr/bin/nu");
        assert_eq!(script_argv("less \"a b\""), ["sh", "-c", "less \"a b\""]);
        set_preferred(&restore);
    }
}
