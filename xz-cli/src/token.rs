#[derive(Clone, PartialEq)]
pub enum TokKind {
    // literals
    Int(u64),
    Float(f64),
    Char(char),
    Str(String),
    RawStr(String),
    Ident(String),
    True,
    False,
    None,

    // keywords
    And,
    As,
    Async,
    Await,
    Break,
    Chan,
    Continue,
    Elif,
    Else,
    Enum,
    ErrKw,
    Extern,
    For,
    Func,
    If,
    Implies,
    In,
    Invariant,
    Is,
    Let,
    Loop,
    Match,
    Mut,
    Not,
    Ok,
    Or,
    Post,
    Pre,
    Recv,
    Record,
    Send,
    Some,
    Task,
    Transfer,

    // punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    Arrow,
    LeftArrow,
    Question,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    Pipe,
    Underscore,

    // doc/comment tokens
    DocIntent,
    DocRequires,
    DocEnsures,
    DocEffects,
    DocTrusted,

    Eof,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub file: String,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

#[derive(Clone)]
pub struct Token {
    pub kind: TokKind,
    pub span: Span,
    pub text: String,
}

#[derive(Clone)]
pub enum DocTag {
    Intent,
    Requires,
    Ensures,
    Effects,
    Trusted,
}

pub struct DocClaim {
    pub tag: DocTag,
    pub text: String,
    pub trusted: bool,
    pub span: Span,
}

pub struct LexError {
    pub message: String,
    pub span: Span,
}