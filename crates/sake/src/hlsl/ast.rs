//! The syntax tree and types.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum B {
    Bool,
    Int,
    Uint,
    Float,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dim {
    Buffer,
    T1D,
    T1DArray,
    T2D,
    T2DArray,
    T2DMS,
    T2DMSArray,
    T3D,
    Cube,
    CubeArray,
}

impl Dim {
    /// coordinates (without the array index)
    pub fn coords(self) -> usize {
        match self {
            Dim::Buffer | Dim::T1D | Dim::T1DArray => 1,
            Dim::T2D | Dim::T2DArray | Dim::T2DMS | Dim::T2DMSArray => 2,
            Dim::T3D | Dim::Cube | Dim::CubeArray => 3,
        }
    }
    pub fn array(self) -> bool {
        matches!(self, Dim::T1DArray | Dim::T2DArray | Dim::T2DMSArray | Dim::CubeArray)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Ty {
    Void,
    Scalar(B),
    Vec(B, u8),
    /// rows, columns
    Mat(B, u8, u8),
    Array(Box<Ty>, u32),
    Struct(usize),
    /// a texture object and its element type
    Tex(Dim, Box<Ty>),
    /// SamplerState / SamplerComparisonState (true)
    Sampler(bool),
    /// sampler2D... of shader model 2/3 code: a texture and a sampler on one register
    Legacy(Dim),
    Str,
}

impl Ty {
    pub fn base(&self) -> B {
        match self {
            Ty::Scalar(b) | Ty::Vec(b, _) | Ty::Mat(b, _, _) => *b,
            Ty::Array(t, _) => t.base(),
            _ => B::Float,
        }
    }
    pub fn numeric(&self) -> bool {
        matches!(self, Ty::Scalar(_) | Ty::Vec(..) | Ty::Mat(..))
    }
    /// components of a scalar, vector or matrix
    pub fn comps(&self) -> usize {
        match self {
            Ty::Scalar(_) => 1,
            Ty::Vec(_, n) => *n as usize,
            Ty::Mat(_, r, c) => (*r * *c) as usize,
            _ => 0,
        }
    }
    pub fn with_base(&self, b: B) -> Ty {
        match self {
            Ty::Scalar(_) => Ty::Scalar(b),
            Ty::Vec(_, n) => Ty::Vec(b, *n),
            Ty::Mat(_, r, c) => Ty::Mat(b, *r, *c),
            t => t.clone(),
        }
    }
    pub fn is_object(&self) -> bool {
        matches!(self, Ty::Tex(..) | Ty::Sampler(_) | Ty::Legacy(_))
    }
    /// vector (or scalar for 1) of `n`
    pub fn vec(b: B, n: usize) -> Ty {
        if n == 1 { Ty::Scalar(b) } else { Ty::Vec(b, n as u8) }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    And,
    Or,
    Xor,
    LAnd,
    LOr,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Un {
    Neg,
    Not,
    BitNot,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int(i64, bool),
    Float(f64),
    Bool(bool),
    Var(String, u32),
    Bin(Bin, Box<Expr>, Box<Expr>),
    Un(Un, Box<Expr>),
    /// compound operator, target, value
    Assign(Option<Bin>, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>, u32),
    Construct(Ty, Vec<Expr>),
    Cast(Ty, Box<Expr>),
    Member(Box<Expr>, String),
    Method(Box<Expr>, String, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    List(Vec<Expr>),
    Comma(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Interp {
    #[default]
    Linear,
    Centroid,
    NoInterp,
    NoPersp,
    NoPerspCentroid,
    Sample,
}

#[derive(Clone, Debug, Default)]
pub struct Var {
    pub name: String,
    pub ty: Option<Ty>,
    pub init: Option<Expr>,
    pub sem: Option<String>,
    /// register(t3) -> ('t', 3)
    pub reg: Option<(char, u32)>,
    /// packoffset(c1.y): byte offset
    pub packoffset: Option<u32>,
    pub is_static: bool,
    pub is_const: bool,
    pub uniform: bool,
    /// row_major (Some(true)) / column_major
    pub row_major: Option<bool>,
    pub interp: Interp,
    /// parameters: in (false, true default), out, inout
    pub is_in: bool,
    pub is_out: bool,
    pub line: u32,
}

impl Var {
    pub fn ty(&self) -> &Ty {
        self.ty.as_ref().unwrap()
    }
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Decl(Vec<Var>),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    While(Expr, Box<Stmt>),
    Do(Box<Stmt>, Expr),
    /// value, cases: (labels, None = default; statements)
    Switch(Expr, Vec<(Vec<Option<Expr>>, Vec<Stmt>)>),
    Break,
    Continue,
    Return(Option<Expr>, u32),
    Discard,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub sem: Option<String>,
    pub interp: Interp,
    pub row_major: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Struct {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    pub ret: Ty,
    pub ret_sem: Option<String>,
    pub params: Vec<Var>,
    pub body: Option<Vec<Stmt>>,
}

#[derive(Clone, Debug)]
pub struct CBuf {
    pub name: String,
    pub reg: Option<u32>,
    pub vars: Vec<Var>,
}

#[derive(Default, Debug)]
pub struct Program {
    pub structs: Vec<Struct>,
    /// globals outside cbuffers (uniforms, statics, objects)
    pub globals: Vec<Var>,
    pub cbufs: Vec<CBuf>,
    pub funcs: Vec<Func>,
}
