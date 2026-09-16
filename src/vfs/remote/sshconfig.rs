//! `~/.ssh/config`, as far as connecting to a host needs it: `Host` blocks
//! with their patterns, and `HostName`, `User`, `Port`, `IdentityFile`,
//! `IdentitiesOnly`, `ProxyJump`, `ProxyCommand` and `HostKeyAlias` in them,
//! with `Include`d files read in place. `Match` blocks are skipped.
//!
//! As in OpenSSH, the first value found for a setting wins (so specific
//! `Host` blocks go before `Host *`), except that every `IdentityFile` found
//! is used.

use std::path::{Path, PathBuf};

/// How deep `Include`s may nest.
const MAX_DEPTH: usize = 16;

/// What the config says about one host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostSettings {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<String>,
    pub identities_only: bool,
    pub proxy_jump: Option<String>,
    pub proxy_command: Option<String>,
    pub host_key_alias: Option<String>,
}

#[derive(Debug, Clone)]
struct Pattern {
    glob: String,
    negated: bool,
}

#[derive(Debug, Clone)]
struct Block {
    /// `None` for what comes before the first `Host` (it applies to every
    /// host) and for a `Match` block (skipped).
    patterns: Option<Vec<Pattern>>,
    matching: bool,
    settings: Vec<(String, Vec<String>)>,
}

impl Block {
    fn applies_to(&self, host: &str) -> bool {
        if self.matching {
            return false;
        }
        let Some(patterns) = &self.patterns else { return true };
        let hit =
            |p: &Pattern| glob_match(&p.glob.to_ascii_lowercase(), &host.to_ascii_lowercase());
        patterns.iter().any(|p| !p.negated && hit(p))
            && !patterns.iter().any(|p| p.negated && hit(p))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SshConfig {
    blocks: Vec<Block>,
}

/// `*` and `?` wildcards.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// A line's keyword and arguments: `Key value`, `Key=value`, arguments in
/// double quotes kept whole.
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let key = line[..end].to_ascii_lowercase();
    let rest = line[end..].trim_start().strip_prefix('=').unwrap_or(line[end..].trim_start());
    let mut args = Vec::new();
    let mut chars = rest.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut arg = String::new();
        if c == '"' {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                arg.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        args.push(arg);
    }
    Some((key, args))
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

/// A leading `~` as the home directory.
pub(crate) fn expand_tilde(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

/// The files an `Include` names: relative to `base`, with `*` and `?`
/// allowed in the last part.
fn include_paths(arg: &str, base: &Path) -> Vec<PathBuf> {
    let path = expand_tilde(arg);
    let path = if path.is_absolute() { path } else { base.join(path) };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return Vec::new() };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| glob_match(&name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found
}

impl SshConfig {
    /// A config from its text; `base` is where relative `Include`s are looked
    /// for (`~/.ssh`).
    pub fn parse(text: &str, base: &Path) -> SshConfig {
        let mut cfg = SshConfig {
            blocks: vec![Block { patterns: None, matching: false, settings: Vec::new() }],
        };
        cfg.read(text, base, 0);
        cfg
    }

    fn read(&mut self, text: &str, base: &Path, depth: usize) {
        for (key, args) in text.lines().filter_map(split_line) {
            match key.as_str() {
                "host" => self.blocks.push(Block {
                    patterns: Some(
                        args.iter()
                            .flat_map(|a| a.split(','))
                            .filter(|a| !a.is_empty())
                            .map(|a| match a.strip_prefix('!') {
                                Some(g) => Pattern { glob: g.to_string(), negated: true },
                                None => Pattern { glob: a.to_string(), negated: false },
                            })
                            .collect(),
                    ),
                    matching: false,
                    settings: Vec::new(),
                }),
                "match" => {
                    self.blocks.push(Block { patterns: None, matching: true, settings: Vec::new() })
                }
                "include" if depth < MAX_DEPTH => {
                    for arg in &args {
                        for path in include_paths(arg, base) {
                            if let Ok(text) = std::fs::read_to_string(&path) {
                                self.read(&text, base, depth + 1);
                            }
                        }
                    }
                }
                _ => {
                    if let Some(block) = self.blocks.last_mut() {
                        block.settings.push((key, args));
                    }
                }
            }
        }
    }

    /// The user's `~/.ssh/config` — none in tests, where a developer's own
    /// config must not change what the tests connect to.
    pub fn load_user() -> SshConfig {
        #[cfg(test)]
        {
            SshConfig::default()
        }
        #[cfg(not(test))]
        {
            let Some(dir) = home_dir().map(|h| h.join(".ssh")) else {
                return SshConfig::default();
            };
            match std::fs::read_to_string(dir.join("config")) {
                Ok(text) => SshConfig::parse(&text, &dir),
                Err(_) => SshConfig::default(),
            }
        }
    }

    /// What the config sets for `host` (as it was typed: an alias or a name).
    pub fn resolve(&self, host: &str) -> HostSettings {
        let mut s = HostSettings::default();
        for block in self.blocks.iter().filter(|b| b.applies_to(host)) {
            for (key, args) in &block.settings {
                let first = args.first().cloned();
                let set = |slot: &mut Option<String>| {
                    if slot.is_none() {
                        *slot = first.clone();
                    }
                };
                match key.as_str() {
                    "hostname" => set(&mut s.hostname),
                    "user" => set(&mut s.user),
                    "port" if s.port.is_none() => s.port = first.and_then(|p| p.parse().ok()),
                    "identityfile" => s.identity_files.extend(first),
                    "identitiesonly" => {
                        s.identities_only |= first.is_some_and(|v| v.eq_ignore_ascii_case("yes"))
                    }
                    "proxyjump" => set(&mut s.proxy_jump),
                    "proxycommand" if s.proxy_command.is_none() => {
                        s.proxy_command = Some(args.join(" "))
                    }
                    "hostkeyalias" => set(&mut s.host_key_alias),
                    _ => {}
                }
            }
        }
        s
    }

    /// The hosts the config names outright (no wildcards, no negations), in
    /// the order written — what a connect form can offer.
    pub fn aliases(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for block in &self.blocks {
            for p in block.patterns.iter().flatten() {
                if !p.negated && !p.glob.contains(['*', '?']) && !out.contains(&p.glob) {
                    out.push(p.glob.clone());
                }
            }
        }
        out
    }
}

/// `%` tokens in a `HostName` or `IdentityFile`: `%h` the host name, `%n`
/// the host as typed, `%p` the port, `%r` the remote user, `%u` the local
/// user, `%d` the home directory, `%%` a percent sign.
pub fn expand_tokens(text: &str, host: &str, alias: &str, port: u16, user: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(host),
            Some('n') => out.push_str(alias),
            Some('p') => out.push_str(&port.to_string()),
            Some('r') => out.push_str(user),
            Some('u') => out.push_str(&local_user()),
            Some('d') => {
                out.push_str(&home_dir().map(|h| h.display().to_string()).unwrap_or_default())
            }
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// The name of the user running the program.
pub fn local_user() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default()
}

/// One hop of a `ProxyJump` list: `[user@]host[:port]`, or the same as an
/// `ssh://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpSpec {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

/// A `ProxyJump` value's hops, in order; empty for `none`.
pub fn jump_specs(value: &str) -> Vec<JumpSpec> {
    if value.trim().eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|spec| {
            let spec = spec.strip_prefix("ssh://").unwrap_or(spec);
            let (user, rest) = match spec.rsplit_once('@') {
                Some((u, r)) => (Some(u.to_string()), r),
                None => (None, spec),
            };
            // `[v6::addr]:port`, or `host:port`.
            let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
                match inner.split_once(']') {
                    Some((h, p)) => {
                        (h.to_string(), p.strip_prefix(':').and_then(|p| p.parse().ok()))
                    }
                    None => (inner.to_string(), None),
                }
            } else {
                match rest.rsplit_once(':') {
                    Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().ok()),
                    _ => (rest.to_string(), None),
                }
            };
            JumpSpec { user, host, port }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
# Global defaults come first only in name: first value wins.
User fallback

Host web prod-*
    HostName %h.example.com
    User deploy
    IdentityFile ~/.ssh/id_deploy

Host prod-db !prod-web
    Port=2222
    ProxyJump bastion,admin@jump2:2200
    IdentityFile "~/.ssh/id with space"

Match host legacy
    User ignored

Host bastion
    HostName bastion.example.com
    HostKeyAlias bastion-key
    IdentitiesOnly yes

Host *
    User everyone
    IdentityFile ~/.ssh/id_ed25519
"#;

    fn cfg() -> SshConfig {
        SshConfig::parse(CONFIG, Path::new("/nonexistent"))
    }

    #[test]
    fn the_first_value_wins_and_identity_files_add_up() {
        let s = cfg().resolve("prod-db");
        assert_eq!(s.hostname.as_deref(), Some("%h.example.com"), "tokens expand at connect");
        assert_eq!(s.user.as_deref(), Some("fallback"), "the global line comes before the host's");
        assert_eq!(s.port, Some(2222));
        assert_eq!(
            s.identity_files,
            vec!["~/.ssh/id_deploy", "~/.ssh/id with space", "~/.ssh/id_ed25519"]
        );
        assert_eq!(s.proxy_jump.as_deref(), Some("bastion,admin@jump2:2200"));
        let b = cfg().resolve("BASTION");
        assert_eq!(
            b.host_key_alias.as_deref(),
            Some("bastion-key"),
            "hosts match regardless of case"
        );
        assert!(b.identities_only);
    }

    #[test]
    fn negations_exclude_and_match_blocks_are_skipped() {
        let web = cfg().resolve("prod-web");
        assert_eq!(web.port, None, "!prod-web keeps the prod-db block away");
        assert_eq!(cfg().resolve("legacy").user.as_deref(), Some("fallback"));
        assert_eq!(cfg().resolve("unknown").identity_files, vec!["~/.ssh/id_ed25519"]);
    }

    #[test]
    fn aliases_are_the_hosts_named_outright() {
        assert_eq!(cfg().aliases(), vec!["web", "prod-db", "bastion"]);
    }

    #[test]
    fn includes_are_read_in_place_with_globs() {
        let dir = std::env::temp_dir().join(format!("rc_sshcfg_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("conf.d")).unwrap();
        std::fs::write(dir.join("conf.d/10-work.conf"), "Host work\n  HostName work.example\n")
            .unwrap();
        std::fs::write(dir.join("conf.d/ignored.txt"), "Host nope\n").unwrap();
        let cfg = SshConfig::parse("Include conf.d/*.conf\nHost after\n", &dir);
        assert_eq!(cfg.aliases(), vec!["work", "after"]);
        assert_eq!(cfg.resolve("work").hostname.as_deref(), Some("work.example"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tokens_and_jumps_parse() {
        assert_eq!(
            expand_tokens("%h-%n:%p/%r %%", "db.example", "db", 2222, "root"),
            "db.example-db:2222/root %"
        );
        assert_eq!(
            jump_specs("bastion, admin@jump2:2200,ssh://u@[2001:db8::1]:22"),
            vec![
                JumpSpec { user: None, host: "bastion".into(), port: None },
                JumpSpec { user: Some("admin".into()), host: "jump2".into(), port: Some(2200) },
                JumpSpec { user: Some("u".into()), host: "2001:db8::1".into(), port: Some(22) },
            ]
        );
        assert!(jump_specs("none").is_empty());
        assert!(glob_match("prod-*", "prod-db") && !glob_match("prod-?", "prod-db"));
    }
}
