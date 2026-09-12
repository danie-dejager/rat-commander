# Rat Commander (`rc`)

A self-contained terminal file manager with modern features and built-in tools,
while staying true to the heritage of classics such as Norton Commander and
[Midnight Commander](https://midnight-commander.org/). Written in Rust with
[Ratatui](https://ratatui.rs/). It aims to need **no external tools** for its
core features: the viewer/editor with syntax highlighting, archive handling,
remote (FTP/SFTP/SCP) clients, disk explorer and process explorer are all built
in.

The installed executable is named **`rc`** for quick typing.

<img alt="image" src="https://img.playspoon.com/25t8uc.gif" />

<img width="1004" height="659" alt="image" src="https://github.com/user-attachments/assets/5b13c3c9-e770-4ce6-ac2b-560e7b5c3bad" />

<img width="1005" height="647" alt="image_2026-09-11_15-42-47" src="https://github.com/user-attachments/assets/7c465859-fb36-44e1-94ee-fa3000e9e8c9" />


---

## Features

- **Two panels** with **full**, **brief**, **details**, **tree** and **3D** view formats,
  vertical or horizontal split, configurable sort, multi-file selection, type
  markers and file-type colors. Full **mouse** support. The **details** view adds
  a background-loaded **preview** of the other panel's item — a syntax-highlighted
  text head, a centered image thumbnail (true-pixel where graphics are available,
  else half-block art) with an EXIF summary, an archive's file list, or a
  directory tree.
- **File operations** — copy / move / delete with a progress window and
  transfer-speed chart, rich overwrite handling, chmod / chown / symlink (with
  recursion), and make-directory.
- **Built-in viewer (F3)** — text and hex modes, goto, line wrap, syntax
  highlighting, a **rendered Markdown** mode for `.md` files, and hex-color
  swatches. Pages huge files straight from disk. Opens **images** fullscreen —
  true-pixel where the terminal supports graphics, half-block art otherwise
  (F8 toggles to the raw bytes).
- **Built-in editor (F4)** — `mcedit`-style block copy/move/delete, clipboard,
  search & replace, undo/redo, syntax highlighting, and an
  in-place **hex editor** for arbitrarily large files.
- **Multi rename** — batch-rename selected files with a masked, live two-column
  preview, counter, case transform and search-and-replace.
- **Search** — one dialog for the editor (F7/F4) *and* the viewer (F7): literal,
  **regex**, **hex** or **wildcard**, with case / whole-word / backwards options.
  **Find all** highlights every line holding the term and keeps it highlighted
  while you work; repeating a search steps to the next occurrence and wraps.
- **Git-aware panels** — inside a git work tree each file is tagged with its VCS
  state (`>` modified, `+` staged, `?` untracked, `!` conflict) in colour, the
  current **branch + ahead/behind** shows on the panel border, and one-key actions
  **stage/unstage** (`Ctrl-G`) or open a side-by-side **diff against HEAD**
  (`Alt-D`).
- **Git functions (`Alt-G`, or File → Git)** — **status**, **log**, **add**, 
  **unstage**, **rm**, **restore**, **commit**, **fetch**, **pull**, **push** 
  (with `--force-with-lease` or `--force`), **sync** (pull + push), 
  **checkout**, **reset**, **init** and **clone**. 
- **Command palette (Ctrl-P)** — one fuzzy-search box over every menu action,
  every setting (switch theme/language/graphics or flip a toggle in place), your
  directory **bookmarks**, the open remote connections, and your saved remote
  servers (reconnect); type a few letters and press Enter.
- **Directory tabs** — each panel keeps as many open directories as you like:
  **Ctrl-N** opens a tab, **Alt-K** closes one, **Ctrl-PageDown**/**Ctrl-PageUp**
  cycle them and **Alt-J** lists them to pick from (**Ctrl-Tab** works too on
  terminals that don't reserve it for their own tabs). A tab remembers its
  directory, view format, sort, filter, marks and cursor, and local tabs come
  back on the next run. The strip only appears once a panel has more than one.
- **Find file**, **Compare directories**, **Find duplicates**, and a
  side-by-side **Compare files** diff with in-place merging.
- **Synchronize directories** — mirror one panel's tree onto the other, in
  **one-way** (optionally deleting whatever the source doesn't have) or
  **two-way** (newer file wins) mode. The plan is **previewed in full** — every
  copy and delete, with totals — before a byte moves, and then runs through the
  ordinary transfer engine, so it shows progress, aborts, and can be sent to the
  **background**: *"mirror this folder to my SFTP server while I keep working"* is
  two dialogs.
- **Checksum** — compute a CRC32/MD5/SHA-1/SHA-256/SHA-512 digest of a file with
  a progress bar, and optionally verify it against a pasted reference checksum.
- **Send over LAN** (File menu) — share the highlighted file with a nearby phone
  or laptop: a one-shot HTTP server starts on a free port bound to your LAN IP,
  and the download URL is shown as a **QR code** (pixel graphics, or half-block
  cell art as a fallback). Select several files or a directory and they are
  zipped first (with a progress bar); the box shows a live download count and the
  server stops when you close it.
- **Auto-refreshing panels** — a panel re-reads itself when something else changes
  the directory it is showing, so a build or a `git checkout` in another window
  shows up without `Ctrl-R`.
- **System clipboard (`Ctrl-Ins`)** — copy the cursor's path, its bare name, or every
  marked path (one per line) to the **system** clipboard; in the editor `Ctrl-C` /
  `Ctrl-X` put the marked block there too. It uses the terminal's **OSC 52**
  sequence rather than a clipboard daemon, so it needs no X or Wayland session and
  **works over SSH** — copying on a remote server lands the text on the clipboard
  of the machine in front of you.
- **Archives** — browse and *edit* `.zip`, `.tar(.gz/.bz2/.xz)` and `.7z` like
  directories: copy and move files in and out, make and delete subdirectories,
  rename, move things around inside the archive, and compress a selection.
- **Remote filesystems** — SFTP, SCP and FTP/FTPS, each mounted into a panel;
  copy/move/delete works transparently across local, remote and archive panels.
  On an **SFTP/SCP** panel, the command line and **Ctrl-O** run a shell
  on the **remote host** over the same SSH connection — its output on the same
  console backdrop, no second login.
- **3D view** — a panel format that draws the directory the *other* panel is in. Two styles,
  chosen in Settings → Visual: **Cubes**, a tree of boxes joined by lines, and
  **Spare no expense**, an homage to IRIX's *fsn* — pale platforms standing on a
  ground plane under a sky gradient, joined by lines running over the ground,
  with the files on them drawn as solids **shaped and coloured by file type**.
  True-pixel on a graphics terminal, half-blocks or an ASCII ramp elsewhere.
- **Disk explorer** (treemap of disk usage), **process explorer** (btop-style
  system monitor), and a **disk manager** (Linux) to mount/unmount/format/sync
  drives and **flash or image** raw disk images.
- **Network connections** (Linux) — listening ports with their programs and all
  active connections with their type, service, live per-connection traffic rate
  (with a sparkline) and a details view; filter, sort, kill the owning process,
  and an optional root password for full visibility. A **per-service overview
  diagram** (Tab) groups connections into colour-coded cards showing each peer IP
  and its direction, with clickable/navigable addresses and reverse-DNS lookups.
- **Look & feel** — many color themes (fully customizable via `themes.toml` or
  the visual theme editor), truecolor gradients on **any element** — panel and
  dialog backgrounds, frames, cursor bars, menus, inputs and buttons each take
  their own two-color ramp, in one of four directions, animated or still, and
  nearly every preset ships with a set — an optional CPU/memory status widget,
  optional **Nerd Font file-type icons** in the listings, and a configurable
  **F2 user menu**.
- **Terminal graphics** — on terminals with a **Kitty**, **Sixel** or **iTerm2**
  graphics protocol, the progress bars, process-explorer graphs, transfer speed
  graph and the disk-explorer **treemap** (a nested "pillow" map of each folder's
  biggest files) are drawn as true-pixel gradient images, falling back
  automatically to block-character rendering elsewhere (can be forced off in settings).
- **Localization** — Configurable UI language with 18
  languages built in (English, German, French, Spanish, Portuguese, Dutch,
  Czech, Slovak, Hungarian, Serbian, Ukrainian, Russian, Japanese, Chinese
  traditional & simplified, Hindi, Persian, Arabic); translations live in
  editable `lang/*.toml` files and new languages can be dropped in. Right-to-left
  scripts (Arabic, Persian) are shaped and bidi-reordered for display on
  terminals without native bidi support (a **Reshape RTL text** setting turns
  this off when the terminal handles bidi itself).
- **Windows support** — Full support for windows drives using the familiar
  Alt-F1/Alt-F2 Norton Commander shortcuts. All features except Drive Manager and
  Network Connections are available. The command line and `Ctrl-O` shell run the
  classic *suspend-and-run* way on Windows (the TUI pauses while `cmd.exe` runs,
  then resumes) rather than the persistent behind-the-panels console used on
  Unix, so there is no live console backdrop there.

For a full, feature-by-feature walkthrough see the **[user manual](doc/MANUAL.md)** —
also available in-program by pressing **F1**.

---

## Keyboard shortcuts

On terminals where the function keys are awkward to reach, every `Fn` shortcut
also has a Midnight-Commander-style alias: press **Esc** then a digit — `Esc 1`
… `Esc 9` for `F1`…`F9`, and `Esc 0` for `F10` (or a quick **Alt**+digit).

### Panels

| Key | Action |
| --- | --- |
| `F1` | Help (the user manual) |
| `F2` | User menu (configurable) |
| `F3` | View file |
| `F4` | Edit file |
| `Shift-F4` | Edit a new file (asks for the name) |
| `F5` | Copy |
| `F6` | Rename / move |
| `Shift-F6` / `Ctrl-F6` | Multi rename (selected files) |
| `F7` | Make directory |
| `F8` | Delete |
| `F9` | Pulldown menu (Left/Right follows the active panel) |
| `F10` | Quit (confirmation) |
| `Ctrl-Q` | Quit immediately |
| `Tab` | Switch active panel |
| `↑ ↓ / PgUp PgDn / Home End` | Move the cursor |
| `Enter` | Open dir / enter archive / open file / run command line |
| `cd <dir>` + `Enter` | Change the active panel's directory |
| `Insert` / `Ctrl-T` | Tag file and advance |
| `+` / `-` / `*` | Select / unselect group (wildcard) / invert selection |
| `Ctrl-O` | Toggle the persistent subshell |
| `Ctrl-P` | Command palette (fuzzy-search every action, setting, bookmark, connection) |
| `Ctrl-\` | Directory hotlist (bookmarks): jump / add / remove |
| `Alt-←` / `Alt-→` (or `Alt-y` / `Alt-u`) | Go back / forward through the panel's visited directories |
| `Alt-H` | Directory history: pick any visited directory from a list |
| `Alt-I` | Point the other panel at this panel's directory |
| `Alt-O` | Show the cursor's directory on the other panel, and step down one entry |
| `Alt-T` | Cycle view format (full / brief / details / tree / 3D) |
| `Alt-Shift-I` | Set / clear the panel's persistent listing filter |
| `Alt-Shift-H` | Shell history window (recall a command without running it) |
| `Alt-G` | Open the **Git menu** (status, log, commit, push/pull, checkout, …) |
| `Ctrl-G` / `Alt-D` | Git: stage/unstage the selection · diff the file against HEAD |
| `Ctrl-Ins` | Copy the selected paths (or the cursor's) to the system clipboard |
| `Ctrl-R` | Re-read the active panel |
| `Alt-S` / `Ctrl-S` | Quick search the active panel (jump to the first matching name) |
| `Ctrl-E` | Toggle reverse sort order |
| `Ctrl-X` | Toggle vertical / horizontal split |
| `Ctrl-U` | Swap the two panels |
| `Ctrl-F1` / `Ctrl-F2` | Hide / show the left / right panel (reveals the console) |
| `Ctrl-F4` | Toggle half-height panels (reveals the console below) |
| `Ctrl-F5` | Show / hide the command prompt (hidden: typing starts a quick search) |
| `Alt-F1` / `Alt-F2` | Drive / connection picker (left / right panel) |

### Viewer (F3)

| Key | Action |
| --- | --- |
| `F1` | Help (opens the user manual) |
| `F2` | Toggle line wrap |
| `F4` | Toggle hex / text mode |
| `F5` | Goto (line / percent / byte offset) |
| `F7` | Search (`n` repeats) |
| `F8` | (Markdown) toggle Raw / Render — (image) toggle Image / Raw |
| `Esc` / `F10` / `q` | Close |

### Editor (F4)

| Key | Action |
| --- | --- |
| `F1` | Editor shortcut help |
| `F2` | Save |
| `Shift-F2` / `Ctrl-F2` | Save as… (browse + name) |
| `F3` | Start / end block mark |
| `F4` | Search & replace |
| `F5` / `F6` / `F8` | Copy / move / delete block |
| `Shift-F5` | Insert a file at the cursor |
| `F7` / `Shift-F7` | Search / search again |
| `F9` | Pulldown menu |
| `Shift-F9` | Toggle word wrap |
| `Ctrl-F9` | Toggle in-place hex editor |
| `Ins` | Toggle insert / overwrite |
| `Ctrl-C` / `Ctrl-X` / `Ctrl-V` | Copy / cut block to clipboard, paste |
| `Ctrl-Z` / `Ctrl-Y` | Undo / redo |
| `Ctrl-A` | Mark the whole file |
| `Ctrl-N` / `Ctrl-F` | New buffer / copy block to a file |
| `Ctrl-S` / `Ctrl-L` | Toggle syntax highlighting / repaint the screen |
| `Alt-L` / `Alt-B` | Go to line / matching bracket |
| `Alt-P` / `Alt-T` / `Alt-U` | Format paragraph / sort block / paste command output |
| `Alt-K` / `Alt-J` / `Alt-I` / `Alt-O` | Bookmark: toggle, next, previous, flush |
| `Esc` / `F10` | Quit (prompts if modified) |

### Dialogs

`Tab`/arrows move between fields **and onto the OK/Cancel buttons**, `Space`
toggles checkboxes and cycles choices, `Enter` confirms, `Esc` cancels (and
aborts progress dialogs, including long-running git network operations and the
directory-sync scan). The OK/Cancel and Yes/No buttons are also clickable.

See the **[user manual](doc/MANUAL.md)** for the process-explorer,
disk-explorer and hex-editor key tables, and for what every feature does.

---

## Installation

### Pre-built packages

Grab a release from the **Releases** page:

- **Linux** — `rc-<ver>-<arch>.tar.gz` archive, or a `.deb`
  (`amd64`, `arm64` for Raspberry Pi 64-bit, `armhf` for 32-bit):
  ```sh
  sudo dpkg -i rat-commander_<ver>_arm64.deb
  ```
- **Windows** — `rc-<ver>-x86_64-pc-windows-msvc.zip`, or the `.msi` installer
  (adds `rc` to your PATH).
- **macOS** — `rc-<ver>-<arch>.tar.gz`, or the `.pkg` installer (installs `rc`
  to `/usr/local/bin`). Intel and Apple Silicon builds are provided. The package
  is unsigned, so the first launch may require *System Settings → Privacy &
  Security → Open anyway*.

### From source

Requires a recent stable Rust toolchain (edition 2024, **Rust ≥ 1.85**), plus a
C++ compiler for the bundled `unrar` library — add `--no-default-features` to
build without RAR support if you'd rather not have one.

The quickest route is to build straight from the repository:

```sh
cargo install --git https://github.com/dividebysandwich/rat-commander
```

Or clone first, if you want to hack on it:

```sh
git clone https://github.com/dividebysandwich/rat-commander
cd rat-commander
cargo install --path .      # installs `rc` into ~/.cargo/bin
# or just run it:
cargo run --release
```

Either way `rc` lands in `~/.cargo/bin`, so make sure that's on your `PATH`.

---

## Building & packaging

```sh
cargo build --release            # target/release/rc
cargo test                       # run the test suite
cargo clippy --all-targets       # lints
```

Release binaries are stripped and optimized via the `[profile.release]` settings
in `Cargo.toml`.

Every push to `main` and every pull request runs
`.github/workflows/ci.yml`, which is exactly:

```sh
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

There is no `cargo fmt` gate — the source is hand-formatted and rustfmt would
rewrite most of it.

### Cross-compiling and packages

The `.github/workflows/release.yml` workflow builds every artifact. To reproduce
a build locally:

```sh
# Debian package (native arch)
cargo install cargo-deb
cargo build --release --target x86_64-unknown-linux-gnu
cargo deb --no-build --target x86_64-unknown-linux-gnu

# Raspberry Pi (cross-compiled) – needs Docker + `cross`
# (--no-default-features drops RAR, whose C++ lib won't cross-compile here)
cargo install cross
cross build --release --no-default-features --target aarch64-unknown-linux-gnu
cargo deb --no-build --no-strip --target aarch64-unknown-linux-gnu

# Windows MSI – on Windows with the WiX toolset
dotnet tool install --global wix --version 4.0.5
wix build packaging/windows/rc.wxs -d Version=0.1.0 \
    -d BinDir=target/x86_64-pc-windows-msvc/release -o rc.msi

# macOS .pkg – on macOS
pkgbuild --identifier com.rat-commander.rc --version 0.1.0 \
    --install-location /usr/local/bin --root <dir-containing-rc> rc.pkg
```

Some dependencies (`unrar`, `bzip2`, `xz2`, archive backends) compile bundled
C/C++ sources, so a C/C++ toolchain is required (provided automatically by
`cross` for the Raspberry Pi targets). RAR support is an optional build feature
(`rar`, on by default), omitted from the Raspberry Pi (arm) packages because the
C++ `unrar` library doesn't build with those cross toolchains.

---

## Configuration

Configuration lives in your platform config directory
(`~/.config/rat-commander/` on Linux): **`config.toml`** (written from the
Settings dialog), **`themes.toml`** (editable color themes), **`lang/`**
(one editable TOML per UI language), and **`menu`** (the F2 user menu, in
Midnight Commander format). See the
**[user manual](doc/MANUAL.md#configuration)** for details.

---

## License

GNU General Public License, version 2 (GPL-2.0-only). See the `LICENSE` file.
