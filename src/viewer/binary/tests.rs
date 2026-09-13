use super::*;
use object::write;

/// A small x86-64 ELF object: a function, a local helper, an import and a
/// string worth finding.
pub(crate) fn sample_elf() -> Vec<u8> {
    let mut obj = write::Object::new(
        BinaryFormat::Elf,
        object::Architecture::X86_64,
        object::Endianness::Little,
    );
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0xc3; 48], 16);
    let rodata = obj.add_section(Vec::new(), b".rodata".to_vec(), SectionKind::ReadOnlyString);
    obj.append_section_data(rodata, b"\0a string worth finding\0", 1);
    for (name, value, size, scope) in [
        ("exported_entry", 0, 16, object::SymbolScope::Dynamic),
        ("_ZN4demo6Widget5valueEi", 16, 16, object::SymbolScope::Dynamic),
        ("local_helper", 32, 16, object::SymbolScope::Compilation),
    ] {
        obj.add_symbol(write::Symbol {
            name: name.as_bytes().to_vec(),
            value,
            size,
            kind: SymbolKind::Text,
            scope,
            weak: false,
            section: write::SymbolSection::Section(text),
            flags: object::SymbolFlags::None,
        });
    }
    obj.add_symbol(write::Symbol {
        name: b"puts".to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Text,
        scope: object::SymbolScope::Dynamic,
        weak: false,
        section: write::SymbolSection::Undefined,
        flags: object::SymbolFlags::None,
    });
    obj.write().unwrap()
}

/// A Mach-O object for `arch`, with a C function and a C++ one.
fn sample_macho(arch: object::Architecture) -> Vec<u8> {
    let mut obj = write::Object::new(BinaryFormat::MachO, arch, object::Endianness::Little);
    obj.mangling = write::Mangling::None;
    let text = obj.add_section(b"__TEXT".to_vec(), b"__text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0u8; 32], 4);
    let cstring =
        obj.add_section(b"__TEXT".to_vec(), b"__cstring".to_vec(), SectionKind::ReadOnlyString);
    obj.append_section_data(cstring, b"Hello from Mach-O\0", 1);
    for (name, value) in [("_c_function", 0), ("__ZNK4demo6Widget5valueEi", 16)] {
        obj.add_symbol(write::Symbol {
            name: name.as_bytes().to_vec(),
            value,
            size: 16,
            kind: SymbolKind::Text,
            scope: object::SymbolScope::Dynamic,
            weak: false,
            section: write::SymbolSection::Section(text),
            flags: object::SymbolFlags::None,
        });
    }
    obj.write().unwrap()
}

/// A universal binary holding the given slices, each tagged with its CPU type.
fn fat(slices: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(0xcafe_babe_u32.to_be_bytes());
    out.extend((slices.len() as u32).to_be_bytes());
    let mut offset = 0x1000u32;
    for (cpu, data) in slices {
        for field in [*cpu, 0, offset, data.len() as u32, 12] {
            out.extend(field.to_be_bytes());
        }
        offset += (data.len() as u32).next_multiple_of(0x1000);
    }
    for (_, data) in slices {
        out.resize(out.len().next_multiple_of(0x1000), 0);
        out.extend_from_slice(data);
    }
    out
}

/// A PE32+ executable for x86-64: a code section with one entry in its unwind
/// table, and an import of `ExitProcess` from KERNEL32.dll.
fn sample_pe() -> Vec<u8> {
    use object::pe;
    use object::write::pe::{NtHeaders, Writer};
    let mut out = Vec::new();
    let mut w = Writer::new(true, 0x1000, 0x200, &mut out);
    w.reserve_dos_header_and_stub();
    w.reserve_nt_headers(16);
    w.reserve_section_headers(3);
    let text = w.reserve_text_section(0x20);
    let idata = w.reserve_idata_section(100);
    let pdata = w.reserve_pdata_section(12);
    w.write_dos_header_and_stub().unwrap();
    w.write_nt_headers(NtHeaders {
        machine: pe::IMAGE_FILE_MACHINE_AMD64,
        time_date_stamp: 1_700_000_000,
        characteristics: pe::IMAGE_FILE_EXECUTABLE_IMAGE | pe::IMAGE_FILE_LARGE_ADDRESS_AWARE,
        major_linker_version: 14,
        minor_linker_version: 0,
        address_of_entry_point: text.virtual_address,
        image_base: 0x1_4000_0000,
        major_operating_system_version: 6,
        minor_operating_system_version: 0,
        major_image_version: 0,
        minor_image_version: 0,
        major_subsystem_version: 6,
        minor_subsystem_version: 0,
        subsystem: pe::IMAGE_SUBSYSTEM_WINDOWS_CUI,
        dll_characteristics: pe::IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE
            | pe::IMAGE_DLLCHARACTERISTICS_NX_COMPAT
            | pe::IMAGE_DLLCHARACTERISTICS_HIGH_ENTROPY_VA,
        size_of_stack_reserve: 0x10_0000,
        size_of_stack_commit: 0x1000,
        size_of_heap_reserve: 0x10_0000,
        size_of_heap_commit: 0x1000,
    });
    w.write_section_headers();
    w.write_section(text.file_offset, &[0xc3; 0x20]);

    // Two import descriptors (one and the terminator), the lookup table and the
    // address table, the hint/name entry, and the library's name.
    let base = idata.virtual_address;
    let mut id = Vec::new();
    for v in [base + 40, 0, 0, base + 86, base + 56] {
        id.extend(v.to_le_bytes());
    }
    id.resize(40, 0);
    for _ in 0..2 {
        id.extend(u64::from(base + 72).to_le_bytes());
        id.extend(0u64.to_le_bytes());
    }
    id.extend(0u16.to_le_bytes());
    id.extend(b"ExitProcess\0");
    id.extend(b"KERNEL32.dll\0");
    id.resize(100, 0);
    w.write_section(idata.file_offset, &id);

    let mut pd = Vec::new();
    for v in [text.virtual_address, text.virtual_address + 0x10, 0] {
        pd.extend(v.to_le_bytes());
    }
    w.write_section(pdata.file_offset, &pd);
    out
}

fn fact<'a>(b: &'a Binary, label: &str) -> Option<&'a str> {
    b.facts.iter().find(|f| f.label == label).map(|f| f.value.as_str())
}

#[test]
fn sniff_knows_the_three_formats_and_not_their_lookalikes() {
    assert!(sniff(b"\x7fELF\x02\x01\x01"));
    for magic in [
        [0xfe, 0xed, 0xfa, 0xce],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xcf, 0xfa, 0xed, 0xfe],
    ] {
        assert!(sniff(&magic), "Mach-O {magic:x?}");
    }
    assert!(sniff(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]), "universal binary with two slices");
    // A Java class file (version 52) starts with the same magic.
    assert!(!sniff(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52]));

    let mut pe = vec![0u8; 0x100];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3c] = 0x80;
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    assert!(sniff(&pe));
    pe[0x80] = b'X';
    assert!(!sniff(&pe), "a DOS program without a PE header");

    assert!(!sniff(b"#!/bin/sh\necho hi\n"));
    assert!(!sniff(b""));
    assert!(sniff(&sample_pe()));
}

/// A small executable every system of its kind has, in that system's own
/// format: the shell on Unix, the command interpreter on Windows.
fn system_executable() -> std::path::PathBuf {
    #[cfg(windows)]
    let path =
        std::path::Path::new(&std::env::var("SystemRoot").unwrap()).join(r"System32\cmd.exe");
    #[cfg(not(windows))]
    let path = std::path::PathBuf::from("/bin/sh");
    path
}

#[test]
fn a_system_executable_analyses_in_its_platforms_own_format() {
    let exe = std::fs::canonicalize(system_executable()).unwrap();
    assert!(sniff_file(&exe));
    let b = analyze_file(&exe, &AtomicBool::new(false)).expect("the system shell parses");
    let format = fact(&b, "Format").unwrap();
    #[cfg(target_os = "linux")]
    assert!(format.starts_with("ELF"), "{format}");
    #[cfg(target_os = "windows")]
    assert!(format.starts_with("PE"), "{format}");
    #[cfg(target_os = "macos")]
    assert!(format.starts_with("Mach-O"), "{format}");
    assert!(b.sections.iter().any(|s| s.kind == "code"), "a code section");
    // Shipped stripped, but its unwind information still finds the functions.
    assert!(b.functions.len() > 10, "functions: {}", b.functions.len());
    assert!(!b.libraries.is_empty() && !b.imports.is_empty(), "it links against the C runtime");
    assert!(!b.strings.is_empty());
    // Every function the file gives an offset for points into a code section.
    let code = code_ranges(&b);
    assert!(
        b.functions
            .iter()
            .filter_map(|f| f.offset)
            .all(|o| code.iter().any(|&(s, e)| o >= s && o < e)),
        "function offsets land in code"
    );
}

#[test]
fn an_elf_object_lists_its_sections_functions_and_strings() {
    let data = sample_elf();
    let b = analyze_bytes(&data).expect("parses");
    assert_eq!(b.summary, "ELF 64-bit x86-64");
    assert_eq!(fact(&b, "Type"), Some("Object file"));
    assert_eq!(fact(&b, "Architecture"), Some("x86-64"));
    assert_eq!(fact(&b, "Byte order"), Some("little-endian"));

    let text = b.sections.iter().find(|s| s.name == ".text").expect(".text");
    assert_eq!((text.kind, text.size), ("code", 48));
    assert!(b.sections.iter().any(|s| s.name == ".rodata" && s.kind == "rodata"));

    let names: Vec<&str> = b.functions.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["exported_entry", "_ZN4demo6Widget5valueEi", "local_helper"]);
    // An object file's symbols count from their section, and so do the offsets.
    let helper = &b.functions[2];
    assert_eq!(helper.offset, Some(text.offset.unwrap() + 32));
    assert_eq!(helper.size, 16);

    let found = b.strings.iter().find(|s| s.text == "a string worth finding").expect("the string");
    assert_eq!(b.section_at(found.offset), Some(".rodata"));
    assert_eq!(&data[found.offset as usize..found.offset as usize + 8], b"a string");
}

#[test]
fn a_pe_image_lists_its_imports_hardening_and_unwind_table_functions() {
    let b = analyze_bytes(&sample_pe()).expect("parses");
    assert_eq!(b.summary, "PE32+ x86-64");
    assert_eq!(fact(&b, "Type"), Some("Executable"));
    assert_eq!(fact(&b, "Subsystem"), Some("Windows console 6.0"));
    assert_eq!(fact(&b, "Link time"), Some("2023-11-14 22:13:20 UTC"));
    assert_eq!(fact(&b, "Hardening"), Some("ASLR (high entropy), DEP"));
    assert_eq!(fact(&b, "Entry point"), Some("0000000140001000"));
    assert_eq!(fact(&b, "Symbol table"), Some("No"));

    assert_eq!(b.libraries.len(), 1);
    assert_eq!(b.libraries[0].name, "KERNEL32.dll");
    assert_eq!(b.imports.len(), 1);
    assert_eq!(
        (b.imports[0].name.as_str(), b.imports[0].library.as_str()),
        ("ExitProcess", "KERNEL32.dll")
    );

    // No symbols at all, but the unwind table knows where the function is.
    assert_eq!(b.functions.len(), 1);
    let f = &b.functions[0];
    assert_eq!((f.address, f.size, f.name.as_str()), (0x1_4000_1000, 0x10, ""));
    let view = view::BinaryView::new(Box::new(b.clone()));
    assert_eq!(view.name(f), "sub_140001000", "a nameless function is named for its address");
    let text = b.sections.iter().find(|s| s.name == ".text").unwrap();
    assert_eq!(f.offset, text.offset);
}

#[test]
fn a_universal_binary_shows_one_slice_and_names_them_all() {
    let arm = sample_macho(object::Architecture::Aarch64);
    let x86 = sample_macho(object::Architecture::X86_64);
    let data =
        fat(&[(object::macho::CPU_TYPE_ARM64.0, &arm), (object::macho::CPU_TYPE_X86_64.0, &x86)]);
    assert!(sniff(&data));
    let b = analyze_bytes(&data).expect("parses");
    assert_eq!(fact(&b, "Format"), Some("Mach-O universal, 64-bit"));
    let arches = fact(&b, "Architectures").unwrap();
    // The slice for the machine running this is the one shown; elsewhere the first.
    let shown = if std::env::consts::ARCH == "x86_64" { "x86-64" } else { "arm64" };
    assert!(arches.contains(&format!("[{shown}]")), "{arches}");
    assert_eq!(fact(&b, "Architecture"), Some(shown));

    // Offsets are into the whole file, not into the slice.
    let hello = b.strings.iter().find(|s| s.text == "Hello from Mach-O").expect("the string");
    assert_eq!(&data[hello.offset as usize..hello.offset as usize + 5], b"Hello");
    let f = b.functions.iter().find(|f| f.name == "_c_function").expect("the function");
    let slice = if shown == "x86-64" { &x86 } else { &arm };
    let text = b.sections.iter().find(|s| s.name == "__TEXT,__text").unwrap();
    let in_slice = (text.offset.unwrap() - f.offset.unwrap()) as usize;
    assert_eq!(in_slice, 0);
    assert!(slice.len() < data.len());
    assert_eq!(demangle(&b.functions[1].name).as_deref(), Some("demo::Widget::value(int) const"));
}

#[test]
fn a_class_file_and_a_broken_elf_are_not_binaries() {
    let mut class = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52];
    class.resize(4096, 0);
    assert!(analyze_bytes(&class).is_none());
    // Its universal-binary look-alike with an impossible slice is refused too.
    assert!(
        analyze_bytes(&fat(&[(object::macho::CPU_TYPE_ARM64.0, b"not a Mach-O at all")])).is_none()
    );

    let mut elf = b"\x7fELF\x02\x01\x01".to_vec();
    elf.resize(64, 0xff);
    assert!(analyze_bytes(&elf).is_none());
}

#[test]
fn eh_frame_hdr_gives_function_starts_and_lengths() {
    // A CIE with the `zR` augmentation (PC-relative 32-bit pointers), one FDE
    // covering 0x40 bytes, and the lookup table pointing at it.
    let mut frame = Vec::new();
    frame.extend(16u32.to_le_bytes());
    frame.extend(0u32.to_le_bytes());
    frame.extend([1, b'z', b'R', 0, 1, 0x78, 16, 1, 0x1b, 0, 0, 0]);
    let fde = frame.len() as u32;
    frame.extend(16u32.to_le_bytes());
    frame.extend((fde + 4).to_le_bytes()); // back to the CIE
    frame.extend(0x1234_i32.to_le_bytes()); // start, PC-relative
    frame.extend(0x40u32.to_le_bytes()); // length
    frame.extend([0, 0, 0, 0]);
    frame.extend(0u32.to_le_bytes());

    let mut hdr = vec![1, 0x1b, 0x03, 0x3b];
    hdr.extend(0i32.to_le_bytes());
    hdr.extend(1u32.to_le_bytes());
    hdr.extend(0x10i32.to_le_bytes());
    hdr.extend(fde.to_le_bytes());

    let mut obj = write::Object::new(
        BinaryFormat::Elf,
        object::Architecture::X86_64,
        object::Endianness::Little,
    );
    let s = obj.add_section(Vec::new(), b".eh_frame_hdr".to_vec(), SectionKind::ReadOnlyData);
    obj.append_section_data(s, &hdr, 4);
    let s = obj.add_section(Vec::new(), b".eh_frame".to_vec(), SectionKind::ReadOnlyData);
    obj.append_section_data(s, &frame, 8);
    let data = obj.write().unwrap();
    let file = object::read::elf::ElfFile64::<object::Endianness>::parse(&data[..]).unwrap();
    assert_eq!(elf::function_starts(&file), vec![(0x10, 0x40)]);
}

#[test]
fn demangling_reads_rust_and_cpp_and_leaves_c_names_alone() {
    assert_eq!(
        demangle("_ZN4core3fmt5write17h0123456789abcdefE").as_deref(),
        Some("core::fmt::write")
    );
    assert_eq!(
        demangle("_ZNK4demo6Widget5valueEi").as_deref(),
        Some("demo::Widget::value(int) const")
    );
    // Mach-O's extra leading underscore.
    assert_eq!(
        demangle("__ZNK4demo6Widget5valueEi").as_deref(),
        Some("demo::Widget::value(int) const")
    );
    assert_eq!(demangle("_ZTVSt11regex_error").as_deref(), Some("vtable for std::regex_error"));
    assert_eq!(demangle("_ZTTSt9strstream").as_deref(), Some("VTT for std::strstream"));
    assert_eq!(demangle("_ZTISt12system_error").as_deref(), Some("typeinfo for std::system_error"));
    assert_eq!(demangle("printf"), None);
    assert_eq!(demangle("_main"), None);
    assert_eq!(demangle(&format!("_Z{}", "x".repeat(10_000))), None);
}

#[test]
fn a_raised_cancel_stops_the_analysis() {
    let exe = std::fs::canonicalize(system_executable()).unwrap();
    assert!(analyze_file(&exe, &AtomicBool::new(true)).is_none());
}
