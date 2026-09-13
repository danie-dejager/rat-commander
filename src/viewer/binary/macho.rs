//! What only a Mach-O file says about itself: the platform and OS version it is
//! built for, its dynamic linker, install name and search paths, and how it is
//! hardened.

use super::{Fact, push, push_word};
use object::macho;
use object::read::ReadRef;
use object::read::macho::{LoadCommandVariant, MachHeader, MachOFile};

pub(super) fn facts<'data, Mach, R>(
    f: &MachOFile<'data, Mach, R>,
    canary: bool,
    out: &mut Vec<Fact>,
) where
    Mach: MachHeader<Endian = object::Endianness>,
    R: ReadRef<'data>,
{
    let endian = f.endian();
    let header = f.macho_header();
    match header.filetype(endian) {
        macho::MH_BUNDLE => push_word(out, "Type", "Bundle"),
        macho::MH_KEXT_BUNDLE => push_word(out, "Type", "Kernel extension"),
        _ => {}
    }

    let mut platform = None;
    let mut dylinker = None;
    let mut install_name = None;
    let mut rpaths = Vec::new();
    let mut signed = false;
    let mut encrypted = false;
    if let Ok(mut commands) = f.macho_load_commands() {
        while let Ok(Some(cmd)) = commands.next() {
            let Ok(variant) = cmd.variant() else { continue };
            let text =
                |s| cmd.string(endian, s).ok().map(|b| String::from_utf8_lossy(b).into_owned());
            match variant {
                LoadCommandVariant::BuildVersion(b, _) => {
                    platform = Some((b.platform.get(endian).0, b.minos.get(endian).0));
                }
                // The older form: the command itself names the platform.
                LoadCommandVariant::VersionMin(v) if platform.is_none() => {
                    let p = match cmd.cmd() {
                        macho::LC_VERSION_MIN_MACOSX => macho::PLATFORM_MACOS,
                        macho::LC_VERSION_MIN_IPHONEOS => macho::PLATFORM_IOS,
                        macho::LC_VERSION_MIN_TVOS => macho::PLATFORM_TVOS,
                        _ => macho::PLATFORM_WATCHOS,
                    };
                    platform = Some((p.0, v.version.get(endian).0));
                }
                LoadCommandVariant::LoadDylinker(d) => dylinker = text(d.name),
                LoadCommandVariant::IdDylib(d) => install_name = text(d.dylib.name),
                LoadCommandVariant::Rpath(r) => rpaths.extend(text(r.path)),
                LoadCommandVariant::LinkeditData(_) if cmd.cmd() == macho::LC_CODE_SIGNATURE => {
                    signed = true;
                }
                LoadCommandVariant::EncryptionInfo32(e) => encrypted |= e.cryptid.get(endian) != 0,
                LoadCommandVariant::EncryptionInfo64(e) => encrypted |= e.cryptid.get(endian) != 0,
                _ => {}
            }
        }
    }

    if let Some((p, minos)) = platform {
        let name = match macho::Platform(p) {
            macho::PLATFORM_MACOS => "macOS",
            macho::PLATFORM_IOS => "iOS",
            macho::PLATFORM_TVOS => "tvOS",
            macho::PLATFORM_WATCHOS => "watchOS",
            macho::PLATFORM_BRIDGEOS => "bridgeOS",
            macho::PLATFORM_MACCATALYST => "Mac Catalyst",
            macho::PLATFORM_IOSSIMULATOR => "iOS Simulator",
            macho::PLATFORM_TVOSSIMULATOR => "tvOS Simulator",
            macho::PLATFORM_WATCHOSSIMULATOR => "watchOS Simulator",
            macho::PLATFORM_DRIVERKIT => "DriverKit",
            macho::PLATFORM_VISIONOS => "visionOS",
            _ => "",
        };
        if !name.is_empty() {
            push(out, "Platform", format!("{name} {}", version(minos)));
        }
    }
    if let Some(d) = dylinker {
        push(out, "Interpreter", d);
    }
    if let Some(n) = install_name {
        push(out, "Library name", n);
    }
    if !rpaths.is_empty() {
        push(out, "Search paths", rpaths.join(":"));
    }

    let mut h = Vec::new();
    if header.flags(endian) & macho::MH_PIE == macho::MH_PIE {
        h.push("PIE");
    }
    if canary {
        h.push("stack canary");
    }
    if signed {
        h.push("code signature");
    }
    if encrypted {
        h.push("encrypted");
    }
    if h.is_empty() {
        push_word(out, "Hardening", "none");
    } else {
        push(out, "Hardening", h.join(", "));
    }
}

/// A Mach-O version, packed as `xxxx.yy.zz` in nibbles, the way Apple writes it:
/// the patch level only when there is one.
pub(super) fn version(v: u32) -> String {
    let (major, minor, patch) = (v >> 16, (v >> 8) & 0xff, v & 0xff);
    if patch == 0 { format!("{major}.{minor}") } else { format!("{major}.{minor}.{patch}") }
}

/// Where the functions of a file start, from its `LC_FUNCTION_STARTS` table —
/// which the linker writes into every image, stripped or not. The table holds no
/// lengths; the caller measures each function to the start of the next.
pub(super) fn function_starts<'data, Mach, R>(f: &MachOFile<'data, Mach, R>) -> Vec<(u64, u64)>
where
    Mach: MachHeader<Endian = object::Endianness>,
    R: ReadRef<'data>,
{
    use object::read::{Object, ObjectSegment};
    let endian = f.endian();
    let Some(text) = f.segments().find(|s| s.name().ok().flatten() == Some("__TEXT")) else {
        return Vec::new();
    };
    let text_addr = text.address();
    let Ok(mut commands) = f.macho_load_commands() else { return Vec::new() };
    while let Ok(Some(cmd)) = commands.next() {
        if cmd.cmd() != macho::LC_FUNCTION_STARTS {
            continue;
        }
        let Ok(LoadCommandVariant::LinkeditData(linkedit)) = cmd.variant() else { continue };
        let Ok(starts) = linkedit.function_starts(endian, f.data(), text_addr) else { break };
        return starts.take(super::MAX_ROWS).map_while(Result::ok).map(|a| (a, 0)).collect();
    }
    Vec::new()
}
