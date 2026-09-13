//! What only a PE image says about itself: its subsystem, whether it is a .NET
//! assembly, when it was linked, how it is hardened, and where its PDB is.

use super::{Fact, push, push_word};
use object::LittleEndian as LE;
use object::pe;
use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile};
use object::read::{Object, ReadRef};

/// Returns whether the image is a .NET assembly.
pub(super) fn facts<'data, Pe, R>(f: &PeFile<'data, Pe, R>, out: &mut Vec<Fact>) -> bool
where
    Pe: ImageNtHeaders,
    R: ReadRef<'data>,
{
    let nt = f.nt_headers();
    let file_header = nt.file_header();
    let optional = nt.optional_header();

    if file_header.characteristics.get(LE) & pe::IMAGE_FILE_DLL == pe::IMAGE_FILE_DLL {
        push_word(out, "Type", "Shared library");
    }
    let subsystem = match optional.subsystem() {
        pe::IMAGE_SUBSYSTEM_NATIVE => Some("native (driver)"),
        pe::IMAGE_SUBSYSTEM_WINDOWS_GUI => Some("Windows GUI"),
        pe::IMAGE_SUBSYSTEM_WINDOWS_CUI => Some("Windows console"),
        pe::IMAGE_SUBSYSTEM_OS2_CUI => Some("OS/2 console"),
        pe::IMAGE_SUBSYSTEM_POSIX_CUI => Some("POSIX console"),
        pe::IMAGE_SUBSYSTEM_WINDOWS_CE_GUI => Some("Windows CE GUI"),
        pe::IMAGE_SUBSYSTEM_EFI_APPLICATION => Some("EFI application"),
        pe::IMAGE_SUBSYSTEM_EFI_BOOT_SERVICE_DRIVER => Some("EFI boot service driver"),
        pe::IMAGE_SUBSYSTEM_EFI_RUNTIME_DRIVER => Some("EFI runtime driver"),
        pe::IMAGE_SUBSYSTEM_EFI_ROM => Some("EFI ROM"),
        pe::IMAGE_SUBSYSTEM_XBOX => Some("Xbox"),
        pe::IMAGE_SUBSYSTEM_WINDOWS_BOOT_APPLICATION => Some("Windows boot application"),
        _ => None,
    };
    if let Some(s) = subsystem {
        let (major, minor) =
            (optional.major_subsystem_version(), optional.minor_subsystem_version());
        push(out, "Subsystem", format!("{s} {major}.{minor}"));
    }
    // A managed assembly is a PE whose real content is IL for the CLR, which is
    // why its imports are just mscoree.dll and its functions list is empty.
    let managed = f
        .data_directory(pe::IMAGE_DIRECTORY_ENTRY_COM_DESCRIPTOR)
        .is_some_and(|d| d.virtual_address.get(LE) != 0 && d.size.get(LE) != 0);
    if managed {
        push(out, "Runtime", ".NET (CLR)".to_string());
    }
    // Reproducible builds put a hash here instead of a time; one that lands
    // before PE existed or far in the future is not shown as a date.
    let stamp = file_header.time_date_stamp.get(LE) as i64;
    if (694_224_000..4_102_444_800).contains(&stamp) {
        let (y, mo, d, h, mi, s) = crate::util::bytes::civil_parts(stamp);
        push(out, "Link time", format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC"));
    }

    let flags = optional.dll_characteristics();
    let set = |bit: pe::DllFlags| flags & bit == bit;
    let mut h = Vec::new();
    if set(pe::IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE) {
        // High-entropy ASLR needs a 64-bit address space to mean anything.
        h.push(if f.is_64() && set(pe::IMAGE_DLLCHARACTERISTICS_HIGH_ENTROPY_VA) {
            "ASLR (high entropy)"
        } else {
            "ASLR"
        });
    }
    if set(pe::IMAGE_DLLCHARACTERISTICS_NX_COMPAT) {
        h.push("DEP");
    }
    if set(pe::IMAGE_DLLCHARACTERISTICS_GUARD_CF) {
        h.push("CFG");
    }
    if h.is_empty() {
        push_word(out, "Hardening", "none");
    } else {
        push(out, "Hardening", h.join(", "));
    }

    if let Ok(Some(cv)) = f.pdb_info() {
        push(out, "PDB", String::from_utf8_lossy(cv.path()).into_owned());
        // The GUID in the form symbol servers index it by, with the age after.
        let g = cv.guid();
        let guid = format!(
            "{:08X}{:04X}{:04X}{}",
            u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
            u16::from_le_bytes([g[4], g[5]]),
            u16::from_le_bytes([g[6], g[7]]),
            g[8..].iter().map(|b| format!("{b:02X}")).collect::<String>()
        );
        push(out, "Build ID", format!("{guid}{:X}", cv.age()));
    }
    managed
}

/// Where the functions of an x64 or ARM64 image start, and how long each is,
/// from its exception directory (`.pdata`): Windows needs an entry for every
/// function that can unwind, so even an image without a symbol in it lists
/// nearly all of its code there. 32-bit x86 images have no such table.
pub(super) fn function_starts<'data, Pe, R>(f: &PeFile<'data, Pe, R>) -> Vec<(u64, u64)>
where
    Pe: ImageNtHeaders,
    R: ReadRef<'data>,
{
    let machine = f.nt_headers().file_header().machine.get(LE);
    let arm64 = matches!(
        machine,
        pe::IMAGE_FILE_MACHINE_ARM64
            | pe::IMAGE_FILE_MACHINE_ARM64EC
            | pe::IMAGE_FILE_MACHINE_ARM64X
    );
    if machine != pe::IMAGE_FILE_MACHINE_AMD64 && !arm64 {
        return Vec::new();
    }
    let sections = f.section_table();
    let Some(table) = f
        .data_directory(pe::IMAGE_DIRECTORY_ENTRY_EXCEPTION)
        .and_then(|d| d.data(f.data(), &sections).ok())
    else {
        return Vec::new();
    };
    let base = f.relative_address_base();
    let u32_at =
        |d: &[u8], at: usize| d.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let entry = if arm64 { 8 } else { 12 };
    table
        .chunks_exact(entry)
        .take(super::MAX_ROWS)
        .filter_map(|e| {
            let begin = u32_at(e, 0)?;
            let len = if arm64 {
                // The low two bits say whether the length is packed into the
                // entry itself or kept in a separate unwind record it points at.
                let unwind = u32_at(e, 4)?;
                if unwind & 3 != 0 {
                    ((unwind >> 2) & 0x7ff) * 4
                } else {
                    sections
                        .pe_data_at(f.data(), unwind)
                        .and_then(|x| u32_at(x, 0))
                        .map_or(0, |h| (h & 0x3_ffff) * 4)
                }
            } else {
                u32_at(e, 4)?.saturating_sub(begin)
            };
            (begin != 0).then(|| (base + u64::from(begin), u64::from(len)))
        })
        .collect()
}
