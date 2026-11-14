//! The .x file parser (sake::xof, d3dxof.dll's IDirectXFile) on any bytes, after a header of each
//! format, with templates registered (the D3DRM-like Vector / Mesh ones, an array-sized one, and one whose
//! array sizes name members a later member of another type reuses):
//! no panic; every object read has its top-level members back to back from offset 0 covering its
//! data, and every string slot inside the data; RegisterTemplates of the same bytes either adds
//! all of a file's templates or none.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::xof::{register, Templates, XFile};

const T: &[u8] = b"xof 0302txt 0032\ntemplate Vector { <3D82AB5E-62DA-11CF-AB39-0020AF71E433> FLOAT x; FLOAT y; FLOAT z; }\n\
template MeshFace { <3D82AB5F-62DA-11CF-AB39-0020AF71E433> DWORD n; array DWORD i[n]; }\n\
template Mesh { <3D82AB44-62DA-11CF-AB39-0020AF71E433> DWORD nv; array Vector v[nv]; DWORD nf; array MeshFace f[nf]; [...] }\n\
template S { <3D82AB43-62DA-11CF-AB39-0020AF71E433> STRING s; WORD w; CHAR c; DOUBLE d; [Vector] }\n\
template Q { <3D82AB43-62DA-11CF-AB39-0020AF71E434> DWORD n; array CHAR n[n]; array DWORD x[n]; WORD m; DWORD m; array WORD y[m]; }\n";

fuzz_target!(|data: &[u8]| {
    let Some((&k, body)) = data.split_first() else { return };
    let h: [&[u8]; 4] = [b"xof 0302txt 0032", b"xof 0302bin 0064", b"xof 0302tzip0032", b"xof 0303bzip0032"];
    let file = [h[k as usize % 4], body].concat();
    let mut reg = Templates::default();
    register(T, &mut reg).unwrap();
    let before = reg.list.len();
    if let Ok(mut f) = XFile::open(&file, &mut reg, 4) {
        for _ in 0..16 {
            if !matches!(f.next_top(&mut reg), Ok(Some(_))) {
                break;
            }
        }
        for n in &f.nodes {
            let mut at = 0;
            for (_, off, size) in &n.members {
                assert_eq!(*off, at);
                at += size;
            }
            assert_eq!(at, n.data.len());
            assert!(n.strs.iter().all(|(o, _)| o + 4 <= n.data.len()));
        }
    }
    let mut r = Templates::default();
    register(T, &mut r).unwrap();
    let ok = register(&file, &mut r).is_ok();
    assert!(ok || r.list.len() == before);
});
