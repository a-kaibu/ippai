//! What vnwasm needs from the x86 PE DLL the wasm module stands in for: its identity, its
//! exports (where in the export address table each name is) and the import address table slot
//! of every imported function (normal and delay-loaded).

use anyhow::{bail, Context, Result};

pub struct Pe {
    pub size_of_image: u32,
    pub timestamp: u32,
    /// the DLL's name in its export directory ("USER32.dll")
    pub name: String,
    /// ordinal base of the export address table
    pub ordinal_base: u32,
    /// export address table entry count
    pub nfuncs: u32,
    /// name -> index into the export address table
    pub exports: Vec<(String, u32)>,
    /// the export address table's RVAs (0 for forwarders and holes)
    pub eat: Vec<u32>,
    /// (dll, symbol, IAT slot RVA); symbol "#<n>" for imports by ordinal
    pub imports: Vec<(String, String, u32)>,
    /// global functions of the COFF symbol table (Wine's DLLs aren't stripped): (symbol as the
    /// linker spells it, "_name@8"; RVA)
    pub functions: Vec<(String, u32)>,
    /// exported syscall thunks ("mov $id,%eax; mov $helper,%edx; call *%edx; ret $n"): name -> id
    pub syscalls: Vec<(String, u32)>,
}

struct Image<'a> {
    b: &'a [u8],
    sections: Vec<(u32, u32, u32)>, // rva, size, file offset
}

impl Image<'_> {
    fn off(&self, rva: u32) -> Result<usize> {
        for &(va, size, raw) in &self.sections {
            if rva >= va && rva < va + size {
                return Ok((raw + rva - va) as usize);
            }
        }
        bail!("rva {rva:#x} is in no section")
    }
    fn u32(&self, rva: u32) -> Result<u32> {
        let o = self.off(rva)?;
        Ok(u32::from_le_bytes(self.b.get(o..o + 4).context("short file")?.try_into()?))
    }
    fn u16(&self, rva: u32) -> Result<u16> {
        let o = self.off(rva)?;
        Ok(u16::from_le_bytes(self.b.get(o..o + 2).context("short file")?.try_into()?))
    }
    fn cstr(&self, rva: u32) -> Result<String> {
        let o = self.off(rva)?;
        let end = self.b[o..].iter().position(|&c| c == 0).context("unterminated string")?;
        Ok(String::from_utf8_lossy(&self.b[o..o + end]).into_owned())
    }
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}

pub fn parse(b: &[u8]) -> Result<Pe> {
    let nt = rd32(b, 0x3c) as usize;
    if b.get(nt..nt + 4) != Some(b"PE\0\0") || rd16(b, nt + 4) != 0x14c {
        bail!("not an i386 PE file");
    }
    let nsect = rd16(b, nt + 6) as usize;
    let timestamp = rd32(b, nt + 8);
    let opt = nt + 24;
    let opt_size = rd16(b, nt + 20) as usize;
    if rd16(b, opt) != 0x10b {
        bail!("not PE32");
    }
    let size_of_image = rd32(b, opt + 56);
    let dirs = opt + 96;
    let dir = |i: usize| (rd32(b, dirs + i * 8), rd32(b, dirs + i * 8 + 4));
    let mut sections = Vec::new();
    for i in 0..nsect {
        let s = opt + opt_size + i * 40;
        let (vsize, va, rawsize, raw) = (rd32(b, s + 8), rd32(b, s + 12), rd32(b, s + 16), rd32(b, s + 20));
        let _ = vsize;
        sections.push((va, rawsize, raw));
    }
    let img = Image { b, sections };

    let (erva, esize) = dir(0);
    let (mut name, mut ordinal_base, mut nfuncs, mut exports, mut eat) = (String::new(), 1, 0, Vec::new(), Vec::new());
    if erva != 0 {
        name = img.cstr(img.u32(erva + 12)?)?;
        ordinal_base = img.u32(erva + 16)?;
        nfuncs = img.u32(erva + 20)?;
        let nnames = img.u32(erva + 24)?;
        let (funcs, names, ords) = (img.u32(erva + 28)?, img.u32(erva + 32)?, img.u32(erva + 36)?);
        for i in 0..nfuncs {
            let r = img.u32(funcs + i * 4)?;
            eat.push(if r >= erva && r < erva + esize { 0 } else { r });
        }
        for i in 0..nnames {
            exports.push((img.cstr(img.u32(names + i * 4)?)?, img.u16(ords + i * 2)? as u32));
        }
    }

    let mut imports = Vec::new();
    let mut thunks = |dll: &str, lookup: u32, iat: u32| -> Result<()> {
        let mut i = 0;
        loop {
            let e = img.u32(lookup + i * 4)?;
            if e == 0 {
                return Ok(());
            }
            let sym = if e & 0x8000_0000 != 0 { format!("#{}", e & 0xffff) } else { img.cstr(e + 2)? };
            imports.push((dll.to_string(), sym, iat + i * 4));
            i += 1;
        }
    };
    let (irva, _) = dir(1);
    if irva != 0 {
        for d in 0.. {
            let at = irva + d * 20;
            let (lookup, name_rva, iat) = (img.u32(at)?, img.u32(at + 12)?, img.u32(at + 16)?);
            if name_rva == 0 {
                break;
            }
            let dll = img.cstr(name_rva)?;
            thunks(&dll, if lookup != 0 { lookup } else { iat }, iat)?;
        }
    }
    // delay imports (attributes, name, module handle, IAT, name table, ...): RVAs in PE32+ style
    let (drva, _) = dir(13);
    if drva != 0 {
        for d in 0.. {
            let at = drva + d * 32;
            let (name_rva, iat, int) = (img.u32(at + 4)?, img.u32(at + 12)?, img.u32(at + 16)?);
            if name_rva == 0 {
                break;
            }
            let dll = img.cstr(name_rva)?;
            thunks(&dll, int, iat)?;
        }
    }
    if drva == 0 {
        // mingw's delay-import libraries (dlltool --output-delaylib) leave the directory empty:
        // find their ImgDelayDescr records (attributes 1, then the DLL name's RVA, the module
        // handle's, the IAT's and the name table's)
        let valid = |rva: u32| img.off(rva).is_ok();
        for &(va, size, raw) in &img.sections {
            let mut o = 0;
            while o + 32 <= size {
                let at = (raw + o) as usize;
                if at + 32 > b.len() {
                    break;
                }
                if rd32(b, at) == 1 {
                    let (name_rva, hmod, iat, int) = (rd32(b, at + 4), rd32(b, at + 8), rd32(b, at + 12), rd32(b, at + 16));
                    if valid(name_rva) && valid(hmod) && valid(iat) && valid(int) && iat != int {
                        if let Ok(dll) = img.cstr(name_rva) {
                            let l = dll.to_ascii_lowercase();
                            if (l.ends_with(".dll") || l.ends_with(".drv")) && dll.len() < 64 {
                                let _ = va;
                                if img.u32(int).unwrap_or(0) != 0 {
                                    thunks(&dll, int, iat)?;
                                } else {
                                    // binutils' delaylib: the name table and IAT pointers end up
                                    // just past the 0-terminated lists
                                    let mut n = 0;
                                    while n < 4096 && img.u32(int - 8 - n * 4).map_or(false, |e| e != 0 && valid(e)) {
                                        n += 1;
                                    }
                                    if n > 0 {
                                        thunks(&dll, int - 4 - n * 4, iat - 4 - n * 4)?;
                                    }
                                }
                                o += 32;
                                continue;
                            }
                        }
                    }
                }
                o += 4;
            }
        }
    }
    let mut syscalls = Vec::new();
    for (n, i) in &exports {
        let Some(&r) = eat.get(*i as usize) else {
            continue;
        };
        let Ok(o) = img.off(r) else { continue };
        if let Some(c) = b.get(o..o + 15) {
            if c[0] == 0xb8 && c[5] == 0xba && c[10] == 0xff && c[11] == 0xd2 && c[12] == 0xc2 {
                syscalls.push((n.clone(), u32::from_le_bytes(c[1..5].try_into().unwrap())));
            }
        }
    }
    let mut functions = Vec::new();
    let (symtab, nsyms) = (rd32(b, nt + 12) as usize, rd32(b, nt + 16) as usize);
    if symtab != 0 && symtab + nsyms * 18 + 4 <= b.len() {
        let strtab = symtab + nsyms * 18;
        let secs: Vec<u32> = (0..nsect).map(|i| rd32(b, opt + opt_size + i * 40 + 12)).collect();
        let mut i = 0;
        while i < nsyms {
            let e = symtab + i * 18;
            let (value, sect, ty, class, naux) = (rd32(b, e + 8), rd16(b, e + 12) as i16, rd16(b, e + 14), b[e + 16], b[e + 17] as usize);
            if class == 2 && sect > 0 && ty == 0x20 && (sect as usize) <= secs.len() {
                let raw = &b[e..e + 8];
                let name = if raw[..4] == [0; 4] {
                    let o = strtab + u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
                    let end = b[o..].iter().position(|&c| c == 0).unwrap_or(0);
                    String::from_utf8_lossy(&b[o..o + end]).into_owned()
                } else {
                    let end = raw.iter().position(|&c| c == 0).unwrap_or(8);
                    String::from_utf8_lossy(&raw[..end]).into_owned()
                };
                functions.push((name, secs[sect as usize - 1] + value));
            }
            i += 1 + naux;
        }
    }
    Ok(Pe { size_of_image, timestamp, name, ordinal_base, nfuncs, exports, eat, imports, functions, syscalls })
}
