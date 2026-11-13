//! Tables for sake's tests from Microsoft's Win32 metadata: Windows.Win32.winmd from the NuGet
//! package Microsoft.Windows.SDK.Win32Metadata (github.com/microsoft/win32metadata), the
//! Windows SDK headers' functions, structs and constants as machine-readable records.
//!
//!   curl -fsSL -o md.zip https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.win32metadata/71.0.45-preview/microsoft.windows.sdk.win32metadata.71.0.45-preview.nupkg
//!   unzip -o md.zip Windows.Win32.winmd
//!   cargo run --release -- Windows.Win32.winmd ../..
//!
//! writes (under the repository root):
//! - crates/sake/src/com_methods.rs: the Direct2D, DirectWrite and WIC interfaces' whole vtables
//!   (crates/sake/src/com.rs builds its objects from them) and their IIDs.
//! - crates/sake/src/d3d11_methods.rs and crates/kura/src/d3d11/names.rs: the DXGI, D3D11 and D3D10
//!   interfaces sake's d3d11.rs numbers 64.. (D3D11 below): each method's argument count, and names.
//! - crates/sake/src/outptrs.txt: each function's out parameters that point at something pointer-sized (4 bytes on x86,
//! 8 on x64: a pointer, handle, interface, SIZE_T, SRWLOCK...): "dll name i,j..." (argument
//! indexes from 0), sorted.
//! sake writes those with mem::putp/wp; put32/w32 leaves the upper half on x64
//! (crates/sake/src/outptr_audit.rs).
use std::collections::{BTreeMap, HashMap};
use windows_metadata::reader::*;
use windows_metadata::*;

/// C layout of the Win32 types for one architecture.
struct Layout<'a> {
    ix: &'a Index,
    ptr: u32,
    memo: std::cell::RefCell<HashMap<String, (u32, u32)>>,
}

impl<'a> Layout<'a> {
    fn new(ix: &'a Index, x64: bool) -> Self {
        Layout { ix, ptr: if x64 { 8 } else { 4 }, memo: Default::default() }
    }
    /// (size, alignment); `parent` resolves nested types (anonymous unions: their names repeat).
    fn of(&self, t: &Type, parent: Option<TypeDef<'a>>) -> (u32, u32) {
        use Type::*;
        match t {
            Void => (0, 1),
            Bool | I8 | U8 => (1, 1),
            Char | I16 | U16 => (2, 2),
            I32 | U32 | F32 => (4, 4),
            I64 | U64 | F64 => (8, 8),
            ArrayFixed(e, n) => {
                let (s, a) = self.of(e, parent);
                (s * *n as u32, a)
            }
            ValueName(n) => match parent.and_then(|p| self.ix.nested(p).find(|x| x.name() == n.name)) {
                Some(d) => self.def(d),
                None => self.named(n),
            },
            _ => (self.ptr, self.ptr),
        }
    }
    fn named(&self, n: &TypeName) -> (u32, u32) {
        if n.namespace == "System" && n.name == "Guid" {
            return (16, 4);
        }
        let key = format!("{}.{}", n.namespace, n.name);
        if let Some(v) = self.memo.borrow().get(&key) {
            return *v;
        }
        let Some(d) = self.ix.get(&n.namespace, &n.name).next() else { return (0, 0) };
        let r = self.def(d);
        self.memo.borrow_mut().insert(key, r);
        r
    }
    fn def(&self, d: TypeDef<'a>) -> (u32, u32) {
        match d.category() {
            TypeCategory::Enum => self.of(&d.underlying_type().unwrap(), None),
            TypeCategory::Struct => {
                let pack = d.class_layout().map(|l| l.packing_size() as u32).filter(|&p| p > 0).unwrap_or(64);
                let union = d.flags().contains(TypeAttributes::ExplicitLayout);
                let (mut end, mut align) = (0u32, 1u32);
                for f in d.fields() {
                    if f.flags().contains(FieldAttributes::Static) || f.constant().is_some() {
                        continue;
                    }
                    let (s, a) = self.of(&f.ty(), Some(d));
                    let a = a.min(pack).max(1);
                    align = align.max(a);
                    end = if union { end.max(s) } else { end.div_ceil(a) * a + s };
                }
                (end.div_ceil(align) * align, align)
            }
            _ => (self.ptr, self.ptr),
        }
    }
    /// Size of what a one-level pointer parameter points at (0: not such a pointer).
    fn pointee(&self, t: &Type) -> u32 {
        match t {
            Type::PtrMut(e, 1) | Type::PtrConst(e, 1) => self.of(e, None).0,
            Type::PtrMut(..) | Type::PtrConst(..) => self.ptr,
            _ => 0,
        }
    }
}


/// Whether a struct stands for a scalar (HRESULT, BOOL, HWND...: C typedefs).
fn scalar(ix: &Index, t: &Type) -> bool {
    match t {
        Type::ValueName(n) => match ix.get(&n.namespace, &n.name).next() {
            Some(d) => d.category() != TypeCategory::Struct || d.has_attribute("NativeTypedefAttribute"),
            None => n.name != "Guid",
        },
        _ => true,
    }
}

/// An interface's TypeDef by name, in any namespace.
fn iface<'a>(ix: &'a Index, name: &str) -> Option<TypeDef<'a>> {
    ix.types().find(|t| t.name() == name && t.category() == TypeCategory::Interface)
}

/// The interface it derives from (None for IUnknown).
fn base<'a>(ix: &'a Index, t: TypeDef<'a>) -> Option<TypeDef<'a>> {
    let b = t.interface_impls().next()?.interface(&[]);
    let (Type::ClassName(n) | Type::ValueName(n)) = b else { return None };
    ix.get(&n.namespace, &n.name).next()
}

/// The whole vtable, inherited methods first: (name, x86 stack dwords with `this` and a hidden
/// result pointer, result: 0 integer, 1 float, 2 a structure through that pointer).
fn vtable(ix: &Index, l: &Layout, t: TypeDef) -> Vec<(String, u32, u8)> {
    let mut v = base(ix, t).map(|b| vtable(ix, l, b)).unwrap_or_default();
    let own: Vec<&str> = t.methods().map(|m| m.name()).collect();
    for (i, m) in t.methods().enumerate() {
        let sig = m.signature(&[]);
        let kind = match &sig.return_type {
            Type::F32 | Type::F64 => 1,
            r @ Type::ValueName(_) if !scalar(ix, r) => 2,
            _ => 0,
        };
        let args: u32 = sig.types.iter().map(|t| l.of(t, None).0.max(1).div_ceil(4)).sum();
        // (overloads in one interface: the earlier ones get a "_" for each later one, as in
        // Wine's IDL, so that an object can implement each)
        let later = own[i + 1..].iter().filter(|n| **n == m.name()).count();
        v.push((format!("{}{}", m.name(), "_".repeat(later)), 1 + args + (kind == 2) as u32, kind));
    }
    v
}

fn chain(ix: &Index, t: TypeDef) -> Vec<String> {
    let mut v = vec![t.name().to_string()];
    if let Some(b) = base(ix, t) {
        v.extend(chain(ix, b));
    }
    v
}

fn guid(t: TypeDef) -> Option<String> {
    let v = t.find_attribute("GuidAttribute")?.value();
    let n: Vec<u64> = v
        .iter()
        .map(|(_, x)| match x {
            Value::U32(a) => *a as u64,
            Value::U16(a) => *a as u64,
            Value::U8(a) => *a as u64,
            _ => 0,
        })
        .collect();
    let b: Vec<String> = n[3..11].iter().map(|x| format!("0x{x:02x}")).collect();
    Some(format!("(0x{:08x}, 0x{:04x}, 0x{:04x}, [{}])", n[0], n[1], n[2], b.join(", ")))
}

/// com_methods.rs: every interface of these namespaces.
fn com(ix: &Index) -> String {
    const NS: [&str; 5] = ["Windows.Win32.Graphics.Direct2D", "Windows.Win32.Graphics.Direct2D.Common", "Windows.Win32.Graphics.DirectWrite", "Windows.Win32.Graphics.Imaging", "Windows.Win32.Graphics.Imaging.D2D"];
    let l = Layout::new(ix, false);
    // (and COM interfaces those objects hand out: IWICMetadataQueryReader::GetEnumerator's)
    const MORE: [&str; 1] = ["IEnumString"];
    let mut names: Vec<TypeDef> = ix
        .types()
        .filter(|t| t.category() == TypeCategory::Interface && (NS.contains(&t.namespace()) || (t.namespace() == "Windows.Win32.System.Com" && MORE.contains(&t.name()))))
        .collect();
    names.sort_by_key(|t| t.name());
    let mut o = String::from(
        "//! generated by tools/win32md from Microsoft's Win32 metadata (Direct2D, DirectWrite, WIC): do not\n\
         //! edit. Per interface, its whole vtable: (method, stack argument dwords with `this` and a hidden\n\
         //! result pointer, result: 0 integer, 1 float, 2 a structure through that pointer), and the\n\
         //! interface names it derives from (itself first), for QueryInterface.\n\n\
         pub type Guid = (u32, u16, u16, [u8; 8]);\n\
         pub const IFACES: &[(&str, &[(&str, u8, u8)], &[&str])] = &[\n",
    );
    let mut iids = BTreeMap::new();
    for t in &names {
        let v: Vec<String> = vtable(ix, &l, *t).iter().map(|(n, a, k)| format!("(\"{n}\", {a}, {k})")).collect();
        let c = chain(ix, *t);
        for b in &c {
            if let Some(g) = iface(ix, b).and_then(guid) {
                iids.insert(b.clone(), g);
            }
        }
        let c: Vec<String> = c.iter().map(|n| format!("\"{n}\"")).collect();
        o += &format!("    (\"{}\", &[{}], &[{}]),\n", t.name(), v.join(", "), c.join(", "));
    }
    o += "];\npub const IIDS: &[(&str, Guid)] = &[\n";
    for (n, g) in iids {
        o += &format!("    (\"{n}\", {g}),\n");
    }
    o + "];\n"
}

/// sake's d3d11.rs interface numbers 64..: (number, the interface whose vtable is used)
const D3D11: &[(u32, &str)] = &[
    (64, "IDXGIFactory7"), (65, "IDXGIAdapter4"), (66, "IDXGIOutput6"), (67, "IDXGISwapChain1"),
    (68, "IDXGIDevice3"), (69, "IDXGISurface2"), (70, "ID3D11Device5"), (71, "ID3D11DeviceContext4"),
    (72, "ID3D11Buffer"), (73, "ID3D11Texture1D"), (74, "ID3D11Texture2D"), (75, "ID3D11Texture3D"),
    (76, "ID3D11ShaderResourceView"), (77, "ID3D11RenderTargetView"), (78, "ID3D11DepthStencilView"),
    (79, "ID3D11UnorderedAccessView"), (80, "ID3D11VertexShader"), (81, "ID3D11PixelShader"),
    (82, "ID3D11GeometryShader"), (83, "ID3D11HullShader"), (84, "ID3D11DomainShader"),
    (85, "ID3D11ComputeShader"), (86, "ID3D11InputLayout"), (87, "ID3D11SamplerState"),
    (88, "ID3D11BlendState1"), (89, "ID3D11DepthStencilState"), (90, "ID3D11RasterizerState2"),
    (91, "ID3D11Query"), (92, "ID3D11ClassLinkage"), (93, "ID3D11CommandList"),
    (94, "ID3D10Multithread"),
    // Direct3D 10 / 10.1 (crates/kura/src/d3d11/d3d10.rs): faces of the same objects
    (95, "ID3D10Device1"), (96, "ID3D10Buffer"), (97, "ID3D10Texture1D"), (98, "ID3D10Texture2D"),
    (99, "ID3D10Texture3D"), (100, "ID3D10ShaderResourceView1"), (101, "ID3D10RenderTargetView"),
    (102, "ID3D10DepthStencilView"), (103, "ID3D10VertexShader"), (104, "ID3D10GeometryShader"),
    (105, "ID3D10PixelShader"), (106, "ID3D10InputLayout"), (107, "ID3D10SamplerState"),
    (108, "ID3D10BlendState1"), (109, "ID3D10DepthStencilState"), (110, "ID3D10RasterizerState"),
    (111, "ID3D10Query"),
    (112, "IDXGIResource"), (113, "ID3DUserDefinedAnnotation"),
];

/// (d3d11_methods.rs, names.rs)
fn d3d11(ix: &Index) -> (String, String) {
    let l = Layout::new(ix, false);
    let vts: Vec<(&str, Vec<(String, u32, u8)>)> = D3D11
        .iter()
        .map(|(_, n)| (*n, vtable(ix, &l, iface(ix, n).unwrap_or_else(|| panic!("no interface {n}")))))
        .collect();
    for (n, v) in &vts {
        for (m, _, k) in v {
            assert!(*k != 2, "{n}::{m} returns a structure");
        }
    }
    let row = |v: &[(String, u32, u8)], float: u32| {
        let a: Vec<String> = v.iter().map(|(_, a, k)| if *k == 1 { float } else { *a }.to_string()).collect();
        format!("    &[{}],\n", a.join(", "))
    };
    let mut s = format!(
        "//! generated by tools/win32md from Microsoft's Win32 metadata: do not edit. Per interface 64..,\n\
         //! each method's argument count (dwords) with `this`; 0 marks a FLOAT method (returns 0.0 in ST0,\n\
         //! 2 args).\n\npub const METHODS: [&[u8]; {}] = [\n",
        vts.len()
    );
    let mut k = String::from(
        "// generated by tools/win32md from Microsoft's Win32 metadata: do not edit\n\n\
         /// (interface name, method names) by interface number - 64\n\
         pub const NAMES: &[(&str, &[&str])] = &[\n",
    );
    for (n, v) in &vts {
        s += &row(v, 0);
        let m: Vec<String> = v.iter().map(|(m, _, _)| format!("\"{m}\"")).collect();
        k += &format!("    (\"{n}\", &[{}]),\n", m.join(", "));
    }
    k += "];\n\n/// Each method's argument count (dwords, `this` included) by interface number - 64\npub const ARGS: &[&[u8]] = &[\n";
    for (_, v) in &vts {
        k += &row(v, 2);
    }
    (s + "];\n", k + "];\n")
}

/// Functions by (dll, name): signature and parameter rows.
fn functions(ix: &Index) -> HashMap<(String, String), MethodDef<'_>> {
    let mut m = HashMap::new();
    for it in ix.items() {
        if let Item::Fn(f) = it {
            if let Some(im) = f.impl_map() {
                let dll = im.import_scope().name().to_lowercase();
                let dll = dll.strip_suffix(".dll").unwrap_or(&dll).to_string();
                m.insert((dll, f.name().to_string()), f);
            }
        }
    }
    m
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let read = |arch| Index::new_for_architecture(vec![File::read(&a[1]).expect("winmd")], arch);
    let (i32_, i64_) = (read(1), read(2));
    let root = std::path::PathBuf::from(a.get(2).expect("usage: win32md WINMD REPO-ROOT"));
    let write = |p: &str, s: String| std::fs::write(root.join(p), s).unwrap();
    write("crates/sake/src/com_methods.rs", com(&i32_));
    let (m, n) = d3d11(&i32_);
    write("crates/sake/src/d3d11_methods.rs", m);
    write("crates/kura/src/d3d11/names.rs", n);
    write("crates/sake/src/outptrs.txt", outptrs(&i32_, &i64_));
}

fn outptrs(i32_: &Index, i64_: &Index) -> String {
    let (l32, l64) = (Layout::new(i32_, false), Layout::new(i64_, true));
    let f32 = functions(i32_);
    let mut out = BTreeMap::<(String, String), Vec<usize>>::new();
    for ((dll, name), f) in functions(i64_) {
        let Some(g) = f32.get(&(dll.clone(), name.clone())) else { continue };
        let (s64, s32) = (f.signature(&[]), g.signature(&[]));
        if s64.types.len() != s32.types.len() {
            continue;
        }
        let Ok(rows) = f.params_by_sequence(s64.types.len()) else { continue };
        for (i, t) in s64.types.iter().enumerate() {
            let out_param = rows.params()[i].is_some_and(|p| p.flags().contains(ParamAttributes::Out));
            if out_param && l64.pointee(t) == 8 && l32.pointee(&s32.types[i]) == 4 {
                out.entry((dll.clone(), name.clone())).or_default().push(i);
            }
        }
    }
    let mut o = String::new();
    for ((dll, name), v) in out {
        let v: Vec<String> = v.iter().map(|i| i.to_string()).collect();
        o += &format!("{dll} {name} {}\n", v.join(","));
    }
    o
}
