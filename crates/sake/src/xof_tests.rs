use super::*;
use proptest::prelude::*;

const HEADER: &[u8] = b"template Header {\n<3D82AB43-62DA-11CF-AB39-0020AF71E433>\nWORD major;\nWORD minor;\nDWORD flags;\n}\n";
const BUFFER: &[u8] = b"template Buffer {\n<3D82AB43-62DA-11CF-AB39-0020AF71E433>\nDWORD num_elem;\narray DWORD value[num_elem];\nDWORD dummy;\n}\n";

fn txt(body: &[u8]) -> B {
    [b"xof 0302txt 0032\n".as_slice(), body].concat()
}
fn bin(body: &[u8]) -> B {
    [b"xof 0302bin 0032".as_slice(), body].concat()
}

fn reg(templates: &[u8]) -> Templates {
    let mut r = Templates::default();
    register(&txt(templates), &mut r).unwrap();
    r
}

/// The first object's data, or the error.
fn first(r: &mut Templates, file: &[u8]) -> Result<Node, u32> {
    let mut f = XFile::open(file, r, 4)?;
    let i = f.next_top(r)?.ok_or(NOMOREOBJECTS)?;
    Ok(f.nodes[i].clone())
}

fn dwords(d: &[u8]) -> Vec<u32> {
    d.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

#[test]
fn headers() {
    assert_eq!(header(b"xof 0302txt 0064"), Ok(Header { binary: false, compressed: false, float64: true }));
    assert_eq!(header(b"xof 0303bzip0032"), Ok(Header { binary: true, compressed: true, float64: false }));
    assert_eq!(header(b"xOf 0302txt 0064"), Err(BADFILETYPE));
    assert_eq!(header(b"xof 0102txt 0064"), Err(BADFILEVERSION));
    assert_eq!(header(b"xof 0302foo 0064"), Err(BADFILETYPE));
    assert_eq!(header(b"xof 0302cmp 0064"), Err(BADFILETYPE));
    assert_eq!(header(b"xof 0302txt 0050"), Err(BADFILEFLOATSIZE));
    assert_eq!(header(b"xof 0302txt 006"), Err(BADFILETYPE));
}

#[test]
fn guids_are_little_endian_fields() {
    let g = parse_guid(b"3D82AB43-62DA-11CF-AB39-0020AF71E433").unwrap();
    assert_eq!(g, [0x43, 0xab, 0x82, 0x3d, 0xda, 0x62, 0xcf, 0x11, 0xab, 0x39, 0x00, 0x20, 0xaf, 0x71, 0xe4, 0x33]);
    assert_eq!(parse_guid(b"3D82AB43-62DA-11CF-AB39-0020AF71E43"), None);
    assert_eq!(parse_guid(b"3D82AB43x62DA-11CF-AB39-0020AF71E433"), None);
}

#[test]
fn header_object_and_members() {
    let mut r = reg(HEADER);
    let n = first(&mut r, &txt(b"Header Object {\n1; 2; 3;\n}\n")).unwrap();
    assert_eq!(n.data, [1, 0, 2, 0, 3, 0, 0, 0]);
    assert_eq!(n.name.as_deref(), Some(&b"Object"[..]));
    assert_eq!(n.members, vec![(b"major".to_vec(), 0, 2), (b"minor".to_vec(), 2, 2), (b"flags".to_vec(), 4, 4)]);
    let n = first(&mut r, &txt(b"Header {\n1; 2; 3;\n}\n")).unwrap();
    assert_eq!(n.name, None);
}

#[test]
fn register_errors() {
    let mut r = Templates::default();
    assert_eq!(register(&txt(b"foobar;\n"), &mut r), Err(PARSEERROR));
    assert_eq!(register(b"xof 0302txt 0064", &mut r), Ok(()));
    assert_eq!(register(b"xof 0302bin 0064", &mut r), Ok(()));
    // (an object is not a template; a failed file adds none of its templates)
    assert_eq!(register(&txt(&[HEADER, b"Header { 1; 2; 3; }"].concat()), &mut r), Err(PARSEERROR));
    assert!(r.list.is_empty());
}

#[test]
fn index_color_is_built_in_and_case_matters() {
    let mut r = Templates::default();
    let lower = b"template MeshVertexColors {\n<1630B821-7842-11cf-8F52-0040333594A3>\nDWORD nVertexColors;\narray indexColor vertexColors[nVertexColors];\n}\n";
    assert_eq!(register(&txt(lower), &mut r), Ok(()));
    let upper = b"template MeshVertexColors2 {\n<1630B821-7842-11cf-8F52-0040333594A3>\nDWORD nVertexColors;\narray IndexColor vertexColors[nVertexColors];\n}\n";
    assert_eq!(register(&txt(upper), &mut r), Err(PARSEERROR));
    let n = first(&mut r, &txt(b"MeshVertexColors { 1; 7; 0.5; 0.25; 1.0; 1.0;;; }")).unwrap();
    assert_eq!(n.data.len(), 4 + 20);
}

const SYNTAX: &[(&[u8], bool)] = &[
    (b"3;\n0; 1; 2;\n5;\n", true),
    (b"3,\n0, 1, 2,\n5,\n", true),
    (b"3;;;;;\n0;;;;; 1;;;;; 2;;;;;\n5;;;;;\n", true),
    (b"3;\n0, 1,, 2;\n5;\n", false),
    (b"3;;;;;,\n0;;;;;, 1;;;;;, 2;;;;;,\n5;;;;;,\n", true),
    (b"3;\n0, 1,; 2;\n5;\n", false),
    (b"3;\n0, 1, 2;\n5\n", false),
    (b"3;\n0 1 2;\n5;\n", false),
];

/// Wine's test_syntax_semicolon_comma (what Windows takes), text.
#[test]
fn separators_text() {
    let mut r = reg(BUFFER);
    for (body, ok) in SYNTAX {
        let file = txt(&[b"Buffer {\n".as_slice(), body, b"}\n"].concat());
        let got = first(&mut r, &file).map(|n| dwords(&n.data));
        if *ok {
            assert_eq!(got, Ok(vec![3, 0, 1, 2, 5]), "{}", String::from_utf8_lossy(body));
        } else {
            assert_eq!(got, Err(PARSEERROR), "{}", String::from_utf8_lossy(body));
        }
    }
}

fn btok(c: u16) -> B {
    c.to_le_bytes().to_vec()
}
fn bint(v: u32) -> B {
    [btok(3), v.to_le_bytes().to_vec()].concat()
}
fn bname(s: &[u8]) -> B {
    [btok(1), (s.len() as u32).to_le_bytes().to_vec(), s.to_vec()].concat()
}
fn blist(v: &[u32]) -> B {
    let mut b = [btok(6), (v.len() as u32).to_le_bytes().to_vec()].concat();
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
    b
}

/// The same in binary, and binary integer lists (no separators after their values).
#[test]
fn separators_binary() {
    let mut r = reg(BUFFER);
    let (semi, comma) = (btok(20), btok(19));
    let obj = |body: B| bin(&[bname(b"Buffer"), btok(10), body, btok(11)].concat());
    let ok = |r: &mut Templates, b: B| first(r, &obj(b)).map(|n| dwords(&n.data));
    let good = Ok(vec![3, 0, 1, 2, 5]);
    assert_eq!(ok(&mut r, [bint(3), semi.clone(), bint(0), semi.clone(), bint(1), semi.clone(), bint(2), semi.clone(), bint(5), semi.clone()].concat()), good);
    assert_eq!(ok(&mut r, [bint(3), comma.clone(), bint(0), comma.clone(), bint(1), comma.clone(), bint(2), comma.clone(), bint(5), comma.clone()].concat()), good);
    assert_eq!(ok(&mut r, [bint(3), semi.clone(), bint(0), bint(1), bint(2), semi.clone(), bint(5), semi.clone()].concat()), Err(PARSEERROR));
    assert_eq!(ok(&mut r, [bint(3), semi.clone(), blist(&[0, 1, 2]), bint(5), semi.clone()].concat()), good);
    assert_eq!(ok(&mut r, blist(&[3, 0, 1, 2, 5])), good);
    assert_eq!(ok(&mut r, [blist(&[3, 0, 1, 2, 5]), semi.clone()].concat()), Err(PARSEERROR));
    assert_eq!(ok(&mut r, [blist(&[3, 0, 1, 2, 5]), comma.clone()].concat()), Err(PARSEERROR));
}

#[test]
fn strings() {
    let mut r = reg(b"template Filename {\n<3D82AB43-62DA-11CF-AB39-0020AF71E433>\nSTRING filename;\n}\n");
    let n = first(&mut r, &txt(b"Filename {\n\"foo;bar\";\n}\n")).unwrap();
    assert_eq!((n.data.len(), n.strs.clone()), (4, vec![(0, b"foo;bar".to_vec())]));
    let s = [btok(2), 6u32.to_le_bytes().to_vec(), b"foobar".to_vec(), btok(20)].concat();
    let n = first(&mut r, &bin(&[bname(b"Filename"), btok(10), s, btok(11)].concat())).unwrap();
    assert_eq!(n.strs, vec![(0, b"foobar".to_vec())]);
    // (8-byte pointers on x64)
    let mut f = XFile::open(&txt(b"Filename { \"a\"; }"), &mut r, 8).unwrap();
    let i = f.next_top(&mut r).unwrap().unwrap();
    assert_eq!(f.nodes[i].data.len(), 8);
}

const COMPLEX: &[u8] = b"template Vector {\n<3D82AB5E-62DA-11CF-AB39-0020AF71E433>\nFLOAT x;\nFLOAT y;\nFLOAT z;\n}\n\
template MeshFace {\n<3D82AB5F-62DA-11CF-AB39-0020AF71E433>\nDWORD nFaceVertexIndices;\narray DWORD faceVertexIndices[nFaceVertexIndices];\n}\n\
template Mesh {\n<3D82AB44-62DA-11CF-AB39-0020AF71E433>\nDWORD nVertices;\narray Vector vertices[nVertices];\nDWORD nFaces;\narray MeshFace faces[nFaces];\n[...]\n}\n";

#[test]
fn complex_object_and_child() {
    let mut r = reg(COMPLEX);
    let file = txt(b"Mesh Object\n{\n4;;;,\n1.0;;;, 0.0;;;, 0.0;;;,\n0.0;;;, 1.0;;;, 0.0;;;,\n0.0;;;, 0.0;;;, 1.0;;;,\n1.0;;;, 1.0;;;, 1.0;;;,\n3;;;,\n3;;;, 0;;;, 1;;;, 2;;;,\n3;;;, 1;;;, 2;;;, 3;;;,\n3;;;, 3;;;, 1;;;, 2;;;,\nVector{0.0;;;, 0.0;;;, 0.0;;;}\n}\n");
    let mut f = XFile::open(&file, &mut r, 4).unwrap();
    let i = f.next_top(&mut r).unwrap().unwrap();
    assert_eq!(f.nodes[i].data.len(), 104);
    assert_eq!(f.nodes[i].children.len(), 1);
    let Child::Obj(c) = f.nodes[i].children[0] else { panic!() };
    assert_eq!(f.nodes[c].data, [0u8; 12]);
    assert_eq!(f.next_top(&mut r), Ok(None));
}

#[test]
fn children_follow_the_restriction() {
    let mut r = reg(&[COMPLEX, b"template Closed {\n<11111111-62DA-11CF-AB39-0020AF71E433>\nDWORD a;\n}\n\
template Only {\n<22222222-62DA-11CF-AB39-0020AF71E433>\nDWORD a;\n[Vector <3D82AB5E-62DA-11CF-AB39-0020AF71E433>]\n}\n"]
        .concat());
    assert!(first(&mut r, &txt(b"Only { 1; Vector { 1;2;3; } }")).is_ok());
    assert_eq!(first(&mut r, &txt(b"Only { 1; Closed { 1; } }")), Err(PARSEERROR));
    assert_eq!(first(&mut r, &txt(b"Closed { 1; Vector { 1;2;3; } }")), Err(PARSEERROR));
    // references: by name, by GUID; resolved by find, reading on
    let mut f = XFile::open(&txt(b"Only { 1; { Later } }\nVector Later { 1;2;3; }"), &mut r, 4).unwrap();
    let i = f.next_top(&mut r).unwrap().unwrap();
    assert_eq!(f.nodes[i].children, vec![Child::Ref { name: Some(b"Later".to_vec()), guid: None }]);
    let k = f.find(&mut r, Some(b"Later"), None).unwrap();
    assert_eq!(f.nodes[k].tpl, b"Vector");
    assert_eq!(f.find(&mut r, Some(b"Nothing"), None), None);
    // (what find read ahead GetNextDataObject still hands out, in order)
    assert_eq!(f.next_top(&mut r), Ok(Some(k)));
    assert_eq!(f.next_top(&mut r), Ok(None));
}

/// Garbage after the objects (Wine's multi-comma test passes a longer size than its buffer):
/// CreateEnumObject takes the file, the objects before it read, then the error.
#[test]
fn errors_come_when_read() {
    let mut r = reg(HEADER);
    let mut f = XFile::open(&txt(b"Header { 1;2;3; }\n\"unterminated"), &mut r, 4).unwrap();
    assert!(f.next_top(&mut r).unwrap().is_some());
    assert_eq!(f.next_top(&mut r), Err(PARSEERROR));
    assert_eq!(register(&txt(b"template X { <3D82AB43-62DA-11CF-AB39-0020AF71E433> DWORD a; } \""), &mut r), Err(PARSEERROR));
    let mut f = XFile::open(&bin(&[0xff, 0x7f]), &mut r, 4).unwrap();
    assert_eq!(f.next_top(&mut r), Err(PARSEERROR));
}

#[test]
fn unknown_template_and_errors_stick() {
    let mut r = reg(HEADER);
    let mut f = XFile::open(&txt(b"Header { 1;2;3; }\nNoSuch { 1; }\nHeader { 1;2;3; }"), &mut r, 4).unwrap();
    assert!(f.next_top(&mut r).unwrap().is_some());
    assert_eq!(f.next_top(&mut r), Err(PARSEERROR));
    assert_eq!(f.next_top(&mut r), Err(PARSEERROR));
}

#[test]
fn comments_and_templates_between_objects() {
    let mut r = Templates::default();
    let file = txt(&[b"// a comment\n# another\n".as_slice(), HEADER, b"Header A { 1;2;3; } // trailing\n", BUFFER, b"Buffer { 0;; 9; }"].concat());
    let mut f = XFile::open(&file, &mut r, 4).unwrap();
    assert!(f.next_top(&mut r).unwrap().is_some());
    let i = f.next_top(&mut r).unwrap().unwrap();
    assert_eq!(dwords(&f.nodes[i].data), [0, 9]);
    assert!(r.find(b"Buffer").is_some());
}

/// MSZIP bodies: Wine's test files (made with MAKECAB) decompress to the plain ones.
#[test]
fn compressed() {
    let t = b"xof 0302tzip0064\x71\x00\x00\x00\x61\x00\x5a\x00\
\x43\x4B\x2B\x49\xCD\x2D\xC8\x49\x2C\x49\x55\xF0\x48\x4D\x4C\x49\
\x2D\xE2\xAA\xE6\xB2\x31\x76\xB1\x30\x72\x74\x32\x31\xD6\x35\x33\
\x72\x71\xD4\x35\x34\x74\x76\xD3\x75\x74\x32\xB6\xD4\x35\x30\x30\
\x32\x70\x74\x33\x37\x74\x35\x31\x36\xB6\xE3\x0A\xF7\x0F\x72\x51\
\xC8\x4D\xCC\xCA\x2F\xB2\x86\xB2\x33\xF3\x40\x6C\x17\x30\x27\x2D\
\x27\x31\xBD\xD8\x9A\xAB\x96\x8B\x0B\x00";
    let o = b"xof 0302tzip0064\x2c\x00\x00\x00\x1c\x00\x20\x00\
\x43\x4b\xf3\x48\x4d\x4c\x49\x2d\x52\xf0\x4f\xca\x4a\x4d\x2e\xe1\
\xaa\xe6\x32\xb4\x56\x30\xb2\x56\x30\xb6\xe6\xaa\xe5\xe2\x02\x00";
    let mut r = Templates::default();
    register(t, &mut r).unwrap();
    assert_eq!(first(&mut r, o).unwrap().data, [1, 0, 2, 0, 3, 0, 0, 0]);
    // "\n" alone, text and binary (one byte: no token)
    assert_eq!(tokens(b"xof 0302tzip0064\x11\x00\x00\x00\x01\x00\x05\x00\x43\x4b\xe3\x02\x00"), Ok((vec![], None)));
    assert_eq!(tokens(b"xof 0302bzip0064\x11\x00\x00\x00\x01\x00\x05\x00\x43\x4b\xe3\x02\x00"), Ok((vec![], None)));
}

/// Several MSZIP blocks, each referring back into the ones before it.
#[test]
fn compressed_blocks_share_history() {
    let plain: B = (0..100_000u32).map(|i| b"template Header "[(i % 16) as usize]).collect();
    let z = zip(&plain, 32768);
    assert!(z.len() < plain.len() / 10);
    assert_eq!(unzip(&z[16..]), Ok(plain));
}

/// MAKECAB's layout: 32 KB blocks of raw deflate, each with the last 32 KB as its dictionary.
fn zip(plain: &[u8], block: usize) -> B {
    use miniz_oxide::deflate::core::{compress, create_comp_flags_from_zip_params, CompressorOxide, TDEFLFlush};
    let mut f = b"xof 0302tzip0032".to_vec();
    f.extend_from_slice(&((plain.len() + 16) as u32).to_le_bytes());
    let mut c = CompressorOxide::new(create_comp_flags_from_zip_params(6, -15, 0));
    for chunk in plain.chunks(block) {
        let mut out = vec![0u8; chunk.len() * 2 + 64];
        let (_, _, n) = compress(&mut c, chunk, &mut out, TDEFLFlush::Sync);
        // (a sync flush ends on a byte with the dictionary kept; an MSZIP block is a whole
        // deflate stream: an empty final block ends it)
        let mut out = out[..n].to_vec();
        out.extend_from_slice(&[0x03, 0x00]);
        f.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        f.extend_from_slice(&((out.len() + 2) as u16).to_le_bytes());
        f.extend_from_slice(b"CK");
        f.extend_from_slice(&out);
    }
    f
}

// ---- property tests ----

/// A member of a generated template: a primitive, alone or as an array (fixed or sized by the
/// DWORD before it).
#[derive(Clone, Debug)]
enum Gen {
    One(Prim),
    Fixed(Prim, u32),
    Counted(Prim),
}

#[derive(Clone, Debug)]
enum Val {
    I(u32),
    F(f32),
    S(B),
}

fn prim() -> impl Strategy<Value = Prim> {
    prop_oneof![Just(Prim::Word), Just(Prim::Dword), Just(Prim::Float), Just(Prim::Double), Just(Prim::Char), Just(Prim::Lpstr), Just(Prim::Sdword)]
}

fn val(p: Prim) -> BoxedStrategy<Val> {
    match p {
        Prim::Word => (0u32..=0xffff).prop_map(Val::I).boxed(),
        Prim::Char => (0u32..=0xff).prop_map(Val::I).boxed(),
        Prim::Dword | Prim::Sdword => any::<u32>().prop_map(Val::I).boxed(),
        Prim::Float | Prim::Double => (-1.0e6f32..1.0e6).prop_map(Val::F).boxed(),
        _ => proptest::collection::vec(prop_oneof![b'a'..=b'z', Just(b';'), Just(b' '), Just(b','), 0x81u8..=0xfc], 0..12).prop_map(Val::S).boxed(),
    }
}

fn member() -> impl Strategy<Value = (Gen, Vec<Val>)> {
    prim().prop_flat_map(|p| {
        prop_oneof![
            val(p).prop_map(move |v| (Gen::One(p), vec![v])),
            proptest::collection::vec(val(p), 1..5).prop_map(move |v| (Gen::Fixed(p, v.len() as u32), v)),
            proptest::collection::vec(val(p), 0..5).prop_map(move |v| (Gen::Counted(p), v)),
        ]
    })
}

fn kw(p: Prim) -> &'static str {
    match p {
        Prim::Word => "WORD",
        Prim::Dword => "DWORD",
        Prim::Sdword => "SDWORD",
        Prim::Float => "FLOAT",
        Prim::Double => "DOUBLE",
        Prim::Char => "CHAR",
        _ => "STRING",
    }
}

fn template_text(ms: &[(Gen, Vec<Val>)]) -> B {
    let mut s = String::from("template T {\n<10203040-5060-7080-90A0-B0C0D0E0F000>\n");
    for (i, (g, _)) in ms.iter().enumerate() {
        match g {
            Gen::One(p) => s += &format!("{} m{i};\n", kw(*p)),
            Gen::Fixed(p, n) => s += &format!("array {} m{i}[{n}];\n", kw(*p)),
            Gen::Counted(p) => s += &format!("DWORD n{i};\narray {} m{i}[n{i}];\n", kw(*p)),
        }
    }
    s += "}\n";
    s.into_bytes()
}

/// What GetData must hold: the values packed, strings as 4-byte slots.
fn packed(ms: &[(Gen, Vec<Val>)]) -> (B, Vec<(usize, B)>) {
    let mut d = Vec::new();
    let mut strs = Vec::new();
    for (g, vs) in ms {
        let p = match g {
            Gen::Counted(p) => {
                d.extend_from_slice(&(vs.len() as u32).to_le_bytes());
                *p
            }
            Gen::One(p) | Gen::Fixed(p, _) => *p,
        };
        for v in vs {
            match (p, v) {
                (Prim::Word, Val::I(x)) => d.extend_from_slice(&(*x as u16).to_le_bytes()),
                (Prim::Char, Val::I(x)) => d.push(*x as u8),
                (Prim::Dword | Prim::Sdword, Val::I(x)) => d.extend_from_slice(&x.to_le_bytes()),
                (Prim::Float, Val::F(f)) => d.extend_from_slice(&f.to_le_bytes()),
                (Prim::Double, Val::F(f)) => d.extend_from_slice(&(*f as f64).to_le_bytes()),
                (_, Val::S(s)) => {
                    strs.push((d.len(), s.clone()));
                    d.extend_from_slice(&[0; 4]);
                }
                _ => unreachable!(),
            }
        }
    }
    (d, strs)
}

/// The object as text: each value then a separator from `seps` (cycled); string bytes raw.
fn object_text(ms: &[(Gen, Vec<Val>)], seps: &[&str]) -> B {
    object_text_raw(ms, seps)
}

fn object_text_raw(ms: &[(Gen, Vec<Val>)], seps: &[&str]) -> B {
    let mut b = b"T obj {\n".to_vec();
    let mut k = 0;
    for (g, vs) in ms {
        if let Gen::Counted(_) = g {
            k += 1;
            b.extend_from_slice(format!("{}{}", vs.len(), seps[k % seps.len()]).as_bytes());
        }
        for v in vs {
            k += 1;
            match v {
                Val::I(x) => b.extend_from_slice(x.to_string().as_bytes()),
                Val::F(f) => b.extend_from_slice(format!("{:?}", *f as f64).as_bytes()),
                Val::S(s) => {
                    b.push(b'"');
                    b.extend_from_slice(s);
                    b.push(b'"');
                }
            }
            b.extend_from_slice(seps[k % seps.len()].as_bytes());
        }
    }
    b.extend_from_slice(b"}\n");
    b
}

/// The object as binary tokens: values one by one with separators, or runs of DWORDs / FLOATs as
/// lists (no separators).
fn object_bin(ms: &[(Gen, Vec<Val>)], lists: bool) -> B {
    let mut b = [bname(b"T"), bname(b"obj"), btok(10)].concat();
    let semi = btok(20);
    let mut ints: Vec<u32> = Vec::new();
    let flush = |b: &mut B, ints: &mut Vec<u32>| {
        if !ints.is_empty() {
            b.extend_from_slice(&blist(ints));
            ints.clear();
        }
    };
    for (g, vs) in ms {
        let p = match g {
            Gen::Counted(p) => {
                if lists {
                    ints.push(vs.len() as u32);
                } else {
                    b.extend_from_slice(&[bint(vs.len() as u32), semi.clone()].concat());
                }
                *p
            }
            Gen::One(p) | Gen::Fixed(p, _) => *p,
        };
        for v in vs {
            match v {
                Val::I(x) if lists && matches!(p, Prim::Dword | Prim::Sdword | Prim::Word | Prim::Char) => ints.push(*x),
                Val::I(x) => {
                    flush(&mut b, &mut ints);
                    b.extend_from_slice(&[bint(*x), semi.clone()].concat());
                }
                Val::F(f) => {
                    flush(&mut b, &mut ints);
                    if lists {
                        b.extend_from_slice(&[btok(7), 1u32.to_le_bytes().to_vec(), f.to_le_bytes().to_vec()].concat());
                    } else {
                        // (one float alone: a list of one, then a separator is a parse error;
                        // as an integer it would lose the fraction: lists only)
                        b.extend_from_slice(&[btok(7), 1u32.to_le_bytes().to_vec(), f.to_le_bytes().to_vec()].concat());
                    }
                }
                Val::S(s) => {
                    flush(&mut b, &mut ints);
                    b.extend_from_slice(&[btok(2), (s.len() as u32).to_le_bytes().to_vec(), s.clone(), semi.clone()].concat());
                }
            }
        }
    }
    flush(&mut b, &mut ints);
    b.extend_from_slice(&btok(11));
    b
}

fn check(r: &mut Templates, file: &[u8], want: &(B, Vec<(usize, B)>)) -> Result<(), TestCaseError> {
    let n = first(r, file).map_err(|e| TestCaseError::fail(format!("{e:#x}")))?;
    prop_assert_eq!(&n.data, &want.0);
    prop_assert_eq!(&n.strs, &want.1);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Any template of primitives and arrays, any values: the text file (with any of the separator
    /// forms Windows takes), the binary file (values one by one, or as lists) and the compressed
    /// text all give the same packed data.
    #[test]
    fn same_data_every_way(ms in proptest::collection::vec(member(), 1..6), sep in 0usize..5) {
        let forms: [&[&str]; 5] = [&[";"], &[","], &[";;;,"], &[";", ",", ";;"], &[";\n", ";;,\n"]];
        let mut r = reg(&template_text(&ms));
        let want = packed(&ms);
        let t = txt(&object_text(&ms, forms[sep]));
        check(&mut r, &t, &want)?;
        check(&mut r, &bin(&object_bin(&ms, false)), &want)?;
        check(&mut r, &bin(&object_bin(&ms, true)), &want)?;
        check(&mut r, &zip(&t[16..], 7), &want)?;
    }

    /// Separator runs after each value: accepted exactly when one is `;`+ `,`? or `,`.
    #[test]
    fn separator_rule(seps in proptest::collection::vec("[;,]{0,4}", 5)) {
        let mut r = reg(BUFFER);
        let vals = ["3", "0", "1", "2", "5"];
        let mut body = String::new();
        for (v, s) in vals.iter().zip(&seps) {
            body += v;
            body += " ";
            body += s;
            body += " ";
        }
        let good = seps.iter().all(|s| {
            let semis = s.bytes().take_while(|&c| c == b';').count();
            let rest = &s[semis..];
            (semis > 0 && (rest.is_empty() || rest == ",")) || (semis == 0 && rest == ",")
        });
        let got = first(&mut r, &txt(format!("Buffer {{ {body} }}").as_bytes())).map(|n| dwords(&n.data));
        prop_assert_eq!(got.is_ok(), good, "{}", body);
        if good {
            prop_assert_eq!(got.unwrap(), vec![3, 0, 1, 2, 5]);
        }
    }

    /// Any bytes after any header: no panic, and an array size past what the file holds is an error,
    /// not an allocation.
    #[test]
    fn any_bytes(kind in 0usize..4, body in proptest::collection::vec(any::<u8>(), 0..400)) {
        let h: [&[u8]; 4] = [b"xof 0302txt 0032", b"xof 0302bin 0064", b"xof 0302tzip0032", b"xof 0303bzip0032"];
        let file = [h[kind], &body].concat();
        let mut r = reg(&[COMPLEX, BUFFER].concat());
        if let Ok(mut f) = XFile::open(&file, &mut r, 4) {
            for _ in 0..8 {
                if !matches!(f.next_top(&mut r), Ok(Some(_))) {
                    break;
                }
            }
        }
        let _ = register(&file, &mut r);
    }

    /// Text tokens of a value list read back as the numbers written.
    #[test]
    fn numbers(xs in proptest::collection::vec(any::<i32>(), 0..20), fs in proptest::collection::vec(-1.0e30f64..1.0e30, 0..20)) {
        let s: String = xs.iter().map(|x| format!("{x};")).chain(fs.iter().map(|f| format!("{f:?},"))).collect();
        let (t, e) = text_tokens(s.as_bytes());
        prop_assert_eq!(e, None);
        let mut want = Vec::new();
        for x in &xs {
            want.push(Tok::Int(*x as u32, false));
            want.push(Tok::Semi);
        }
        for f in &fs {
            want.push(Tok::Float(*f, false));
            want.push(Tok::Comma);
        }
        prop_assert_eq!(t, want);
    }
}

#[test]
fn huge_array_sizes_are_errors() {
    let mut r = reg(BUFFER);
    assert_eq!(first(&mut r, &txt(b"Buffer { 4000000000; 1; 2; }")), Err(PARSEERROR));
    let mut r = reg(b"template E {\n<10203040-5060-7080-90A0-B0C0D0E0F000>\n}\ntemplate A {\n<10203040-5060-7080-90A0-B0C0D0E0F001>\nDWORD n;\narray E e[n];\n}\n");
    assert_eq!(first(&mut r, &txt(b"A { 4000000; }")), Err(PARSEERROR));
}

/// The Softpal/AGS-era exporters' files (Pygmalion 体験版's motion.x): Frame / Mesh / skin
/// templates from the file itself and the standard ones, MSVC NaN spellings.
#[test]
fn a_real_layout() {
    let std = b"template Vector {\n<3D82AB5E-62DA-11CF-AB39-0020AF71E433>\nFLOAT x;\nFLOAT y;\nFLOAT z;\n}\n\
template ColorRGBA {\n<35FF44E0-6C7C-11cf-8F52-0040333594A3>\nFLOAT red;\nFLOAT green;\nFLOAT blue;\nFLOAT alpha;\n}\n\
template ColorRGB {\n<D3E16E81-7835-11cf-8F52-0040333594A3>\nFLOAT red;\nFLOAT green;\nFLOAT blue;\n}\n\
template TextureFilename {\n<A42790E1-7810-11cf-8F52-0040333594A3>\nSTRING filename;\n}\n\
template Material {\n<3D82AB4D-62DA-11CF-AB39-0020AF71E433>\nColorRGBA faceColor;\nFLOAT power;\nColorRGB specularColor;\nColorRGB emissiveColor;\n[...]\n}\n\
template Matrix4x4 {\n<F6F23F45-7686-11cf-8F52-0040333594A3>\narray FLOAT matrix[16];\n}\n\
template FrameTransformMatrix {\n<F6F23F41-7686-11cf-8F52-0040333594A3>\nMatrix4x4 frameMatrix;\n}\n\
template Frame {\n<3D82AB46-62DA-11CF-AB39-0020AF71E433>\n[...]\n}\n";
    let mut r = reg(std);
    let file = txt(b"template XSkinMeshHeader {\n <3cf169ce-ff7c-44ab-93c0-f78f62d172e2>\n WORD nMaxSkinWeightsPerVertex;\n WORD nMaxSkinWeightsPerFace;\n WORD nBones;\n}\n\n\
Material st0030000_010arm_R {\n 0.784314;0.784314;0.784314;1.000000;;\n 51.200001;\n 0.000000;0.000000;0.000000;;\n -1.#IND00;0.000000;0.000000;;\n TextureFilename {\n  \"st003000.psd\";\n }\n}\n\
Frame Bip01 {\n FrameTransformMatrix {\n  1.0,0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,1.0;;\n }\n XSkinMeshHeader { 1; 2; 3; }\n { st0030000_010arm_R }\n}\n");
    let mut f = XFile::open(&file, &mut r, 4).unwrap();
    let m = f.next_top(&mut r).unwrap().unwrap();
    assert_eq!(f.nodes[m].data.len(), 44);
    assert!(f32::from_le_bytes(f.nodes[m].data[32..36].try_into().unwrap()).is_nan());
    let Child::Obj(t) = f.nodes[m].children[0] else { panic!() };
    assert_eq!(f.nodes[t].strs, vec![(0, b"st003000.psd".to_vec())]);
    let fr = f.next_top(&mut r).unwrap().unwrap();
    assert_eq!(f.nodes[fr].children.len(), 3);
    let Child::Obj(x) = f.nodes[fr].children[0] else { panic!() };
    assert_eq!(f.nodes[x].data.len(), 64);
    assert_eq!(f.nodes[fr].children[2], Child::Ref { name: Some(b"st0030000_010arm_R".to_vec()), guid: None });
    assert_eq!(f.find(&mut r, Some(b"st0030000_010arm_R"), None), Some(m));
}

/// review b11: an array sized by a member whose name a later member of another type reuses
/// read the data of that later member (empty here) as the size: a panic, the VM gone
#[test]
fn array_size_by_reused_name() {
    let mut r = reg(b"template T {\n<00000000-0000-0000-0000-00000000AA01>\nDWORD n;\narray CHAR n[n];\narray DWORD x[n];\n}\n");
    assert_eq!(first(&mut r, &txt(b"T {\n0;\n;\n;\n}\n")).err(), Some(PARSEERROR));
    let mut r = reg(b"template U {\n<00000000-0000-0000-0000-00000000AA02>\nWORD n;\nDWORD n;\narray DWORD x[n];\n}\n");
    // (the latest n, a DWORD, sizes the array, as before)
    let n = first(&mut r, &txt(b"U {\n7;\n2;\n5; 6;\n}\n")).unwrap();
    assert_eq!(n.data, [7, 0, 2, 0, 0, 0, 5, 0, 0, 0, 6, 0, 0, 0]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Templates whose members reuse a few names (scalars, arrays sized by any of them, of
    /// any type), and any values: no panic, whatever reads is whole members.
    #[test]
    fn reused_member_names(members in proptest::collection::vec((0usize..6, 0usize..3, proptest::option::of(0usize..3)), 1..6), vals in proptest::collection::vec(0u32..4, 0..24), kind in 0usize..2) {
        let tys = ["WORD", "DWORD", "CHAR", "FLOAT", "UCHAR", "SDWORD"];
        let names = ["a", "b", "c"];
        let mut t = String::from("template R {\n<00000000-0000-0000-0000-00000000AA03>\n");
        for (ty, n, dim) in &members {
            match dim {
                Some(d) => t += &format!("array {} {}[{}];\n", tys[*ty], names[*n], names[*d]),
                None => t += &format!("{} {};\n", tys[*ty], names[*n]),
            }
        }
        t += "}\n";
        let mut r = Templates::default();
        if register(&txt(t.as_bytes()), &mut r).is_err() {
            return Ok(());
        }
        let file = if kind == 0 {
            let body: String = vals.iter().map(|v| format!("{v};\n")).collect();
            txt(format!("R {{\n{body}}}\n").as_bytes())
        } else {
            // binary: TOKEN_NAME "R", {, an integer list of the values, }
            let mut b = vec![1, 0, 1, 0, 0, 0, b'R', 10, 0];
            b.extend_from_slice(&6u16.to_le_bytes());
            b.extend_from_slice(&(vals.len() as u32).to_le_bytes());
            for v in &vals {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&[11, 0]);
            bin(&b)
        };
        if let Ok(n) = first(&mut r, &file) {
            prop_assert!(n.members.iter().all(|(_, o, s)| o + s <= n.data.len()));
        }
    }

    /// find by name or GUID gives the first object read with it, as a scan of every object did.
    #[test]
    fn find_is_the_first_match(objs in proptest::collection::vec((0usize..5, proptest::option::of(0u8..3)), 1..30), look in 0usize..6, g in proptest::option::of(0u8..3)) {
        let names = ["n0", "n1", "n2", "n3", "n4", "none"];
        let mut body = String::new();
        for (n, guid) in &objs {
            let gs = guid.map_or(String::new(), |g| format!("<00000000-0000-0000-0000-0000000000{g:02X}>"));
            body += &format!("Header {} {{ {gs} 1; 2; 3; }}\n", names[*n]);
        }
        let mut r = reg(HEADER);
        let file = txt(body.as_bytes());
        let mut f = XFile::open(&file, &mut r, 4).unwrap();
        let gid = g.map(|g| { let mut x = [0u8; 16]; x[15] = g; x });
        let got = f.find(&mut r, Some(names[look].as_bytes()), gid.as_ref());
        while matches!(f.read_top(&mut r), Ok(Some(_))) {}
        let want = f.nodes.iter().position(|n| n.name.as_deref() == Some(names[look].as_bytes()) && gid.as_ref().is_none_or(|g| n.guid.as_ref() == Some(g)));
        prop_assert_eq!(got, want);
        let got = f.find(&mut r, None, gid.as_ref());
        let want = gid.and_then(|g| f.nodes.iter().position(|n| n.guid == Some(g)));
        prop_assert_eq!(got, want);
    }
}
