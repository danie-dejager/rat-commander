//! Every bundled template must compile — and run without crashing on
//! arbitrary input.

use super::bundle;
use super::lex::Diag;
use super::preproc::{Source, preprocess};

/// Compile the bundled template `name`, resolving includes from the bundle.
pub(crate) fn compile_bundled(name: &str) -> Result<super::ast::Program, Diag> {
    let text = bundle::get(name).expect("bundled").to_vec();
    let root = Source { name: name.into(), path: None, text };
    let mut loader = |inc: &str, _: Option<&std::path::Path>| {
        let base = inc.rsplit(['/', '\\']).next().unwrap_or(inc);
        bundle::get(base).map(|t| Source { name: base.into(), path: None, text: t.to_vec() })
    };
    super::parse::parse(preprocess(root, &mut loader)?)
}

#[test]
fn every_bundled_template_parses() {
    let mut failures = Vec::new();
    for (name, _) in bundle::entries() {
        if let Err(d) = compile_bundled(name) {
            failures.push(format!("{name}:{}:{}: {}", d.pos.line, d.pos.col, d.msg));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} failed:\n{}",
        failures.len(),
        bundle::entries().len(),
        failures.join("\n")
    );
}

/// Run the bundled template `name` over `data` with small limits.
pub(crate) fn run_bundled(
    name: &str,
    data: Vec<u8>,
    limits: super::interp::Limits,
) -> (super::interp::Interp, Option<(String, super::lex::Pos)>) {
    let prog = std::sync::Arc::new(compile_bundled(name).expect("compiles"));
    let mut it = super::interp::Interp::new(
        prog,
        Box::new(super::source::MemSource(data)),
        "sample.bin",
        limits,
    );
    let err = it.run();
    (it, err)
}

fn small_limits() -> super::interp::Limits {
    super::interp::Limits {
        max_nodes: 50_000,
        max_depth: 64,
        max_steps: 300_000,
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
        max_alloc: 4 << 20,
        max_output_lines: 1000,
    }
}

#[test]
fn every_bundled_template_survives_junk_input() {
    let handle = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
            let random: Vec<u8> = (0..4096)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    rng as u8
                })
                .collect();
            let inputs = [Vec::new(), vec![0u8; 4096], random];
            for (name, _) in bundle::entries() {
                for data in &inputs {
                    let _ = run_bundled(name, data.clone(), small_limits());
                }
            }
        })
        .expect("spawn");
    if let Err(e) = handle.join() {
        std::panic::resume_unwind(e);
    }
}

/// Run every template on real files found under `$BT_SAMPLES` (directories
/// separated by `:`), picking for each file the template auto-selection
/// would, and print what went wrong. A diagnostic, not a gate:
/// `BT_SAMPLES=/usr/share:/usr/bin cargo test --bin rc bt::corpus::samples -- --ignored --nocapture`
#[test]
#[ignore]
fn samples() {
    let dirs = std::env::var("BT_SAMPLES").unwrap_or_default();
    let per_template: usize =
        std::env::var("BT_PER").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let only = std::env::var("BT_ONLY").ok();
    let handle = std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(move || {
            let templates = super::library::discover_in(None);
            let mut used: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut ok = 0;
            let mut bad = 0;
            for dir in dirs.split(':').filter(|d| !d.is_empty()) {
                for e in walkdir::WalkDir::new(dir)
                    .max_depth(6)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if !e.file_type().is_file() {
                        continue;
                    }
                    let Ok(meta) = e.metadata() else { continue };
                    if meta.len() == 0 || meta.len() > 64 << 20 {
                        continue;
                    }
                    let name = e.file_name().to_string_lossy().into_owned();
                    let Ok(mut f) = std::fs::File::open(e.path()) else { continue };
                    let mut head = vec![0u8; 2048];
                    let n = std::io::Read::read(&mut f, &mut head).unwrap_or(0);
                    head.truncate(n);
                    let Some(best) = super::library::auto_pick(&templates, &name, &head) else {
                        continue;
                    };
                    let tname = templates[best].file_name.clone();
                    if only
                        .as_deref()
                        .is_some_and(|o| !o.split(',').any(|x| x.eq_ignore_ascii_case(&tname)))
                    {
                        continue;
                    }
                    let count = used.entry(tname.clone()).or_default();
                    if *count >= per_template {
                        continue;
                    }
                    *count += 1;
                    let Ok(data) = std::fs::read(e.path()) else { continue };
                    let mut limits = super::interp::Limits::run();
                    limits.deadline =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(20));
                    let started = std::time::Instant::now();
                    let (it, err) = run_bundled(&tname, data, limits);
                    let took = started.elapsed();
                    match err {
                        None => {
                            ok += 1;
                            println!(
                                "ok   {tname:24} {:>7} nodes {:>6}ms {}",
                                it.tree.nodes.len(),
                                took.as_millis(),
                                e.path().display()
                            );
                        }
                        Some((msg, pos)) => {
                            bad += 1;
                            println!(
                                "FAIL {tname:24} {}: {msg}  [{}]",
                                it.prog.pos_text(pos),
                                e.path().display()
                            );
                        }
                    }
                }
            }
            println!("\n{ok} ok, {bad} failed, {} templates exercised", used.len());
        })
        .expect("spawn");
    if let Err(e) = handle.join() {
        std::panic::resume_unwind(e);
    }
}

/// Run `$BT_FILE` (a template path, or a bundled name) over `$BT_DATA` and
/// dump the output and the start of the tree. A debugging aid:
/// `BT_FILE=x.bt BT_DATA=file cargo test --bin rc bt::corpus::debug_run -- --ignored --nocapture`
#[test]
#[ignore]
fn debug_run() {
    let file = std::env::var("BT_FILE").expect("BT_FILE");
    let data = std::fs::read(std::env::var("BT_DATA").expect("BT_DATA")).expect("data");
    let rows: usize = std::env::var("BT_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(move || {
            let text = std::fs::read(&file)
                .ok()
                .or_else(|| bundle::get(&file).map(<[u8]>::to_vec))
                .expect("template");
            let dir = std::path::Path::new(&file).parent().map(|p| p.to_path_buf());
            let root = Source { name: file.clone(), path: Some(file.clone().into()), text };
            let mut loader = |inc: &str, _: Option<&std::path::Path>| {
                let base = inc.rsplit(['/', '\\']).next().unwrap_or(inc);
                let p = dir.as_ref().map(|d| d.join(base));
                p.and_then(|p| std::fs::read(&p).ok())
                    .or_else(|| bundle::get(base).map(<[u8]>::to_vec))
                    .map(|t| Source { name: base.into(), path: None, text: t })
            };
            let prog = match preprocess(root, &mut loader).and_then(super::parse::parse) {
                Ok(p) => std::sync::Arc::new(p),
                Err(d) => panic!("{}:{}: {}", d.pos.line, d.pos.col, d.msg),
            };
            let mut limits = super::interp::Limits::run();
            if let Some(d) = std::env::var("BT_DEPTH").ok().and_then(|v| v.parse().ok()) {
                limits.max_depth = d;
            }
            let mut it = super::interp::Interp::new(
                prog,
                Box::new(super::source::MemSource(data)),
                "sample",
                limits,
            );
            let err = it.run();
            for l in &it.output {
                println!("| {l}");
            }
            println!("error: {err:?}");
            let lines = super::interp::display::dump(&mut it, rows);
            for l in lines {
                println!("{l}");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
