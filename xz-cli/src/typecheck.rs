use std::collections::HashMap;

use crate::ast;
use crate::ast::{Program, Item, Type, Expr, Stmt, BinOp, UnaryOp, Contract};

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
    pub file: String,
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
        errors: vec![],
    };
    tc.build_world(program);
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
    fn error(&mut self, message: String, file: String) {
        self.errors.push(TypeError { message: message, file: file });
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
                        let ft: Vec<Kind> = v.fields.iter().map(|f| self.from_ast(&f.ty)).collect();
                        self.variants.insert(v.name.clone(), (name.clone(), ft));
                        vnames.push(v.name.clone());
                    }
                    self.enum_variants.insert(name.clone(), vnames);
                }
                Item::Chan(c) => {
                    let payload = self.from_ast(&c.payload);
                    self.chans.insert(c.name.clone(), payload);
                }
                _ => {}
            }
        }
    }

    fn collect_sigs(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                Item::Func(f) => {
                    self.cur_tparams = f.type_params.iter().map(|tp| tp.name.clone()).collect();
                    self.cur_constraints = f.type_params.iter().map(|tp| tp.constraint.clone()).collect();
                    let param_tys: Vec<Kind> = f.params.iter().map(|p| self.from_ast(&p.ty)).collect();
                    let ret_ty: Option<Kind> = match &f.ret { Some(t) => Some(self.from_ast(t)), None => None };
                    let tvs: Vec<Kind> = f.type_params.iter().enumerate().map(|(i, _)| Kind::TypeVar(i)).collect();
                    self.funcs.insert(f.name.clone(), (param_tys, ret_ty, tvs));
                }
                Item::Extern(e) => {
                    self.cur_tparams = e.type_params.iter().map(|tp| tp.name.clone()).collect();
                    self.cur_constraints = e.type_params.iter().map(|tp| tp.constraint.clone()).collect();
                    let param_tys: Vec<Kind> = e.params.iter().map(|p| self.from_ast(&p.ty)).collect();
                    let ret_ty: Option<Kind> = match &e.ret { Some(t) => Some(self.from_ast(t)), None => None };
                    let tvs: Vec<Kind> = e.type_params.iter().enumerate().map(|(i, _)| Kind::TypeVar(i)).collect();
                    self.funcs.insert(e.name.clone(), (param_tys, ret_ty, tvs));
                }
                Item::Chan(c) => {
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
                    self.cur_tparams = f.type_params.iter().map(|tp| tp.name.clone()).collect();
                    self.cur_constraints = f.type_params.iter().map(|tp| tp.constraint.clone()).collect();
                    let ret_ty: Option<Kind> = match &f.ret { Some(t) => Some(self.from_ast(t)), None => None };
                    self.cur_error_channel = match &ret_ty {
                        Some(Kind::Result(_, e)) => Some((**e).clone()),
                        _ => None,
                    };
                    let mut env: HashMap<String, Kind> = HashMap::new();
                    for p in &f.params {
                        env.insert(p.name.clone(), self.from_ast(&p.ty));
                    }
                    self.seed_env(&mut env);
                    self.check_contracts(&f.contracts, &mut env, ret_ty);
                    self.check_block(&f.body, &mut env);
                    
                }
                Item::Task(t) => {
                    let mut env: HashMap<String, Kind> = HashMap::new();
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
                self.error(format!("type parameter must be constrained (T: Ordered) to compare values of type {:?}/{:?}", at, bt), "".to_string());
            }
        } else {
            self.error(format!("cannot use arithmetic on type parameter (constraint not defined)"), "".to_string());
        }
    }

    fn seed_env(&self, env: &mut HashMap<String, Kind>) {
        for (name, ty) in &self.chans {
            env.insert(name.clone(), ty.clone());
        }
    }

    fn check_contracts(&mut self, contracts: &Vec<Contract>, env: &mut HashMap<String, Kind>, ret_ty: Option<Kind>) {
        for c in contracts {
            match c {
                Contract::Pre(e) => {
                    let t = self.check_expr(e, env);
                    if t != Kind::Bool && t != Kind::Unknown {
                        self.error(String::from("precondition must be a Bool expression"), "".to_string());
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
                        self.error(String::from("postcondition must be a Bool expression"), "".to_string());
                    }
                }
                Contract::Invariant(e) => {
                    let t = self.check_expr(e, env);
                    if t != Kind::Bool && t != Kind::Unknown {
                        self.error(String::from("invariant must be a Bool expression"), "".to_string());
                    }
                }
            }
        }
    }

    fn lookup_name(&self, n: &str, env: &HashMap<String, Kind>) -> Kind {
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

    fn check_block(&mut self, block: &ast::Block, env: &mut HashMap<String, Kind>) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Decl(d) => {
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
                                env.insert(d.name.clone(), *inner);
                            }
                            Some(_) => self.error(format!("'{}' is not a channel", d.chan.clone().unwrap()), "".to_string()),
                            None => self.error(format!("unknown channel '{}'", d.chan.clone().unwrap()), "".to_string()),
                        }
                        continue;
                    }
                    match (declared_ty, init_ty) {
                        (Some(dt), Some(it)) => {
                            if !self.accepts(&it, &dt) {
                                self.error(format!("binding '{}' declared as {:?} but initializer is {:?}", d.name, dt, it), "".to_string());
                            }
                            env.insert(d.name.clone(), dt);
                        }
                        (Some(dt), None) => {
                            env.insert(d.name.clone(), dt);
                        }
                        (None, Some(it)) => {
                            env.insert(d.name.clone(), it);
                        }
                        (None, None) => {
                            env.insert(d.name.clone(), Kind::Unknown);
                        }
                    }
                }
                Stmt::Assign(a) => {
                    let val_ty = self.check_expr(&a.value, env);
                    match &a.target {
                        ast::AssignTarget::Name(n) => {
                            let target_ty = env.get(n).cloned();
                            match target_ty {
                                Some(tt) => {
                                    if !self.accepts(&val_ty, &tt) {
                                        self.error(format!("cannot assign {:?} to '{}' of type {:?}", val_ty, n, tt), "".to_string());
                                    }
                                }
                                None => self.error(format!("unknown name '{}' in assignment", n), "".to_string()),
                            }
                        }
                        ast::AssignTarget::Field(base, fname) => {
                            let base_ty = self.check_expr(base, env);
                            match &base_ty {
                                Kind::Record(rec) => {
                                    match self.fields.get(&(rec.clone(), fname.clone())) {
                                        Some(ft) => {
                                            if !self.accepts(&val_ty, ft) {
                                                self.error(format!("cannot assign {:?} to field '{}' of type {:?}", val_ty, fname, ft), "".to_string());
                                            }
                                        }
                                        None => self.error(format!("record '{}' has no field '{}'", rec, fname), "".to_string()),
                                    }
                                }
                                _ => self.error(format!("cannot assign field on non-record"), "".to_string()),
                            }
                        }
                    }
                }
                Stmt::Expr(e) => {
                    let _ = self.check_expr(e, env);
                }
                Stmt::Break => {}
                Stmt::Continue => {}
            }
        }
    }

    fn check_expr(&mut self, e: &Expr, env: &mut HashMap<String, Kind>) -> Kind {
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
                                self.error(format!("variant '{}' expects {} fields, got {}", n, fts.len(), args.len()), "".to_string());
                            }
                            for (i, a) in args.iter().enumerate() {
                                let at = self.check_expr(a, env);
                                if i < fts.len() {
                                    if !self.accepts(&at, &fts[i]) {
                                        self.error(format!("variant '{}' field {} type mismatch", n, i), "".to_string());
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
                                self.error(format!("'{}' expects {} args, got {}", n, ft.len(), args.len()), "".to_string());
                            }
                            for (i, a) in args.iter().enumerate() {
                                let at = self.check_expr(a, env);
                                if i < ft.len() {
                                    if !self.accepts(&at, &ft[i]) {
                                        self.error(format!("argument {} to record constructor '{}' type mismatch", i, n), "".to_string());
                                    }
                                }
                            }
                            return Kind::Record(n.clone());
                        }
                        // regular function call
                        match self.funcs.get(n).cloned() {
                            Some((params, ret, _tvs)) => {
                                if args.len() != params.len() {
                                    self.error(format!("function '{}' expects {} args, got {}", n, params.len(), args.len()), "".to_string());
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
                                            self.error(format!("argument {} to '{}' type mismatch", i, n), "".to_string());
                                        }
                                    }
                                }
                                match ret {
                                    Some(t) => subst(&t, &bindings),
                                    None => Kind::Unit,
                                }
                            }
                            None => {
                                self.error(format!("unknown function '{}'", n), "".to_string());
                                Kind::Unknown
                            }
                        }
                    }
                    Expr::Field(receiver, method) => {
                        let rt = self.check_expr(receiver, env);
                        let mt = method_type(&rt, method);
                        // check arg count for the method
                        if matches!(method.as_str(), "len" | "abs" | "to_str" | "to_upper" | "to_lower" | "is_empty") && args.len() != 0 {
                            self.error(format!("method '{}' takes no arguments", method), "".to_string());
                        }
                        for a in args {
                            let _ = self.check_expr(a, env);
                        }
                        match mt {
                            Some(t) => t,
                            None => {
                                self.error(format!("no method '{}' on {:?}", method, rt), "".to_string());
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
                                self.error(format!("record '{}' has no field '{}'", rec, fname), "".to_string());
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
                            self.error(format!("Result has no field '{}' (use .value / .err)", fname), "".to_string());
                            Kind::Unknown
                        }
                    }
                    Kind::Enum(_) => Kind::Unknown,
                    _ => {
                        self.error(format!("cannot access field '{}' of {:?}", fname, bt), "".to_string());
                        Kind::Unknown
                    }
                }
            }
            Expr::Index(base, idx) => {
                let _ = self.check_expr(base, env);
                let _ = self.check_expr(idx, env);
                Kind::Unknown
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
                                    self.error(format!("cannot propagate {:?} via '?': the enclosing function's error channel is {:?}", callee_e, caller_e), "".to_string());
                                }
                            }
                            None => {
                                self.error(format!("cannot use '?' here: the enclosing function has no error channel (declare Result[_, E])"), "".to_string());
                            }
                        }
                        (**t).clone()
                    }
                    _ => {
                        self.error(format!("'?' not allowed on {:?}", bt), "".to_string());
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
                            self.error(format!("arithmetic on mismatched types"), "".to_string());
                        }
                        at
                    }
                    BinOp::And | BinOp::Or | BinOp::Implies => {
                        if at != Kind::Bool || bt != Kind::Bool {
                            self.error(format!("logical op requires Bools"), "".to_string());
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
                    self.error(format!("cannot cast {:?} to {:?} (allowed: Int<->usize, Int->Float, Float->Int, Str<->Bytes)", at, tt), "".to_string());
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
                        self.error(format!("match arms produce different types: {:?} vs {:?}", result_ty, bt), "".to_string());
                    }
                }
                // exhaustiveness: an enum subject must cover all its variants
                if let Kind::Enum(ename) = &st {
                    if !has_wildcard {
                        if let Some(vnames) = self.enum_variants.get(ename).cloned() {
                            for v in &vnames {
                                if !covered.contains(v) {
                                    self.error(format!("match on enum '{}' is not exhaustive: missing variant '{}'", ename, v), "".to_string());
                                }
                            }
                        }
                    }
                }
                result_ty
            }
            Expr::If(ifx) => {
                let _ = self.check_expr(&ifx.cond, env);
                let _ = self.check_block(&ifx.then_block, env);
                // cond etc ignored for type
                Kind::Unknown
            }
            Expr::Loop(b) => {
                self.check_block(b, env);
                Kind::Never
            }
            Expr::For(name, iter, b) => {
                let it = self.check_expr(iter, env);
                let _ = it;
                env.insert(name.clone(), Kind::Unknown);
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
            // Float accepts Float; unit case
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
