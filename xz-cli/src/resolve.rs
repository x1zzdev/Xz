use std::collections::HashSet;

use crate::ast;
use crate::ast::{Program, Item, Type, Expr, Stmt, Pattern};
use crate::token::Span;

pub struct ResolveError {
    pub message: String,
    pub span: Span,
}

/// A scope of in-scope names for expression resolution.
struct Scope {
    names: HashSet<String>,
}

impl Scope {
    fn new() -> Scope {
        Scope { names: HashSet::new() }
    }

    fn insert(&mut self, name: String) {
        self.names.insert(name);
    }

    fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

pub struct Resolver {
    /// Top-level value symbols: functions, tasks, channels, externs, and
    /// enum variants (all callable/addressable by bare name).
    values: HashSet<String>,
    /// Top-level type symbols: records, enums, and their variants are type-
    /// space names; type references are checked against this set.
    types: HashSet<String>,
    errors: Vec<ResolveError>,
}

pub fn resolve(program: &Program) -> Result<(), Vec<ResolveError>> {
    let mut r = Resolver {
        values: HashSet::new(),
        types: stdlib_types(),
        errors: vec![],
    };
    r.collect_top(program);
    if r.errors.len() > 0 {
        return Err(r.errors);
    }
    r.resolve_bodies(program);
    if r.errors.len() > 0 {
        return Err(r.errors);
    }
    Ok(())
}

impl Resolver {
    fn error(&mut self, message: String, span: Span) {
        self.errors.push(ResolveError { message: message, span: span });
    }

    fn collect_top(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                Item::Func(f) => {
                    if !self.values.insert(f.name.clone()) {
                        self.error(format!("duplicate symbol '{}'", f.name), f.span.clone());
                    }
                }
                Item::Task(t) => {
                    if !self.values.insert(t.name.clone()) {
                        self.error(format!("duplicate symbol '{}'", t.name), t.span.clone());
                    }
                }
                Item::Chan(c) => {
                    if !self.values.insert(c.name.clone()) {
                        self.error(format!("duplicate symbol '{}'", c.name), c.span.clone());
                    }
                }
                Item::Extern(e) => {
                    if !self.values.insert(e.name.clone()) {
                        self.error(format!("duplicate symbol '{}'", e.name), e.span.clone());
                    }
                }
                Item::Record(rec) => {
                    if !self.types.insert(rec.name.clone()) {
                        self.error(format!("duplicate type '{}'", rec.name), rec.span.clone());
                    }
                }
                Item::Enum(en) => {
                    if !self.types.insert(en.name.clone()) {
                        self.error(format!("duplicate type '{}'", en.name), en.span.clone());
                    }
                    for v in &en.variants {
                        if !self.values.insert(v.name.clone()) {
                            self.error(format!("duplicate symbol '{}'", v.name), v.span.clone());
                        }
                    }
                }
            }
        }
    }

    fn resolve_bodies(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                Item::Func(f) => {
                    let mut scope = Scope::new();
                    for p in &f.params {
                        scope.insert(p.name.clone());
                    }
                    // stdlib functions are trusted; references to them are
                    // predeclared (see stdlib surface). We add common ones so
                    // examples resolve. Phase 2 replaces this with real typing.
                    self.predeclare_stdlib(&mut scope);
                    self.resolve_contracts(&f.contracts, &scope);
                    self.resolve_block(&f.body, &mut scope);
                }
                Item::Task(t) => {
                    let mut scope = Scope::new();
                    self.predeclare_stdlib(&mut scope);
                    self.resolve_block(&t.body, &mut scope);
                }
                Item::Record(rec) => {
                    self.resolve_type_opt_fields(&rec.fields);
                }
                Item::Enum(en) => {
                    for v in &en.variants {
                        for field in &v.fields {
                            self.resolve_type(&field.ty);
                        }
                    }
                }
                Item::Chan(c) => {
                    self.resolve_type(&c.payload);
                }
                Item::Extern(e) => {
                    for p in &e.params {
                        self.resolve_type(&p.ty);
                    }
                    match &e.ret {
                        Some(t) => self.resolve_type(t),
                        None => {}
                    }
                }
            }
        }
    }

    fn predeclare_stdlib(&self, scope: &mut Scope) {
        // stdlib functions and methods (see stdlib surface)
        for n in ["print", "to_str", "len", "is_empty", "to_upper", "to_lower",
                  "at", "to_bytes", "abs", "is_some", "is_none", "approx_sqrt",
                  "PI", "E"] {
            scope.insert(n.to_string());
        }
        // error records are value constructors too
        for t in stdlib_error_types() {
            scope.insert(t);
        }
    }

    fn resolve_contracts(&mut self, contracts: &Vec<ast::Contract>, scope: &Scope) {
        for c in contracts {
            match c {
                ast::Contract::Pre(e) => self.resolve_expr(e, scope),
                ast::Contract::Post(e) => {
                    let mut post_scope = child_scope(scope);
                    post_scope.insert("result".to_string());
                    self.resolve_expr(e, &post_scope);
                }
                ast::Contract::Invariant(e) => self.resolve_expr(e, scope),
            }
        }
    }

    fn resolve_type_opt_fields(&mut self, fields: &Vec<ast::Field>) {
        for f in fields {
            self.resolve_type(&f.ty);
        }
    }

    fn resolve_type(&mut self, ty: &Type) {
        match ty {
            Type::Named(name, args) => {
                if !is_builtin_type(name) && !self.types.contains(name) {
                    self.error(format!("unknown type '{}'", name), Span { file: "".to_string(), start: (0, 0), end: (0, 0) });
                }
                for a in args {
                    self.resolve_type(a);
                }
            }
            Type::Union(members) => {
                for m in members {
                    self.resolve_type(m);
                }
            }
            Type::NamedPlain(name) => {
                if !is_builtin_type(name) && !self.types.contains(name) {
                    self.error(format!("unknown type '{}'", name), Span { file: "".to_string(), start: (0, 0), end: (0, 0) });
                }
            }
        }
    }

    fn resolve_block(&mut self, block: &ast::Block, scope: &mut Scope) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Decl(d) => {
                    match &d.init {
                        Some(e) => self.resolve_expr(e, scope),
                        None => {}
                    }
                    if d.recv {
                        match &d.chan {
                            Some(ch) => {
                                if !scope.contains(ch) && !self.values.contains(ch) {
                                    self.error(format!("unknown channel '{}'", ch), d.span.clone());
                                }
                            }
                            None => {}
                        }
                    }
                    scope.insert(d.name.clone());
                }
                Stmt::Assign(a) => {
                    match &a.target {
                        ast::AssignTarget::Name(n) => {
                            if !scope.contains(n) {
                                self.error(format!("unknown name '{}'", n), a.span.clone());
                            }
                        }
                        ast::AssignTarget::Field(base, _) => self.resolve_expr(base, scope),
                    }
                    self.resolve_expr(&a.value, scope);
                }
                Stmt::Expr(e) => self.resolve_expr(e, scope),
                Stmt::Break => {}
                Stmt::Continue => {}
            }
        }
    }

    fn resolve_expr(&mut self, e: &Expr, scope: &Scope) {
        match e {
            Expr::Int(_) => {}
            Expr::Float(_) => {}
            Expr::Char(_) => {}
            Expr::Str(_) => {}
            Expr::RawStr(_) => {}
            Expr::Bool(_) => {}
            Expr::None => {}
            Expr::Name(n) => {
                if !scope.contains(n) && !self.values.contains(n) && !is_builtin_type(n) && !self.types.contains(n) {
                    self.error(format!("unknown name '{}'", n), Span { file: "".to_string(), start: (0, 0), end: (0, 0) });
                }
            }
            Expr::Call(callee, args) => {
                self.resolve_expr(callee, scope);
                for a in args {
                    self.resolve_expr(a, scope);
                }
            }
            Expr::Field(base, _) => self.resolve_expr(base, scope),
            Expr::Index(base, idx) => {
                self.resolve_expr(base, scope);
                self.resolve_expr(idx, scope);
            }
            Expr::ListLit(elems) => {
                for e in elems {
                    self.resolve_expr(e, scope);
                }
            }
            Expr::Prop(base, _) => self.resolve_expr(base, scope),
            Expr::Unary(op, a) => {
                let _ = op;
                self.resolve_expr(a, scope);
            }
            Expr::Binary(op, a, b) => {
                self.resolve_expr(a, scope);
                self.resolve_expr(b, scope);
                let _ = op;
            }
            Expr::Cast(a, ty) => {
                self.resolve_expr(a, scope);
                self.resolve_type(ty);
            }
            Expr::Match(subject, arms) => {
                self.resolve_expr(subject, scope);
                for (pat, body) in arms {
                    let mut inner = Scope::new();
                    for n in scope.names.iter() {
                        inner.insert(n.clone());
                    }
                    bind_pattern(&mut inner, pat);
                    self.resolve_expr(body, &inner);
                }
            }
            Expr::If(ifx) => {
                self.resolve_expr(&ifx.cond, scope);
                let mut then_scope = child_scope(scope);
                self.resolve_block(&ifx.then_block, &mut then_scope);
                for (c, b) in &ifx.elif {
                    self.resolve_expr(c, scope);
                    let mut s = child_scope(scope);
                    self.resolve_block(b, &mut s);
                }
                match &ifx.else_block {
                    Some(b) => {
                        let mut s = child_scope(scope);
                        self.resolve_block(b, &mut s);
                    }
                    None => {}
                }
            }
            Expr::Loop(b) => {
                let mut inner = child_scope(scope);
                self.resolve_block(b, &mut inner);
            }
            Expr::For(name, iter, b) => {
                self.resolve_expr(iter, scope);
                let mut inner = child_scope(scope);
                inner.insert(name.clone());
                self.resolve_block(b, &mut inner);
            }
            Expr::Await(a) => self.resolve_expr(a, scope),
            Expr::Send(ch, value) => {
                self.resolve_expr(ch, scope);
                self.resolve_expr(value, scope);
            }
            Expr::Recv(ch) => {
                if !scope.contains(ch) && !self.values.contains(ch) {
                    self.error(format!("unknown channel '{}'", ch), Span { file: "".to_string(), start: (0, 0), end: (0, 0) });
                }
            }
            Expr::Transfer(a) => self.resolve_expr(a, scope),
            Expr::Ok(inner) => {
                match inner {
                    Some(v) => self.resolve_expr(v, scope),
                    None => {}
                }
            }
            Expr::Err(a) => self.resolve_expr(a, scope),
            Expr::Some(a) => self.resolve_expr(a, scope),
        }
    }
}

fn child_scope(scope: &Scope) -> Scope {
    let mut s = Scope::new();
    for n in scope.names.iter() {
        s.insert(n.clone());
    }
    s
}

fn bind_pattern(scope: &mut Scope, pat: &Pattern) {
    match pat {
        Pattern::Wildcard => {}
        Pattern::NoneLit => {}
        Pattern::Name(n) => {
            scope.insert(n.clone());
        }
        Pattern::Variant(_, names) => {
            for n in names {
                scope.insert(n.clone());
            }
        }
    }
}

fn is_builtin_type(name: &str) -> bool {
    match name {
        "Bool" | "Int" | "usize" | "Float" | "Char" | "Str" | "Bytes" | "Unit" | "Ptr" => true,
        _ => false,
    }
}

fn stdlib_error_types() -> Vec<String> {
    ["IoError", "DomainError", "ParseError", "AllocError",
     "IndexError", "DecodeError", "HttpError", "Err"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn stdlib_types() -> HashSet<String> {
    let mut set = HashSet::new();
    for t in stdlib_error_types() {
        set.insert(t);
    }
    set
}