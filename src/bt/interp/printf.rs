//! `printf`-style formatting for `Printf`, `SPrintf`, `Str` and `Warning`,
//! and the matching scanner for `SScanf`.

use super::{Interp, R};
use crate::bt::value::Value;

struct Spec {
    left: bool,
    zero: bool,
    plus: bool,
    space: bool,
    alt: bool,
    width: Option<usize>,
    prec: Option<usize>,
    long64: bool,
    short: u8,
    conv: u8,
}

/// Format `fmt` with `args`.
pub fn format(it: &mut Interp, fmt: &[u8], args: &[Value]) -> R<Vec<u8>> {
    let mut out = Vec::new();
    let mut ai = 0usize;
    let mut i = 0usize;
    let mut next = |it: &mut Interp| -> R<Value> {
        let v = args.get(ai).cloned().unwrap_or(Value::int(0));
        ai += 1;
        Ok(match v {
            Value::Node(r) => it.node_value(r)?,
            v => v,
        })
    };
    while i < fmt.len() {
        let c = fmt[i];
        if c != b'%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= fmt.len() {
            out.push(b'%');
            break;
        }
        if fmt[i] == b'%' {
            out.push(b'%');
            i += 1;
            continue;
        }
        let mut s = Spec {
            left: false,
            zero: false,
            plus: false,
            space: false,
            alt: false,
            width: None,
            prec: None,
            long64: false,
            short: 0,
            conv: b'd',
        };
        while i < fmt.len() {
            match fmt[i] {
                b'-' => s.left = true,
                b'0' => s.zero = true,
                b'+' => s.plus = true,
                b' ' => s.space = true,
                b'#' => s.alt = true,
                _ => break,
            }
            i += 1;
        }
        if i < fmt.len() && fmt[i] == b'*' {
            let w = next(it)?;
            s.width = Some(it.int_of(&w)?.max(0) as usize);
            i += 1;
        } else {
            let start = i;
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                i += 1;
            }
            if i > start {
                s.width = std::str::from_utf8(&fmt[start..i]).ok().and_then(|t| t.parse().ok());
            }
        }
        if i < fmt.len() && fmt[i] == b'.' {
            i += 1;
            if i < fmt.len() && fmt[i] == b'*' {
                let p = next(it)?;
                s.prec = Some(it.int_of(&p)?.max(0) as usize);
                i += 1;
            } else {
                let start = i;
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    i += 1;
                }
                s.prec = Some(
                    std::str::from_utf8(&fmt[start..i])
                        .ok()
                        .and_then(|t| t.parse().ok())
                        .unwrap_or(0),
                );
            }
        }
        // Length modifiers.
        loop {
            let rest = &fmt[i.min(fmt.len())..];
            if rest.starts_with(b"I64") {
                s.long64 = true;
                i += 3;
            } else if rest.starts_with(b"I32") {
                i += 3;
            } else if rest.starts_with(b"ll") || rest.starts_with(b"L") || rest.starts_with(b"q") {
                s.long64 = true;
                i += if rest.starts_with(b"ll") { 2 } else { 1 };
            } else if rest.starts_with(b"hh") {
                s.short = 1;
                i += 2;
            } else if rest.starts_with(b"h") {
                s.short = 2;
                i += 1;
            } else if rest.starts_with(b"l")
                || rest.starts_with(b"j")
                || rest.starts_with(b"z")
                || rest.starts_with(b"t")
            {
                i += 1;
            } else {
                break;
            }
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        s.conv = conv;
        let body: Vec<u8> = match conv {
            b'd' | b'i' | b'u' | b'x' | b'X' | b'o' | b'b' => {
                let v = next(it)?;
                int_body(it, &s, &v)?
            }
            b'c' => {
                let v = next(it)?;
                match v {
                    Value::Str(t) => t.first().copied().into_iter().collect(),
                    v => {
                        let code = it.int_of(&v)? as u32;
                        if code < 0x80 {
                            vec![code as u8]
                        } else if let Some(ch) = char::from_u32(code).filter(|_| code > 0xff) {
                            ch.to_string().into_bytes()
                        } else {
                            vec![code as u8]
                        }
                    }
                }
            }
            b's' | b'S' => {
                let v = next(it)?;
                let mut t = match &v {
                    Value::Int(..) | Value::Float(..) => it.display_value(&v).into_bytes(),
                    Value::Node(_) => Vec::new(),
                    other => it.bytes_of(other).unwrap_or_default(),
                };
                if let Some(p) = s.prec {
                    t.truncate(p);
                }
                t
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                let v = next(it)?;
                let f = it.float_of(&v).unwrap_or(0.0);
                float_body(&s, f).into_bytes()
            }
            b'p' => {
                let v = next(it)?;
                format!("{:08X}", it.int_of(&v)?).into_bytes()
            }
            b'n' => {
                next(it)?;
                Vec::new()
            }
            other => vec![b'%', other],
        };
        pad(&mut out, &s, body);
    }
    Ok(out)
}

fn int_body(it: &mut Interp, s: &Spec, v: &Value) -> R<Vec<u8>> {
    let (raw, signed_ty, bytes) = match v {
        Value::Int(b, t) => (*b, t.signed, t.bytes),
        Value::Float(f, _) => (*f as i64 as u64, true, 8),
        Value::Str(t) => (t.first().copied().unwrap_or(0) as u64, false, 1),
        other => (it.int_of(other)? as u64, true, 8),
    };
    // Unsigned conversions print the value at its own width, as C does with
    // the matching length modifier.
    let width_bits = if s.long64 { 64 } else { (bytes.max(4) as u32) * 8 };
    let masked = match s.short {
        1 => raw & 0xff,
        2 => raw & 0xffff,
        _ if width_bits >= 64 => raw,
        _ => raw & ((1u64 << width_bits) - 1),
    };
    let text = match s.conv {
        b'd' | b'i' => {
            let n: i64 = match s.short {
                1 => raw as i8 as i64,
                2 => raw as i16 as i64,
                _ if !signed_ty && bytes >= 8 => return Ok(sign(s, false, raw.to_string())),
                _ if !signed_ty => raw as i64,
                _ if s.long64 || bytes >= 8 => raw as i64,
                _ => raw as i64,
            };
            return Ok(sign(s, n < 0, n.unsigned_abs().to_string()));
        }
        b'u' => masked.to_string(),
        b'x' => format!("{masked:x}"),
        b'X' => format!("{masked:X}"),
        b'o' => format!("{masked:o}"),
        _ => format!("{masked:b}"),
    };
    let mut t = text.into_bytes();
    if let Some(p) = s.prec
        && t.len() < p
    {
        let mut z = vec![b'0'; p - t.len()];
        z.extend_from_slice(&t);
        t = z;
    }
    if s.alt && masked != 0 {
        let prefix: &[u8] = match s.conv {
            b'x' => b"0x",
            b'X' => b"0X",
            b'o' => b"0",
            _ => b"",
        };
        let mut z = prefix.to_vec();
        z.extend_from_slice(&t);
        t = z;
    }
    Ok(t)
}

fn sign(s: &Spec, neg: bool, digits: String) -> Vec<u8> {
    let mut d = digits.into_bytes();
    if let Some(p) = s.prec
        && d.len() < p
    {
        let mut z = vec![b'0'; p - d.len()];
        z.extend_from_slice(&d);
        d = z;
    }
    let mut out = Vec::new();
    if neg {
        out.push(b'-');
    } else if s.plus {
        out.push(b'+');
    } else if s.space {
        out.push(b' ');
    }
    out.extend_from_slice(&d);
    out
}

fn pad(out: &mut Vec<u8>, s: &Spec, body: Vec<u8>) {
    let width = s.width.unwrap_or(0);
    if body.len() >= width {
        out.extend_from_slice(&body);
        return;
    }
    let fill = width - body.len();
    if s.left {
        out.extend_from_slice(&body);
        out.extend(std::iter::repeat_n(b' ', fill));
    } else if s.zero && !matches!(s.conv, b's' | b'c') {
        // Zeros go after the sign or `0x`.
        let split = match body.first() {
            Some(b'-' | b'+' | b' ') => 1,
            _ if body.starts_with(b"0x") || body.starts_with(b"0X") => 2,
            _ => 0,
        };
        out.extend_from_slice(&body[..split]);
        out.extend(std::iter::repeat_n(b'0', fill));
        out.extend_from_slice(&body[split..]);
    } else {
        out.extend(std::iter::repeat_n(b' ', fill));
        out.extend_from_slice(&body);
    }
}

fn float_body(s: &Spec, f: f64) -> String {
    let p = s.prec.unwrap_or(6);
    let neg = f.is_sign_negative() && f != 0.0;
    let a = f.abs();
    let upper = s.conv.is_ascii_uppercase();
    let mut t = if !a.is_finite() {
        if a.is_nan() { "nan".to_string() } else { "inf".to_string() }
    } else {
        match s.conv.to_ascii_lowercase() {
            b'f' => format!("{a:.p$}"),
            b'e' => exp_form(a, p),
            b'a' => format!("{a}"),
            _ => {
                let p = if p == 0 { 1 } else { p };
                if a == 0.0 {
                    "0".to_string()
                } else {
                    let x = a.log10().floor() as i32;
                    let (mut t, is_exp) = if x < -4 || x >= p as i32 {
                        (exp_form(a, p - 1), true)
                    } else {
                        (format!("{a:.*}", (p as i32 - 1 - x).max(0) as usize), false)
                    };
                    if !s.alt {
                        t = strip_zeros(&t, is_exp);
                    }
                    t
                }
            }
        }
    };
    if upper {
        t = t.to_uppercase();
    }
    let sign = if neg {
        "-"
    } else if s.plus {
        "+"
    } else if s.space {
        " "
    } else {
        ""
    };
    format!("{sign}{t}")
}

/// `d.dddddde+XX`.
fn exp_form(a: f64, p: usize) -> String {
    let s = format!("{a:.p$e}");
    match s.split_once('e') {
        Some((m, e)) => {
            let n: i32 = e.parse().unwrap_or(0);
            let sign = if n < 0 { '-' } else { '+' };
            format!("{m}e{sign}{:02}", n.abs())
        }
        None => s,
    }
}

fn strip_zeros(t: &str, is_exp: bool) -> String {
    let (m, e) = if is_exp {
        match t.split_once('e') {
            Some((m, e)) => (m.to_string(), format!("e{e}")),
            None => (t.to_string(), String::new()),
        }
    } else {
        (t.to_string(), String::new())
    };
    let m =
        if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.').to_string() } else { m };
    format!("{m}{e}")
}

/// One `SScanf` conversion's result.
pub enum Scanned {
    Int(i64),
    Float(f64),
    Text(Vec<u8>),
}

/// Scan `input` by `fmt`, returning the converted values in order.
pub fn scan(input: &[u8], fmt: &[u8]) -> Vec<Scanned> {
    let mut out = Vec::new();
    let (mut i, mut f) = (0usize, 0usize);
    while f < fmt.len() {
        let c = fmt[f];
        if c.is_ascii_whitespace() {
            while i < input.len() && input[i].is_ascii_whitespace() {
                i += 1;
            }
            f += 1;
            continue;
        }
        if c != b'%' {
            if input.get(i) != Some(&c) {
                break;
            }
            i += 1;
            f += 1;
            continue;
        }
        f += 1;
        if fmt.get(f) == Some(&b'%') {
            if input.get(i) != Some(&b'%') {
                break;
            }
            i += 1;
            f += 1;
            continue;
        }
        let start = f;
        while f < fmt.len() && fmt[f].is_ascii_digit() {
            f += 1;
        }
        let width: usize = std::str::from_utf8(&fmt[start..f])
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(usize::MAX);
        while f < fmt.len() && matches!(fmt[f], b'l' | b'L' | b'h' | b'I' | b'6' | b'4' | b'q') {
            f += 1;
        }
        let Some(&conv) = fmt.get(f) else { break };
        f += 1;
        if conv != b'c' {
            while i < input.len() && input[i].is_ascii_whitespace() {
                i += 1;
            }
        }
        let limit = |i: usize| i.saturating_add(width).min(input.len());
        match conv {
            b'd' | b'i' | b'u' => {
                let end = limit(i);
                let mut j = i;
                if j < end && (input[j] == b'-' || input[j] == b'+') {
                    j += 1;
                }
                while j < end && input[j].is_ascii_digit() {
                    j += 1;
                }
                let Some(n) =
                    std::str::from_utf8(&input[i..j]).ok().and_then(|t| t.parse::<i64>().ok())
                else {
                    break;
                };
                out.push(Scanned::Int(n));
                i = j;
            }
            b'x' | b'X' => {
                let end = limit(i);
                let mut j = i;
                if input[j..end].starts_with(b"0x") || input[j..end].starts_with(b"0X") {
                    j += 2;
                }
                let ds = j;
                while j < end && input[j].is_ascii_hexdigit() {
                    j += 1;
                }
                let Some(n) = std::str::from_utf8(&input[ds..j])
                    .ok()
                    .and_then(|t| u64::from_str_radix(t, 16).ok())
                else {
                    break;
                };
                out.push(Scanned::Int(n as i64));
                i = j;
            }
            b'o' => {
                let end = limit(i);
                let mut j = i;
                while j < end && (b'0'..=b'7').contains(&input[j]) {
                    j += 1;
                }
                let Some(n) = std::str::from_utf8(&input[i..j])
                    .ok()
                    .and_then(|t| i64::from_str_radix(t, 8).ok())
                else {
                    break;
                };
                out.push(Scanned::Int(n));
                i = j;
            }
            b'f' | b'e' | b'g' | b'E' | b'G' => {
                let end = limit(i);
                let mut j = i;
                while j < end && (input[j].is_ascii_digit() || b"+-.eE".contains(&input[j])) {
                    j += 1;
                }
                let Some(n) =
                    std::str::from_utf8(&input[i..j]).ok().and_then(|t| t.parse::<f64>().ok())
                else {
                    break;
                };
                out.push(Scanned::Float(n));
                i = j;
            }
            b's' => {
                let end = limit(i);
                let mut j = i;
                while j < end && !input[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j == i {
                    break;
                }
                out.push(Scanned::Text(input[i..j].to_vec()));
                i = j;
            }
            b'c' => {
                let Some(&ch) = input.get(i) else { break };
                out.push(Scanned::Int(ch as i64));
                i += 1;
            }
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_format_like_c() {
        let spec = |conv: u8, prec: Option<usize>| Spec {
            left: false,
            zero: false,
            plus: false,
            space: false,
            alt: false,
            width: None,
            prec,
            long64: false,
            short: 0,
            conv,
        };
        assert_eq!(float_body(&spec(b'f', None), 3.5), "3.500000");
        assert_eq!(float_body(&spec(b'g', None), 3.5), "3.5");
        assert_eq!(float_body(&spec(b'g', None), 1e10), "1e+10");
        assert_eq!(float_body(&spec(b'g', None), 0.0001), "0.0001");
        assert_eq!(float_body(&spec(b'e', Some(2)), 1234.5), "1.23e+03");
        assert_eq!(float_body(&spec(b'.', Some(2)), -0.5), "-0.5");
    }

    #[test]
    fn scanning_reads_numbers_and_hex() {
        let r = scan(b"#FF8000 12 3.5 word", b"#%02X%02X%02X %d %f %s");
        let ints: Vec<i64> = r
            .iter()
            .filter_map(|x| if let Scanned::Int(n) = x { Some(*n) } else { None })
            .collect();
        assert_eq!(ints, vec![0xFF, 0x80, 0x00, 12]);
        assert_eq!(r.len(), 6);
    }
}
