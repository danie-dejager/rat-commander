//! The built-in functions and constants templates call.
//!
//! Everything that reads the file, formats text, or computes is here. What a
//! template could use to change the file, open other files, run programs or
//! ask the user something isn't: those functions stop the template with an
//! error (keeping what it built), or — for prompts — take the default answer.

use super::printf::{self, Scanned};
use super::time;
use super::{Interp, R, Stop, Target};
use crate::bt::ast::{Expr, Prim, TypeKind};
use crate::bt::tree::{Format, NO_COLOR};
use crate::bt::value::{IntTy, LocalArray, Record, Value, bytes_to_wide, wide_to_bytes};
use digest::Digest;

pub type Builtin = fn(&mut Interp, &[Expr]) -> R<Value>;

// ---- argument helpers ----------------------------------------------------

fn need(it: &Interp, a: &[Expr], n: usize, name: &str) -> R<()> {
    if a.len() < n {
        return it.err(format!("{name} needs {n} argument{}", if n == 1 { "" } else { "s" }));
    }
    Ok(())
}

fn val(it: &mut Interp, a: &[Expr], i: usize) -> R<Value> {
    match a.get(i) {
        Some(e) => it.eval(e),
        None => Ok(Value::Void),
    }
}

fn int(it: &mut Interp, a: &[Expr], i: usize) -> R<i64> {
    let v = val(it, a, i)?;
    it.int_of(&v)
}

fn int_or(it: &mut Interp, a: &[Expr], i: usize, default: i64) -> R<i64> {
    if a.len() > i { int(it, a, i) } else { Ok(default) }
}

fn flt(it: &mut Interp, a: &[Expr], i: usize) -> R<f64> {
    let v = val(it, a, i)?;
    it.float_of(&v)
}

fn text(it: &mut Interp, a: &[Expr], i: usize) -> R<Vec<u8>> {
    let v = val(it, a, i)?;
    it.bytes_of(&v)
}

fn text_or(it: &mut Interp, a: &[Expr], i: usize, default: &str) -> R<Vec<u8>> {
    if a.len() > i { text(it, a, i) } else { Ok(default.as_bytes().to_vec()) }
}

/// Store `v` into the variable argument `i` names.
fn set_out(it: &mut Interp, a: &[Expr], i: usize, v: Value) -> R<()> {
    let Some(e) = a.get(i) else { return it.err("missing output argument") };
    match it.target(e)? {
        Target::Place(p) => it.store(&p, v),
        _ => it.err("the output argument must be a variable"),
    }
}

/// The remaining arguments' values (for `...`).
fn rest(it: &mut Interp, a: &[Expr], from: usize) -> R<Vec<Value>> {
    let mut out = Vec::new();
    for e in a.iter().skip(from) {
        out.push(it.eval(e)?);
    }
    Ok(out)
}

fn s(v: impl AsRef<[u8]>) -> Value {
    Value::Str(v.as_ref().to_vec())
}

fn unsupported(it: &mut Interp, what: &str) -> R<Value> {
    it.err(format!(
        "{what} is not supported here (templates can't change the file or open other files)"
    ))
}

// ---- reading ---------------------------------------------------------------

fn read_prim(it: &mut Interp, a: &[Expr], p: Prim) -> R<Value> {
    let pos = int_or(it, a, 0, it.pos as i64)?;
    if pos < 0 {
        return it.err(format!("read at negative position {pos}"));
    }
    let big = it.big_endian;
    it.read_scalar(p, pos as u64, big)
}

fn ftell(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::int64(it.pos as i64))
}

fn fseek_to(it: &mut Interp, pos: i64) -> Value {
    if pos < 0 || pos as u64 > it.file_len() {
        return Value::int(-1);
    }
    it.pos = pos as u64;
    it.bits.unit = None;
    Value::int(0)
}

fn fseek(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "FSeek")?;
    let p = int(it, a, 0)?;
    Ok(fseek_to(it, p))
}

fn fskip(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "FSkip")?;
    let n = int(it, a, 0)?;
    let to = (it.pos as i64).saturating_add(n);
    Ok(fseek_to(it, to))
}

fn feof(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::bool(it.pos >= it.file_len()))
}

fn filesize(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::int64(it.file_len() as i64))
}

macro_rules! reader {
    ($name:ident, $prim:expr) => {
        fn $name(it: &mut Interp, a: &[Expr]) -> R<Value> {
            read_prim(it, a, $prim)
        }
    };
}
reader!(read_byte, Prim::Char);
reader!(read_ubyte, Prim::UChar);
reader!(read_short, Prim::Short);
reader!(read_ushort, Prim::UShort);
reader!(read_int, Prim::Int);
reader!(read_uint, Prim::UInt);
reader!(read_int64, Prim::Int64);
reader!(read_uint64, Prim::UInt64);
reader!(read_float, Prim::Float);
reader!(read_double, Prim::Double);
reader!(read_hfloat, Prim::HFloat);

fn read_bytes_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "ReadBytes")?;
    let pos = int(it, a, 1)?;
    let n = int(it, a, 2)?;
    if pos < 0 || n < 0 || n as usize > it.limits.max_alloc {
        return it.err("ReadBytes: bad position or size");
    }
    let data = it.read_bytes(pos as u64, n as usize);
    if data.len() < n as usize {
        return it.err(format!("ReadBytes: read past the end of the file at 0x{pos:X}"));
    }
    let uchar = it.prog.prim(Prim::UChar);
    let items = data.into_iter().map(|b| Value::Int(b as u64, IntTy::new(1, false))).collect();
    set_out(it, a, 0, Value::Array(Box::new(LocalArray { elem: uchar, items })))?;
    Ok(Value::Void)
}

/// A NUL-terminated string at `pos`: its bytes, and whether the NUL was found.
fn scan_string(it: &mut Interp, pos: u64, max: i64, wide: bool) -> (Vec<u8>, bool) {
    let cap = if max < 0 { it.limits.max_alloc } else { (max as usize).min(it.limits.max_alloc) };
    let unit = if wide { 2 } else { 1 };
    let mut out = Vec::new();
    let mut at = pos;
    let big = it.big_endian;
    while out.len() < cap * unit {
        let chunk = it.read_bytes(at, 4096);
        if chunk.is_empty() {
            return (out, false);
        }
        let mut k = 0;
        while k + unit <= chunk.len() {
            let is_nul = if wide { chunk[k] == 0 && chunk[k + 1] == 0 } else { chunk[k] == 0 };
            if is_nul {
                return (out, true);
            }
            out.extend_from_slice(&chunk[k..k + unit]);
            if out.len() >= cap * unit {
                return (out, false);
            }
            k += unit;
        }
        if k == 0 {
            return (out, false);
        }
        at += k as u64;
    }
    let _ = big;
    (out, false)
}

fn read_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "ReadString")?;
    let pos = int(it, a, 0)?.max(0) as u64;
    let max = int_or(it, a, 1, -1)?;
    Ok(Value::Str(scan_string(it, pos, max, false).0))
}

fn read_string_length(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "ReadStringLength")?;
    let pos = int(it, a, 0)?.max(0) as u64;
    let max = int_or(it, a, 1, -1)?;
    let (s, nul) = scan_string(it, pos, max, false);
    Ok(Value::int(s.len() as i64 + nul as i64))
}

fn read_wstring(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "ReadWString")?;
    let pos = int(it, a, 0)?.max(0) as u64;
    let max = int_or(it, a, 1, -1)?;
    let big = it.big_endian;
    let raw = scan_string(it, pos, max, true).0;
    Ok(Value::WStr(super::decode_wide(&raw, big)))
}

fn read_wstring_length(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "ReadWStringLength")?;
    let pos = int(it, a, 0)?.max(0) as u64;
    let max = int_or(it, a, 1, -1)?;
    let (s, nul) = scan_string(it, pos, max, true);
    Ok(Value::int((s.len() / 2) as i64 + nul as i64))
}

fn read_line(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "ReadLine")?;
    let pos = int(it, a, 0)?.max(0) as u64;
    let max = int_or(it, a, 1, -1)?;
    let keep = int_or(it, a, 2, 1)? != 0;
    let cap = if max < 0 { it.limits.max_alloc } else { max as usize };
    let mut out = Vec::new();
    let mut at = pos;
    'outer: while out.len() < cap {
        let chunk = it.read_bytes(at, 4096);
        if chunk.is_empty() {
            break;
        }
        for (k, &b) in chunk.iter().enumerate() {
            if b == b'\n' || b == b'\r' {
                if keep {
                    out.push(b);
                    if b == b'\r' && chunk.get(k + 1) == Some(&b'\n') {
                        out.push(b'\n');
                    }
                }
                break 'outer;
            }
            out.push(b);
            if out.len() >= cap {
                break 'outer;
            }
        }
        at += chunk.len() as u64;
    }
    Ok(Value::Str(out))
}

fn bytes_of_array(it: &mut Interp, a: &[Expr], i: usize) -> R<Vec<u8>> {
    let v = val(it, a, i)?;
    match v {
        Value::Array(arr) => {
            Ok(arr.items.iter().map(|x| x.as_i64_lossy().unwrap_or(0) as u8).collect())
        }
        Value::Node(r) => {
            let n = it.tree.node(r.id);
            let (start, size) = (n.start + r.shift, n.size);
            Ok(it.read_bytes(start, size.min(it.limits.max_alloc as u64) as usize))
        }
        Value::Str(s) => Ok(s),
        other => it.bytes_of(&other),
    }
}

fn convert_bytes(it: &mut Interp, a: &[Expr], p: Prim) -> R<Value> {
    need(it, a, 1, "ConvertBytesTo…")?;
    let b = bytes_of_array(it, a, 0)?;
    let size = p.size() as usize;
    if b.len() < size {
        return it.err("not enough bytes to convert");
    }
    let mut raw = 0u64;
    for k in 0..size {
        let byte = if it.big_endian { b[k] } else { b[size - 1 - k] };
        raw = (raw << 8) | byte as u64;
    }
    Ok(super::scalar_from_raw(p, raw))
}

fn convert_to_double(it: &mut Interp, a: &[Expr]) -> R<Value> {
    convert_bytes(it, a, Prim::Double)
}
fn convert_to_float(it: &mut Interp, a: &[Expr]) -> R<Value> {
    convert_bytes(it, a, Prim::Float)
}
fn convert_to_hfloat(it: &mut Interp, a: &[Expr]) -> R<Value> {
    convert_bytes(it, a, Prim::HFloat)
}

fn convert_data_to_bytes(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "ConvertDataToBytes")?;
    let v = val(it, a, 0)?;
    let v = match v {
        Value::Node(r) => it.node_value(r)?,
        v => v,
    };
    let bytes = match &v {
        Value::Int(_, t) => {
            let p = match (t.bytes, t.signed) {
                (1, _) => Prim::UChar,
                (2, _) => Prim::UShort,
                (4, _) => Prim::UInt,
                _ => Prim::UInt64,
            };
            super::encode_scalar(p, &v, it.big_endian)
        }
        Value::Float(_, true) => super::encode_scalar(Prim::Float, &v, it.big_endian),
        Value::Float(..) => super::encode_scalar(Prim::Double, &v, it.big_endian),
        other => it.bytes_of(other)?,
    };
    let n = bytes.len();
    let uchar = it.prog.prim(Prim::UChar);
    let items = bytes.into_iter().map(|b| Value::Int(b as u64, IntTy::new(1, false))).collect();
    set_out(it, a, 1, Value::Array(Box::new(LocalArray { elem: uchar, items })))?;
    Ok(Value::int(n as i64))
}

// ---- state -------------------------------------------------------------------

fn little_endian(it: &mut Interp, _: &[Expr]) -> R<Value> {
    it.big_endian = false;
    Ok(Value::Void)
}
fn big_endian(it: &mut Interp, _: &[Expr]) -> R<Value> {
    it.big_endian = true;
    Ok(Value::Void)
}
fn is_big_endian(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::bool(it.big_endian))
}
fn is_little_endian(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::bool(!it.big_endian))
}
fn bitfield_disable_padding(it: &mut Interp, _: &[Expr]) -> R<Value> {
    it.bits.padding_off = true;
    it.bits.unit = None;
    Ok(Value::Void)
}
fn bitfield_enable_padding(it: &mut Interp, _: &[Expr]) -> R<Value> {
    it.bits.padding_off = false;
    it.bits.stream = None;
    Ok(Value::Void)
}
fn bitfield_ltr(it: &mut Interp, _: &[Expr]) -> R<Value> {
    it.bits.ltr = Some(true);
    it.bits.unit = None;
    Ok(Value::Void)
}
fn bitfield_rtl(it: &mut Interp, _: &[Expr]) -> R<Value> {
    it.bits.ltr = Some(false);
    it.bits.unit = None;
    Ok(Value::Void)
}
fn is_bitfield_ltr(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::bool(it.bits.ltr.unwrap_or(it.big_endian)))
}
fn is_bitfield_padding(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::bool(!it.bits.padding_off))
}

fn set_back_color(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "SetBackColor")?;
    it.bg = int(it, a, 0)? as u32;
    Ok(Value::Void)
}
fn set_fore_color(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "SetForeColor")?;
    it.fg = int(it, a, 0)? as u32;
    Ok(Value::Void)
}
fn set_color(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "SetColor")?;
    it.fg = int(it, a, 0)? as u32;
    it.bg = int(it, a, 1)? as u32;
    Ok(Value::Void)
}
fn get_back_color(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::Int(it.bg as u64, IntTy::U32))
}
fn get_fore_color(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::Int(it.fg as u64, IntTy::U32))
}
fn set_style(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "SetStyle")?;
    it.style = int(it, a, 0)?.clamp(0, 255) as u8;
    Ok(Value::Void)
}
fn get_style(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::int(it.style as i64))
}

macro_rules! display_format {
    ($name:ident, $f:expr) => {
        fn $name(it: &mut Interp, _: &[Expr]) -> R<Value> {
            it.format = $f;
            Ok(Value::Void)
        }
    };
}
display_format!(display_hex, Format::Hex);
display_format!(display_decimal, Format::Decimal);
display_format!(display_binary, Format::Binary);
display_format!(display_octal, Format::Octal);
display_format!(display_decimal_hex, Format::DecimalHex);

fn nothing(_: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::Void)
}
fn zero(_: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::int(0))
}
fn one(_: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::int(1))
}
fn empty_string(_: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(s(""))
}

// ---- output and control -----------------------------------------------------

fn printf_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Printf")?;
    let fmt = text(it, a, 0)?;
    let args = rest(it, a, 1)?;
    let out = printf::format(it, &fmt, &args)?;
    it.print(&String::from_utf8_lossy(&out));
    Ok(Value::int(out.len() as i64))
}

fn warning_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Warning")?;
    let fmt = text(it, a, 0)?;
    let args = rest(it, a, 1)?;
    let out = printf::format(it, &fmt, &args)?;
    let msg = String::from_utf8_lossy(&out).into_owned();
    it.print(&format!("Warning: {}\n", msg.trim_end()));
    Ok(Value::int(0))
}

fn sprintf_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "SPrintf")?;
    let fmt = text(it, a, 1)?;
    let args = rest(it, a, 2)?;
    let out = printf::format(it, &fmt, &args)?;
    let n = out.len();
    set_out(it, a, 0, Value::Str(out))?;
    Ok(Value::int(n as i64))
}

fn str_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Str")?;
    let fmt = text(it, a, 0)?;
    let args = rest(it, a, 1)?;
    Ok(Value::Str(printf::format(it, &fmt, &args)?))
}

fn sscanf_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "SScanf")?;
    let input = text(it, a, 0)?;
    let fmt = text(it, a, 1)?;
    let got = printf::scan(&input, &fmt);
    let n = got.len().min(a.len().saturating_sub(2));
    for (k, g) in got.into_iter().take(n).enumerate() {
        let v = match g {
            Scanned::Int(x) => Value::int64(x),
            Scanned::Float(f) => Value::Float(f, false),
            Scanned::Text(t) => Value::Str(t),
        };
        set_out(it, a, 2 + k, v)?;
    }
    Ok(Value::int(n as i64))
}

fn exit_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let code = int_or(it, a, 0, 0)?;
    if code != 0 {
        it.print(&format!("Template exited with code {code}\n"));
    }
    Err(Stop::Exit)
}

fn assert_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Assert")?;
    let v = val(it, a, 0)?;
    if !it.truthy(&v)? {
        let msg = text_or(it, a, 1, "")?;
        let msg = String::from_utf8_lossy(&msg);
        return it.err(if msg.is_empty() {
            "Assert failed".to_string()
        } else {
            format!("Assert failed: {msg}")
        });
    }
    Ok(Value::Void)
}

fn message_box(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "MessageBox")?;
    let mask = int(it, a, 0)?;
    let title = text(it, a, 1)?;
    let fmt = text(it, a, 2)?;
    let args = rest(it, a, 3)?;
    let msg = printf::format(it, &fmt, &args)?;
    it.print(&format!("{}: {}\n", String::from_utf8_lossy(&title), String::from_utf8_lossy(&msg)));
    // Without a user to ask, the answer is "Yes" (or "OK").
    Ok(Value::int(if mask & 0xf == 3 || mask & 0xf == 4 { 6 } else { 1 }))
}

fn status_message(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "StatusMessage")?;
    let fmt = text(it, a, 0)?;
    let args = rest(it, a, 1)?;
    let out = printf::format(it, &fmt, &args)?;
    it.print(&format!("{}\n", String::from_utf8_lossy(&out).trim_end()));
    Ok(Value::Void)
}

/// `Input*(title, caption, default)`: the default.
fn input_default(it: &mut Interp, a: &[Expr]) -> R<Value> {
    match a.get(2) {
        Some(e) => it.eval(e),
        None => Ok(s("")),
    }
}

fn input_radio(it: &mut Interp, a: &[Expr]) -> R<Value> {
    Ok(Value::int(int_or(it, a, 2, 0)?))
}

fn get_file_name(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(s(it.file_name.clone()))
}
fn get_file_name_w(it: &mut Interp, _: &[Expr]) -> R<Value> {
    Ok(Value::WStr(it.file_name.encode_utf16().collect()))
}

fn file_exists(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let p = text(it, a, 0)?;
    Ok(Value::bool(std::path::Path::new(&*String::from_utf8_lossy(&p)).is_file()))
}
fn directory_exists(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let p = text(it, a, 0)?;
    Ok(Value::bool(std::path::Path::new(&*String::from_utf8_lossy(&p)).is_dir()))
}

fn find_files(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "FindFiles")?;
    let dir = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let filter = String::from_utf8_lossy(&text(it, a, 1)?).into_owned();
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten().take(100_000) {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                dirs.push(name);
            } else if crate::bt::header::mask_matches(&filter, &name) {
                files.push(name);
            }
        }
    }
    files.sort();
    dirs.sort();
    let prog = it.prog.clone();
    let sym = |n: &str| prog.syms.lookup(n).unwrap_or(0);
    let str_ty = prog.prim(Prim::Str);
    let rec = |field: &str, v: String| {
        Value::Record(Box::new(Record { fields: vec![(sym(field), Value::Str(v.into_bytes()))] }))
    };
    let fields = vec![
        (sym("filecount"), Value::int(files.len() as i64)),
        (
            sym("file"),
            Value::Array(Box::new(LocalArray {
                elem: str_ty,
                items: files.into_iter().map(|f| rec("filename", f)).collect(),
            })),
        ),
        (sym("dircount"), Value::int(dirs.len() as i64)),
        (
            sym("dir"),
            Value::Array(Box::new(LocalArray {
                elem: str_ty,
                items: dirs.into_iter().map(|d| rec("dirname", d)).collect(),
            })),
        ),
    ];
    Ok(Value::Record(Box::new(Record { fields })))
}

fn get_temp_directory(_: &mut Interp, _: &[Expr]) -> R<Value> {
    let mut d = std::env::temp_dir().display().to_string();
    if !d.ends_with(std::path::MAIN_SEPARATOR) {
        d.push(std::path::MAIN_SEPARATOR);
    }
    Ok(s(d))
}

fn get_current_date_time(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let fmt = text_or(it, a, 0, "MM/dd/yyyy hh:mm:ss")?;
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let c = time::from_unix(now.as_secs() as i64, now.subsec_nanos());
    Ok(s(time::format(&c, &String::from_utf8_lossy(&fmt))))
}

macro_rules! not_here {
    ($name:ident, $what:expr) => {
        fn $name(it: &mut Interp, _: &[Expr]) -> R<Value> {
            unsupported(it, $what)
        }
    };
}
not_here!(no_write, "writing to the file");
not_here!(no_insert, "inserting or deleting bytes");
not_here!(no_files, "opening, creating or saving files");
not_here!(no_exec, "running programs or other templates");
not_here!(no_text, "text-mode line functions");

// ---- strings -------------------------------------------------------------------

fn strlen(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Strlen")?;
    Ok(Value::int(text(it, a, 0)?.len() as i64))
}

fn ordering(o: std::cmp::Ordering) -> Value {
    Value::int(o as i64)
}

fn strcmp(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "Strcmp")?;
    let x = text(it, a, 0)?;
    let y = text(it, a, 1)?;
    Ok(ordering(x.cmp(&y)))
}

fn stricmp(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "Stricmp")?;
    let x = text(it, a, 0)?.to_ascii_lowercase();
    let y = text(it, a, 1)?.to_ascii_lowercase();
    Ok(ordering(x.cmp(&y)))
}

fn strncmp(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "Strncmp")?;
    let x = text(it, a, 0)?;
    let y = text(it, a, 1)?;
    let n = int(it, a, 2)?.max(0) as usize;
    Ok(ordering(x[..x.len().min(n)].cmp(&y[..y.len().min(n)])))
}

fn strnicmp(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "Strnicmp")?;
    let x = text(it, a, 0)?.to_ascii_lowercase();
    let y = text(it, a, 1)?.to_ascii_lowercase();
    let n = int(it, a, 2)?.max(0) as usize;
    Ok(ordering(x[..x.len().min(n)].cmp(&y[..y.len().min(n)])))
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn strstr(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "Strstr")?;
    let x = text(it, a, 0)?;
    let y = text(it, a, 1)?;
    Ok(Value::int(find_sub(&x, &y).map_or(-1, |p| p as i64)))
}

fn strchr(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "Strchr")?;
    let x = text(it, a, 0)?;
    let c = int(it, a, 1)? as u8;
    Ok(Value::int(x.iter().position(|&b| b == c).map_or(-1, |p| p as i64)))
}

fn substr(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "SubStr")?;
    let src = val(it, a, 0)?;
    let wide = matches!(src, Value::WStr(_));
    let st = int(it, a, 1)?.max(0) as usize;
    let count = int_or(it, a, 2, -1)?;
    if wide {
        let w = it.wide_text(&src)?;
        let st = st.min(w.len());
        let end = if count < 0 { w.len() } else { (st + count as usize).min(w.len()) };
        return Ok(Value::WStr(w[st..end].to_vec()));
    }
    let b = it.bytes_of(&src)?;
    let st = st.min(b.len());
    let end = if count < 0 { b.len() } else { (st + count as usize).min(b.len()) };
    Ok(Value::Str(b[st..end].to_vec()))
}

fn strdel(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "StrDel")?;
    let src = val(it, a, 0)?;
    let st = int(it, a, 1)?.max(0) as usize;
    let count = int(it, a, 2)?.max(0) as usize;
    let mut b = it.bytes_of(&src)?;
    let st = st.min(b.len());
    let end = (st + count).min(b.len());
    b.drain(st..end);
    Ok(Interp::text_like(&src, b))
}

fn strcat(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "Strcat")?;
    let dest = val(it, a, 0)?;
    let mut d = it.bytes_of(&dest)?;
    d.extend_from_slice(&text(it, a, 1)?);
    let v = Interp::text_like(&dest, d);
    set_out(it, a, 0, v)?;
    Ok(Value::Void)
}

fn strcpy(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "Strcpy")?;
    let src = val(it, a, 1)?;
    let b = it.bytes_of(&src)?;
    set_out(it, a, 0, Interp::text_like(&src, b))?;
    Ok(Value::Void)
}

fn strncpy(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "Strncpy")?;
    let dest = val(it, a, 0)?;
    let src = text(it, a, 1)?;
    let n = int(it, a, 2)?.max(0) as usize;
    let mut out = src[..src.len().min(n)].to_vec();
    if let Ok(d) = it.bytes_of(&dest)
        && d.len() > n
    {
        if out.len() < n {
            out.resize(n, 0);
        }
        out.extend_from_slice(&d[n..]);
    }
    set_out(it, a, 0, Value::Str(out))?;
    Ok(Value::Void)
}

fn memcmp(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "Memcmp")?;
    let x = bytes_of_array(it, a, 0)?;
    let y = bytes_of_array(it, a, 1)?;
    let n = int(it, a, 2)?.max(0) as usize;
    let x = &x[..x.len().min(n)];
    let y = &y[..y.len().min(n)];
    Ok(ordering(x.cmp(y)))
}

fn memcpy(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "Memcpy")?;
    let mut dest = bytes_of_array(it, a, 0).unwrap_or_default();
    let src = bytes_of_array(it, a, 1)?;
    let n = int(it, a, 2)?.max(0) as usize;
    let doff = int_or(it, a, 3, 0)?.max(0) as usize;
    let soff = int_or(it, a, 4, 0)?.max(0) as usize;
    if doff + n > it.limits.max_alloc {
        return it.err("Memcpy: too large");
    }
    if dest.len() < doff + n {
        dest.resize(doff + n, 0);
    }
    for k in 0..n {
        dest[doff + k] = src.get(soff + k).copied().unwrap_or(0);
    }
    let uchar = it.prog.prim(Prim::UChar);
    let items = dest.into_iter().map(|b| Value::Int(b as u64, IntTy::new(1, false))).collect();
    set_out(it, a, 0, Value::Array(Box::new(LocalArray { elem: uchar, items })))?;
    Ok(Value::Void)
}

fn memset(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "Memset")?;
    let mut dest = bytes_of_array(it, a, 0).unwrap_or_default();
    let c = int(it, a, 1)? as u8;
    let n = (int(it, a, 2)?.max(0) as usize).min(it.limits.max_alloc);
    if dest.len() < n {
        dest.resize(n, 0);
    }
    for b in dest.iter_mut().take(n) {
        *b = c;
    }
    let uchar = it.prog.prim(Prim::UChar);
    let items = dest.into_iter().map(|b| Value::Int(b as u64, IntTy::new(1, false))).collect();
    set_out(it, a, 0, Value::Array(Box::new(LocalArray { elem: uchar, items })))?;
    Ok(Value::Void)
}

fn to_lower(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "ToLower")?;
    match val(it, a, 0)? {
        Value::Str(t) => Ok(Value::Str(t.to_ascii_lowercase())),
        v => {
            let c = it.int_of(&v)?;
            Ok(Value::int(if (b'A' as i64..=b'Z' as i64).contains(&c) { c + 32 } else { c }))
        }
    }
}

fn to_upper(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "ToUpper")?;
    match val(it, a, 0)? {
        Value::Str(t) => Ok(Value::Str(t.to_ascii_uppercase())),
        v => {
            let c = it.int_of(&v)?;
            Ok(Value::int(if (b'a' as i64..=b'z' as i64).contains(&c) { c - 32 } else { c }))
        }
    }
}

fn atoi(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Atoi")?;
    let t = String::from_utf8_lossy(&text(it, a, 0)?).trim().to_string();
    let end = t
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    Ok(Value::int(t[..end].parse::<i64>().unwrap_or(0)))
}

fn atof(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Atof")?;
    let t = String::from_utf8_lossy(&text(it, a, 0)?).trim().to_string();
    let end = t
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || "+-.eE".contains(*c)))
        .map_or(t.len(), |(i, _)| i);
    let mut slice = &t[..end];
    let f = loop {
        if let Ok(f) = slice.parse::<f64>() {
            break f;
        }
        if slice.is_empty() {
            break 0.0;
        }
        slice = &slice[..slice.len() - 1];
    };
    Ok(Value::Float(f, false))
}

fn binary_str_to_int(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "BinaryStrToInt")?;
    let t: String = String::from_utf8_lossy(&text(it, a, 0)?)
        .chars()
        .filter(|c| *c == '0' || *c == '1')
        .collect();
    Ok(Value::int64(u64::from_str_radix(&t, 2).unwrap_or(0) as i64))
}

fn int_to_binary_str(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "IntToBinaryStr")?;
    let n = int(it, a, 0)? as u64;
    let groups = int_or(it, a, 1, 0)?;
    let spaces = int_or(it, a, 2, 1)? != 0;
    let bits = if groups > 0 {
        (groups as usize * 8).min(64)
    } else if n >> 32 != 0 {
        64
    } else if n >> 16 != 0 {
        32
    } else if n >> 8 != 0 {
        16
    } else {
        8
    };
    let mut out = String::new();
    for k in (0..bits).rev() {
        out.push(if (n >> k) & 1 == 1 { '1' } else { '0' });
        if spaces && k % 8 == 0 && k != 0 {
            out.push(' ');
        }
    }
    Ok(s(out))
}

fn string_to_wstring(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "StringToWString")?;
    let t = text(it, a, 0)?;
    Ok(Value::WStr(bytes_to_wide(&t)))
}

fn wstring_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "WStringToString")?;
    let v = val(it, a, 0)?;
    let w = it.wide_text(&v)?;
    Ok(Value::Str(wide_to_bytes(&w)))
}

fn wstrlen(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "WStrlen")?;
    let v = val(it, a, 0)?;
    let w = it.wide_text(&v)?;
    Ok(Value::int(w.iter().take_while(|&&c| c != 0).count() as i64))
}

fn wstrcmp(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "WStrcmp")?;
    let x = val(it, a, 0)?;
    let y = val(it, a, 1)?;
    let (x, y) = (it.wide_text(&x)?, it.wide_text(&y)?);
    Ok(ordering(x.cmp(&y)))
}

fn wstricmp(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "WStricmp")?;
    let x = String::from_utf16_lossy(&{
        let v = val(it, a, 0)?;
        it.wide_text(&v)?
    })
    .to_lowercase();
    let y = String::from_utf16_lossy(&{
        let v = val(it, a, 1)?;
        it.wide_text(&v)?
    })
    .to_lowercase();
    Ok(ordering(x.cmp(&y)))
}

fn wstrstr(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "WStrstr")?;
    let x = val(it, a, 0)?;
    let y = val(it, a, 1)?;
    let (x, y) = (it.wide_text(&x)?, it.wide_text(&y)?);
    let pos = if y.is_empty() { Some(0) } else { x.windows(y.len()).position(|w| w == y) };
    Ok(Value::int(pos.map_or(-1, |p| p as i64)))
}

fn wstrchr(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "WStrchr")?;
    let x = val(it, a, 0)?;
    let x = it.wide_text(&x)?;
    let c = int(it, a, 1)? as u16;
    Ok(Value::int(x.iter().position(|&b| b == c).map_or(-1, |p| p as i64)))
}

fn wstrcat(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "WStrcat")?;
    let d = val(it, a, 0)?;
    let mut d = it.wide_text(&d)?;
    let src = val(it, a, 1)?;
    d.extend(it.wide_text(&src)?);
    set_out(it, a, 0, Value::WStr(d))?;
    Ok(Value::Void)
}

fn wstrcpy(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "WStrcpy")?;
    let src = val(it, a, 1)?;
    let w = it.wide_text(&src)?;
    set_out(it, a, 0, Value::WStr(w))?;
    Ok(Value::Void)
}

fn wstrncpy(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 3, "WStrncpy")?;
    let src = val(it, a, 1)?;
    let w = it.wide_text(&src)?;
    let n = int(it, a, 2)?.max(0) as usize;
    set_out(it, a, 0, Value::WStr(w[..w.len().min(n)].to_vec()))?;
    Ok(Value::Void)
}

fn wsubstr(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "WSubStr")?;
    let src = val(it, a, 0)?;
    let w = it.wide_text(&src)?;
    let st = (int(it, a, 1)?.max(0) as usize).min(w.len());
    let count = int_or(it, a, 2, -1)?;
    let end = if count < 0 { w.len() } else { (st + count as usize).min(w.len()) };
    Ok(Value::WStr(w[st..end].to_vec()))
}

fn enum_name(it: &Interp, v: &Value) -> Option<String> {
    let Value::Int(bits, t) = v else { return None };
    if t.enum_ty == crate::bt::tree::NONE {
        return None;
    }
    let list = it.enum_lists.get(&t.enum_ty)?;
    let want = *bits as i64;
    list.iter()
        .find(|(_, x)| t.norm(*x as u64) as i64 == want || *x == want)
        .map(|(s, _)| it.prog.name(*s).to_string())
}

fn enum_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "EnumToString")?;
    let v = val(it, a, 0)?;
    let v = match v {
        Value::Node(r) => it.node_value(r)?,
        v => v,
    };
    Ok(s(enum_name(it, &v).unwrap_or_default()))
}

fn enum_flags_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "EnumFlagsToString")?;
    let v = val(it, a, 0)?;
    let v = match v {
        Value::Node(r) => it.node_value(r)?,
        v => v,
    };
    Ok(s(it.flags_text(&v)))
}

impl Interp {
    /// The name of an enum value, if it has one.
    pub(crate) fn enum_text(&self, v: &Value) -> Option<String> {
        enum_name(self, v)
    }

    /// An enum value as `A | B | 0x40` of its flag constants.
    pub(crate) fn flags_text(&self, v: &Value) -> String {
        let Value::Int(bits, t) = v else { return String::new() };
        let Some(list) = self.enum_lists.get(&t.enum_ty) else { return String::new() };
        let mut left = *bits;
        let mut parts = Vec::new();
        for (s, x) in list {
            let x = *x as u64;
            if x != 0 && left & x == x {
                parts.push(self.prog.name(*s).to_string());
                left &= !x;
            }
        }
        if left != 0 {
            parts.push(format!("0x{left:X}"));
        }
        parts.join(" | ")
    }

    /// Every constant of enum `ty`.
    pub fn enum_constants(&self, ty: crate::bt::ast::TypeId) -> Vec<(String, i64)> {
        self.enum_lists
            .get(&ty)
            .map(|l| l.iter().map(|(s, v)| (self.prog.name(*s).to_string(), *v)).collect())
            .unwrap_or_default()
    }
}

fn file_name_get_base(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "FileNameGetBase")?;
    let p = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let with_ext = int_or(it, a, 1, 1)? != 0;
    let base = p.rsplit(['/', '\\']).next().unwrap_or("").to_string();
    let base = if with_ext {
        base
    } else {
        base.rsplit_once('.').map_or(base.clone(), |(b, _)| b.to_string())
    };
    Ok(s(base))
}

fn file_name_get_extension(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "FileNameGetExtension")?;
    let p = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let base = p.rsplit(['/', '\\']).next().unwrap_or("");
    Ok(s(base.rsplit_once('.').map_or(String::new(), |(_, e)| format!(".{e}"))))
}

fn file_name_get_path(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "FileNameGetPath")?;
    let p = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let slash = int_or(it, a, 1, 1)? != 0;
    Ok(s(match p.rfind(['/', '\\']) {
        Some(i) => p[..if slash { i + 1 } else { i }].to_string(),
        None => String::new(),
    }))
}

fn file_name_set_extension(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "FileNameSetExtension")?;
    let p = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let ext = String::from_utf8_lossy(&text(it, a, 1)?).into_owned();
    let cut =
        p.rfind('.').filter(|&d| p.rfind(['/', '\\']).is_none_or(|sl| d > sl)).unwrap_or(p.len());
    let ext = if ext.starts_with('.') || ext.is_empty() { ext } else { format!(".{ext}") };
    Ok(s(format!("{}{ext}", &p[..cut])))
}

fn date_string(
    it: &mut Interp,
    a: &[Expr],
    default_fmt: &str,
    conv: fn(&Value) -> time::Civil,
) -> R<Value> {
    need(it, a, 1, "…ToString")?;
    let v = val(it, a, 0)?;
    let v = match v {
        Value::Node(r) => it.node_value(r)?,
        v => v,
    };
    let fmt = text_or(it, a, 1, default_fmt)?;
    Ok(s(time::format(&conv(&v), &String::from_utf8_lossy(&fmt))))
}

fn ival(v: &Value) -> u64 {
    v.as_i64_lossy().unwrap_or(0) as u64
}

fn dos_date_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    date_string(it, a, "MM/dd/yyyy", |v| time::from_dosdate(ival(v) as u16))
}
fn dos_time_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    date_string(it, a, "hh:mm:ss", |v| time::from_dostime(ival(v) as u16))
}
fn file_time_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    date_string(it, a, "MM/dd/yyyy hh:mm:ss", |v| time::from_filetime(ival(v)))
}
fn ole_time_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    date_string(it, a, "MM/dd/yyyy hh:mm:ss", |v| {
        time::from_oletime(v.as_f64_lossy().unwrap_or(0.0))
    })
}
fn time_t_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    date_string(it, a, "MM/dd/yyyy hh:mm:ss", |v| time::from_unix(v.as_i64_lossy().unwrap_or(0), 0))
}

fn string_to_file_time(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "StringToFileTime")?;
    let t = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let nums: Vec<i64> = t
        .split(|c: char| !c.is_ascii_digit())
        .filter(|x| !x.is_empty())
        .filter_map(|x| x.parse().ok())
        .collect();
    if nums.len() < 3 {
        return Ok(Value::int(-1));
    }
    let (mo, d, y) = (nums[0] as u32, nums[1] as u32, nums[2]);
    let secs = time::days_from_civil(y, mo, d) * 86_400
        + nums.get(3).copied().unwrap_or(0) * 3600
        + nums.get(4).copied().unwrap_or(0) * 60
        + nums.get(5).copied().unwrap_or(0);
    let ft = ((secs + 11_644_473_600) as u64).wrapping_mul(10_000_000);
    set_out(it, a, 1, Value::uint64(ft))?;
    Ok(Value::int(0))
}

fn guid_to_string(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "GUIDToString")?;
    let b = bytes_of_array(it, a, 0)?;
    if b.len() < 16 {
        return it.err("GUIDToString needs 16 bytes");
    }
    Ok(s(guid_text(&b)))
}

pub(crate) fn guid_text(b: &[u8]) -> String {
    format!(
        "{{{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        b[3],
        b[2],
        b[1],
        b[0],
        b[5],
        b[4],
        b[7],
        b[6],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

fn regex_match(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "RegExMatch")?;
    let t = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let re = String::from_utf8_lossy(&text(it, a, 1)?).into_owned();
    let re = regex_source(re.as_bytes());
    match fancy_regex::Regex::new(&format!("^(?:{re})$")) {
        Ok(r) => Ok(Value::bool(r.is_match(&t).unwrap_or(false))),
        Err(e) => it.err(format!("RegExMatch: {e}")),
    }
}

fn regex_search(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "RegExSearch")?;
    let t = String::from_utf8_lossy(&text(it, a, 0)?).into_owned();
    let re = String::from_utf8_lossy(&text(it, a, 1)?).into_owned();
    let from = int_or(it, a, 3, 0)?.max(0) as usize;
    let r = match fancy_regex::Regex::new(&regex_source(re.as_bytes())) {
        Ok(r) => r,
        Err(e) => return it.err(format!("RegExSearch: {e}")),
    };
    let from = (0..=from.min(t.len())).rev().find(|&i| t.is_char_boundary(i)).unwrap_or(0);
    match r.find_from_pos(&t, from).ok().flatten() {
        Some(m) => {
            if a.len() > 2 {
                set_out(it, a, 2, Value::int((m.end() - m.start()) as i64))?;
            }
            Ok(Value::int(m.start() as i64))
        }
        None => Ok(Value::int(-1)),
    }
}

macro_rules! char_class {
    ($name:ident, $test:expr) => {
        fn $name(it: &mut Interp, a: &[Expr]) -> R<Value> {
            let c = int(it, a, 0)?;
            let ch = char::from_u32(c as u32).unwrap_or('\0');
            Ok(Value::bool($test(ch)))
        }
    };
}
char_class!(is_char_alpha, |c: char| c.is_alphabetic());
char_class!(is_char_num, |c: char| c.is_numeric());
char_class!(is_char_alnum, |c: char| c.is_alphanumeric());
char_class!(is_char_punct, |c: char| c.is_ascii_punctuation());
char_class!(is_char_symbol, |c: char| c.is_ascii_punctuation() && !c.is_alphanumeric());
char_class!(is_char_space, |c: char| c.is_whitespace());

// ---- math ----------------------------------------------------------------------

macro_rules! math1 {
    ($name:ident, $f:expr) => {
        fn $name(it: &mut Interp, a: &[Expr]) -> R<Value> {
            let x = flt(it, a, 0)?;
            Ok(Value::Float($f(x), false))
        }
    };
}
math1!(abs_fn, f64::abs);
math1!(ceil_fn, f64::ceil);
math1!(floor_fn, f64::floor);
math1!(sqrt_fn, f64::sqrt);
math1!(log_fn, f64::ln);
math1!(log10_fn, f64::log10);
math1!(exp_fn, f64::exp);
math1!(sin_fn, f64::sin);
math1!(cos_fn, f64::cos);
math1!(tan_fn, f64::tan);

fn pow_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let x = flt(it, a, 0)?;
    let y = flt(it, a, 1)?;
    Ok(Value::Float(x.powf(y), false))
}
fn min_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let x = flt(it, a, 0)?;
    let y = flt(it, a, 1)?;
    Ok(Value::Float(x.min(y), false))
}
fn max_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let x = flt(it, a, 0)?;
    let y = flt(it, a, 1)?;
    Ok(Value::Float(x.max(y), false))
}
fn random_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let max = int_or(it, a, 0, i32::MAX as i64)?.max(1) as u64;
    it.rng ^= it.rng << 13;
    it.rng ^= it.rng >> 7;
    it.rng ^= it.rng << 17;
    Ok(Value::int((it.rng % max) as i64))
}
fn srand_fn(it: &mut Interp, a: &[Expr]) -> R<Value> {
    it.rng = (int(it, a, 0)? as u64) | 1;
    Ok(Value::Void)
}

fn swap_bytes(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "SwapBytes")?;
    let v = val(it, a, 0)?;
    let v = match v {
        Value::Node(r) => it.node_value(r)?,
        v => v,
    };
    Ok(match v {
        Value::Int(b, t) => {
            let n = t.bytes as usize;
            let le = b.to_le_bytes();
            let mut raw = 0u64;
            for &byte in &le[..n] {
                raw = (raw << 8) | byte as u64;
            }
            Value::Int(t.norm(raw), t)
        }
        Value::Float(f, true) => {
            Value::Float(f32::from_bits((f as f32).to_bits().swap_bytes()) as f64, true)
        }
        Value::Float(f, false) => Value::Float(f64::from_bits(f.to_bits().swap_bytes()), false),
        other => other,
    })
}

// ---- checksums and search -------------------------------------------------------

pub const CHECKSUM_NAMES: &[&str] = &[
    "CHECKSUM_BYTE",
    "CHECKSUM_SHORT_LE",
    "CHECKSUM_SHORT_BE",
    "CHECKSUM_INT_LE",
    "CHECKSUM_INT_BE",
    "CHECKSUM_INT64_LE",
    "CHECKSUM_INT64_BE",
    "CHECKSUM_SUM8",
    "CHECKSUM_SUM16",
    "CHECKSUM_SUM32",
    "CHECKSUM_SUM64",
    "CHECKSUM_CRC16",
    "CHECKSUM_CRCCCITT",
    "CHECKSUM_CRC32",
    "CHECKSUM_ADLER32",
    "CHECKSUM_MD2",
    "CHECKSUM_MD4",
    "CHECKSUM_MD5",
    "CHECKSUM_RIPEMD160",
    "CHECKSUM_SHA1",
    "CHECKSUM_SHA256",
    "CHECKSUM_SHA384",
    "CHECKSUM_SHA512",
    "CHECKSUM_TIGER",
];

/// A checksum's result: a number, or a digest.
enum Sum {
    Num(u64),
    Digest(Vec<u8>),
}

fn run_checksum(alg: usize, data: &[u8]) -> Option<Sum> {
    let words = |size: usize, big: bool| -> u64 {
        data.chunks(size).fold(0u64, |acc, c| {
            let mut v = 0u64;
            for k in 0..c.len() {
                let b = if big { c[k] } else { c[c.len() - 1 - k] };
                v = (v << 8) | b as u64;
            }
            acc.wrapping_add(v)
        })
    };
    let bytes = || data.iter().fold(0u64, |a, &b| a.wrapping_add(b as u64));
    Some(match alg {
        0 | 10 => Sum::Num(bytes()),
        1 => Sum::Num(words(2, false)),
        2 => Sum::Num(words(2, true)),
        3 => Sum::Num(words(4, false)),
        4 => Sum::Num(words(4, true)),
        5 => Sum::Num(words(8, false)),
        6 => Sum::Num(words(8, true)),
        7 => Sum::Num(bytes() & 0xff),
        8 => Sum::Num(bytes() & 0xffff),
        9 => Sum::Num(bytes() & 0xffff_ffff),
        11 => {
            // CRC-16/ARC.
            let mut crc: u16 = 0;
            for &b in data {
                crc ^= b as u16;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
                }
            }
            Sum::Num(crc as u64)
        }
        12 => {
            let mut crc: u16 = 0xFFFF;
            for &b in data {
                crc ^= (b as u16) << 8;
                for _ in 0..8 {
                    crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
                }
            }
            Sum::Num(crc as u64)
        }
        13 => Sum::Num(crc32fast::hash(data) as u64),
        14 => {
            let (mut s1, mut s2) = (1u32, 0u32);
            for chunk in data.chunks(5552) {
                for &b in chunk {
                    s1 += b as u32;
                    s2 += s1;
                }
                s1 %= 65521;
                s2 %= 65521;
            }
            Sum::Num(((s2 << 16) | s1) as u64)
        }
        17 => Sum::Digest(md5::compute(data).0.to_vec()),
        19 => Sum::Digest(sha1::Sha1::digest(data).to_vec()),
        20 => Sum::Digest(sha2::Sha256::digest(data).to_vec()),
        21 => Sum::Digest(sha2::Sha384::digest(data).to_vec()),
        22 => Sum::Digest(sha2::Sha512::digest(data).to_vec()),
        _ => return None,
    })
}

fn range_data(it: &mut Interp, start: i64, size: i64) -> R<Vec<u8>> {
    let len = it.file_len();
    let (start, size) =
        if start == 0 && size == 0 { (0, len) } else { (start.max(0) as u64, size.max(0) as u64) };
    if size as usize > 256 << 20 {
        return it.err("checksum range too large");
    }
    Ok(it.read_bytes(start, size as usize))
}

fn checksum(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "Checksum")?;
    let alg = int(it, a, 0)?;
    let start = int_or(it, a, 1, 0)?;
    let size = int_or(it, a, 2, 0)?;
    let data = range_data(it, start, size)?;
    match run_checksum(alg as usize, &data) {
        Some(Sum::Num(n)) => Ok(Value::int64(n as i64)),
        _ => Ok(Value::int64(-1)),
    }
}

fn sum_text(sum: Sum) -> String {
    match sum {
        Sum::Num(n) => format!("{n:X}"),
        Sum::Digest(d) => d.iter().map(|b| format!("{b:02X}")).collect(),
    }
}

fn checksum_alg_str(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 2, "ChecksumAlgStr")?;
    let alg = int(it, a, 0)?;
    let start = int_or(it, a, 2, 0)?;
    let size = int_or(it, a, 3, 0)?;
    let data = range_data(it, start, size)?;
    let Some(sum) = run_checksum(alg as usize, &data) else { return Ok(Value::int(-1)) };
    let t = sum_text(sum);
    let n = t.len();
    set_out(it, a, 1, s(t))?;
    Ok(Value::int(n as i64))
}

fn checksum_array_str(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 4, "ChecksumAlgArrayStr")?;
    let alg = int(it, a, 0)?;
    let mut data = bytes_of_array(it, a, 2)?;
    let size = int(it, a, 3)?.max(0) as usize;
    data.truncate(size);
    let Some(sum) = run_checksum(alg as usize, &data) else { return Ok(Value::int(-1)) };
    let t = sum_text(sum);
    let n = t.len();
    set_out(it, a, 1, s(t))?;
    Ok(Value::int(n as i64))
}

fn checksum_array_bytes(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 4, "ChecksumAlgArrayBytes")?;
    let alg = int(it, a, 0)?;
    let mut data = bytes_of_array(it, a, 2)?;
    let size = int(it, a, 3)?.max(0) as usize;
    data.truncate(size);
    let bytes = match run_checksum(alg as usize, &data) {
        Some(Sum::Digest(d)) => d,
        Some(Sum::Num(n)) => n.to_be_bytes().to_vec(),
        None => return Ok(Value::int(-1)),
    };
    let n = bytes.len();
    let uchar = it.prog.prim(Prim::UChar);
    let items = bytes.into_iter().map(|b| Value::Int(b as u64, IntTy::new(1, false))).collect();
    set_out(it, a, 1, Value::Array(Box::new(LocalArray { elem: uchar, items })))?;
    Ok(Value::int(n as i64))
}

/// The bytes a `Find*` call searches for: a string's, or a number's in the
/// current byte order.
fn search_bytes(it: &mut Interp, a: &[Expr]) -> R<Vec<u8>> {
    let v = val(it, a, 0)?;
    let v = match v {
        Value::Node(r) => it.node_value(r)?,
        v => v,
    };
    Ok(match &v {
        Value::Int(_, t) => {
            let p = match t.bytes {
                1 => Prim::UChar,
                2 => Prim::UShort,
                4 => Prim::UInt,
                _ => Prim::UInt64,
            };
            super::encode_scalar(p, &v, it.big_endian)
        }
        Value::Float(_, f32) => {
            super::encode_scalar(if *f32 { Prim::Float } else { Prim::Double }, &v, it.big_endian)
        }
        Value::WStr(w) => w
            .iter()
            .flat_map(|c| if it.big_endian { c.to_be_bytes() } else { c.to_le_bytes() })
            .collect(),
        other => it.bytes_of(other)?,
    })
}

/// Every match of `needle` in `start..end`, in order, up to `limit`.
fn search(
    it: &mut Interp,
    needle: &[u8],
    case: bool,
    start: u64,
    end: u64,
    limit: usize,
) -> Vec<u64> {
    let mut hits = Vec::new();
    if needle.is_empty() || start >= end {
        return hits;
    }
    let chunk = 1usize << 20;
    let mut at = start;
    let fold = |b: &[u8]| if case { b.to_vec() } else { b.to_ascii_lowercase() };
    let n = fold(needle);
    while at < end && hits.len() < limit {
        let want = ((end - at) as usize).min(chunk + n.len() - 1);
        let buf = fold(&it.read_bytes(at, want));
        if buf.len() < n.len() {
            break;
        }
        let mut k = 0;
        while let Some(p) = memchr::memmem::find(&buf[k..], &n) {
            let abs = at + (k + p) as u64;
            if abs + n.len() as u64 > end {
                break;
            }
            hits.push(abs);
            if hits.len() >= limit {
                break;
            }
            k += p + 1;
        }
        at += (buf.len() - (n.len() - 1)) as u64;
    }
    hits.dedup();
    hits
}

/// A 010 Editor regex, made acceptable to fancy-regex: octal escapes
/// (`\000`) become hex ones.
fn regex_source(pattern: &[u8]) -> String {
    let p = String::from_utf8_lossy(pattern);
    let b = p.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() && (b'0'..=b'7').contains(&b[i + 1]) {
            let mut j = i + 1;
            let mut v = 0u32;
            while j < b.len() && j < i + 4 && (b'0'..=b'7').contains(&b[j]) {
                v = v * 8 + (b[j] - b'0') as u32;
                j += 1;
            }
            out.push_str(&format!("\\x{{{v:02X}}}"));
            i = j;
            continue;
        }
        let ch_len = p[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&p[i..i + ch_len]);
        if b[i] == b'\\' && i + 1 < b.len() {
            let n = p[i + 1..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&p[i + 1..i + 1 + n]);
            i += 1 + n;
            continue;
        }
        i += ch_len;
    }
    out
}

/// A wildcard pattern (`*`, `?`) as a regex.
fn wildcard_source(pattern: &[u8], max_star: i64) -> String {
    let mut out = String::new();
    for ch in String::from_utf8_lossy(pattern).chars() {
        match ch {
            '*' => out.push_str(&format!(".{{0,{}}}?", max_star.max(0))),
            '?' => out.push('.'),
            c => out.push_str(&fancy_regex::escape(&c.to_string())),
        }
    }
    out
}

/// A pattern that begins with a lookbehind, `(?<=X)Y`, as `(?:X)(Y)`: the same
/// matches (as group 1) for the fast byte-regex engine, which has no
/// lookaround. `None` if the pattern isn't of that shape.
fn lookbehind_as_group(source: &str) -> Option<String> {
    let rest = source.strip_prefix("(?<=")?;
    let b = rest.as_bytes();
    let (mut depth, mut i, mut class) = (1usize, 0usize, false);
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'[' if !class => class = true,
            b']' if class => class = false,
            b'(' if !class => depth += 1,
            b')' if !class => {
                depth -= 1;
                if depth == 0 {
                    let (behind, tail) = (&rest[..i], &rest[i + 1..]);
                    if tail.contains("(?<") || tail.contains("(?=") || tail.contains("(?!") {
                        return None;
                    }
                    return Some(format!("(?:{behind})({tail})"));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Every regex match in `start..end`: (position, length). Plain patterns (and
/// ones only starting with a lookbehind) run on the bytes with the `regex`
/// crate; the rest see the bytes as Latin-1 text, one character per byte.
fn search_regex(
    it: &mut Interp,
    source: &str,
    case: bool,
    start: u64,
    end: u64,
    limit: usize,
) -> R<Vec<(u64, u64)>> {
    let flags = if case { "(?-u)" } else { "(?-u)(?i)" };
    let (fast_source, grouped) = match lookbehind_as_group(source) {
        Some(g) => (g, true),
        None => (source.to_string(), false),
    };
    if let Ok(re) = regex::bytes::Regex::new(&format!("{flags}{fast_source}")) {
        let size = (end.saturating_sub(start) as usize).min(64 << 20);
        let data = it.read_bytes(start, size);
        let mut hits = Vec::new();
        for caps in re.captures_iter(&data) {
            let m = if grouped { caps.get(1) } else { caps.get(0) };
            if let Some(m) = m {
                hits.push((start + m.start() as u64, (m.end() - m.start()) as u64));
            }
            if hits.len() >= limit {
                break;
            }
        }
        return Ok(hits);
    }
    let source = if case { source.to_string() } else { format!("(?i){source}") };
    let re = match fancy_regex::Regex::new(&source) {
        Ok(r) => r,
        Err(e) => return it.err(format!("bad regular expression: {e}")),
    };
    let size = (end.saturating_sub(start) as usize).min(64 << 20);
    let data = it.read_bytes(start, size);
    let text: String = data.iter().map(|&b| b as char).collect();
    let mut hits = Vec::new();
    // Byte offsets in `text` to file offsets: count characters as we go.
    let (mut seen_bytes, mut seen_chars) = (0usize, 0u64);
    let mut to_file = |off: usize| {
        seen_chars +=
            text.as_bytes()[seen_bytes..off].iter().filter(|&&b| b & 0xc0 != 0x80).count() as u64;
        seen_bytes = off;
        start + seen_chars
    };
    for m in re.find_iter(&text) {
        let Ok(m) = m else { break };
        let s = to_file(m.start());
        let len = m.as_str().chars().count() as u64;
        hits.push((s, len));
        if hits.len() >= limit {
            break;
        }
    }
    Ok(hits)
}

/// The matches a `FindAll` / `FindFirst` call asks for: (position, length).
#[allow(clippy::too_many_arguments)]
fn find_matches(
    it: &mut Interp,
    a: &[Expr],
    method_arg: usize,
    wild_arg: usize,
    start: u64,
    end: u64,
    limit: usize,
) -> R<Vec<(u64, u64)>> {
    let case = int_or(it, a, 1, 1)? != 0;
    let method = int_or(it, a, method_arg, 0)?;
    match method {
        1 | 2 => {
            let pattern = text(it, a, 0)?;
            let source = if method == 2 {
                regex_source(&pattern)
            } else {
                let max = int_or(it, a, wild_arg, 24)?;
                wildcard_source(&pattern, max)
            };
            search_regex(it, &source, case, start, end, limit)
        }
        _ => {
            let needle = search_bytes(it, a)?;
            let n = needle.len() as u64;
            Ok(search(it, &needle, case, start, end, limit).into_iter().map(|h| (h, n)).collect())
        }
    }
}

fn find_all(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "FindAll")?;
    let start = int_or(it, a, 5, 0)?.max(0) as u64;
    let size = int_or(it, a, 6, 0)?.max(0) as u64;
    let len = it.file_len();
    let end = if size == 0 { len } else { (start + size).min(len) };
    let matches = find_matches(it, a, 3, 7, start, end, 1_000_000)?;
    let prog = it.prog.clone();
    let sym = |n: &str| prog.syms.lookup(n).unwrap_or(0);
    let i64t = prog.prim(Prim::Int64);
    let fields = vec![
        (sym("count"), Value::int(matches.len() as i64)),
        (
            sym("start"),
            Value::Array(Box::new(LocalArray {
                elem: i64t,
                items: matches.iter().map(|&(h, _)| Value::int64(h as i64)).collect(),
            })),
        ),
        (
            sym("size"),
            Value::Array(Box::new(LocalArray {
                elem: i64t,
                items: matches.iter().map(|&(_, n)| Value::int64(n as i64)).collect(),
            })),
        ),
    ];
    Ok(Value::Record(Box::new(Record { fields })))
}

fn find_first(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "FindFirst")?;
    let dir = int_or(it, a, 5, 1)?;
    let start = int_or(it, a, 6, 0)?.max(0) as u64;
    let size = int_or(it, a, 7, 0)?.max(0) as u64;
    let len = it.file_len();
    let end = if size == 0 { len } else { (start + size).min(len) };
    let method = int_or(it, a, 3, 0)?;
    if method != 0 {
        // Regex and wildcard searches: FindNext isn't supported for these.
        let all = find_matches(it, a, 3, 8, start, end, if dir >= 0 { 1 } else { 1_000_000 })?;
        it.find = None;
        let hit = if dir >= 0 { all.first() } else { all.last() };
        return Ok(Value::int64(hit.map_or(-1, |h| h.0 as i64)));
    }
    let needle = search_bytes(it, a)?;
    let case = int_or(it, a, 1, 1)? != 0;
    it.find = Some((needle.clone(), case, 0));
    let hit = if dir >= 0 {
        search(it, &needle, case, start, end, 1).first().copied()
    } else {
        search(it, &needle, case, start, end, usize::MAX).last().copied()
    };
    if let (Some(h), Some(f)) = (hit, &mut it.find) {
        f.2 = h;
    }
    Ok(Value::int64(hit.map_or(-1, |h| h as i64)))
}

fn find_next(it: &mut Interp, a: &[Expr]) -> R<Value> {
    let dir = int_or(it, a, 0, 1)?;
    let Some((needle, case, last)) = it.find.clone() else { return Ok(Value::int64(-1)) };
    let len = it.file_len();
    let hit = if dir >= 0 {
        search(it, &needle, case, last + 1, len, 1).first().copied()
    } else {
        search(
            it,
            &needle,
            case,
            0,
            (last + needle.len() as u64).saturating_sub(1).min(len),
            usize::MAX,
        )
        .into_iter()
        .rfind(|&h| h < last)
    };
    if let (Some(h), Some(f)) = (hit, &mut it.find) {
        f.2 = h;
    }
    Ok(Value::int64(hit.map_or(-1, |h| h as i64)))
}

// ---- the tables -------------------------------------------------------------

pub static TABLE: &[(&str, Builtin)] = &[
    ("FTell", ftell),
    ("FSeek", fseek),
    ("FSkip", fskip),
    ("FEof", feof),
    ("FileSize", filesize),
    ("ReadByte", read_byte),
    ("ReadUByte", read_ubyte),
    ("ReadShort", read_short),
    ("ReadUShort", read_ushort),
    ("ReadInt", read_int),
    ("ReadUInt", read_uint),
    ("ReadInt64", read_int64),
    ("ReadUInt64", read_uint64),
    ("ReadQuad", read_int64),
    ("ReadUQuad", read_uint64),
    ("ReadFloat", read_float),
    ("ReadDouble", read_double),
    ("ReadHFloat", read_hfloat),
    ("ReadBytes", read_bytes_fn),
    ("ReadString", read_string),
    ("ReadStringLength", read_string_length),
    ("ReadWString", read_wstring),
    ("ReadWStringLength", read_wstring_length),
    ("ReadLine", read_line),
    ("ConvertBytesToDouble", convert_to_double),
    ("ConvertBytesToFloat", convert_to_float),
    ("ConvertBytesToHFloat", convert_to_hfloat),
    ("ConvertDataToBytes", convert_data_to_bytes),
    ("LittleEndian", little_endian),
    ("BigEndian", big_endian),
    ("IsBigEndian", is_big_endian),
    ("IsLittleEndian", is_little_endian),
    ("BitfieldDisablePadding", bitfield_disable_padding),
    ("BitfieldEnablePadding", bitfield_enable_padding),
    ("BitfieldLeftToRight", bitfield_ltr),
    ("BitfieldRightToLeft", bitfield_rtl),
    ("IsBitfieldLeftToRight", is_bitfield_ltr),
    ("IsBitfieldPaddingEnabled", is_bitfield_padding),
    ("BitfieldGetCurrentShift", zero),
    ("BitfieldSetAutoCheckBox", nothing),
    ("BitfieldGetAutoCheckBox", one),
    ("SetBackColor", set_back_color),
    ("SetForeColor", set_fore_color),
    ("SetColor", set_color),
    ("GetBackColor", get_back_color),
    ("GetForeColor", get_fore_color),
    ("SetStyle", set_style),
    ("GetStyle", get_style),
    ("DisplayFormatHex", display_hex),
    ("DisplayFormatDecimal", display_decimal),
    ("DisplayFormatBinary", display_binary),
    ("DisplayFormatOctal", display_octal),
    ("DisplayFormatDecimalHex", display_decimal_hex),
    ("Printf", printf_fn),
    ("Warning", warning_fn),
    ("SPrintf", sprintf_fn),
    ("Str", str_fn),
    ("SScanf", sscanf_fn),
    ("Exit", exit_fn),
    ("Terminate", exit_fn),
    ("Assert", assert_fn),
    ("RequiresVersion", nothing),
    ("RequiresFile", nothing),
    ("MessageBox", message_box),
    ("StatusMessage", status_message),
    ("InputString", input_default),
    ("InputWString", input_default),
    ("InputNumber", input_default),
    ("InputFloat", input_default),
    ("InputRadioButtonBox", input_radio),
    ("InputOpenFileName", empty_string),
    ("InputOpenFileNames", empty_string),
    ("InputSaveFileName", empty_string),
    ("InputDirectory", empty_string),
    ("OutputPaneClear", nothing),
    ("OutputPaneSave", nothing),
    ("OutputPaneCopy", nothing),
    ("ExpandAll", nothing),
    ("SetUnoptimizedArraysCollapsible", nothing),
    ("ThemeAutoScaleColors", nothing),
    ("ThemeIsDark", one),
    ("Sleep", nothing),
    ("DisableUndo", nothing),
    ("EnableUndo", nothing),
    ("DisasmSetMode", nothing),
    ("DisasmGetMode", zero),
    ("SetCursorPos", nothing),
    ("SetSelection", nothing),
    ("GetCursorPos", zero),
    ("GetSelStart", zero),
    ("GetSelSize", zero),
    ("AddBookmark", nothing),
    ("RemoveBookmark", nothing),
    ("GetNumBookmarks", zero),
    ("GetBookmarkPos", zero),
    ("GetBookmarkName", empty_string),
    ("GetBookmarkType", zero),
    ("GetBookmarkArraySize", zero),
    ("SetStartingAddress", nothing),
    ("GetStartingAddress", zero),
    ("GetFileNum", zero),
    ("FileCount", one),
    ("GetNumArgs", zero),
    ("GetArg", empty_string),
    ("GetEnv", empty_string),
    ("IsNoUIMode", zero),
    ("IsDrive", zero),
    ("IsLogicalDrive", zero),
    ("IsPhysicalDrive", zero),
    ("IsProcess", zero),
    ("OffsetGetStart", zero),
    ("OffsetGetLimitSize", zero),
    ("OffsetSetStart", nothing),
    ("OffsetSetLimitSize", nothing),
    ("OffsetClear", nothing),
    ("ClearClipboard", nothing),
    ("CopyToClipboard", nothing),
    ("CopyStringToClipboard", nothing),
    ("SetReadOnly", nothing),
    ("GetReadOnly", one),
    ("GetFileName", get_file_name),
    ("GetFileNameW", get_file_name_w),
    ("FileExists", file_exists),
    ("DirectoryExists", directory_exists),
    ("FindFiles", find_files),
    ("GetTempDirectory", get_temp_directory),
    ("GetCurrentDateTime", get_current_date_time),
    ("WriteByte", no_write),
    ("WriteUByte", no_write),
    ("WriteShort", no_write),
    ("WriteUShort", no_write),
    ("WriteInt", no_write),
    ("WriteUInt", no_write),
    ("WriteInt64", no_write),
    ("WriteUInt64", no_write),
    ("WriteQuad", no_write),
    ("WriteUQuad", no_write),
    ("WriteFloat", no_write),
    ("WriteDouble", no_write),
    ("WriteHFloat", no_write),
    ("WriteBytes", no_write),
    ("WriteString", no_write),
    ("WriteWString", no_write),
    ("OverwriteBytes", no_write),
    ("InsertBytes", no_insert),
    ("DeleteBytes", no_insert),
    ("FileOpen", no_files),
    ("FileNew", no_files),
    ("FileSave", no_files),
    ("FileSaveRange", no_files),
    ("FileClose", no_files),
    ("FileSelect", no_files),
    ("FindOpenFile", no_files),
    ("FPrintf", no_files),
    ("MakeDir", no_files),
    ("DeleteFile", no_files),
    ("RenameFile", no_files),
    ("ImportFile", no_files),
    ("ExportFile", no_files),
    ("InsertFile", no_files),
    ("Exec", no_exec),
    ("RunTemplate", no_exec),
    ("TextGetNumLines", no_text),
    ("TextGetLineSize", no_text),
    ("TextAddressToLine", no_text),
    ("TextLineToAddress", no_text),
    ("TextReadLine", no_text),
    ("Strlen", strlen),
    ("Strcmp", strcmp),
    ("Stricmp", stricmp),
    ("Strncmp", strncmp),
    ("Strnicmp", strnicmp),
    ("Strstr", strstr),
    ("Strchr", strchr),
    ("SubStr", substr),
    ("StrDel", strdel),
    ("Strcat", strcat),
    ("Strcpy", strcpy),
    ("Strncpy", strncpy),
    ("Memcmp", memcmp),
    ("Memcpy", memcpy),
    ("Memset", memset),
    ("ToLower", to_lower),
    ("ToUpper", to_upper),
    ("ToLowerW", to_lower),
    ("ToUpperW", to_upper),
    ("Atoi", atoi),
    ("Atof", atof),
    ("BinaryStrToInt", binary_str_to_int),
    ("IntToBinaryStr", int_to_binary_str),
    ("StringToWString", string_to_wstring),
    ("WStringToString", wstring_to_string),
    ("StringToUTF8", strcpy_value),
    ("WStringToUTF8", wstring_to_string),
    ("ConvertString", strcpy_value),
    ("WStrlen", wstrlen),
    ("WStrcmp", wstrcmp),
    ("WStricmp", wstricmp),
    ("WStrstr", wstrstr),
    ("WStrchr", wstrchr),
    ("WStrcat", wstrcat),
    ("WStrcpy", wstrcpy),
    ("WStrncpy", wstrncpy),
    ("WSubStr", wsubstr),
    ("EnumToString", enum_to_string),
    ("EnumFlagsToString", enum_flags_to_string),
    ("FileNameGetBase", file_name_get_base),
    ("FileNameGetBaseW", file_name_get_base),
    ("FileNameGetExtension", file_name_get_extension),
    ("FileNameGetExtensionW", file_name_get_extension),
    ("FileNameGetPath", file_name_get_path),
    ("FileNameGetPathW", file_name_get_path),
    ("FileNameSetExtension", file_name_set_extension),
    ("DosDateToString", dos_date_to_string),
    ("DosTimeToString", dos_time_to_string),
    ("FileTimeToString", file_time_to_string),
    ("OleTimeToString", ole_time_to_string),
    ("TimeTToString", time_t_to_string),
    ("Time64TToString", time_t_to_string),
    ("StringToFileTime", string_to_file_time),
    ("GUIDToString", guid_to_string),
    ("RegExMatch", regex_match),
    ("RegExSearch", regex_search),
    ("IsCharAlpha", is_char_alpha),
    ("IsCharNum", is_char_num),
    ("IsCharAlphaNum", is_char_alnum),
    ("IsCharPunct", is_char_punct),
    ("IsCharSymbol", is_char_symbol),
    ("IsCharWhitespace", is_char_space),
    ("IsCharAlphaW", is_char_alpha),
    ("IsCharNumW", is_char_num),
    ("IsCharAlphaNumW", is_char_alnum),
    ("IsCharWhitespaceW", is_char_space),
    ("Abs", abs_fn),
    ("Ceil", ceil_fn),
    ("Floor", floor_fn),
    ("Sqrt", sqrt_fn),
    ("Log", log_fn),
    ("Log10", log10_fn),
    ("Exp", exp_fn),
    ("Sin", sin_fn),
    ("Cos", cos_fn),
    ("Tan", tan_fn),
    ("Pow", pow_fn),
    ("Min", min_fn),
    ("Max", max_fn),
    ("Random", random_fn),
    ("SRand", srand_fn),
    ("SwapBytes", swap_bytes),
    ("Checksum", checksum),
    ("ChecksumAlgStr", checksum_alg_str),
    ("ChecksumAlgArrayStr", checksum_array_str),
    ("ChecksumAlgArrayBytes", checksum_array_bytes),
    ("FindAll", find_all),
    ("FindFirst", find_first),
    ("FindNext", find_next),
];

fn strcpy_value(it: &mut Interp, a: &[Expr]) -> R<Value> {
    need(it, a, 1, "StringToUTF8")?;
    Ok(Value::Str(text(it, a, 0)?))
}

/// A built-in constant.
pub fn constant(name: &str) -> Option<Value> {
    let color = |c: u32| Some(Value::Int(c as u64, IntTy::U32));
    match name {
        "true" | "TRUE" => Some(Value::int(1)),
        "false" | "FALSE" => Some(Value::int(0)),
        "M_PI" | "PI" => Some(Value::Float(std::f64::consts::PI, false)),
        "cBlack" => color(0x000000),
        "cRed" => color(0x0000ff),
        "cDkRed" => color(0x000080),
        "cLtRed" => color(0x8080ff),
        "cGreen" => color(0x00ff00),
        "cDkGreen" => color(0x008000),
        "cLtGreen" => color(0x80ff80),
        "cBlue" => color(0xff0000),
        "cDkBlue" => color(0x800000),
        "cLtBlue" => color(0xff8080),
        "cPurple" => color(0xff00ff),
        "cDkPurple" => color(0x800080),
        "cLtPurple" => color(0xffe0ff),
        "cAqua" => color(0xffff00),
        "cDkAqua" => color(0x808000),
        "cLtAqua" => color(0xffffe0),
        "cYellow" => color(0x00ffff),
        "cDkYellow" => color(0x008080),
        "cLtYellow" => color(0x80ffff),
        "cDkGray" => color(0x404040),
        "cGray" => color(0x808080),
        "cSilver" => color(0xc0c0c0),
        "cLtGray" => color(0xe0e0e0),
        "cWhite" => color(0xffffff),
        "cNone" => color(NO_COLOR),
        "FINDMETHOD_NORMAL" => Some(Value::int(0)),
        "FINDMETHOD_WILDCARDS" => Some(Value::int(1)),
        "FINDMETHOD_REGEX" => Some(Value::int(2)),
        "IDOK" => Some(Value::int(1)),
        "IDCANCEL" => Some(Value::int(2)),
        "IDYES" => Some(Value::int(6)),
        "IDNO" => Some(Value::int(7)),
        "MB_OK" => Some(Value::int(0)),
        "MB_OKCANCEL" => Some(Value::int(1)),
        "MB_YESNOCANCEL" => Some(Value::int(3)),
        "MB_YESNO" => Some(Value::int(4)),
        "MB_ICONERROR" => Some(Value::int(0x10)),
        "MB_ICONQUESTION" => Some(Value::int(0x20)),
        "MB_ICONWARNING" => Some(Value::int(0x30)),
        "MB_ICONINFORMATION" => Some(Value::int(0x40)),
        "CHARSET_ASCII" => Some(Value::int(0)),
        "CHARSET_ANSI" => Some(Value::int(1)),
        "CHARSET_OEM" => Some(Value::int(2)),
        "CHARSET_EBCDIC" => Some(Value::int(3)),
        "CHARSET_UNICODE" => Some(Value::int(4)),
        "CHARSET_UTF8" => Some(Value::int(5)),
        "DISASM_X86_16" => Some(Value::int(0)),
        "DISASM_X86_32" => Some(Value::int(1)),
        "DISASM_X86_64" => Some(Value::int(2)),
        "DISASM_ARM_32" => Some(Value::int(3)),
        "DISASM_ARM_64" => Some(Value::int(4)),
        _ => {
            if let Some(i) = CHECKSUM_NAMES.iter().position(|n| *n == name) {
                return Some(Value::int(i as i64));
            }
            if let Some(i) = super::decl::STYLES.iter().position(|n| *n == name) {
                return Some(Value::int(i as i64));
            }
            None
        }
    }
}

/// Whether a type is a GUID (for display).
pub(crate) fn is_guid(it: &Interp, ty: crate::bt::ast::TypeId) -> bool {
    let mut t = ty;
    for _ in 0..32 {
        let td = it.prog.ty(t);
        if it.prog.name(td.name) == "GUID" {
            return true;
        }
        match &td.kind {
            TypeKind::Alias { target, .. } => t = *target,
            _ => return false,
        }
    }
    false
}
