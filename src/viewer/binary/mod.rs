//! Binary mode: what an executable or a library is made of.
//!
//! F3 on an ELF, PE or Mach-O file opens this instead of a screen of mojibake:
//! the format, architecture and type, how it is linked and hardened, and then
//! its sections, the libraries it loads, the symbols it imports and exports, its
//! functions and the readable strings inside it — each a list the viewer scrolls,
//! searches and filters (see [`view`]).
//!
//! Parsing goes through the `object` crate, which reads all three formats on
//! every platform this runs on, so a Windows DLL can be looked into from Linux
//! and a Mach-O from Windows. Only headers and tables are read, paged in from
//! the file as they are needed; the one pass over the whole file — for its
//! strings — streams it in chunks. Every list stops at [`MAX_ROWS`], so a
//! pathological file costs bounded memory.
//!
//! Analysis can take a few seconds on a debug build of a large library, so the
//! viewer runs it off the UI thread and shows the result when it lands. What
//! decides the mode the viewer *opens* in is [`sniff`], a look at the first
//! kilobyte; a file that sniffs right but then does not parse falls back to the
//! text view.

mod elf;
mod macho;
mod pe;
pub mod strings;
pub mod view;

#[cfg(test)]
pub(crate) mod tests;

use object::read::{
    ExportTarget, ImportLibraryFlags, NameOrOrdinal, Object, ObjectSection, ObjectSymbol, ReadRef,
};
use object::{BinaryFormat, FileKind, ObjectKind, SectionKind, SymbolKind};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Most rows any one list holds. A stripped firmware image can yield millions of
/// "strings" and a debug build of a browser engine a million symbols; past this
/// the list is cut and its count shown with a `+`, as the text view does for a
/// line count it has not finished.
pub const MAX_ROWS: usize = 250_000;

/// Bytes of a file's head that [`sniff`] looks at.
pub const SNIFF_BYTES: usize = 1024;

/// Most architectures a universal binary is believed to hold. Real ones carry
/// two or three; the bound is what tells one apart from a Java class file,
/// which starts with the same `CAFEBABE`.
const MAX_FAT_ARCHES: u32 = 32;

/// The lists, in the order their tabs appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Info,
    Sections,
    Libraries,
    Imports,
    Exports,
    Functions,
    Strings,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Info,
        Tab::Sections,
        Tab::Libraries,
        Tab::Imports,
        Tab::Exports,
        Tab::Functions,
        Tab::Strings,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The tab's title, untranslated (it is the l10n key).
    pub fn label(self) -> &'static str {
        match self {
            Tab::Info => "Info",
            Tab::Sections => "Sections",
            Tab::Libraries => "Libraries",
            Tab::Imports => "Imports",
            Tab::Exports => "Exports",
            Tab::Functions => "Functions",
            Tab::Strings => "Strings",
        }
    }
}

/// One line of the Info tab.
#[derive(Debug, Clone)]
pub struct Fact {
    /// What the line is about — an l10n key.
    pub label: &'static str,
    pub value: String,
    /// Whether `value` is a word to translate ("Executable", "Yes") rather than
    /// data shown as found (an architecture, a path, a hash).
    pub translate: bool,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub address: u64,
    pub size: u64,
    /// Where the section's bytes start in the file; `None` for one that takes
    /// no room there, such as `.bss`.
    pub offset: Option<u64>,
    /// A short, format-neutral word for what the section holds.
    pub kind: &'static str,
}

#[derive(Debug, Clone)]
pub struct Library {
    pub name: String,
    /// How it is loaded, when that is not simply "at startup": `delay-load`,
    /// `weak`, `re-export`, … — plus the version a Mach-O names.
    pub note: String,
}

/// An import, an export or a function.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// The name as stored — mangled, when it is. [`demangle`] makes it readable.
    pub name: String,
    /// For an import, the library it comes from (and the symbol version on
    /// ELF); for an export, where a forwarded one really lives.
    pub library: String,
    pub address: u64,
    pub size: u64,
    pub offset: Option<u64>,
}

/// A run of readable text found in the file.
#[derive(Debug, Clone)]
pub struct Str {
    pub offset: u64,
    pub text: String,
    /// Found as UTF-16 (little-endian), the way Windows stores most text.
    pub wide: bool,
}

/// Everything Binary mode shows about one file.
#[derive(Debug, Clone, Default)]
pub struct Binary {
    /// A few words for the viewer header: `ELF 64-bit x86-64`.
    pub summary: String,
    /// Addresses are 64-bit, so they are drawn with sixteen digits.
    pub is_64: bool,
    pub facts: Vec<Fact>,
    pub sections: Vec<Section>,
    pub libraries: Vec<Library>,
    pub imports: Vec<Symbol>,
    pub exports: Vec<Symbol>,
    pub functions: Vec<Symbol>,
    pub strings: Vec<Str>,
    /// Which lists stopped at [`MAX_ROWS`], by [`Tab::index`].
    pub capped: [bool; 7],
    /// A .NET assembly: its code section holds IL and metadata — type names,
    /// the program's own strings — rather than machine code, so the strings
    /// scan reads it as data.
    pub managed: bool,
}

impl Binary {
    /// Rows in `tab`'s list.
    pub fn len(&self, tab: Tab) -> usize {
        match tab {
            Tab::Info => self.facts.len(),
            Tab::Sections => self.sections.len(),
            Tab::Libraries => self.libraries.len(),
            Tab::Imports => self.imports.len(),
            Tab::Exports => self.exports.len(),
            Tab::Functions => self.functions.len(),
            Tab::Strings => self.strings.len(),
        }
    }

    /// Name of the section whose bytes hold file offset `off`, for the strings
    /// list. A linear walk: it runs for the rows on screen, over a section
    /// table that rarely reaches a hundred entries.
    pub fn section_at(&self, off: u64) -> Option<&str> {
        self.sections
            .iter()
            .filter(|s| s.size > 0)
            .find(|s| s.offset.is_some_and(|o| off >= o && off - o < s.size))
            .map(|s| s.name.as_str())
    }
}

/// Whether `head` — the first [`SNIFF_BYTES`] of a file, or all of a shorter
/// one — starts like a format Binary mode reads: ELF, a PE image, a Mach-O file
/// or a Mach-O universal binary.
///
/// Conservative and cheap, because it picks the mode the viewer opens in before
/// the analysis has run; the analysis then has the final word.
pub fn sniff(head: &[u8]) -> bool {
    match head {
        [0x7f, b'E', b'L', b'F', ..] => true,
        [0xfe, 0xed, 0xfa, 0xce | 0xcf, ..] | [0xce | 0xcf, 0xfa, 0xed, 0xfe, ..] => true,
        // Where a universal binary keeps its architecture count, a class file
        // keeps its version — never below 45 — so a small count is the tell.
        [0xca, 0xfe, 0xba, 0xbe | 0xbf, a, b, c, d, ..] => {
            (1..=MAX_FAT_ARCHES).contains(&u32::from_be_bytes([*a, *b, *c, *d]))
        }
        // A DOS stub is only the start of a PE image: the PE signature must be
        // where the stub's header says. A bare DOS program is not one.
        [b'M', b'Z', ..] if head.len() >= 0x40 => {
            let at = u32::from_le_bytes([head[0x3c], head[0x3d], head[0x3e], head[0x3f]]) as usize;
            head.get(at..at + 4) == Some(b"PE\0\0")
        }
        _ => false,
    }
}

/// [`sniff`] on the head of the file at `path`.
pub fn sniff_file(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut head = vec![0u8; SNIFF_BYTES];
    let mut got = 0;
    while got < head.len() {
        match f.read(&mut head[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    sniff(&head[..got])
}

/// Analyse the file at `path`. `None` when it is not a binary this reads, when
/// it is malformed past reading, or when `cancel` was raised part-way (the
/// viewer that asked has gone).
pub fn analyze_file(path: &Path, cancel: &AtomicBool) -> Option<Binary> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let cache = object::read::ReadCache::new(file);
    let image = locate(&cache, len)?;
    let mut bin = if image.base == 0 && image.size == len {
        build(&cache, &image, cancel)?
    } else {
        build(cache.range(image.base, image.size), &image, cancel)?
    };
    // A second handle for the strings pass. The cache keeps every block it has
    // read for as long as it lives — right for tables, wrong for a walk through
    // a whole file that may be gigabytes.
    let mut raw = File::open(path).ok()?;
    let read = |start: u64, n: usize| -> Vec<u8> {
        let mut buf = vec![0u8; n];
        if raw.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }
        let mut got = 0;
        while got < n {
            match raw.read(&mut buf[got..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => got += k,
            }
        }
        buf.truncate(got);
        buf
    };
    let (found, capped) = strings::scan(read, image.base, image.size, &code_ranges(&bin), cancel)?;
    bin.strings = found;
    bin.capped[Tab::Strings.index()] = capped;
    Some(bin)
}

/// Analyse a file already in memory.
#[cfg(test)]
pub fn analyze_bytes(data: &[u8]) -> Option<Binary> {
    let never = AtomicBool::new(false);
    let image = locate(data, data.len() as u64)?;
    let (base, size) = (image.base as usize, image.size as usize);
    let mut bin = build(&data[base..base + size], &image, &never)?;
    let read = |start: u64, n: usize| -> Vec<u8> {
        let start = (start as usize).min(data.len());
        data[start..(start + n).min(data.len())].to_vec()
    };
    let (found, capped) = strings::scan(read, image.base, image.size, &code_ranges(&bin), &never)?;
    bin.strings = found;
    bin.capped[Tab::Strings.index()] = capped;
    Some(bin)
}

/// Which part of a file is the image to analyse.
struct Image {
    /// Where it starts: zero, except for one architecture of a universal binary.
    base: u64,
    size: u64,
    /// For a universal binary, every architecture in it, and which one is shown.
    fat: Option<(Vec<String>, usize)>,
}

/// Find the image in a file: the file itself, or — for a universal binary — the
/// slice for the architecture this program runs on, else the first.
fn locate<'data, R: ReadRef<'data>>(data: R, len: u64) -> Option<Image> {
    use object::read::macho::{FatArch, MachOFatFile32, MachOFatFile64};
    let arches: Vec<(object::Architecture, u64, u64)> = match FileKind::parse(data).ok()? {
        FileKind::Elf32
        | FileKind::Elf64
        | FileKind::MachO32
        | FileKind::MachO64
        | FileKind::Pe32
        | FileKind::Pe64 => return Some(Image { base: 0, size: len, fat: None }),
        FileKind::MachOFat32 => MachOFatFile32::parse(data)
            .ok()?
            .arches()
            .iter()
            .map(|a| (a.architecture(), a.file_range().0, a.file_range().1))
            .collect(),
        FileKind::MachOFat64 => MachOFatFile64::parse(data)
            .ok()?
            .arches()
            .iter()
            .map(|a| (a.architecture(), a.file_range().0, a.file_range().1))
            .collect(),
        _ => return None,
    };
    // Only slices that lie within the file and really are Mach-O count; a class
    // file that got past `sniff` fails here.
    let usable: Vec<usize> = (0..arches.len())
        .filter(|&i| {
            let (_, off, size) = arches[i];
            off.checked_add(size).is_some_and(|end| end <= len)
                && size > 0
                && data
                    .read_bytes_at(off, size.min(8))
                    .is_ok_and(|head| sniff(head) && !head.starts_with(&[0xca, 0xfe]))
        })
        .collect();
    let host = host_architecture();
    let chosen =
        usable.iter().copied().find(|&i| Some(arches[i].0) == host).or(usable.first().copied())?;
    let names = arches.iter().map(|a| arch_name(a.0, None)).collect();
    Some(Image { base: arches[chosen].1, size: arches[chosen].2, fat: Some((names, chosen)) })
}

/// The architecture this program was built for, to pick that slice of a
/// universal binary first.
fn host_architecture() -> Option<object::Architecture> {
    use object::Architecture as A;
    match std::env::consts::ARCH {
        "x86_64" => Some(A::X86_64),
        "aarch64" => Some(A::Aarch64),
        "x86" => Some(A::I386),
        "arm" => Some(A::Arm),
        "powerpc" => Some(A::PowerPc),
        "powerpc64" => Some(A::PowerPc64),
        _ => None,
    }
}

/// Everything except the strings, from an image that starts at `image.base`.
fn build<'data, R: ReadRef<'data>>(data: R, image: &Image, cancel: &AtomicBool) -> Option<Binary> {
    let file = object::File::parse(data).ok()?;
    let format = file.format();
    if !matches!(format, BinaryFormat::Elf | BinaryFormat::Pe | BinaryFormat::MachO) {
        return None;
    }
    let base = image.base;
    let is_64 = file.is_64();
    let arch = arch_name(file.architecture(), file.sub_architecture());
    let mut bin = Binary { is_64, ..Binary::default() };

    for s in file.sections() {
        let name = String::from_utf8_lossy(s.name_bytes().unwrap_or_default()).into_owned();
        // Mach-O sections live inside segments, and the same section name can
        // appear in two of them, so the pair is the name.
        let name = match s.segment_name() {
            Ok(Some(seg)) if format == BinaryFormat::MachO => format!("{seg},{name}"),
            _ => name,
        };
        bin.sections.push(Section {
            name,
            address: s.address(),
            size: s.size(),
            offset: s.file_range().map(|(o, _)| base + o),
            kind: section_kind(s.kind()),
        });
    }

    // File offsets for addresses, through the sections that have both.
    let relocatable = file.kind() == ObjectKind::Relocatable;
    let mut spans: Vec<(u64, u64, u64)> = bin
        .sections
        .iter()
        .filter(|s| s.size > 0)
        .filter_map(|s| s.offset.map(|o| (s.address, s.size, o)))
        .collect();
    spans.sort_unstable();
    let locate_addr = |addr: u64| -> Option<u64> {
        let i = spans.partition_point(|s| s.0 <= addr).checked_sub(1)?;
        let (start, size, off) = spans[i];
        (addr - start < size).then(|| off + (addr - start))
    };

    if let Ok(libs) = file.import_libraries() {
        for lib in libs.flatten() {
            if bin.libraries.len() >= MAX_ROWS {
                break;
            }
            let name = String::from_utf8_lossy(lib.name()).into_owned();
            if name.is_empty() || bin.libraries.iter().any(|l| l.name == name) {
                continue;
            }
            bin.libraries.push(Library { name, note: library_note(lib.flags()) });
        }
    }

    if let Ok(imports) = file.imports() {
        let mut seen = std::collections::HashSet::new();
        for (i, imp) in imports.flatten().enumerate() {
            if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            if bin.imports.len() >= MAX_ROWS {
                bin.capped[Tab::Imports.index()] = true;
                break;
            }
            let name = name_or_ordinal(imp.name());
            let mut library = String::from_utf8_lossy(imp.library()).into_owned();
            if let object::read::ImportFlags::Elf { version: Some(v), .. } = imp.flags() {
                let v = String::from_utf8_lossy(v);
                library =
                    if library.is_empty() { v.into_owned() } else { format!("{library} ({v})") };
            }
            // Mach-O can list one import twice — once from the symbol table and
            // once from the dynamic linker's binding info.
            if seen.insert((name.clone(), library.clone())) {
                bin.imports.push(Symbol { name, library, address: 0, size: 0, offset: None });
            }
        }
    }

    if let Ok(exports) = file.exports() {
        for (i, exp) in exports.flatten().enumerate() {
            if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            if bin.exports.len() >= MAX_ROWS {
                bin.capped[Tab::Exports.index()] = true;
                break;
            }
            let name = name_or_ordinal(exp.name());
            let (address, library) = match exp.target() {
                ExportTarget::Address { address } => (address, String::new()),
                ExportTarget::Resolver { resolver, stub } => {
                    (stub.unwrap_or(resolver), String::new())
                }
                ExportTarget::TlvDescriptor { address } => (address, String::new()),
                ExportTarget::Reexport { library, name } => {
                    let lib = String::from_utf8_lossy(library);
                    (0, format!("→ {lib}!{}", name_or_ordinal(name)))
                }
                _ => (0, String::new()),
            };
            let offset = if address == 0 { None } else { locate_addr(address) };
            bin.exports.push(Symbol { name, library, address, size: 0, offset });
        }
        bin.exports.sort_by(|a, b| a.address.cmp(&b.address).then_with(|| a.name.cmp(&b.name)));
    }

    // Functions: the full symbol table when there is one, else what the dynamic
    // table still says about a stripped file.
    for dynamic in [false, true] {
        let symbols = if dynamic { file.dynamic_symbols() } else { file.symbols() };
        for (i, sym) in symbols.enumerate() {
            if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            if bin.functions.len() >= MAX_ROWS {
                bin.capped[Tab::Functions.index()] = true;
                break;
            }
            if sym.kind() != SymbolKind::Text || !sym.is_definition() {
                continue;
            }
            let Ok(name) = sym.name_bytes() else { continue };
            if name.is_empty() {
                continue;
            }
            let address = sym.address();
            let section = sym.section_index().and_then(|idx| file.section_by_index(idx).ok());
            // Linker-made markers such as Mach-O's `__mh_execute_header` are
            // "text" symbols too, but sit outside the code they are filed under.
            if !relocatable
                && section
                    .as_ref()
                    .is_some_and(|s| address < s.address() || address - s.address() >= s.size())
            {
                continue;
            }
            let offset = match section {
                // An object file's symbols count from the start of their section.
                Some(s) if relocatable => s.file_range().map(|(o, _)| base + o + address),
                _ => locate_addr(address),
            };
            bin.functions.push(Symbol {
                name: String::from_utf8_lossy(name).into_owned(),
                library: String::new(),
                address,
                size: sym.size(),
                offset,
            });
        }
        if !bin.functions.is_empty() {
            break;
        }
    }
    // Every function the unwind tables know of, which in a stripped file is
    // nearly all of them. One with no symbol takes an export's name when there
    // is one at its address, and otherwise stays nameless.
    let starts = match &file {
        object::File::Elf32(f) => elf::function_starts(f),
        object::File::Elf64(f) => elf::function_starts(f),
        object::File::Pe32(f) => pe::function_starts(f),
        object::File::Pe64(f) => pe::function_starts(f),
        object::File::MachO32(f) => macho::function_starts(f),
        object::File::MachO64(f) => macho::function_starts(f),
        _ => Vec::new(),
    };
    if !starts.is_empty() {
        let mut known: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
        for (i, f) in bin.functions.iter().enumerate() {
            known.entry(f.address).or_insert(i);
        }
        let exported: std::collections::HashMap<u64, &str> = bin
            .exports
            .iter()
            .filter(|e| e.address != 0)
            .map(|e| (e.address, e.name.as_str()))
            .collect();
        let mut found = Vec::new();
        for (address, size) in starts {
            match known.get(&address) {
                Some(&i) if bin.functions[i].size == 0 => bin.functions[i].size = size,
                Some(_) => {}
                None => {
                    known.insert(address, usize::MAX);
                    found.push(Symbol {
                        name: exported.get(&address).map_or_else(String::new, |n| n.to_string()),
                        library: String::new(),
                        address,
                        size,
                        offset: locate_addr(address),
                    });
                }
            }
        }
        let room = MAX_ROWS.saturating_sub(bin.functions.len());
        if found.len() > room {
            bin.capped[Tab::Functions.index()] = true;
            found.truncate(room);
        }
        bin.functions.extend(found);
    }
    // An export that points into code is a function too — on 32-bit Windows,
    // where there is no unwind table to read, the only ones a DLL names.
    let in_code = |addr: u64| {
        bin.sections
            .iter()
            .any(|s| s.kind == "code" && addr >= s.address && addr - s.address < s.size)
    };
    let exported_code: Vec<Symbol> = bin
        .exports
        .iter()
        .filter(|e| e.address != 0 && in_code(e.address))
        .filter(|e| !bin.functions.iter().any(|f| f.address == e.address))
        .cloned()
        .collect();
    let room = MAX_ROWS.saturating_sub(bin.functions.len());
    bin.functions.extend(exported_code.into_iter().take(room));
    bin.functions.sort_by(|a, b| a.address.cmp(&b.address).then_with(|| a.name.cmp(&b.name)));
    bin.functions.dedup_by(|a, b| a.address == b.address && a.name == b.name);
    // Mach-O records no function lengths anywhere: measure each to the next
    // function, or to the end of the section it is in.
    if format == BinaryFormat::MachO {
        for i in 0..bin.functions.len() {
            if bin.functions[i].size != 0 {
                continue;
            }
            let at = bin.functions[i].address;
            let next = bin.functions[i + 1..].iter().map(|f| f.address).find(|&a| a > at);
            let end = bin
                .sections
                .iter()
                .find(|s| s.kind == "code" && at >= s.address && at - s.address < s.size)
                .map(|s| s.address + s.size);
            if let Some(stop) = [next, end].into_iter().flatten().min() {
                bin.functions[i].size = stop - at;
            }
        }
    }

    // The Info tab: what any binary has, then what only its format says.
    let facts = &mut bin.facts;
    let format_name = match (format, is_64) {
        (BinaryFormat::Elf, true) => "ELF 64-bit",
        (BinaryFormat::Elf, false) => "ELF 32-bit",
        (BinaryFormat::Pe, true) => "PE32+",
        (BinaryFormat::Pe, false) => "PE32",
        (_, true) => "Mach-O 64-bit",
        (_, false) => "Mach-O 32-bit",
    };
    bin.summary = format!("{format_name} {arch}");
    match &image.fat {
        Some((arches, chosen)) => {
            let bits = if is_64 { "64-bit" } else { "32-bit" };
            push(facts, "Format", format!("Mach-O universal, {bits}"));
            let list: Vec<String> = arches
                .iter()
                .enumerate()
                .map(|(i, a)| if i == *chosen { format!("[{a}]") } else { a.clone() })
                .collect();
            push(facts, "Architectures", list.join(", "));
        }
        None => push(facts, "Format", format_name.to_string()),
    }
    let kind = match file.kind() {
        ObjectKind::Executable => Some("Executable"),
        ObjectKind::Dynamic => Some("Shared library"),
        ObjectKind::Relocatable => Some("Object file"),
        ObjectKind::Core => Some("Core dump"),
        _ => None,
    };
    if let Some(kind) = kind {
        push_word(facts, "Type", kind);
    }
    push(facts, "Architecture", arch);
    push_word(
        facts,
        "Byte order",
        if file.is_little_endian() { "little-endian" } else { "big-endian" },
    );
    if file.entry() != 0 {
        push(facts, "Entry point", hex(file.entry(), is_64));
    }
    push(facts, "Size", crate::util::bytes::human_size(image.size));

    let imported = |needle: &str| bin_imports_contain(&bin.imports, needle);
    let canary = imported("__stack_chk_fail") || imported("__stack_chk_guard");
    let mut specific = Vec::new();
    match &file {
        object::File::Elf32(f) => elf::facts(f, canary, &bin.imports, &mut specific),
        object::File::Elf64(f) => elf::facts(f, canary, &bin.imports, &mut specific),
        object::File::Pe32(f) => bin.managed = pe::facts(f, &mut specific),
        object::File::Pe64(f) => bin.managed = pe::facts(f, &mut specific),
        object::File::MachO32(f) => macho::facts(f, canary, &mut specific),
        object::File::MachO64(f) => macho::facts(f, canary, &mut specific),
        _ => {}
    }
    // A format-specific type ("Position-independent executable", "Bundle")
    // replaces the generic one rather than adding a second line.
    for fact in specific {
        match bin.facts.iter_mut().find(|f| f.label == fact.label && f.label == "Type") {
            Some(existing) => *existing = fact,
            None => bin.facts.push(fact),
        }
    }

    let facts = &mut bin.facts;
    // A PE image always has a symbol table to ask for, usually an empty one.
    let has_symbols = file.symbols().next().is_some();
    push_word(facts, "Symbol table", if has_symbols { "Yes" } else { "No" });
    push_word(facts, "Debug info", if file.has_debug_symbols() { "Yes" } else { "No" });
    if let Ok(Some(id)) = file.build_id() {
        push(facts, "Build ID", id.iter().map(|b| format!("{b:02x}")).collect());
    }
    if let Ok(Some(uuid)) = file.mach_uuid() {
        push(facts, "Build ID", format_uuid(&uuid));
    }
    Some(bin)
}

/// The file ranges of the code sections, sorted and merged, for the strings
/// scan to treat more strictly.
fn code_ranges(bin: &Binary) -> Vec<(u64, u64)> {
    if bin.managed {
        return Vec::new();
    }
    let mut code: Vec<(u64, u64)> = bin
        .sections
        .iter()
        .filter(|s| s.kind == "code" && s.size > 0)
        .filter_map(|s| s.offset.map(|o| (o, o + s.size)))
        .collect();
    code.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(code.len());
    for (start, end) in code {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn bin_imports_contain(imports: &[Symbol], needle: &str) -> bool {
    // Mach-O puts an underscore in front of every C name.
    imports.iter().any(|s| s.name == needle || s.name.strip_prefix('_') == Some(needle))
}

fn push(facts: &mut Vec<Fact>, label: &'static str, value: String) {
    facts.push(Fact { label, value, translate: false });
}

fn push_word(facts: &mut Vec<Fact>, label: &'static str, word: &'static str) {
    facts.push(Fact { label, value: word.to_string(), translate: true });
}

/// An address in the width the file uses.
pub fn hex(addr: u64, is_64: bool) -> String {
    if is_64 { format!("{addr:016x}") } else { format!("{addr:08x}") }
}

fn format_uuid(u: &[u8; 16]) -> String {
    let h: Vec<String> = u.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

fn name_or_ordinal(n: NameOrOrdinal<&[u8]>) -> String {
    match n {
        NameOrOrdinal::Name(name) => String::from_utf8_lossy(name).into_owned(),
        NameOrOrdinal::Ordinal(o) => format!("#{o}"),
    }
}

fn library_note(flags: ImportLibraryFlags) -> String {
    match flags {
        ImportLibraryFlags::Pe { delay: true } => "delay-load".to_string(),
        ImportLibraryFlags::MachO { cmd, current_version, .. } => {
            let how = match cmd {
                object::macho::LC_LOAD_WEAK_DYLIB => "weak ",
                object::macho::LC_REEXPORT_DYLIB => "re-export ",
                object::macho::LC_LAZY_LOAD_DYLIB => "lazy ",
                object::macho::LC_LOAD_UPWARD_DYLIB => "upward ",
                _ => "",
            };
            format!("{how}{}", macho::version(current_version.0))
        }
        _ => String::new(),
    }
}

fn section_kind(kind: SectionKind) -> &'static str {
    match kind {
        SectionKind::Text => "code",
        SectionKind::Data => "data",
        SectionKind::ReadOnlyData
        | SectionKind::ReadOnlyDataWithRel
        | SectionKind::ReadOnlyString => "rodata",
        SectionKind::UninitializedData => "bss",
        SectionKind::Tls | SectionKind::UninitializedTls | SectionKind::TlsVariables => "tls",
        SectionKind::Debug | SectionKind::DebugString => "debug",
        SectionKind::Linker => "linker",
        SectionKind::Note => "note",
        SectionKind::Metadata => "meta",
        _ => "other",
    }
}

/// A readable name for an architecture — the names toolchains print, rather than
/// the crate's enum spelling.
fn arch_name(a: object::Architecture, sub: Option<object::SubArchitecture>) -> String {
    use object::Architecture as A;
    let name = match a {
        A::X86_64 => "x86-64",
        A::X86_64_X32 => "x32",
        A::I386 => "x86",
        A::Aarch64 => match sub {
            Some(object::SubArchitecture::Arm64E) => "arm64e",
            Some(object::SubArchitecture::Arm64EC) => "arm64ec",
            _ => "arm64",
        },
        A::Aarch64_Ilp32 => "arm64_32",
        A::Arm => "arm",
        A::Riscv32 => "riscv32",
        A::Riscv64 => "riscv64",
        A::Mips => "mips",
        A::Mips64 => "mips64",
        A::PowerPc => "powerpc",
        A::PowerPc64 => "powerpc64",
        A::S390x => "s390x",
        A::Sparc | A::Sparc32Plus => "sparc",
        A::Sparc64 => "sparc64",
        A::LoongArch64 => "loongarch64",
        A::Wasm32 => "wasm32",
        A::Unknown => return "unknown".to_string(),
        other => return format!("{other:?}").to_ascii_lowercase(),
    };
    name.to_string()
}

/// `name` as written in the source, when it is a mangled Rust or C++ name.
///
/// Rust is tried first: a legacy Rust symbol is also valid Itanium C++, and the
/// C++ reading of it keeps the hash that the Rust demangler drops.
pub fn demangle(name: &str) -> Option<String> {
    // Past this a name is not a symbol anyone reads, and the C++ demangler's
    // work grows with it.
    if name.len() > 4096 {
        return None;
    }
    if let Ok(d) = rustc_demangle::try_demangle(name) {
        return Some(format!("{d:#}"));
    }
    // Mach-O puts an underscore in front of every name, C++ ones included.
    let itanium = name
        .strip_prefix('_')
        .filter(|rest| rest.starts_with("_Z"))
        .or_else(|| name.starts_with("_Z").then_some(name))?;
    let text = cpp_demangle::Symbol::new(itanium).ok()?.demangle().ok()?;
    // Two special names come out in the demangler's own notation; show them the
    // way `c++filt` and every debugger does.
    for (from, to) in [("{vtable(", "vtable for "), ("{vtt(", "VTT for ")] {
        if let Some(inner) = text.strip_prefix(from).and_then(|t| t.strip_suffix(")}")) {
            return Some(format!("{to}{inner}"));
        }
    }
    Some(text)
}
