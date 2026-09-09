use crate::token::{Span, DocTag};

#[derive(Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Clone)]
pub enum Item {
    Func(FuncDecl),
    Task(TaskDecl),
    Chan(ChanDecl),
    Extern(ExternDecl),
    Record(RecordDecl),
    Enum(EnumDecl),
}

#[derive(Clone)]
pub struct FuncDecl {
    pub is_async: bool,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub contracts: Vec<Contract>,
    pub body: Block,
    pub doc: Option<DocComment>,
    pub span: Span,
}

#[derive(Clone)]
pub struct TypeParam {
    pub name: String,
    pub constraint: Option<String>,
    pub span: Span,
}

#[derive(Clone)]
pub struct TaskDecl {
    pub name: String,
    pub body: Block,
    pub doc: Option<DocComment>,
    pub span: Span,
}

#[derive(Clone)]
pub struct ChanDecl {
    pub name: String,
    pub payload: Type,
    pub span: Span,
}

#[derive(Clone)]
pub struct ExternDecl {
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub span: Span,
}

#[derive(Clone)]
pub struct RecordDecl {
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Clone)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<Variant>,
    pub span: Span,
}

#[derive(Clone)]
pub struct Param {
    pub mutable: bool,
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Clone)]
pub enum Contract {
    Pre(Expr),
    Post(Expr),
    Invariant(Expr),
}

#[derive(Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone)]
pub enum Stmt {
    Decl(Decl),
    Assign(Assign),
    Expr(Expr),
    Break,
    Continue,
}

#[derive(Clone)]
pub struct Decl {
    pub mutable: bool,
    pub name: String,
    pub ty: Option<Type>,
    pub init: Option<Box<Expr>>,
    pub recv: bool,       // true when `let x <- recv(ch)`
    pub chan: Option<String>,
    pub span: Span,
}

#[derive(Clone)]
pub struct Assign {
    pub target: AssignTarget,
    pub op: AssignOp,
    pub value: Box<Expr>,
    pub span: Span,
}

#[derive(Clone)]
pub enum AssignTarget {
    Name(String),
    Field(Box<Expr>, String),
}

#[derive(Clone)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone)]
pub struct DocComment {
    pub claims: Vec<DocClaim>,
    pub span: Span,
}

#[derive(Clone)]
pub struct DocClaim {
    pub tag: DocTag,
    pub text: String,
    pub trusted: bool,
    pub reviewed: bool,
    pub span: Span,
}

#[derive(Clone)]
pub enum Type {
    Named(String, Vec<Type>),
    Union(Vec<Type>),
    NamedPlain(String),
}

#[derive(Clone)]
pub enum Expr {
    Int(u64),
    Float(f64),
    Char(char),
    Str(String),
    RawStr(String),
    Bool(bool),
    None,
    Name(String),
    Call(Box<Expr>, Vec<Expr>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Prop(Box<Expr>, PropKind),       // postfix `?`
    Unary(UnaryOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Cast(Box<Expr>, Type),
    Match(Box<Expr>, Vec<(Pattern, Box<Expr>)>),
    If(IfExpr),
    Loop(Block),
    For(String, Box<Expr>, Block),
    Await(Box<Expr>),
    Send(Box<Expr>, Box<Expr>),
    Recv(String),
    Transfer(Box<Expr>),
    Ok(Option<Box<Expr>>),
    Err(Box<Expr>),
    Some(Box<Expr>),
}

#[derive(Clone)]
pub struct IfExpr {
    pub cond: Box<Expr>,
    pub then_block: Block,
    pub elif: Option<(Box<Expr>, Block)>,
    pub else_block: Option<Block>,
}

#[derive(Clone)]
pub enum PropKind {
    Fallible,      // `?`
}

#[derive(Clone)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Implies,
    IsOk,
    IsErr,
    IsNone,
    IsSome,
}

#[derive(Clone)]
pub enum Pattern {
    Wildcard,
    NoneLit,
    Name(String),
    Variant(String, Vec<String>),
}