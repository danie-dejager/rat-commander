//! What only an ELF file says about itself: its interpreter, how it is linked,
//! the name it goes by and where it looks for libraries, and how it is hardened.

use super::{Fact, Symbol, push, push_word};
use object::elf;
use object::read::ReadRef;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};

pub(super) fn facts<'data, Elf, R>(
    f: &ElfFile<'data, Elf, R>,
    canary: bool,
    imports: &[Symbol],
    out: &mut Vec<Fact>,
) where
    Elf: FileHeader<Endian = object::Endianness>,
    R: ReadRef<'data>,
{
    let endian = f.endian();
    let data = f.data();
    let header = f.elf_header();
    let segments = f.elf_program_headers();

    let interp = segments
        .iter()
        .find_map(|p| p.interpreter(endian, data).ok().flatten())
        .map(|s| String::from_utf8_lossy(s).into_owned());
    let has = |t: elf::ProgramType| segments.iter().any(|p| p.p_type(endian) == t);
    let dynamic = has(elf::PT_DYNAMIC) || interp.is_some();

    let mut soname = None;
    let mut paths = Vec::new();
    let mut bind_now = false;
    let mut pie_flag = false;
    if let Ok(table) = f.elf_dynamic_table() {
        for d in table.iter() {
            let text = || table.string(d).ok().map(|s| String::from_utf8_lossy(s).into_owned());
            match d.tag {
                elf::DT_SONAME => soname = text(),
                elf::DT_RPATH | elf::DT_RUNPATH => paths.extend(text()),
                elf::DT_BIND_NOW => bind_now = true,
                elf::DT_FLAGS => bind_now |= d.val & elf::DF_BIND_NOW.0 != 0,
                elf::DT_FLAGS_1 => {
                    bind_now |= d.val & elf::DF_1_NOW.0 != 0;
                    pie_flag = d.val & elf::DF_1_PIE.0 != 0;
                }
                _ => {}
            }
        }
    }

    // A shared object with an interpreter is a PIE; so is anything the linker
    // flagged as one, which catches a static PIE that has no interpreter.
    let e_type = header.e_type(endian);
    let pie = e_type == elf::ET_DYN && (pie_flag || interp.is_some());
    if pie {
        push_word(out, "Type", "Position-independent executable");
    }

    let os = match header.e_ident().os_abi {
        elf::ELFOSABI_SYSV => None,
        elf::ELFOSABI_LINUX => Some("GNU/Linux"),
        elf::ELFOSABI_FREEBSD => Some("FreeBSD"),
        elf::ELFOSABI_NETBSD => Some("NetBSD"),
        elf::ELFOSABI_OPENBSD => Some("OpenBSD"),
        elf::ELFOSABI_SOLARIS => Some("Solaris"),
        elf::ELFOSABI_STANDALONE => Some("Standalone"),
        _ => None,
    };
    if let Some(os) = os {
        push(out, "Platform", os.to_string());
    }
    if let Some(interp) = interp {
        push(out, "Interpreter", interp);
    }
    if matches!(e_type, elf::ET_EXEC | elf::ET_DYN) {
        push_word(out, "Linking", if dynamic { "dynamic" } else { "static" });
    }
    if let Some(soname) = soname {
        push(out, "Library name", soname);
    }
    if !paths.is_empty() {
        push(out, "Search paths", paths.join(":"));
    }

    // The checks `checksec` makes, for a file that will be run or loaded.
    if matches!(e_type, elf::ET_EXEC | elf::ET_DYN) {
        let mut h = Vec::new();
        if pie {
            h.push("PIE");
        }
        // Without a PT_GNU_STACK note the loader makes the stack executable.
        let nx = segments.iter().any(|p| {
            p.p_type(endian) == elf::PT_GNU_STACK && p.p_flags(endian) & elf::PF_X != elf::PF_X
        });
        if nx {
            h.push("NX");
        }
        if has(elf::PT_GNU_RELRO) {
            h.push(if bind_now { "full RELRO" } else { "partial RELRO" });
        }
        if canary {
            h.push("stack canary");
        }
        if imports.iter().any(|s| s.name.starts_with("__") && s.name.ends_with("_chk")) {
            h.push("FORTIFY");
        }
        if h.is_empty() {
            push_word(out, "Hardening", "none");
        } else {
            push(out, "Hardening", h.join(", "));
        }
    }
}

/// Where the functions of a file start, and how long each is, from its unwind
/// tables — which a stripped binary keeps, because exceptions and backtraces
/// need them, long after its symbol table is gone.
///
/// `.eh_frame_hdr` holds a table of every function's start address sorted for
/// the unwinder's binary search; each entry points at the frame description in
/// `.eh_frame` that gives the function's length. Only the table layout that
/// every current linker writes (32-bit entries relative to the header) is read.
pub(super) fn function_starts<'data, Elf, R>(f: &ElfFile<'data, Elf, R>) -> Vec<(u64, u64)>
where
    Elf: FileHeader<Endian = object::Endianness>,
    R: ReadRef<'data>,
{
    use object::read::{Object, ObjectSection};
    let big = f.endian() == object::Endianness::Big;
    let wide = f.is_64();
    let Some(hdr_section) = f.section_by_name(".eh_frame_hdr") else { return Vec::new() };
    let Ok(hdr) = hdr_section.data() else { return Vec::new() };
    let hdr_addr = hdr_section.address();
    let frames = f.section_by_name(".eh_frame");
    let frame_addr = frames.as_ref().map_or(0, |s| s.address());
    let frame_data = frames.and_then(|s| s.data().ok()).unwrap_or_default();

    const DATAREL_SDATA4: u8 = 0x3b;
    let [1, ptr_enc, count_enc, DATAREL_SDATA4, ..] = *hdr else { return Vec::new() };
    let mut pos = 4;
    let ctx = Pointers { big, wide, section_addr: hdr_addr };
    if ctx.read(hdr, &mut pos, ptr_enc).is_none() {
        return Vec::new();
    }
    let Some(count) = ctx.read(hdr, &mut pos, count_enc) else { return Vec::new() };
    let count = (count as usize).min((hdr.len() - pos) / 8).min(super::MAX_ROWS);

    let mut cie_encodings = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = pos + i * 8;
        let (Some(start), Some(fde)) = (i32_at(hdr, at, big), i32_at(hdr, at + 4, big)) else {
            break;
        };
        let start = hdr_addr.wrapping_add(start as i64 as u64);
        let fde = hdr_addr.wrapping_add(fde as i64 as u64);
        let len = fde
            .checked_sub(frame_addr)
            .and_then(|off| fde_length(frame_data, off as usize, &ctx, &mut cie_encodings))
            .unwrap_or(0);
        out.push((start, len));
    }
    out
}

/// How to read DWARF exception-handling pointers (`DW_EH_PE_*`) in one file.
struct Pointers {
    big: bool,
    wide: bool,
    /// Address the data being read is loaded at, for PC-relative values.
    section_addr: u64,
}

impl Pointers {
    /// A pointer in encoding `enc` at `data[*pos]`, advancing past it. `None`
    /// for an encoding this does not read (LEB128 and indirect forms, which no
    /// linker uses in these tables).
    fn read(&self, data: &[u8], pos: &mut usize, enc: u8) -> Option<u64> {
        let here = self.section_addr.wrapping_add(*pos as u64);
        let (value, size) = match enc & 0x0f {
            0x00 if self.wide => (u64_at(data, *pos, self.big)?, 8),
            0x00 => (u64::from(u32_at(data, *pos, self.big)?), 4),
            0x02 => (u64::from(u16_at(data, *pos, self.big)?), 2),
            0x03 => (u64::from(u32_at(data, *pos, self.big)?), 4),
            0x04 | 0x0c => (u64_at(data, *pos, self.big)?, 8),
            0x0a => (u16_at(data, *pos, self.big)? as i16 as i64 as u64, 2),
            0x0b => (u32_at(data, *pos, self.big)? as i32 as i64 as u64, 4),
            _ => return None,
        };
        *pos += size;
        match enc & 0x70 {
            0x00 => Some(value),
            0x10 => Some(here.wrapping_add(value)),
            _ => None,
        }
    }
}

/// The length of the function a frame description entry at `off` covers, read
/// with the pointer encoding its CIE declares (remembered per CIE).
fn fde_length(
    data: &[u8],
    off: usize,
    ctx: &Pointers,
    cies: &mut std::collections::HashMap<usize, u8>,
) -> Option<u64> {
    let mut pos = off;
    let len = u32_at(data, pos, ctx.big)?;
    pos += 4;
    if len == 0xffff_ffff {
        pos += 8;
    }
    let id_pos = pos;
    let cie_ptr = u32_at(data, pos, ctx.big)? as usize;
    pos += 4;
    // A zero here marks a CIE, not a description of a function.
    if cie_ptr == 0 {
        return None;
    }
    let cie = id_pos.checked_sub(cie_ptr)?;
    let enc = match cies.get(&cie) {
        Some(&e) => e,
        None => {
            let e = cie_pointer_encoding(data, cie, ctx)?;
            cies.insert(cie, e);
            e
        }
    };
    // The start address, then the length: the same size of number, but the
    // length is a plain one rather than relative to anything.
    ctx.read(data, &mut pos, enc & 0x0f)?;
    ctx.read(data, &mut pos, enc & 0x0f)
}

/// The `R` augmentation of the CIE at `off`: how its FDEs encode addresses.
fn cie_pointer_encoding(data: &[u8], off: usize, ctx: &Pointers) -> Option<u8> {
    let mut b = object::read::Bytes(data.get(off..)?);
    let extended = u32_at(data, off, ctx.big)? == 0xffff_ffff;
    b.skip(if extended { 12 } else { 4 }).ok()?;
    b.skip(4).ok()?; // CIE id
    let version = *b.read::<u8>().ok()?;
    let aug = b.read_string().ok()?;
    b.read_uleb128().ok()?; // code alignment
    b.read_sleb128().ok()?; // data alignment
    if version == 1 {
        b.skip(1).ok()?;
    } else {
        b.read_uleb128().ok()?;
    }
    let rest = aug.strip_prefix(b"z")?;
    b.read_uleb128().ok()?; // augmentation data length
    for &c in rest {
        match c {
            b'R' => return b.read::<u8>().ok().copied(),
            b'L' => b.skip(1).ok()?,
            b'P' => {
                let enc = *b.read::<u8>().ok()?;
                let size = match enc & 0x0f {
                    0x00 if ctx.wide => 8,
                    0x00 | 0x03 | 0x0b => 4,
                    0x02 | 0x0a => 2,
                    0x04 | 0x0c => 8,
                    _ => return None,
                };
                b.skip(size).ok()?;
            }
            b'S' | b'B' | b'G' => {}
            _ => return None,
        }
    }
    // No `R`: addresses are plain pointers.
    Some(0x00)
}

fn u16_at(d: &[u8], at: usize, big: bool) -> Option<u16> {
    let b: [u8; 2] = d.get(at..at + 2)?.try_into().ok()?;
    Some(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
}

fn u32_at(d: &[u8], at: usize, big: bool) -> Option<u32> {
    let b: [u8; 4] = d.get(at..at + 4)?.try_into().ok()?;
    Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
}

fn i32_at(d: &[u8], at: usize, big: bool) -> Option<i32> {
    u32_at(d, at, big).map(|v| v as i32)
}

fn u64_at(d: &[u8], at: usize, big: bool) -> Option<u64> {
    let b: [u8; 8] = d.get(at..at + 8)?.try_into().ok()?;
    Some(if big { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) })
}
