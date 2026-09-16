use std::collections::{HashMap, HashSet};

use crate::ast;
use crate::ast::{Program, Item, Type, Expr, StmtKind, BinOp, UnaryOp, Contract};
use crate::token::Span;

/// Scope map from name to type plus whether the binding is `mut`
/// (docs/04, docs/11): mutation requires an explicit `mut` binding.
#[derive(Clone, Default)]
struct Env {
    types: HashMap<String, Kind>,
    mutables: HashSet<String>,
}

impl Env {
    fn get(&self, n: &str) -> Option<&Kind> {
        self.types.get(n)
    }

    fn insert(&mut self, n: String, ty: Kind) {
        self.types.insert(n, ty);
    }

    fn insert_binding(&mut self, n: String, ty: Kind, mutable: bool) {
        if mutable {
            self.mutables.insert(n.clone());
        } else {
            self.mutables.remove(&n);
        }
        self.types.insert(n, ty);
    }

    fn is_mut(&self, n: &str) -> bool {
        self.mutables.contains(n)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    Int,
    Usize,
    Float,
    Char,
    Str,
    Bytes,
    Unit,
    Ptr,
    Option(Box<Kind>),
    Result(Box<Kind>, Box<Kind>),
    List(Box<Kind>),
    Map(Box<Kind>, Box<Kind>),
    Set(Box<Kind>),
    Chan(Box<Kind>),
    Record(String),
    Enum(String),
    Err,                       // root error type
    ErrUnion(Vec<Kind>),       // error union E1 | E2 (docs/06)
    TypeVar(usize),
    Unknown,
    Never,
}

pub struct TypeError {
    pub message: String,
    pub span: Span,
}

pub struct TypeChecker {
    /// type declarations by name (record, enum, aliases)
    decls: HashMap<String, Kind>,
    /// record/enum field types: (record_name, field) -> type
    fields: HashMap<(String, String), Kind>,
    /// enum variant -> (enum_name, field_types)
    variants: HashMap<String, (String, Vec<Kind>)>,
    /// enum name -> variant names (for exhaustiveness)
    enum_variants: HashMap<String, Vec<String>>,
    /// function signatures: name -> (params, ret, type-parameter list)
    funcs: HashMap<String, (Vec<Kind>, Option<Kind>, Vec<Kind>)>,
    /// record constructor -> field types in order
    record_ctors: HashMap<String, Vec<Kind>>,
    /// channels: name -> payload type
    chans: HashMap<String, Kind>,
    /// type parameters in scope while from_ast is called (for generic sigs)
    cur_tparams: Vec<String>,
    /// constraints of the in-scope type parameters, by index (e.g. Ordered)
    cur_constraints: Vec<Option<String>>,
    /// the enclosing function's declared error channel (for `?` acceptance)
    cur_error_channel: Option<Kind>,
    /// span of the construct currently being checked; `error` attaches it so
    /// front-end diagnostics (LSP, check-json) can point at a real location.
    cur_span: Span,
    errors: Vec<TypeError>,
}

pub fn typecheck(program: &Program) -> Result<(), Vec<TypeError>> {
    let mut tc = TypeChecker {
        decls: HashMap::new(),
        fields: HashMap::new(),
        variants: HashMap::new(),
        enum_variants: HashMap::new(),
        funcs: HashMap::new(),
        record_ctors: HashMap::new(),
        chans: HashMap::new(),
        cur_tparams: vec![],
        cur_constraints: vec![],
        cur_error_channel: None,
        cur_span: Span::default(),
        errors: vec![],
    };
    tc.build_world(program);
    tc.check_cstructs(program);
    tc.check_exports(program);
    tc.collect_sigs(program);
    tc.predeclare_stdlib();
    if tc.errors.len() > 0 {
        return Err(tc.errors);
    }
    tc.check_bodies(program);
    if tc.errors.len() > 0 {
        return Err(tc.errors);
    }
    Ok(())
}

impl TypeChecker {
    fn error(&mut self, message: String) {
        self.errors.push(TypeError { message: message, span: self.cur_span.clone() });
    }

    fn error_at(&mut self, message: String, span: Span) {
        self.errors.push(TypeError { message: message, span: span });
    }

    fn build_world(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                Item::Record(rec) => {
                    let name = rec.name.clone();
                    let ty = Kind::Record(name.clone());
                    self.decls.insert(name.clone(), ty);
                    let mut ctors: Vec<Kind> = vec![];
                    for f in &rec.fields {
                        self.cur_span = f.span.clone();
                        let ft = self.from_ast(&f.ty);
                        self.fields.insert((name.clone(), f.name.clone()), ft.clone());
                        ctors.push(ft);
                    }
                    self.record_ctors.insert(name.clone(), ctors);
                }
                Item::Enum(en) => {
                    let name = en.name.clone();
                    let ty = Kind::Enum(name.clone());
                    self.decls.insert(name.clone(), ty);
                    let mut vnames: Vec<String> = vec![];
                    for v in &en.variants {
                        self.cur_span = v.span.clone();
                        let ft: Vec<Kind> = v.fields.iter().map(|f| self.from_ast(&f.ty)).collect();
                        self.variants.insert(v.name.clone(), (name.clone(), ft));
                        vnames.push(v.name.clone());
                    }
                    self.enum_variants.insert(name.clone(), vnames);
                }
                Item::Chan(c) => {
                    self.cur_span = c.span.clone();
                    let payload = self.from_ast(&c.payload);
                    self.chans.insert(c.name.clone(), payload);
                }
                _ => {}
            }
        }
    }

    /// A `@cstruct` record promises the C ABI struct layout (docs/10), so its
    /// fields must be C-representable: a primitive, or another `@cstruct`
    /// record nested by value (no cycles). Everything else is a compile error
    /// — the alternative would be a layout no C caller can agree on.
    fn check_cstructs(&mut self, program: &Program) {
        let mut records: HashMap<String, &ast::RecordDecl> = HashMap::new();
        let mut enums: HashSet<String> = HashSet::new();
        let mut cstruct: HashSet<String> = HashSet::new();
        for item in &program.items {
            match item {
                Item::Record(r) => {
                    records.insert(r.name.clone(), r);
                    if r.cstruct {
                        cstruct.insert(r.name.clone());
                    }
                }
                Item::Enum(e) => {
                    enums.insert(e.name.clone());
                }
                _ => {}
            }
        }

        for item in &program.items {
            if let Item::Record(r) = item {
                if !r.cstruct {
                    continue;
                }
                let mut visiting: HashSet<String> = HashSet::new();
                visiting.insert(r.name.clone());
                for f in &r.fields {
                    if let Err(msg) = cstruct_field_ok(&f.ty, &cstruct, &records, &enums, &mut visiting) {
                        self.error_at(
                            format!("@cstruct record '{}' field '{}': {}", r.name, f.name, msg),
                            f.span.clone(),
                        );
                    }
                }
            }
        }
    }

    /// An `@export` function becomes a C ABI symbol (`xz build --shared`,
    /// docs/10), so its signature must be C-representable end to end and it
    /// must be concrete: no type parameters, no `async`, not `main`.
    fn check_exports(&mut self, program: &Program) {
        let mut records: HashMap<String, &ast::RecordDecl> = HashMap::new();
        let mut enums: HashSet<String> = HashSet::new();
        let mut cstruct: HashSet<String> = HashSet::new();
        for item in &program.items {
            match item {
                Item::Record(r) => {
                    records.insert(r.name.clone(), r);
                    if r.cstruct {
                        cstruct.insert(r.name.clone());
                    }
                }
                Item::Enum(e) => {
                    enums.insert(e.name.clone());
                }
                _ => {}
            }
        }

        for item in &program.items {
            let f = match item {
                Item::Func(f) => f,
                _ => continue,
            };
            if !f.exported {
                continue;
            }
            if f.name == "main" {
                self.error_at(String::from("'main' cannot be @export; a library has no entry point"), f.span.clone());
                continue;
            }
            if !f.type_params.is_empty() {
                self.error_at(format!("@export func '{}' cannot be generic; a C symbol has one concrete signature", f.name), f.span.clone());
                continue;
            }
            if f.is_async {
                self.error_at(format!("@export func '{}' cannot be async", f.name), f.span.clone());
                continue;
            }
            let mut visiting: HashSet<String> = HashSet::new();
            for p in &f.params {
                if let Err(msg) = cstruct_field_ok(&p.ty, &cstruct, &records, &enums, &mut visiting) {
                    self.error_at(format!("@export func '{}' parameter '{}': {}", f.name, p.name, msg), p.span.clone());
                }
            }
            if let Some(rt) = &f.ret {
                if !is_unit_type(rt) {
                    if let Err(msg) = cstruct_field_ok(rt, &cstruct, &records, &enums, &mut visiting) {
                        self.error_at(format!("@export func '{}' return: {}", f.name, msg), f.span.clone());
                    }
                }
            }
        }
    }

    fn collect_sigs(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                Item::Func(f) => {
                    self.cur_span = f.span.clone();
                    self.cur_tparams = f.type_params.iter().map(|tp| tp.name.clone()).collect();
                    self.cur_constraints = f.type_params.iter().map(|tp| tp.constraint.clone()).collect();
                    let param_tys: Vec<Kind> = f.params.iter().map(|p| self.from_ast(&p.ty)).collect();
                    let ret_ty: Option<Kind> = match &f.ret { Some(t) => Some(self.from_ast(t)), None => None };
                    let tvs: Vec<Kind> = f.type_params.iter().enumerate().map(|(i, _)| Kind::TypeVar(i)).collect();
                    self.funcs.insert(f.name.clone(), (param_tys, ret_ty, tvs));
                }
                Item::Extern(e) => {
                    self.cur_span = e.span.clone();
                    self.cur_tparams = e.type_params.iter().map(|tp| tp.name.clone()).collect();
                    self.cur_constraints = e.type_params.iter().map(|tp| tp.constraint.clone()).collect();
                    let param_tys: Vec<Kind> = e.params.iter().map(|p| self.from_ast(&p.ty)).collect();
                    let ret_ty: Option<Kind> = match &e.ret { Some(t) => Some(self.from_ast(t)), None => None };
                    let tvs: Vec<Kind> = e.type_params.iter().enumerate().map(|(i, _)| Kind::TypeVar(i)).collect();
                    self.funcs.insert(e.name.clone(), (param_tys, ret_ty, tvs));
                }
                Item::Chan(c) => {
                    self.cur_span = c.span.clone();
                    let payload = self.from_ast(&c.payload);
                    self.chans.insert(c.name.clone(), Kind::Chan(Box::new(payload)));
                }
                Item::Task(_) => {}
                _ => {}
            }
        }
    }

    fn predeclare_stdlib(&mut self) {
        self.funcs.insert("print".to_string(), (vec![Kind::Str], Some(Kind::Unit), vec![]));
        self.funcs.insert("read_file".to_string(), (vec![Kind::Str], Some(Kind::Result(Box::new(Kind::Str), Box::new(Kind::Err))), vec![]));
        self.funcs.insert("approx_sqrt".to_string(), (vec![Kind::Float], Some(Kind::Float), vec![]));
        self.funcs.insert("now".to_string(), (vec![], Some(Kind::Float), vec![]));
        self.funcs.insert("monotonic".to_string(), (vec![], Some(Kind::Float), vec![]));
    }

    fn from_ast(&mut self, ty: &crate::ast::Type) -> Kind {
        match ty {
            Type::Named(name, args) => {
                self.from_ast_named(name, args)
            }
            Type::NamedPlain(name) => self.from_ast_named(name, &vec![]),
            Type::Union(members) => {
                // error unions (docs/06): preserved so `?` acceptance can
                // check membership
                let kinds: Vec<Kind> = members.iter().map(|m| self.from_ast(m)).collect();
                Kind::ErrUnion(kinds)
            }
        }
    }

    fn from_ast_named(&mut self, name: &str, args: &Vec<ast::Type>) -> Kind {
        match name {
            "Bool" => Kind::Bool,
            "Int" => Kind::Int,
            "usize" => Kind::Usize,
            "Float" => Kind::Float,
            "Char" => Kind::Char,
            "Str" => Kind::Str,
            "Bytes" => Kind::Bytes,
            "Unit" => Kind::Unit,
            "Ptr" => Kind::Ptr,
            "Option" => {
                let inner = self.from_ast(&args[0]);
                Kind::Option(Box::new(inner))
            }
            "Result" => {
                let t = self.from_ast(&args[0]);
                let e = self.from_ast(&args[1]);
                Kind::Result(Box::new(t), Box::new(e))
            }
            "Chan" => {
                let inner = self.from_ast(&args[0]);
                Kind::Chan(Box::new(inner))
            }
            "List" => {
                let inner = self.from_ast(&args[0]);
                Kind::List(Box::new(inner))
            }
            "Map" => {
                let key = self.from_ast(&args[0]);
                let val = self.from_ast(&args[1]);
                if !map_key_ok(&key) && !matches!(key, Kind::Unknown | Kind::TypeVar(_)) {
                    self.error(format!("Map key type must be Int, usize, Bool, Char, or Str, got {:?}", key));
                }
                Kind::Map(Box::new(key), Box::new(val))
            }
            "Set" => {
                let elem = self.from_ast(&args[0]);
                if !map_key_ok(&elem) && !matches!(elem, Kind::Unknown | Kind::TypeVar(_)) {
                    self.error(format!("Set element type must be Int, usize, Bool, Char, or Str, got {:?}", elem));
                }
                Kind::Set(Box::new(elem))
            }
            "Err" => Kind::Err,
            _ => {
                // type parameter in scope (generic signature/body)
                for (i, tp) in self.cur_tparams.iter().enumerate() {
                    if *tp == *name {
                        return Kind::TypeVar(i);
                    }
                }
                // record/enum/error type by name
                match self.decls.get(name) {
                    Some(t) => return t.clone(),
                    None => {}
                }
                // error records: { message: Str } conforms to Err
                if is_error_record(name) {
                    return Kind::Err;
                }
                Kind::Unknown
            }
        }
    }

    fn check_bodies(&mut self, program: &Program) {
        for item in &program.items {
            match item {
Item::Func(f) => {
                    self.cur_span = f.span.clone();
                    self.cur_tparams = f.type_params.iter().map(|tp| tp.name.clone()).collect();
                    self.cur_constraints = f.type_params.iter().map(|tp| tp.constraint.clone()).collect();
                    let ret_ty: Option<Kind> = match &f.ret { Some(t) => Some(self.from_ast(t)), None => None };
                    self.cur_error_channel = match &ret_ty {
                        Some(Kind::Result(_, e)) => Some((**e).clone()),
                        _ => None,
                    };
                    let mut env: Env = Env::default();
                    for p in &f.params {
                        self.cur_span = p.span.clone();
                        env.insert_binding(p.name.clone(), self.from_ast(&p.ty), p.mutable);
                    }
                    self.seed_env(&mut env);
                    self.check_contracts(&f.contracts, &mut env, ret_ty);
                    self.check_block(&f.body, &mut env);
                    
                }
                Item::Task(t) => {
                    self.cur_span = t.span.clone();
                    let mut env: Env = Env::default();
                    self.seed_env(&mut env);
                    self.check_block(&t.body, &mut env);
                }
                _ => {}
            }
        }
    }

    /// The `?` acceptance rule (docs/06): a callee error E is legal to propagate
/// if the caller's declared error channel accepts it.
fn accepts_error(&self, callee_e: &Kind, caller_e: &Kind) -> bool {
    // 3. Err is the root: accepts any narrower error type
    if caller_e == &Kind::Err {
        return true;
    }
    match (callee_e, caller_e) {
        // 1. identical
        _ if callee_e == caller_e => true,
        // 2. the caller is a union and the callee is one of its members
        (c, Kind::ErrUnion(members)) => {
            let mut ok = false;
            for m in members {
                if c == m {
                    ok = true;
                }
            }
            ok
        }
        _ => false,
    }
}

fn check_tvar_op(&mut self, at: &Kind, bt: &Kind, op: &BinOp) {
        // A bare type parameter may be stored/passed/returned, but comparing
        // or doing arithmetic on it requires a justifying constraint.
        // Currently only `T: Ordered` (comparison) is defined (docs/03).
        let idx = typevar_index(at).or_else(|| typevar_index(bt));
        let idx = idx.unwrap();
        let is_cmp = matches!(op, BinOp::Eq) || matches!(op, BinOp::Ne)
            || matches!(op, BinOp::Lt) || matches!(op, BinOp::Le)
            || matches!(op, BinOp::Gt) || matches!(op, BinOp::Ge);
        if is_cmp {
            let constrained = idx < self.cur_constraints.len()
                && match &self.cur_constraints[idx] { Some(c) => c == "Ordered", None => false };
            if !constrained {
                self.error(format!("type parameter must be constrained (T: Ordered) to compare values of type {:?}/{:?}", at, bt));
            }
        } else {
            self.error(format!("cannot use arithmetic on type parameter (constraint not defined)"));
        }
    }

    fn seed_env(&self, env: &mut Env) {
        for (name, ty) in &self.chans {
            env.insert(name.clone(), ty.clone());
        }
    }

    fn check_contracts(&mut self, contracts: &Vec<Contract>, env: &mut Env, ret_ty: Option<Kind>) {
        for c in contracts {
            match c {
                Contract::Pre(e) => {
                    let t = self.check_expr(e, env);
                    if t != Kind::Bool && t != Kind::Unknown {
                        self.error(String::from("precondition must be a Bool expression"));
                    }
                }
                Contract::Post(e) => {
                    let mut post_env = env.clone();
                    // `result` refers to the return value (see 06); give it
                    // the declared return type so `is ok`/comparisons typecheck.
                    let ret = match &ret_ty {
                        Some(t) => t.clone(),
                        None => Kind::Unknown,
                    };
                    post_env.insert("result".to_string(), ret);
                    let t = self.check_expr(e, &mut post_env);
                    if t != Kind::Bool && t != Kind::Unknown {
                        self.error(String::from("postcondition must be a Bool expression"));
                    }
                }
                Contract::Invariant(e) => {
                    let t = self.check_expr(e, env);
                    if t != Kind::Bool && t != Kind::Unknown {
                        self.error(String::from("invariant must be a Bool expression"));
                    }
                }
            }
        }
    }

    fn lookup_name(&self, n: &str, env: &Env) -> Kind {
        match env.get(n) {
            Some(t) => return t.clone(),
            None => {}
        }
        // constants
        if n == "PI" || n == "E" {
            return Kind::Float;
        }
        // channel
        if let Some(p) = self.chans.get(n) {
            return Kind::Chan(Box::new(p.clone()));
        }
        // enum variant / record constructor used as a value
        if self.variants.contains_key(n) {
            return Kind::Enum("".to_string());
        }
        if let Some(ret) = self.funcs.get(n).and_then(|(_, r, _)| r.clone()) {
            return ret;
        }
        Kind::Unknown
    }

    fn check_block(&mut self, block: &ast::Block, env: &mut Env) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Decl(d) => {
                    self.cur_span = d.span.clone();
                    let declared_ty = match &d.ty {
                        Some(t) => Some(self.from_ast(t)),
                        None => None,
                    };
                    let mut init_ty = None;
                    match &d.init {
                        Some(e) => init_ty = Some(self.check_expr(e, env)),
                        None => {}
                    }
                    if d.recv {
                        // let x <- recv(ch); payload type of channel env entry
                        let ch_ty = env.get(&d.chan.clone().unwrap()).cloned();
                        match ch_ty {
                            Some(Kind::Chan(inner)) => {
                                env.insert_binding(d.name.clone(), *inner, d.mutable);
                            }
                            Some(_) => self.error(format!("'{}' is not a channel", d.chan.clone().unwrap())),
                            None => self.error(format!("unknown channel '{}'", d.chan.clone().unwrap())),
                        }
                        continue;
                    }
                    match (declared_ty, init_ty) {
                        (Some(dt), Some(it)) => {
                            if !self.accepts(&it, &dt) {
                                self.error(format!("binding '{}' declared as {:?} but initializer is {:?}", d.name, dt, it));
                            }
                            env.insert_binding(d.name.clone(), dt, d.mutable);
                        }
                        (Some(dt), None) => {
                            env.insert_binding(d.name.clone(), dt, d.mutable);
                        }
                        (None, Some(it)) => {
                            env.insert_binding(d.name.clone(), it, d.mutable);
                        }
                        (None, None) => {
                            env.insert_binding(d.name.clone(), Kind::Unknown, d.mutable);
                        }
                    }
                }
                StmtKind::Assign(a) => {
                    self.cur_span = a.span.clone();
                    let val_ty = self.check_expr(&a.value, env);
                    match &a.target {
                        ast::AssignTarget::Name(n) => {
                            if env.get(n).is_some() && !env.is_mut(n) {
                                self.error(format!("cannot assign to immutable binding '{}' (declare it with `mut`)", n));
                            }
                            let target_ty = env.get(n).cloned();
                            match target_ty {
                                Some(tt) => {
                                    if !self.accepts(&val_ty, &tt) {
                                        self.error(format!("cannot assign {:?} to '{}' of type {:?}", val_ty, n, tt));
                                    }
                                }
                                None => self.error(format!("unknown name '{}' in assignment", n)),
                            }
                        }
                        ast::AssignTarget::Field(base, fname) => {
                            if let Some(root) = root_name(base) {
                                if env.get(&root).is_some() && !env.is_mut(&root) {
                                    self.error(format!("cannot assign to field of immutable binding '{}' (declare it with `mut`)", root));
                                }
                            }
                            let base_ty = self.check_expr(base, env);
                            match &base_ty {
                                Kind::Record(rec) => {
                                    match self.fields.get(&(rec.clone(), fname.clone())) {
                                        Some(ft) => {
                                            if !self.accepts(&val_ty, ft) {
                                                self.error(format!("cannot assign {:?} to field '{}' of type {:?}", val_ty, fname, ft));
                                            }
                                        }
                                        None => self.error(format!("record '{}' has no field '{}'", rec, fname)),
                                    }
                                }
                                _ => self.error(format!("cannot assign field on non-record")),
                            }
                        }
                    }
                }
                StmtKind::Expr(e) => {
                    self.check_expr(e, env);
                }
                StmtKind::Break => {}
                StmtKind::Continue => {}
            }
        }
    }

    fn check_expr(&mut self, e: &Expr, env: &mut Env) -> Kind {
        match e {
            Expr::Int(_) => Kind::Int,
            Expr::Float(_) => Kind::Float,
            Expr::Char(_) => Kind::Char,
            Expr::Str(_) => Kind::Str,
            Expr::RawStr(_) => Kind::Str,
            Expr::Bool(_) => Kind::Bool,
            Expr::None => Kind::Option(Box::new(Kind::Unknown)),
            Expr::Name(n) => {
                let ty = self.lookup_name(n, env);
                ty
            }
            Expr::Call(callee, args) => {
                match &**callee {
                    Expr::Name(n) => {
                        if n == "print" {
                            for a in args { self.check_expr(a, env); }
                            return Kind::Unit;
                        }
                        if n == "ok" {
                            if args.len() == 0 {
                                return Kind::Result(Box::new(Kind::Unit), Box::new(Kind::Unknown));
                            }
                            let t = self.check_expr(&args[0], env);
                            return Kind::Result(Box::new(t), Box::new(Kind::Unknown));
                        }
                        if n == "err" {
                            let et = self.check_expr(&args[0], env);
                            return Kind::Result(Box::new(Kind::Unknown), Box::new(et));
                        }
                        if n == "some" {
                            let t = self.check_expr(&args[0], env);
                            return Kind::Option(Box::new(t));
                        }
                        if n == "sqrt" {
                            for a in args { self.check_expr(a, env); }
                            return Kind::Result(Box::new(Kind::Float), Box::new(Kind::Err));
                        }
                        if is_error_ctor(n) {
                            // error record constructor: e.g. DomainError("msg"), AllocError(...)
                            for a in args { self.check_expr(a, env); }
                            return Kind::Err;
                        }
                        // enum variant constructor
                        if let Some((en, fts)) = self.variants.get(n).cloned() {
                            if args.len() != fts.len() {
                                self.error(format!("variant '{}' expects {} fields, got {}", n, fts.len(), args.len()));
                            }
                            for (i, a) in args.iter().enumerate() {
                                let at = self.check_expr(a, env);
                                if i < fts.len() {
                                    if !self.accepts(&at, &fts[i]) {
                                        self.error(format!("variant '{}' field {} type mismatch", n, i));
                                    }
                                }
                            }
                            return Kind::Enum(en.clone());
                        }
                        // error records as constructors
                        if is_error_record(n) {
                            for a in args { self.check_expr(a, env); }
                            return Kind::Err;
                        }
                        // record constructor
                        if let Some(ft) = self.record_ctors.get(n).cloned() {
                            if args.len() != ft.len() {
                                self.error(format!("'{}' expects {} args, got {}", n, ft.len(), args.len()));
                            }
                            for (i, a) in args.iter().enumerate() {
                                let at = self.check_expr(a, env);
                                if i < ft.len() {
                                    if !self.accepts(&at, &ft[i]) {
                                        self.error(format!("argument {} to record constructor '{}' type mismatch", i, n));
                                    }
                                }
                            }
                            return Kind::Record(n.clone());
                        }
                        // regular function call
                        match self.funcs.get(n).cloned() {
                            Some((params, ret, _tvs)) => {
                                if args.len() != params.len() {
                                    self.error(format!("function '{}' expects {} args, got {}", n, params.len(), args.len()));
                                }
                                // infer type arguments from the argument list:
                                // wherever a parameter is TypeVar(i), bind it to the arg's type
                                let mut bindings: Vec<Option<Kind>> = vec![];
                                for (i, a) in args.iter().enumerate() {
                                    let at = self.check_expr(a, env);
                                    if i < params.len() {
                                        if let Kind::TypeVar(ti) = &params[i] {
                                            while bindings.len() <= *ti {
                                                bindings.push(None);
                                            }
                                            bindings[*ti] = Some(at.clone());
                                        } else if !self.accepts(&at, &params[i]) {
                                            self.error(format!("argument {} to '{}' type mismatch", i, n));
                                        }
                                    }
                                }
                                match ret {
                                    Some(t) => subst(&t, &bindings),
                                    None => Kind::Unit,
                                }
                            }
                            None => {
                                self.error(format!("unknown function '{}'", n));
                                Kind::Unknown
                            }
                        }
                    }
                    Expr::Field(receiver, method) => {
                        let rt = self.check_expr(receiver, env);
                        let mt = method_type(&rt, method);
                        // check arg count for the method
                        if matches!(method.as_str(), "len" | "abs" | "to_str" | "to_upper" | "to_lower" | "is_empty" | "keys" | "values") && args.len() != 0 {
                            self.error(format!("method '{}' takes no arguments", method));
                        }
                        if method == "append" {
                            if args.len() != 1 {
                                self.error(format!("append takes one argument"));
                            }
                            if let Kind::List(t) = &rt {
                                for a in args {
                                    let at = self.check_expr(a, env);
                                    if !self.accepts(&at, t) {
                                        self.error(format!("append expects {:?}, got {:?}", t, at));
                                    }
                                }
                            } else {
                                for a in args { let _ = self.check_expr(a, env); }
                            }
                        } else if method == "insert" {
                            match &rt {
                                Kind::Map(k, v) => {
                                    if args.len() != 2 {
                                        self.error(String::from("insert takes two arguments"));
                                    }
                                    if let Some(a) = args.first() {
                                        let at = self.check_expr(a, env);
                                        if !self.accepts(&at, k) {
                                            self.error(format!("insert key expects {:?}, got {:?}", k, at));
                                        }
                                    }
                                    if let Some(a) = args.get(1) {
                                        let at = self.check_expr(a, env);
                                        if !self.accepts(&at, v) {
                                            self.error(format!("insert value expects {:?}, got {:?}", v, at));
                                        }
                                    }
                                }
                                Kind::Set(t) => {
                                    if args.len() != 1 {
                                        self.error(String::from("insert takes one argument"));
                                    }
                                    if let Some(a) = args.first() {
                                        let at = self.check_expr(a, env);
                                        if !self.accepts(&at, t) {
                                            self.error(format!("insert element expects {:?}, got {:?}", t, at));
                                        }
                                    }
                                }
                                _ => {
                                    for a in args { let _ = self.check_expr(a, env); }
                                }
                            }
                        } else if method == "contains" {
                            if args.len() != 1 {
                                self.error(String::from("contains takes one argument"));
                            }
                            match &rt {
                                Kind::Set(t) => {
                                    if let Some(a) = args.first() {
                                        let at = self.check_expr(a, env);
                                        if !self.accepts(&at, t) {
                                            self.error(format!("contains element expects {:?}, got {:?}", t, at));
                                        }
                                    }
                                }
                                _ => {
                                    for a in args { let _ = self.check_expr(a, env); }
                                }
                            }
                        } else if method == "get" {
                            if args.len() != 1 {
                                self.error(String::from("get takes one argument"));
                            }
                            match &rt {
                                Kind::Map(k, _) => {
                                    if let Some(a) = args.first() {
                                        let at = self.check_expr(a, env);
                                        if !self.accepts(&at, k) {
                                            self.error(format!("get key expects {:?}, got {:?}", k, at));
                                        }
                                    }
                                }
                                _ => {
                                    for a in args { let _ = self.check_expr(a, env); }
                                }
                            }
                        } else {
                            for a in args {
                                let _ = self.check_expr(a, env);
                            }
                        }
                        match mt {
                            Some(t) => t,
                            None => {
                                self.error(format!("no method '{}' on {:?}", method, rt));
                                Kind::Unknown
                            }
                        }
                    }
                    _ => {
                        let _ = self.check_expr(callee, env);
                        Kind::Unknown
                    }
                }
            }
            Expr::Field(base, fname) => {
                let bt = self.check_expr(base, env);
                match &bt {
                    Kind::Record(rec) => {
                        match self.fields.get(&(rec.clone(), fname.clone())) {
                            Some(ft) => ft.clone(),
                            None => {
                                self.error(format!("record '{}' has no field '{}'", rec, fname));
                                Kind::Unknown
                            }
                        }
                    }
                    // Result payload access: `result.value` under `is ok`
                    // (docs/06). `.value` is the T payload; `.err` the E.
                    Kind::Result(t, _) => {
                        if fname == "value" {
                            (**t).clone()
                        } else if fname == "err" {
                            Kind::Unknown
                        } else {
                            self.error(format!("Result has no field '{}' (use .value / .err)", fname));
                            Kind::Unknown
                        }
                    }
                    Kind::Enum(_) => Kind::Unknown,
                    _ => {
                        self.error(format!("cannot access field '{}' of {:?}", fname, bt));
                        Kind::Unknown
                    }
                }
            }
            Expr::ListLit(elems) => {
                if elems.is_empty() {
                    // Element type is supplied by the binding's declared type.
                    Kind::List(Box::new(Kind::Unknown))
                } else {
                    let first = self.check_expr(&elems[0], env);
                    for e in &elems[1..] {
                        let t = self.check_expr(e, env);
                        if t != first {
                            self.error(format!("list literal elements must share a type: {:?} vs {:?}", first, t));
                        }
                    }
                    Kind::List(Box::new(first))
                }
            }
            Expr::MapLit(entries) => {
                if entries.is_empty() {
                    // Key/value types are supplied by the binding's declared type.
                    Kind::Map(Box::new(Kind::Unknown), Box::new(Kind::Unknown))
                } else {
                    let k0 = self.check_expr(&entries[0].0, env);
                    let v0 = self.check_expr(&entries[0].1, env);
                    for (k, v) in &entries[1..] {
                        let kt = self.check_expr(k, env);
                        let vt = self.check_expr(v, env);
                        if kt != k0 {
                            self.error(format!("map literal keys must share a type: {:?} vs {:?}", k0, kt));
                        }
                        if vt != v0 {
                            self.error(format!("map literal values must share a type: {:?} vs {:?}", v0, vt));
                        }
                    }
                    if !map_key_ok(&k0) && !matches!(k0, Kind::Unknown) {
                        self.error(format!("Map key type must be Int, usize, Bool, Char, or Str, got {:?}", k0));
                    }
                    Kind::Map(Box::new(k0), Box::new(v0))
                }
            }
            Expr::SetLit(elems) => {
                if elems.is_empty() {
                    // Element type is supplied by the binding's declared type.
                    Kind::Set(Box::new(Kind::Unknown))
                } else {
                    let e0 = self.check_expr(&elems[0], env);
                    for e in &elems[1..] {
                        let et = self.check_expr(e, env);
                        if et != e0 {
                            self.error(format!("set literal elements must share a type: {:?} vs {:?}", e0, et));
                        }
                    }
                    if !map_key_ok(&e0) && !matches!(e0, Kind::Unknown) {
                        self.error(format!("Set element type must be Int, usize, Bool, Char, or Str, got {:?}", e0));
                    }
                    Kind::Set(Box::new(e0))
                }
            }
            Expr::Index(base, idx) => {
                let bt = self.check_expr(base, env);
                let it = self.check_expr(idx, env);
                if it != Kind::Int && it != Kind::Unknown {
                    self.error(format!("index must be an Int, got {:?}", it));
                }
                match &bt {
                    // bounds-checked indexing: Result[T, IndexError] (docs/12)
                    Kind::List(t) => Kind::Result(t.clone(), Box::new(Kind::Err)),
                    _ => {
                        self.error(format!("indexing is only defined for List[T], got {:?}", bt));
                        Kind::Unknown
                    }
                }
            }
            Expr::Prop(base, _) => {
                let bt = self.check_expr(base, env);
                match &bt {
                    Kind::Result(t, e) => {
                        // `?` unwraps the payload; the callee's error must be
                        // accepted by the enclosing function's error channel
                        // (docs/06, the ? acceptance rule)
                        let callee_e = (**e).clone();
                        match &self.cur_error_channel {
                            Some(caller_e) => {
                                if !self.accepts_error(&callee_e, caller_e) {
                                    self.error(format!("cannot propagate {:?} via '?': the enclosing function's error channel is {:?}", callee_e, caller_e));
                                }
                            }
                            None => {
                                self.error(format!("cannot use '?' here: the enclosing function has no error channel (declare Result[_, E])"));
                            }
                        }
                        (**t).clone()
                    }
                    _ => {
                        self.error(format!("'?' not allowed on {:?}", bt));
                        Kind::Unknown
                    }
                }
            }
            Expr::Unary(op, a) => {
                let at = self.check_expr(a, env);
                match op {
                    UnaryOp::Neg => at,
                    UnaryOp::Not => Kind::Bool,
                }
            }
            Expr::Binary(op, a, b) => {
                let at = self.check_expr(a, env);
                let bt = self.check_expr(b, env);
                // operations on a bare type parameter require a justifying constraint
                if is_typevar(&at) || is_typevar(&bt) {
                    self.check_tvar_op(&at, &bt, op);
                }
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        // string concat special-case
                        if *op == BinOp::Add && at == Kind::Str {
                            return Kind::Str;
                        }
                        if at != bt {
                            self.error(format!("arithmetic on mismatched types"));
                        }
                        at
                    }
                    BinOp::And | BinOp::Or | BinOp::Implies => {
                        if at != Kind::Bool || bt != Kind::Bool {
                            self.error(format!("logical op requires Bools"));
                        }
                        Kind::Bool
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Kind::Bool,
                    BinOp::IsOk | BinOp::IsErr | BinOp::IsNone | BinOp::IsSome => Kind::Bool,
                    BinOp::IsOk | BinOp::IsErr | BinOp::IsNone | BinOp::IsSome => Kind::Bool,
                }
            }
            Expr::Cast(a, ty) => {
                let at = self.check_expr(a, env);
                let tt = self.from_ast(ty);
                if !self.accepts_cast(&at, &tt) {
                    self.error(format!("cannot cast {:?} to {:?} (allowed: Int<->usize, Int->Float, Float->Int, Str<->Bytes)", at, tt));
                }
                tt
            }
            Expr::Match(subject, arms) => {
                let st = self.check_expr(subject, env);
                let mut covered: Vec<String> = vec![];
                let mut has_wildcard = false;
                let mut result_ty = Kind::Unknown;
                for (pat, body) in arms {
                    // bind pattern names into env BEFORE checking the body
                    match pat {
                        ast::Pattern::Name(nm) => {
                            env.insert(nm.clone(), Kind::Unknown);
                            has_wildcard = true;
                        }
                        ast::Pattern::Wildcard => {
                            has_wildcard = true;
                        }
                        ast::Pattern::Variant(vname, names) => {
                            let fts = self.variants.get(vname).cloned().map(|t| t.1);
                            if let Some(fts) = fts {
                                for (i, nm) in names.iter().enumerate() {
                                    if i < fts.len() {
                                        env.insert(nm.clone(), fts[i].clone());
                                    }
                                }
                            }
                            covered.push(vname.clone());
                        }
                        _ => {}
                    }
                    let bt = self.check_expr(body, env);
                    if result_ty == Kind::Unknown {
                        result_ty = bt.clone();
                    }
                    if result_ty != bt {
                        self.error(format!("match arms produce different types: {:?} vs {:?}", result_ty, bt));
                    }
                }
                // exhaustiveness: an enum subject must cover all its variants
                if let Kind::Enum(ename) = &st {
                    if !has_wildcard {
                        if let Some(vnames) = self.enum_variants.get(ename).cloned() {
                            for v in &vnames {
                                if !covered.contains(v) {
                                    self.error(format!("match on enum '{}' is not exhaustive: missing variant '{}'", ename, v));
                                }
                            }
                        }
                    }
                }
                result_ty
            }
            Expr::If(ifx) => {
                let cond_ty = self.check_expr(&ifx.cond, env);
                let _ = cond_ty;
                // flow typing: a positive test narrows the bound name inside
                // the then-branch; each elif and the else are their own branch.
                let pos_narrow = extract_narrowing(&ifx.cond);
                let mut then_env = env.clone();
                apply_narrowing(&mut then_env, &pos_narrow);
                let _ = self.check_block(&ifx.then_block, &mut then_env);
                for (c, b) in &ifx.elif {
                    let _ = self.check_expr(c, env);
                    let mut s = env.clone();
                    let n = extract_narrowing(c);
                    apply_narrowing(&mut s, &n);
                    let _ = self.check_block(b, &mut s);
                }
                match &ifx.else_block {
                    Some(b) => {
                        let mut s = env.clone();
                        apply_complement(&mut s, &pos_narrow);
                        let _ = self.check_block(b, &mut s);
                    }
                    None => {}
                }
                Kind::Unknown
            }
            Expr::Loop(b) => {
                self.check_block(b, env);
                Kind::Never
            }
            Expr::For(name, iter, b) => {
                let it = self.check_expr(iter, env);
                // `for i in n` iterates the Int range 0..n; `for x in xs`
                // iterates a List[T] in order (docs/02, docs/12).
                let elem = match &it {
                    Kind::Int => Kind::Int,
                    Kind::List(t) => (**t).clone(),
                    Kind::Set(t) => (**t).clone(),
                    Kind::Unknown => Kind::Unknown,
                    _ => {
                        self.error(format!("for-in iterable must be an Int range, a List[T], or a Set[T], got {:?}", it));
                        Kind::Unknown
                    }
                };
                env.insert(name.clone(), elem);
                self.check_block(b, env);
                Kind::Never
            }
            Expr::Await(a) => self.check_expr(a, env),
            Expr::Send(ch, value) => {
                let _ = self.check_expr(ch, env); // channel type env
                let vt = self.check_expr(value, env);
                let _ = vt;
                Kind::Unit
            }
            Expr::Recv(ch) => {
                // channel payload
                let ct = env.get(ch).cloned();
                match ct {
                    Some(Kind::Chan(inner)) => *inner,
                    _ => Kind::Unknown,
                }
            }
            Expr::Transfer(a) => self.check_expr(a, env),
            Expr::Ok(inner) => {
                let inner_ty = match inner {
                    Some(v) => self.check_expr(v, env),
                    None => Kind::Unit,
                };
                Kind::Result(Box::new(inner_ty), Box::new(Kind::Unknown))
            }
            Expr::Err(a) => {
                let et = self.check_expr(a, env);
                Kind::Result(Box::new(Kind::Unknown), Box::new(et))
            }
            Expr::Some(a) => {
                let t = self.check_expr(a, env);
                Kind::Option(Box::new(t))
            }
        }
    }

    fn accepts_cast(&self, value: &Kind, target: &Kind) -> bool {
        if value == target {
            return true;
        }
        let v = value;
        let t = target;
        if matches!(v, Kind::Int) && matches!(t, Kind::Usize) { return true; }
        if matches!(v, Kind::Usize) && matches!(t, Kind::Int) { return true; }
        if matches!(v, Kind::Int) && matches!(t, Kind::Float) { return true; }
        if matches!(v, Kind::Float) && matches!(t, Kind::Int) { return true; }
        if matches!(v, Kind::Str) && matches!(t, Kind::Bytes) { return true; }
        if matches!(v, Kind::Bytes) && matches!(t, Kind::Str) { return true; }
        false
    }

    fn accepts(&self, value: &Kind, target: &Kind) -> bool {
        match (value, target) {
            _ if value == target => true,
            // Err root accepts any error
            (_ , Kind::Err) => true,
            // result errors narrow: any error type fits Err
            (Kind::Result(_, _), Kind::Result(_, _)) => true,
            // `none` is the absence literal: it fits any Option[T] (docs/03)
            (Kind::Option(inner), Kind::Option(_)) if matches!(**inner, Kind::Unknown) => true,
            // `[]` has no element type of its own: it fits any List[T] (docs/12)
            (Kind::List(inner), Kind::List(_)) if matches!(**inner, Kind::Unknown) => true,
            // `{}` has no key/value type of its own: it fits any Map[K, V] (docs/12)
            (Kind::Map(ku, vu), Kind::Map(_, _))
                if matches!(**ku, Kind::Unknown) && matches!(**vu, Kind::Unknown) => true,
            // `{}` is also an empty Set literal when the binding is a Set[T].
            (Kind::Map(ku, vu), Kind::Set(_))
                if matches!(**ku, Kind::Unknown) && matches!(**vu, Kind::Unknown) => true,
            // an empty Set literal fits any Set[T] (docs/12)
            (Kind::Set(inner), Kind::Set(_)) if matches!(**inner, Kind::Unknown) => true,
            _ => false,
        }
    }
}

fn is_error_record(name: &str) -> bool {
    ["IoError", "DomainError", "ParseError", "AllocError",
     "IndexError", "DecodeError", "HttpError", "Err"].contains(&name)
}

fn is_error_ctor(name: &str) -> bool {
    is_error_record(name)
}

/// Whether a type may be a `Map` key: only types with decidable equality
/// (`Float` is excluded because of NaN; records/enums/collections are future
/// work). See docs/12-stdlib.md.
fn map_key_ok(k: &Kind) -> bool {
    matches!(k, Kind::Int | Kind::Usize | Kind::Bool | Kind::Char | Kind::Str)
}

/// Whether a field type is representable in a `@cstruct` record (docs/10).
/// `visiting` holds the cstruct records currently being descended, so a
/// by-value nesting cycle is reported rather than tolerated.
fn cstruct_field_ok(
    ty: &ast::Type,
    cstruct: &HashSet<String>,
    records: &HashMap<String, &ast::RecordDecl>,
    enums: &HashSet<String>,
    visiting: &mut HashSet<String>,
) -> Result<(), String> {
    match ty {
        ast::Type::Named(name, args) => {
            if ["Unit", "Option", "Result", "List", "Map", "Set", "Chan"].contains(&name.as_str()) {
                return Err(format!("type '{}' is not a C type", name));
            }
            if !args.is_empty() {
                return Err(format!("'{}' has type arguments; @cstruct fields must be concrete", name));
            }
            cstruct_named_ok(name, cstruct, records, enums, visiting)
        }
        ast::Type::NamedPlain(name) => cstruct_named_ok(name, cstruct, records, enums, visiting),
        ast::Type::Union(_) => Err(String::from("an error union is not a C type")),
    }
}

/// Whether a type is `Unit` (the only C-representable type allowed only as a
/// return, not as a parameter or field).
fn is_unit_type(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::Named(name, args) => name == "Unit" && args.is_empty(),
        ast::Type::NamedPlain(name) => name == "Unit",
        ast::Type::Union(_) => false,
    }
}

fn cstruct_named_ok(
    name: &str,
    cstruct: &HashSet<String>,
    records: &HashMap<String, &ast::RecordDecl>,
    enums: &HashSet<String>,
    visiting: &mut HashSet<String>,
) -> Result<(), String> {
    if ["Bool", "Int", "usize", "Float", "Char", "Str", "Bytes", "Ptr"].contains(&name) {
        return Ok(());
    }
    if cstruct.contains(name) {
        if !visiting.insert(name.to_string()) {
            return Err(format!("'{}' nests itself by value; a @cstruct cycle has infinite size", name));
        }
        if let Some(rec) = records.get(name) {
            for f in &rec.fields {
                cstruct_field_ok(&f.ty, cstruct, records, enums, visiting)
                    .map_err(|e| format!("nested field '{}': {}", f.name, e))?;
            }
        }
        visiting.remove(name);
        return Ok(());
    }
    if records.contains_key(name) {
        return Err(format!("type '{}' is a plain record; declare it @cstruct", name));
    }
    if enums.contains(name) {
        return Err(format!("type '{}' is an enum; enums are not C-representable", name));
    }
    Err(format!("type '{}' is not C-representable", name))
}

fn method_type(receiver: &Kind, method: &str) -> Option<Kind> {
    let r = receiver;
    if method == "to_str" {
        return Some(Kind::Str);
    }
    if r == &Kind::Str {
        return match method {
            "len" => Some(Kind::Int),
            "is_empty" => Some(Kind::Bool),
            "to_upper" => Some(Kind::Str),
            "to_lower" => Some(Kind::Str),
            "at" => Some(Kind::Result(Box::new(Kind::Char), Box::new(Kind::Err))),
            "to_bytes" => Some(Kind::Bytes),
            _ => None,
        };
    }
    if r == &Kind::Int {
        return match method {
            "len" => Some(Kind::Int),
            "abs" => Some(Kind::Int),
            _ => None,
        };
    }
    if let Kind::List(t) = r {
        return match method {
            "len" => Some(Kind::Int),
            "is_empty" => Some(Kind::Bool),
            "append" => Some(Kind::List(t.clone())),
            _ => None,
        };
    }
    if let Kind::Map(k, v) = r {
        return match method {
            "len" => Some(Kind::Int),
            "is_empty" => Some(Kind::Bool),
            "get" => Some(Kind::Option(v.clone())),
            "insert" => Some(Kind::Map(k.clone(), v.clone())),
            "keys" => Some(Kind::List(k.clone())),
            "values" => Some(Kind::List(v.clone())),
            _ => None,
        };
    }
    if let Kind::Set(t) = r {
        return match method {
            "len" => Some(Kind::Int),
            "is_empty" => Some(Kind::Bool),
            "contains" => Some(Kind::Bool),
            "insert" => Some(Kind::Set(t.clone())),
            _ => None,
        };
    }
    if r == &Kind::Float {
        return match method {
            "abs" => Some(Kind::Float),
            _ => None,
        };
    }
    if r == &Kind::Bytes {
        return match method {
            "len" => Some(Kind::Int),
            _ => None,
        };
    }
    if r == &Kind::Bool {
        return match method {
            "to_str" => Some(Kind::Str),
            _ => None,
        };
    }
    None
}


/// Substitute type-parameter bindings into a type (generic instantiation).
/// TypeVar(i) with an unbound slot stays a fresh TypeVar (parametric).
fn is_typevar(k: &Kind) -> bool {
    match k {
        Kind::TypeVar(_) => true,
        _ => false,
    }
}

fn typevar_index(k: &Kind) -> Option<usize> {
    match k {
        Kind::TypeVar(i) => Some(*i),
        _ => None,
    }
}

/// What a positive `is` test narrows to, for flow typing inside `if`/`elif`.
enum NarrowKind {
    Some,      // x is some  -> Option[T] narrows to T
    None,      // x is none  -> narrows to "absent" (no value type)
    Ok,        // x is ok    -> Result[T,E] narrows to T
    Err,       // x is err   -> narrows to "error" (payload unused)
}

/// If `cond` is `NAME is some|none|ok|err`, return (name, narrowing).
fn extract_narrowing(cond: &Expr) -> Option<(String, NarrowKind)> {
    match cond {
        Expr::Binary(op, a, b) => {
            let _ = b;
            match &**a {
                Expr::Name(n) => {
                    if op == &BinOp::IsSome {
                        Some((n.clone(), NarrowKind::Some))
                    } else if op == &BinOp::IsNone {
                        Some((n.clone(), NarrowKind::None))
                    } else if op == &BinOp::IsOk {
                        Some((n.clone(), NarrowKind::Ok))
                    } else if op == &BinOp::IsErr {
                        Some((n.clone(), NarrowKind::Err))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Apply a positive narrowing: bind `name` to its unwrapped type.
fn apply_narrowing(env: &mut Env, narrow: &Option<(String, NarrowKind)>) {
    match narrow {
        Some((name, kind)) => {
            match env.get(name) {
                Some(ty) => {
                    match kind {
                        NarrowKind::Some => {
                            match ty {
                                Kind::Option(inner) => { let _ = env.insert(name.clone(), (**inner).clone()); }
                                _ => {}
                            }
                        }
                        NarrowKind::Ok => {
                            match ty {
                                Kind::Result(t, _) => { let _ = env.insert(name.clone(), (**t).clone()); }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                None => {}
            }
        }
        None => {}
    }
}

/// The else branch sees the complement: `is none` / `is err` narrow the name
/// to the absent case (an empty type is not expressible, so we leave it).
fn apply_complement(env: &mut Env, narrow: &Option<(String, NarrowKind)>) {
    match narrow {
        Some((name, kind)) => {
            match kind {
                NarrowKind::None | NarrowKind::Err => {
                    // x is none / x is err: in the else branch x has a value;
                    // narrow Option/Result to the payload.
                    match env.get(name) {
                        Some(ty) => {
                            match ty {
                                Kind::Option(inner) => { let _ = env.insert(name.clone(), (**inner).clone()); }
                                Kind::Result(t, _) => { let _ = env.insert(name.clone(), (**t).clone()); }
                                _ => {}
                            }
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
        None => {}
    }
}

/// The root binding a field-assignment target is rooted at, e.g. `a` for
/// `a.b.c = ...`. Field writes mutate the root binding, so `mut` is required
/// on that binding (docs/04).
fn root_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.clone()),
        Expr::Field(base, _) => root_name(base),
        _ => None,
    }
}

fn subst(ty: &Kind, bindings: &Vec<Option<Kind>>) -> Kind {
    match ty {
        Kind::TypeVar(i) => {
            if *i < bindings.len() {
                match &bindings[*i] {
                    Some(t) => return t.clone(),
                    None => {}
                }
            }
            Kind::TypeVar(*i)
        }
        Kind::Option(inner) => Kind::Option(Box::new(subst(inner, bindings))),
        Kind::Result(t, e) => Kind::Result(Box::new(subst(t, bindings)), Box::new(subst(e, bindings))),
        Kind::List(inner) => Kind::List(Box::new(subst(inner, bindings))),
        Kind::Map(k, v) => Kind::Map(Box::new(subst(k, bindings)), Box::new(subst(v, bindings))),
        Kind::Set(inner) => Kind::Set(Box::new(subst(inner, bindings))),
        Kind::Chan(inner) => Kind::Chan(Box::new(subst(inner, bindings))),
        Kind::ErrUnion(members) => {
            let ms: Vec<Kind> = members.iter().map(|m| subst(m, bindings)).collect();
            Kind::ErrUnion(ms)
        }
        Kind::Record(_) | Kind::Enum(_) => ty.clone(),
        Kind::Bool | Kind::Int | Kind::Usize | Kind::Float | Kind::Char
        | Kind::Str | Kind::Bytes | Kind::Unit | Kind::Ptr | Kind::Err
        | Kind::Unknown | Kind::Never => ty.clone(),
    }
}
