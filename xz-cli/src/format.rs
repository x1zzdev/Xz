//! `xz fmt`: a comment-preserving AST pretty-printer (docs/07-compiler.md).
//!
//! The printer walks the parsed program and emits a canonical layout while
//! interleaving the comments the token stream drops. Comments are placed by
//! source position: before each top-level declaration or statement the printer
//! flushes any comment that starts before it, as its own line at that
//! indentation; a comment on the same line as a statement's start is kept
//! trailing. Every statement carries a start span for exactly this reason.
use crate::ast::{
    Assign, AssignOp, AssignTarget, BinOp, Block, Contract, Decl, EnumDecl, Expr, FuncDecl, IfExpr,
    Item, Param, Pattern, Program, RecordDecl, Stmt, StmtKind, Type, TypeParam, UnaryOp, Variant,
};
use crate::lexer::lex_with_comments;
use crate::parser::parse;
use crate::token::Comment;

/// Format `source` and return the canonical rendering, or a human-readable
/// lex/parse error (same wording the CLI prints) when the file is not
/// well-formed. The formatter never emits output for a file it cannot parse.
pub fn format_source(source: &str, file: &str) -> Result<String, String> {
    let (tokens, comments) = lex_with_comments(source.to_string(), file.to_string())
        .map_err(|e| at(&e.message, &e.span))?;
    let program = parse(tokens).map_err(|e| at(&e.message, &e.span))?;
    Ok(Printer::new(comments).program(&program))
}

fn at(message: &str, span: &crate::token::Span) -> String {
    format!("{} at {}:{}:{}", message, span.file, span.start.0, span.start.1)
}

// Operator precedence, lowest to highest (docs/11-grammar.md).
const P_IMPLIES: u8 = 1;
const P_OR: u8 = 2;
const P_AND: u8 = 3;
const P_NOT: u8 = 4;
const P_CAST: u8 = 5;
const P_CMP: u8 = 6;
const P_ADD: u8 = 7;
const P_MUL: u8 = 8;
const P_UNARY: u8 = 9;
const P_POSTFIX: u8 = 10;
const P_PRIMARY: u8 = 11;

struct Printer {
    out: String,
    comments: Vec<Comment>,
    ci: usize,
    last_line: usize,
}

impl Printer {
    fn new(comments: Vec<Comment>) -> Printer {
        Printer { out: String::new(), comments: comments, ci: 0, last_line: 0 }
    }

    fn program(mut self, program: &Program) -> String {
        for item in &program.items {
            self.emit_item(item);
        }
        self.flush_before(&(usize::MAX, usize::MAX), 0);
        self.out
    }

    /// Preserve at most one blank line: emit one when the next construct
    /// starts two or more source lines after the last emitted line.
    fn blank_if_gap(&mut self, line: usize) {
        if self.last_line > 0 && line >= self.last_line + 2 {
            self.out.push('\n');
        }
    }

    // ---- comment placement ----

    fn flush_before(&mut self, pos: &(usize, usize), indent: usize) {
        while self.ci < self.comments.len() && before(&self.comments[self.ci].span.start, pos) {
            let c = self.comments[self.ci].clone();
            self.ci += 1;
            self.blank_if_gap(c.span.start.0);
            self.emit_comment(&c, indent);
        }
    }

    fn emit_comment(&mut self, c: &Comment, indent: usize) {
        let pad = indent_str(indent);
        for (i, line) in c.text.trim_end().lines().enumerate() {
            if i > 0 {
                self.out.push('\n');
            }
            self.out.push_str(&pad);
            self.out.push_str(line);
        }
        self.out.push('\n');
        self.last_line = c.span.end.0;
    }

    /// Emit any comments that start on the same line as the construct's start
    /// or end position, kept inline and separated by two spaces. Matching the
    /// end line too keeps the output idempotent when a comment inside a
    /// multi-line statement was attached after its closing brace.
    fn flush_trailing(&mut self, start: &(usize, usize), end: &(usize, usize)) {
        let mut first = true;
        while self.ci < self.comments.len() {
            let c = &self.comments[self.ci];
            let on_start = c.span.start.0 == start.0 && !before(&c.span.start, start);
            let on_end = c.span.start.0 == end.0 && !before(&c.span.start, end);
            if !(on_start || on_end) {
                break;
            }
            let c = c.clone();
            self.ci += 1;
            if first {
                self.out.push_str("  ");
                first = false;
            } else {
                self.out.push(' ');
            }
            self.out.push_str(c.text.trim_end());
        }
    }

    // ---- top-level ----

    fn emit_item(&mut self, item: &Item) {
        let (start, end) = item_bounds(item);
        self.flush_before(&start, 0);
        self.blank_if_gap(start.0);
        match item {
            Item::Func(f) => self.emit_func(f),
            Item::Task(t) => {
                self.out.push_str(&format!("task {} ", t.name));
                self.emit_block(&t.body, 0);
            }
            Item::Chan(c) => {
                self.out.push_str(&format!("chan {}: Chan[{}]", c.name, self.type_str(&c.payload)));
            }
            Item::Extern(e) => {
                let mut s = String::from("extern func ");
                s.push_str(&e.name);
                s.push_str(&self.type_params_str(&e.type_params));
                s.push_str(&self.params_str(&e.params));
                if let Some(ret) = &e.ret {
                    s.push_str(" -> ");
                    s.push_str(&self.type_str(ret));
                }
                self.out.push_str(&s);
            }
            Item::Record(r) => self.emit_record(r),
            Item::Enum(e) => self.emit_enum(e),
        }
        self.flush_trailing(&start, &end);
        self.out.push('\n');
        self.last_line = end.0;
    }

    fn emit_func(&mut self, f: &FuncDecl) {
        let mut sig = String::new();
        if f.exported {
            sig.push_str("@export ");
        }
        if f.is_async {
            sig.push_str("async ");
        }
        sig.push_str("func ");
        sig.push_str(&f.name);
        sig.push_str(&self.type_params_str(&f.type_params));
        sig.push_str(&self.params_str(&f.params));
        if let Some(ret) = &f.ret {
            sig.push_str(" -> ");
            sig.push_str(&self.type_str(ret));
        }
        self.out.push_str(&sig);
        if f.contracts.is_empty() {
            self.out.push(' ');
            self.emit_block(&f.body, 0);
        } else {
            self.out.push('\n');
            for c in &f.contracts {
                let (kw, e) = match c {
                    Contract::Pre(e) => ("pre", e),
                    Contract::Post(e) => ("post", e),
                    Contract::Invariant(e) => ("invariant", e),
                };
                self.out.push_str(&indent_str(1));
                self.out.push_str(kw);
                self.out.push(' ');
                self.emit_expr(e, 1, 0);
                self.out.push('\n');
            }
            self.emit_block(&f.body, 0);
        }
    }

    fn emit_record(&mut self, r: &RecordDecl) {
        self.last_line = r.span.start.0;
        let head = if r.cstruct { "@cstruct record " } else { "record " };
        self.out.push_str(head);
        self.out.push_str(&r.name);
        if r.fields.is_empty() {
            self.out.push_str(" {}");
            return;
        }
        self.out.push_str(" {\n");
        for f in &r.fields {
            self.flush_before(&f.span.start, 1);
            self.out.push_str(&indent_str(1));
            self.out.push_str(&format!("{}: {}\n", f.name, self.type_str(&f.ty)));
        }
        self.flush_before(&r.span.end, 1);
        self.out.push('}');
    }

    fn emit_enum(&mut self, e: &EnumDecl) {
        self.last_line = e.span.start.0;
        self.out.push_str(&format!("enum {} {{\n", e.name));
        for v in &e.variants {
            self.flush_before(&v.span.start, 1);
            self.out.push_str(&indent_str(1));
            self.out.push_str(&self.variant_str(v));
            self.out.push('\n');
        }
        self.flush_before(&e.span.end, 1);
        self.out.push('}');
    }

    // ---- blocks and statements ----

    fn emit_block(&mut self, block: &Block, indent: usize) {
        self.last_line = block.span.start.0;
        self.out.push_str("{\n");
        for stmt in &block.stmts {
            self.emit_stmt(stmt, indent + 1);
        }
        self.flush_before(&block.span.end, indent + 1);
        self.out.push_str(&indent_str(indent));
        self.out.push('}');
        self.last_line = block.span.end.0;
    }

    fn emit_stmt(&mut self, stmt: &Stmt, indent: usize) {
        self.flush_before(&stmt.span.start, indent);
        self.blank_if_gap(stmt.span.start.0);
        self.out.push_str(&indent_str(indent));
        match &stmt.kind {
            StmtKind::Decl(d) => self.emit_decl(d, indent),
            StmtKind::Assign(a) => self.emit_assign(a, indent),
            StmtKind::Expr(e) => self.emit_expr(e, indent, 0),
            StmtKind::Break => self.out.push_str("break"),
            StmtKind::Continue => self.out.push_str("continue"),
        }
        self.flush_trailing(&stmt.span.start, &stmt.span.end);
        self.out.push('\n');
        self.last_line = stmt.span.end.0;
    }

    fn emit_decl(&mut self, d: &Decl, indent: usize) {
        self.out.push_str(if d.mutable { "mut " } else { "let " });
        self.out.push_str(&d.name);
        if d.recv {
            self.out.push_str(" <- recv(");
            self.out.push_str(d.chan.as_deref().unwrap_or(""));
            self.out.push(')');
            return;
        }
        if let Some(ty) = &d.ty {
            self.out.push_str(": ");
            self.out.push_str(&self.type_str(ty));
        }
        if let Some(init) = &d.init {
            self.out.push_str(" = ");
            self.emit_expr(init, indent, 0);
        }
    }

    fn emit_assign(&mut self, a: &Assign, indent: usize) {
        match &a.target {
            AssignTarget::Name(n) => self.out.push_str(n),
            AssignTarget::Field(base, name) => {
                self.emit_expr(base, indent, P_POSTFIX);
                self.out.push('.');
                self.out.push_str(name);
            }
        }
        let op = match a.op {
            AssignOp::Set => " = ",
            AssignOp::Add => " += ",
            AssignOp::Sub => " -= ",
            AssignOp::Mul => " *= ",
            AssignOp::Div => " /= ",
        };
        self.out.push_str(op);
        self.emit_expr(&a.value, indent, 0);
    }

    // ---- expressions ----

    fn emit_expr(&mut self, e: &Expr, indent: usize, min: u8) {
        if prec_of(e) < min {
            self.out.push('(');
            self.emit_expr_inner(e, indent);
            self.out.push(')');
        } else {
            self.emit_expr_inner(e, indent);
        }
    }

    fn emit_expr_inner(&mut self, e: &Expr, indent: usize) {
        match e {
            Expr::Int(v) => self.out.push_str(&v.to_string()),
            Expr::Float(v) => self.out.push_str(&format!("{:?}", v)),
            Expr::Char(c) => self.out.push_str(&char_lit(*c)),
            Expr::Str(s) => self.out.push_str(&str_lit(s)),
            Expr::RawStr(s) => {
                self.out.push_str("r\"");
                self.out.push_str(s);
                self.out.push('"');
            }
            Expr::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
            Expr::None => self.out.push_str("none"),
            Expr::Name(n) => self.out.push_str(n),
            Expr::Call(f, args) => {
                self.emit_expr(f, indent, P_POSTFIX);
                self.out.push('(');
                self.emit_args(args, indent);
                self.out.push(')');
            }
            Expr::Field(base, name) => {
                self.emit_expr(base, indent, P_POSTFIX);
                self.out.push('.');
                self.out.push_str(name);
            }
            Expr::Index(base, idx) => {
                self.emit_expr(base, indent, P_POSTFIX);
                self.out.push('[');
                self.emit_expr(idx, indent, 0);
                self.out.push(']');
            }
            Expr::Prop(base, _) => {
                self.emit_expr(base, indent, P_POSTFIX);
                self.out.push('?');
            }
            Expr::ListLit(xs) => {
                self.out.push('[');
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(x, indent, 0);
                }
                self.out.push(']');
            }
            Expr::MapLit(entries) => {
                self.out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(k, indent, 0);
                    self.out.push_str(": ");
                    self.emit_expr(v, indent, 0);
                }
                self.out.push('}');
            }
            Expr::SetLit(xs) => {
                self.out.push('{');
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(x, indent, 0);
                }
                self.out.push('}');
            }
            Expr::Binary(op, l, r) => {
                if let Some(kw) = is_kw(op) {
                    self.emit_expr(l, indent, P_CMP);
                    self.out.push_str(" is ");
                    self.out.push_str(kw);
                } else {
                    let prec = binop_prec(op);
                    let (lmin, rmin) = if *op == BinOp::Implies { (prec + 1, prec) } else { (prec, prec + 1) };
                    self.emit_expr(l, indent, lmin);
                    self.out.push(' ');
                    self.out.push_str(&binop_str(op));
                    self.out.push(' ');
                    self.emit_expr(r, indent, rmin);
                }
            }
            Expr::Unary(UnaryOp::Neg, x) => {
                self.out.push('-');
                self.emit_expr(x, indent, P_UNARY);
            }
            Expr::Unary(UnaryOp::Not, x) => {
                self.out.push_str("not ");
                self.emit_expr(x, indent, P_NOT);
            }
            Expr::Cast(x, ty) => {
                self.emit_expr(x, indent, P_CAST);
                self.out.push_str(" as ");
                self.out.push_str(&self.type_str(ty));
            }
            Expr::Await(x) => {
                self.out.push_str("await ");
                self.emit_expr(x, indent, 0);
            }
            Expr::Send(ch, val) => {
                self.out.push_str("send(");
                self.emit_expr(ch, indent, 0);
                self.out.push_str(", ");
                self.emit_expr(val, indent, 0);
                self.out.push(')');
            }
            Expr::Recv(ch) => self.out.push_str(&format!("recv({})", ch)),
            Expr::Transfer(x) => {
                self.out.push_str("transfer(");
                self.emit_expr(x, indent, 0);
                self.out.push(')');
            }
            Expr::Ok(inner) => {
                self.out.push_str("ok(");
                if let Some(x) = inner {
                    self.emit_expr(x, indent, 0);
                }
                self.out.push(')');
            }
            Expr::Err(x) => {
                self.out.push_str("err(");
                self.emit_expr(x, indent, 0);
                self.out.push(')');
            }
            Expr::Some(x) => {
                self.out.push_str("some(");
                self.emit_expr(x, indent, 0);
                self.out.push(')');
            }
            Expr::If(ifx) => self.emit_if(ifx, indent),
            Expr::Match(subject, arms) => {
                self.out.push_str("match ");
                self.emit_expr(subject, indent, 0);
                self.out.push_str(" {\n");
                for (pat, body) in arms {
                    self.out.push_str(&indent_str(indent + 1));
                    self.out.push_str(&self.pattern_str(pat));
                    self.out.push_str(" -> ");
                    self.emit_expr(body, indent + 1, 0);
                    self.out.push('\n');
                }
                self.out.push_str(&indent_str(indent));
                self.out.push('}');
            }
            Expr::Loop(block) => {
                self.out.push_str("loop ");
                self.emit_block(block, indent);
            }
            Expr::For(name, iter, block) => {
                self.out.push_str("for ");
                self.out.push_str(name);
                self.out.push_str(" in ");
                self.emit_expr(iter, indent, 0);
                self.out.push(' ');
                self.emit_block(block, indent);
            }
        }
    }

    fn emit_if(&mut self, ifx: &IfExpr, indent: usize) {
        self.out.push_str("if ");
        self.emit_expr(&ifx.cond, indent, 0);
        self.out.push(' ');
        self.emit_block(&ifx.then_block, indent);
        for (cond, block) in &ifx.elif {
            self.out.push_str(" elif ");
            self.emit_expr(cond, indent, 0);
            self.out.push(' ');
            self.emit_block(block, indent);
        }
        if let Some(block) = &ifx.else_block {
            self.out.push_str(" else ");
            self.emit_block(block, indent);
        }
    }

    fn emit_args(&mut self, args: &[Expr], indent: usize) {
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.emit_expr(a, indent, 0);
        }
    }

    // ---- signatures and types ----

    fn type_params_str(&self, params: &[TypeParam]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let items: Vec<String> = params
            .iter()
            .map(|p| match &p.constraint {
                Some(c) => format!("{}: {}", p.name, c),
                None => p.name.clone(),
            })
            .collect();
        format!("[{}]", items.join(", "))
    }

    fn params_str(&self, params: &[Param]) -> String {
        let items: Vec<String> = params
            .iter()
            .map(|p| {
                let m = if p.mutable { "mut " } else { "" };
                format!("{}{}: {}", m, p.name, self.type_str(&p.ty))
            })
            .collect();
        format!("({})", items.join(", "))
    }

    fn variant_str(&self, v: &Variant) -> String {
        let fields: Vec<String> = v
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, self.type_str(&f.ty)))
            .collect();
        format!("{}({})", v.name, fields.join(", "))
    }

    fn pattern_str(&self, p: &Pattern) -> String {
        match p {
            Pattern::Wildcard => "_".to_string(),
            Pattern::NoneLit => "none".to_string(),
            Pattern::Name(n) => n.clone(),
            Pattern::Variant(name, names) => format!("{}({})", name, names.join(", ")),
        }
    }

    fn type_str(&self, t: &Type) -> String {
        match t {
            Type::Named(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let items: Vec<String> = args.iter().map(|a| self.type_str(a)).collect();
                    format!("{}[{}]", name, items.join(", "))
                }
            }
            Type::NamedPlain(name) => name.clone(),
            Type::Union(members) => {
                let items: Vec<String> = members.iter().map(|m| self.type_str(m)).collect();
                items.join(" | ")
            }
        }
    }
}

fn item_bounds(item: &Item) -> ((usize, usize), (usize, usize)) {
    match item {
        Item::Func(f) => (f.span.start, f.body.span.end),
        Item::Task(t) => (t.span.start, t.body.span.end),
        Item::Chan(c) => (c.span.start, c.span.end),
        Item::Extern(e) => (e.span.start, e.span.end),
        Item::Record(r) => (r.span.start, r.span.end),
        Item::Enum(e) => (e.span.start, e.span.end),
    }
}

fn before(a: &(usize, usize), b: &(usize, usize)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 < b.1)
}

fn indent_str(indent: usize) -> String {
    " ".repeat(indent * 4)
}

fn is_kw(op: &BinOp) -> Option<&'static str> {
    match op {
        BinOp::IsOk => Some("ok"),
        BinOp::IsErr => Some("err"),
        BinOp::IsNone => Some("none"),
        BinOp::IsSome => Some("some"),
        _ => None,
    }
}

fn binop_str(op: &BinOp) -> String {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Implies => "implies",
        BinOp::IsOk | BinOp::IsErr | BinOp::IsNone | BinOp::IsSome => "is",
    }
    .to_string()
}

fn binop_prec(op: &BinOp) -> u8 {
    match op {
        BinOp::Implies => P_IMPLIES,
        BinOp::Or => P_OR,
        BinOp::And => P_AND,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => P_CMP,
        BinOp::IsOk | BinOp::IsErr | BinOp::IsNone | BinOp::IsSome => P_CMP,
        BinOp::Add | BinOp::Sub => P_ADD,
        BinOp::Mul | BinOp::Div | BinOp::Mod => P_MUL,
    }
}

fn prec_of(e: &Expr) -> u8 {
    match e {
        Expr::Binary(op, _, _) => {
            if is_kw(op).is_some() {
                P_CMP
            } else {
                binop_prec(op)
            }
        }
        Expr::Unary(UnaryOp::Neg, _) => P_UNARY,
        Expr::Unary(UnaryOp::Not, _) => P_NOT,
        Expr::Cast(_, _) => P_CAST,
        Expr::Call(_, _)
        | Expr::Field(_, _)
        | Expr::Index(_, _)
        | Expr::Prop(_, _) => P_POSTFIX,
        _ => P_PRIMARY,
    }
}

fn char_lit(c: char) -> String {
    match c {
        '\n' => "'\\n'".to_string(),
        '\t' => "'\\t'".to_string(),
        '\r' => "'\\r'".to_string(),
        '\\' => "'\\\\'".to_string(),
        '\'' => "'\\''".to_string(),
        '"' => "'\\\"'".to_string(),
        _ => format!("'{}'", c),
    }
}

fn str_lit(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
