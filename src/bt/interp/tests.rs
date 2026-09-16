use super::*;
use crate::bt::preproc::{Source, preprocess};
use crate::bt::source::MemSource;
use crate::bt::tree::{ArrayKind, NodeKind, NodeRef, ROOT};

pub(crate) fn compile(src: &str) -> Arc<Program> {
    let root = Source { name: "t.bt".into(), path: None, text: src.as_bytes().to_vec() };
    Arc::new(crate::bt::parse::parse(preprocess(root, &mut |_, _| None).unwrap()).unwrap())
}

pub(crate) fn run(src: &str, data: &[u8]) -> (Interp, Option<(String, Pos)>) {
    let prog = compile(src);
    let mut it = Interp::new(prog, Box::new(MemSource(data.to_vec())), "test.bin", Limits::run());
    let err = it.run();
    (it, err)
}

/// The node at a dotted path of names from the root (`a.b[2].c`).
pub(crate) fn node(it: &mut Interp, path: &str) -> NodeRef {
    let prog = it.prog.clone();
    let mut cur = NodeRef::new(ROOT);
    for part in path.split('.') {
        let (name, idx) = match part.split_once('[') {
            Some((n, rest)) => (n, Some(rest.trim_end_matches(']').parse::<u64>().unwrap())),
            None => (part, None),
        };
        let sym = prog.syms.lookup(name).unwrap_or_else(|| panic!("no symbol {name}"));
        let t = it.member_target(cur, sym).unwrap().unwrap_or_else(|| panic!("no member {name}"));
        cur = match (t, idx) {
            (Target::Place(Place::Node(r)), None) => r,
            (Target::Place(Place::Node(r)), Some(i)) => it.tree.element(r, i).unwrap(),
            (Target::Dup(ids), Some(i)) => NodeRef::new(ids[i as usize]),
            (Target::Dup(ids), None) => NodeRef::new(*ids.last().unwrap()),
            other => panic!("unexpected {other:?}"),
        };
    }
    cur
}

pub(crate) fn int_at(it: &mut Interp, path: &str) -> i64 {
    let r = node(it, path);
    let v = it.node_value(r).unwrap();
    it.int_of(&v).unwrap()
}

fn out(it: &Interp) -> String {
    it.output.join("\n")
}

#[test]
fn scalars_in_both_byte_orders() {
    let (mut it, err) = run(
        "uint a; BigEndian(); ushort b; LittleEndian(); short c;",
        &[1, 0, 0, 0, 0x12, 0x34, 0xfe, 0xff],
    );
    assert!(err.is_none(), "{err:?}");
    assert_eq!(int_at(&mut it, "a"), 1);
    assert_eq!(int_at(&mut it, "b"), 0x1234);
    assert_eq!(int_at(&mut it, "c"), -2);
    assert_eq!(it.pos, 8);
}

#[test]
fn structs_see_their_own_fields_and_enclosing_ones() {
    let src = r#"
        typedef struct { string label; char data[length - Strlen(label) - 1]; } TEXT;
        typedef struct { uint length; char type[4]; if (type == "tEXt") TEXT text; } CHUNK;
        BigEndian();
        CHUNK chunk;
    "#;
    let mut data = vec![0, 0, 0, 6];
    data.extend_from_slice(b"tEXtab\0xyz");
    let (mut it, err) = run(src, &data);
    assert!(err.is_none(), "{err:?}");
    let r = node(&mut it, "chunk.text.data");
    assert_eq!(it.tree.node(r.id).size, 3);
    let v = it.node_value(r).unwrap();
    assert!(matches!(v, Value::Str(s) if s == b"xyz"));
}

#[test]
fn an_outer_struct_is_visible_by_path_while_it_is_built() {
    let src = "struct { struct { uchar n; } hdr; uchar items[file.hdr.n]; } file;";
    let (mut it, err) = run(src, &[3, 1, 2, 3]);
    assert!(err.is_none(), "{err:?}");
    let r = node(&mut it, "file.items");
    assert_eq!(it.tree.node(r.id).size, 3);
}

#[test]
fn duplicate_arrays_and_exists() {
    let src = r#"
        local int i;
        while (!FEof()) { uchar x; }
        Printf("%d %d %d %d\n", x, x[0], exists(x[3]), exists(x[4]));
    "#;
    let (it, err) = run(src, &[7, 8, 9, 10]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "10 7 1 0");
}

#[test]
fn elements_of_earlier_declarations_are_readable_while_later_ones_build() {
    let src = r#"
        typedef struct { uchar kind; if (kind == 2) uchar extra[chunk[0].kind]; } CHUNK;
        while (!FEof()) CHUNK chunk;
    "#;
    let (mut it, err) = run(src, &[3, 2, 0xaa, 0xbb, 0xcc]);
    assert!(err.is_none(), "{err:?}");
    let r = node(&mut it, "chunk[1].extra");
    assert_eq!(it.tree.node(r.id).size, 3);
}

#[test]
fn optimized_arrays_run_the_body_once_and_shift_the_rest() {
    let src = r#"
        local int runs = 0;
        typedef struct { runs++; ushort a; ushort b; } PAIR;
        PAIR pairs[4];
        Printf("%d %d", runs, pairs[2].b);
    "#;
    let data: Vec<u8> = (0..16).collect();
    let (mut it, err) = run(src, &data);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "1 2826");
    let r = node(&mut it, "pairs");
    assert!(matches!(
        it.tree.node(r.id).kind,
        NodeKind::Array { kind: ArrayKind::Optimized, count: 4, .. }
    ));
    assert_eq!(int_at(&mut it, "pairs[3].a"), 0x0d0c);
}

#[test]
fn variable_size_struct_arrays_are_parsed_in_full() {
    let src = r#"
        typedef struct { uchar len; uchar data[len]; } REC;
        REC recs[3];
        typedef struct { uchar len; uchar data[len]; } REC2;
        REC2 forced[2] <optimize=true>;
    "#;
    let data = [1, 0xaa, 2, 0xbb, 0xcc, 0, 1, 9, 1, 8];
    let (mut it, err) = run(src, &data);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(int_at(&mut it, "recs[1].len"), 2);
    assert_eq!(int_at(&mut it, "recs[2].len"), 0);
    let r = node(&mut it, "forced");
    assert!(matches!(it.tree.node(r.id).kind, NodeKind::Array { kind: ArrayKind::Optimized, .. }));
}

#[test]
fn unions_take_the_largest_member() {
    let (mut it, err) =
        run("union { ushort s; uint i; uchar b; } u; uchar after;", &[1, 2, 3, 4, 5]);
    assert!(err.is_none(), "{err:?}");
    let r = node(&mut it, "u");
    assert_eq!(it.tree.node(r.id).size, 4);
    assert_eq!(int_at(&mut it, "u.s"), 0x0201);
    assert_eq!(int_at(&mut it, "after"), 5);
}

#[test]
fn padded_bitfields_follow_the_manual() {
    // Little endian: cccccbbb bbbbaaaa stored as bbbbaaaa cccccbbb.
    let src = "ushort a : 4; ushort b : 7; ushort c : 5;";
    let (mut it, err) = run(src, &[0b1010_0101, 0b1100_1110]);
    assert!(err.is_none(), "{err:?}");
    let word = 0b1100_1110_1010_0101u16;
    assert_eq!(int_at(&mut it, "a"), (word & 0xf) as i64);
    assert_eq!(int_at(&mut it, "b"), ((word >> 4) & 0x7f) as i64);
    assert_eq!(int_at(&mut it, "c"), (word >> 11) as i64);
    // Big endian: aaaabbbb bbbccccc.
    let (mut it, err) = run(format!("BigEndian(); {src}").as_str(), &[0b1010_0101, 0b1100_1110]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(int_at(&mut it, "a"), 0b1010);
    assert_eq!(int_at(&mut it, "b"), 0b010_1110);
    assert_eq!(int_at(&mut it, "c"), 0b01110);
    // A width that doesn't fit starts a new unit; a type change does too.
    let (mut it, err) = run(
        "uint apple:10; uint orange:20; uint banana:10; uint peach:12; ushort grape:4;",
        &[0xff; 10],
    );
    assert!(err.is_none(), "{err:?}");
    let banana = node(&mut it, "banana");
    let grape = node(&mut it, "grape");
    assert_eq!(it.tree.node(banana.id).start, 4);
    assert_eq!(it.tree.node(grape.id).start, 8);
    assert_eq!(it.pos, 10);
}

#[test]
fn unpadded_bitfields_are_a_bit_stream() {
    let src = "BitfieldDisablePadding(); BitfieldLeftToRight(); ushort a : 10; uint b : 20; ushort c : 10; uchar tail;";
    // aaaaaaaa aabbbbbb bbbbbbbb bbbbbbcc cccccccc
    let data = [0b1111_1111, 0b1100_0000, 0, 0b0000_0011, 0b1111_1111, 0x42];
    let (mut it, err) = run(src, &data);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(int_at(&mut it, "a"), 0x3ff);
    assert_eq!(int_at(&mut it, "b"), 0);
    assert_eq!(int_at(&mut it, "c"), 0x3ff);
    assert_eq!(int_at(&mut it, "tail"), 0x42);
}

#[test]
fn enums_count_up_and_name_their_values() {
    let src = r#"
        enum <uchar> KIND { A, B = 5, C } k;
        Printf("%s %d %s", EnumToString(k), C, EnumToString(C));
    "#;
    let (it, err) = run(src, &[6]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "C 6 C");
}

#[test]
fn sizeof_works_on_simple_structs_only() {
    let src = r#"
        typedef struct { uint a; ushort b[3]; uchar c : 4; uchar d : 4; } FIXED;
        typedef struct { uchar n; uchar d[n]; } VARIABLE;
        Printf("%d", sizeof(FIXED));
        Printf(" %d", sizeof(VARIABLE));
    "#;
    let (it, err) = run(src, &[]);
    assert_eq!(out(&it), "11");
    assert!(err.is_some_and(|(m, _)| m.contains("variable-size")));
}

#[test]
fn on_demand_structs_wait_until_needed() {
    let src = r#"
        local int runs = 0;
        typedef struct { runs++; uint v; } LAZY <size=4>;
        LAZY l;
        uchar after;
        Printf("%d ", runs);
        Printf("%d %d", l.v, runs);
    "#;
    let (mut it, err) = run(src, &[1, 0, 0, 0, 9]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "0 1 1");
    assert_eq!(int_at(&mut it, "after"), 9);
}

#[test]
fn functions_take_references_and_recurse() {
    let src = r#"
        void bump(int &x) { x += 1; }
        int fact(int n) { if (n <= 1) return 1; return n * fact(n - 1); }
        string twice(string s) { return s + s; }
        local int v = 4;
        bump(v);
        Printf("%d %d %s", v, fact(5), twice("ab"));
    "#;
    let (it, err) = run(src, &[]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "5 120 abab");
}

#[test]
fn switch_falls_through_and_return_stops() {
    let src = r#"
        local int i, n = 0;
        for (i = 0; i < 4; i++) {
            switch (i) { case 0: n += 1; case 1: n += 10; break; default: n += 100; }
        }
        Printf("%d", n);
        return;
        Printf("never");
    "#;
    let (it, err) = run(src, &[]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "221");
}

#[test]
fn strings_compare_concatenate_and_index() {
    let src = r#"
        char magic[2];
        local string s = "x";
        s += magic;
        s[0] = 'Y';
        local char buf[16];
        SPrintf(buf, "%04X-%s", 255, s);
        Printf("%d %s %c %s %d", magic == "BM", s, s[1], buf, Strlen(buf));
    "#;
    let (it, err) = run(src, b"BM");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "1 YBM B 00FF-YBM 8");
}

#[test]
fn integer_arithmetic_wraps_like_c() {
    let src = r#"
        local uchar b = 250;
        b += 10;
        local int neg = -1;
        local uint big = 0xFFFFFFFF;
        Printf("%d %d %u %x %d", b, neg >> 1, big + 1, -1, 7 / 2);
    "#;
    let (it, err) = run(src, &[]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "4 -1 0 ffffffff 3");
}

#[test]
fn colors_stamp_later_declarations_and_attributes_override() {
    let src = "SetBackColor(cRed); uchar a; uchar b <bgcolor=cBlue>; SetBackColor(cNone); uchar c;";
    let (mut it, err) = run(src, &[1, 2, 3]);
    assert!(err.is_none(), "{err:?}");
    let a = node(&mut it, "a");
    let b = node(&mut it, "b");
    let c = node(&mut it, "c");
    assert_eq!(it.tree.node(a.id).bg, 0x0000ff);
    assert_eq!(it.tree.node(b.id).bg, 0xff0000);
    assert_eq!(it.tree.node(c.id).bg, crate::bt::tree::NO_COLOR);
    assert_eq!(
        it.tree.colors_in(0, 3).iter().map(|c| c.1).collect::<Vec<_>>(),
        vec![0xff, 0xff0000, crate::bt::tree::NO_COLOR]
    );
}

#[test]
fn past_the_end_is_an_error_that_keeps_the_tree() {
    let (mut it, err) = run("uchar a; uint b;", &[1, 2]);
    assert!(err.is_some_and(|(m, _)| m.contains("past the end")));
    assert_eq!(int_at(&mut it, "a"), 1);
}

#[test]
fn runaway_recursion_is_an_error_not_a_crash() {
    let run_big = |src: &'static str| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || run(src, &[]).1)
            .unwrap()
            .join()
            .unwrap()
    };
    let err = run_big("int f(int n) { return f(n + 1); } f(0);");
    assert!(err.is_some_and(|(m, _)| m.contains("deeply")));
    let err = run_big("struct S; struct S { S inner; } s;");
    assert!(err.is_some());
}

#[test]
fn cancelling_stops_a_long_run() {
    let prog = compile("local int i; for (;;) i++;");
    let mut it = Interp::new(prog, Box::new(MemSource(Vec::new())), "x", Limits::run());
    let cancel = Arc::new(AtomicBool::new(true));
    it.set_cancel(cancel, Arc::new(AtomicU64::new(0)));
    assert!(it.run().is_some_and(|(m, _)| m == "cancelled"));
}

#[test]
fn checksums_and_search() {
    let src = r#"
        Printf("%08X %d %d", Checksum(CHECKSUM_CRC32), FindFirst("lo"), FindAll("l").count);
    "#;
    let (it, err) = run(src, b"hello world");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), format!("{:08X} 3 3", crc32fast::hash(b"hello world")));
}

#[test]
fn struct_locals_are_reachable_by_path() {
    let src = r#"
        struct { local uint count = 0; uchar a; count = a; } header;
        void touch() { header.count += 1; }
        touch();
        Printf("%d", header.count);
    "#;
    let (it, err) = run(src, &[41]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "42");
}

#[test]
fn a_typedef_completes_an_earlier_struct_reference() {
    let src = r#"
        typedef struct (int level) {
            uchar n;
            if (level < 2 && n) struct NODE child(level + 1);
        } NODE;
        NODE root(0);
    "#;
    let (mut it, err) = run(src, &[1, 1, 0]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(int_at(&mut it, "root.child.child.n"), 0);
}

#[test]
fn regex_and_wildcard_searches() {
    let src = r#"
        local TFindResults r = FindAll("(?<=[\\n\\000])obj \\d+", true, false, FINDMETHOD_REGEX);
        local TFindResults w = FindAll("o?j*9", true, false, FINDMETHOD_WILDCARDS);
        Printf("%d %d %d %d", r.count, r.start[0], r.size[0], w.count);
    "#;
    let (it, err) = run(src, b"xobj 1\nobj 22\0obj 9");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "2 7 6 1");
}

#[test]
fn bitfields_read_unsigned() {
    let (mut it, err) = run("char flag : 2; char rest : 6;", &[0b0000_0010]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(int_at(&mut it, "flag"), 2);
}

#[test]
fn callbacks_on_an_array_of_structs_belong_to_its_elements() {
    let src = r#"
        typedef struct { uchar tag; } T;
        string ReadT(T &t) { return Str("tag %d", t.tag); }
        T items[2] <read=ReadT>;
    "#;
    let (mut it, err) = run(src, &[5, 6]);
    assert!(err.is_none(), "{err:?}");
    let mut cache = display::LazyCache::default();
    let arr = node(&mut it, "items");
    assert_eq!(it.row_value(&mut cache, arr), "");
    let e1 = it.tree.element(arr, 1).unwrap();
    assert_eq!(it.row_value(&mut cache, e1), "tag 6");
}

#[test]
fn a_leading_lookbehind_finds_the_same_matches_either_way() {
    // The first is rewritten for the fast engine; the lookahead keeps the
    // second on fancy-regex. Both must agree with 010 Editor's meaning.
    let src = r#"
        local TFindResults a = FindAll("(?<=[\\n ])obj", true, false, FINDMETHOD_REGEX);
        local TFindResults b = FindAll("(?<=[\\n ])obj(?=\\d)", true, false, FINDMETHOD_REGEX);
        Printf("%d %d %d %d %d", a.count, a.start[0], a.start[1], b.count, b.start[0]);
    "#;
    let (it, err) = run(src, b"xobj obj\nobj1");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(out(&it), "2 5 9 1 9");
}

#[test]
fn byte_arrays_show_as_text_or_hex() {
    let (mut it, err) = run("char magic[4]; uchar pad[3]; uchar name[5];", b"\x7fELF\0\0\0abc\0\0");
    assert!(err.is_none(), "{err:?}");
    let mut cache = display::LazyCache::default();
    let magic = node(&mut it, "magic");
    let pad = node(&mut it, "pad");
    let name = node(&mut it, "name");
    assert_eq!(it.row_value(&mut cache, magic), "\"\\x7FELF\"");
    assert_eq!(it.row_value(&mut cache, pad), "00 00 00");
    assert_eq!(it.row_value(&mut cache, name), "\"abc\"");
}

#[test]
fn a_byte_is_named_by_the_path_to_its_field() {
    let src = r#"
        struct ENTRY { ushort id; uint crc; };
        struct HEADER { char magic[4]; ENTRY entries[3]; } header;
        uchar pad;
        uchar pad;
    "#;
    let (mut it, err) = run(src, &[0u8; 32]);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(it.field_path(1), "header.magic[1]");
    assert_eq!(it.field_path(4), "header.entries[0].id");
    assert_eq!(it.field_path(16), "header.entries[2].id");
    assert_eq!(it.field_path(19), "header.entries[2].crc");
    assert_eq!(it.field_path(23), "pad[1]");
    assert_eq!(it.field_path(30), "", "no variable covers it");
}
