use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::intrinsics::Intrinsic;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::ast::{self, Block, Expr, Pattern, Stmt, Type};
use crate::backend::llvm_backend::{kind_from_ast, LlvmBackend};
use crate::typecheck::Kind;

pub const PI: f64 = 3.14159265358979323846;
pub const E: f64 = 2.71828182845904523536;

/// Per-function codegen state. It borrows the shared backend and keeps the
/// lexical scope (name -> alloca) plus the enclosing return type.
pub struct Codegen<'b, 'ctx> {
    pub backend: &'b mut LlvmBackend<'ctx>,
    /// name -> (alloca, its LLVM type) for loads and assignments
    pub scope: HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    /// the enclosing function's declared return type (for ok/err payloads)
    pub ret_kind: Option<Kind>,
    /// the enclosing function's LLVM return type; None for void (Unit)
    pub ret_llvm: Option<BasicTypeEnum<'ctx>>,
    str_count: usize,
    /// while evaluating a `let name: Option[T] = none`, the declared kind, so the
    /// `none` literal can materialize a zero of that Option's struct
    none_hint: Option<Kind>,
    /// name -> owns a heap-allocated Str buffer, meaning this binding is the
    /// only reference to it (safe to free on overwrite / scope exit). A value
    /// is recorded here only when it was produced by concat/to_str in this
    /// function and has not been copied since. Absence means "static or
    /// shared — never free" (see docs/13-codegen.md § Str memory).
    owns: HashMap<String, bool>,
    /// true when the enclosing function can escape Str storage through its
    /// return value (return type is or contains Str). Under that rule the
    /// function's Str bindings are never released at exit — conservative leak
    /// that keeps every returned buffer alive for the caller.
    leak_strs: bool,
    /// the innermost enclosing loop's continue-target and break-target blocks,
    /// for `break`/`continue` statements (nested loops push/pop).
    loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
    /// true for `main`, which is the C entry point and returns i32 (0), not
    /// its declared Xz return type.
    is_main: bool,
}

type GenResult<'ctx> = Result<BasicValueEnum<'ctx>, String>;

/// `true` when `k` is or transitively contains a `Str`/`Bytes`. A function
/// whose return type is, or contains, Str can escape an owned buffer through
/// its return value, so its Str bindings are never freed at exit (the caller
/// owns them — conservative leak that keeps the contract sound).
fn kind_contains_str(k: &Kind, backend: &LlvmBackend<'_>) -> bool {
    match k {
        Kind::Str | Kind::Bytes => true,
        Kind::Option(t) | Kind::Result(t, _) => kind_contains_str(t, backend),
        Kind::Record(name) => backend
            .record_fields
            .get(name)
            .map(|fs| fs.iter().any(|f| kind_contains_str(f, backend)))
            .unwrap_or(false),
        Kind::Enum(name) => backend
            .variants
            .values()
            .any(|(en, _, fields)| en == name && fields.iter().any(|f| kind_contains_str(f, backend))),
        Kind::ErrUnion(members) => members.iter().any(|m| kind_contains_str(m, backend)),
        _ => false,
    }
}

impl<'b, 'ctx> Codegen<'b, 'ctx> {
    fn fail<T>(&mut self, msg: &str) -> Result<T, String> {
        self.backend.fail(msg);
        Err(msg.to_string())
    }

    /// The LLVM basic type of a value.
    fn basic_type_of(&self, v: BasicValueEnum<'ctx>) -> BasicTypeEnum<'ctx> {
        v.get_type()
    }

    fn is_float(&self, v: BasicValueEnum<'ctx>) -> bool {
        matches!(v.get_type(), inkwell::types::BasicTypeEnum::FloatType(_))
    }
    fn is_struct(&self, v: BasicValueEnum<'ctx>) -> bool {
        matches!(v.get_type(), inkwell::types::BasicTypeEnum::StructType(_))
    }
    fn is_str(&self, v: BasicValueEnum<'ctx>) -> bool {
        match v.get_type() {
            inkwell::types::BasicTypeEnum::StructType(st) => st == self.backend.types.xz_str,
            _ => false,
        }
    }
    fn is_ptr(&self, v: BasicValueEnum<'ctx>) -> bool {
        matches!(v.get_type(), inkwell::types::BasicTypeEnum::PointerType(_))
    }

    fn build_load(&mut self, ty: BasicTypeEnum<'ctx>, ptr: PointerValue<'ctx>, name: &str) -> BasicValueEnum<'ctx> {
        self.backend.builder.build_load(ty, ptr, name).unwrap()
    }

    /// Alloca + store a value and bind it in scope, remembering its type.
    fn bind_value(&mut self, name: &str, v: BasicValueEnum<'ctx>) -> PointerValue<'ctx> {
        let ty = self.basic_type_of(v);
        let a = self.backend.builder.build_alloca(ty, name).unwrap();
        self.backend.builder.build_store(a, v).unwrap();
        self.scope.insert(name.to_string(), (a, ty));
        a
    }

    /// Bind a value with an explicit declared type (used when the value's own
    /// type is not the binding's type, e.g. `none` in `let x: Option[T]`).
    fn bind_typed(&mut self, name: &str, ty: BasicTypeEnum<'ctx>, v: BasicValueEnum<'ctx>) {
        let a = self.backend.builder.build_alloca(ty, name).unwrap();
        self.backend.builder.build_store(a, v).unwrap();
        self.scope.insert(name.to_string(), (a, ty));
    }

    /// If `cond` is `NAME is some|ok` (true → narrow to payload in the then
    /// branch) or `NAME is none|err` (false → narrow to payload in the else),
    /// return (name, then_payload). Mirrors the type checker's flow typing.
    fn extract_narrow(cond: &Expr) -> Option<(String, bool)> {
        match cond {
            Expr::Binary(op, a, b) => {
                let _ = b;
                match &**a {
                    Expr::Name(n) => {
                        if op == &ast::BinOp::IsSome || op == &ast::BinOp::IsOk {
                            Some((n.clone(), true))
                        } else if op == &ast::BinOp::IsNone || op == &ast::BinOp::IsErr {
                            Some((n.clone(), false))
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

    /// Rebind `name` so it points at the payload field (index 0) of its
    /// Option/Result struct, for a narrowed branch. Returns the old binding.
    fn narrow_scope(&mut self, name: &str) -> Option<(String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>))> {
        match self.scope.get(name) {
            Some((ptr, ty)) => {
                let ptr = *ptr;
                let ty = *ty;
                if matches!(ty, BasicTypeEnum::StructType(_)) {
                    let st = ty.into_struct_type();
                    if st.count_fields() == 2 {
                        if let Some(pty) = st.get_field_type_at_index(0) {
                            let pptr = self
                                .backend
                                .builder
                                .build_struct_gep(st, ptr, 0, &format!("{}.payload", name))
                                .unwrap();
                            let old = self.scope.get(name).cloned().unwrap();
                            self.scope.insert(name.to_string(), (pptr, pty));
                            return Some((name.to_string(), old));
                        }
                    }
                }
                None
            }
            None => None,
        }
    }

    fn restore_scope(&mut self, saved: Option<(String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>))>) {
        if let Some((name, old)) = saved {
            self.scope.insert(name, old);
        }
    }

    /// Materialize a string literal as an LLVM global; returns {ptr, len}.
    fn str_value(&mut self, s: &str) -> BasicValueEnum<'ctx> {
        let name = format!(".xzstr{}", self.str_count);
        self.str_count += 1;
        let g = self.backend.builder.build_global_string_ptr(s, &name).unwrap();
        let ptr = g.as_pointer_value();
        let len = self.backend.types.int.const_int(s.len() as u64, false);
        let mut st = self.backend.types.xz_str.const_zero();
        st = self.backend.builder.build_insert_value(st, ptr, 0, "s.ptr").unwrap().into_struct_value();
        st = self.backend.builder.build_insert_value(st, len, 1, "s.len").unwrap().into_struct_value();
        st.into()
    }

    fn str_parts(&mut self, v: BasicValueEnum<'ctx>) -> (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let st = v.into_struct_value();
        let ptr = self.backend.builder.build_extract_value(st, 0, "sp").unwrap().into_pointer_value();
        let len = self.backend.builder.build_extract_value(st, 1, "sl").unwrap().into_int_value();
        (ptr, len)
    }

    /// Allocate a heap box for an aggregate type of the given ABI size, by
    /// calling the module's declared `malloc(i64)`. See `gen_enum_ctor`.
    fn malloc_box(
        &mut self,
        ty: inkwell::types::StructType<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, String> {
        let size = match &self.backend.target_data {
            Some(td) => td.get_abi_size(&ty),
            None => {
                return self.fail(&format!("cannot size {} box: no target data", name));
            }
        };
        let f = self.backend.module.get_function("malloc").ok_or("malloc missing")?;
        let sz = self.backend.types.int.const_int(size, false);
        let call = self
            .backend
            .builder
            .build_direct_call(f, &[sz.into()], &format!("{}.box", name))
            .unwrap();
        Ok(call.try_as_basic_value().basic().unwrap().into_pointer_value())
    }

    /// Emit a call to an LLVM intrinsic. Unlike host calls, intrinsics are
    /// understood by the optimizer and lowered inline by the backend, so
    /// `llvm.sqrt.f64` / `llvm.fabs.f64` become native instructions (no C
    /// ABI round-trip). `param_tys` disambiguates overloaded intrinsics.
    fn call_intrinsic(
        &mut self,
        name: &str,
        param_tys: Vec<BasicTypeEnum<'ctx>>,
        args: &[BasicValueEnum<'ctx>],
        label: &str,
    ) -> GenResult<'ctx> {
        let intr = Intrinsic::find(name).ok_or_else(|| format!("intrinsic {} unknown", name))?;
        let fv = intr
            .get_declaration(&self.backend.module, &param_tys)
            .ok_or_else(|| format!("cannot declare intrinsic {}", name))?;
        let call_args: Vec<inkwell::values::BasicMetadataValueEnum> = args.iter().map(|v| (*v).into()).collect();
        let call = self.backend.builder.build_direct_call(fv, &call_args, label).unwrap();
        Ok(call.try_as_basic_value().basic().unwrap())
    }

    /// Emit `xz_str_free(ptr, len)` for a Str value. The runtime no-ops for
    /// literals and already-freed buffers, so the only unsound use is freeing
    /// a *shared* heap buffer (see adopt_ownership / free_owned_bindings).
    fn emit_str_free(&mut self, v: BasicValueEnum<'ctx>) {
        if !self.is_str(v) {
            return;
        }
        let (p, l) = self.str_parts(v);
        if let Some(f) = self.backend.module.get_function("xz_str_free") {
            let _ = self
                .backend
                .builder
                .build_direct_call(f, &[p.into(), l.into()], "strfree")
                .unwrap();
        }
    }

    /// Is `e` an expression whose value is a *fresh* heap Str buffer that no
    /// binding holds? Concat and scalar-to_str produce new buffers; `Str.to_str()`
    /// is identity, so a Str-typed base means the result aliases the base (not
    /// a fresh buffer). Used to recognize the "temp passed straight to a call"
    /// pattern (e.g. `print(n.to_str())`) and to keep `print(s)` where `s` is a
    /// live binding alive for its own scope-exit release.
    fn is_fresh_temp(&self, e: &Expr) -> bool {
        match e {
            Expr::Str(_) | Expr::RawStr(_) => false,
            Expr::Binary(op, ..) if op == &ast::BinOp::Add => true,
            Expr::Call(callee, _) => match &**callee {
                Expr::Field(recv, m) if m == "to_str" => {
                    if self.base_is_str(recv) {
                        self.is_fresh_temp(recv)
                    } else {
                        true // scalar.to_str(): a new buffer
                    }
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn base_is_str(&self, e: &Expr) -> bool {
        match e {
            Expr::Str(_) | Expr::RawStr(_) => true,
            Expr::Name(n) => matches!(self.scope.get(n), Some((_, ty)) if *ty == self.backend.types.xz_str.into()),
            _ => false,
        }
    }

    /// The binding a Str expression reads from, if any (a copy alias). Used to
    /// downgrade the source binding's ownership when it is copied.
    fn alias_source(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Name(n) => Some(n.clone()),
            Expr::Call(callee, _) => match &**callee {
                Expr::Field(recv, m) if m == "to_str" && self.base_is_str(recv) => self.alias_source(recv),
                _ => None,
            },
            _ => None,
        }
    }

    /// After binding `name` to `v` (a Str), record whether it is a unique
    /// heap-owning binding, and mark any existing binding it copied as no
    /// longer (the buffer is now shared → never free).
    fn adopt_ownership(&mut self, name: &str, init: &Expr) {
        if let Some(base) = self.alias_source(init) {
            self.owns.remove(&base);
        }
        if self.is_fresh_temp(init) {
            self.owns.insert(name.to_string(), true);
        } else {
            self.owns.remove(name);
        }
    }

    /// Mark every owned binding reachable through `e` (a Name, or nested in a
    /// larger expression) as shared. Used when a Str value flows into an
    /// aggregate or phi we do not track: the target may outlive this binding,
    /// so the source buffer must not be freed underneath it.
    fn degrade_str_expr(&mut self, e: &Expr) {
        match e {
            Expr::Name(n) => {
                self.owns.remove(n);
            }
            Expr::Binary(_op, a, b) => {
                self.degrade_str_expr(a);
                self.degrade_str_expr(b);
            }
            Expr::Call(callee, args) => {
                self.degrade_str_expr(callee);
                for a in args {
                    self.degrade_str_expr(a);
                }
            }
            Expr::Field(base, _) => self.degrade_str_expr(base),
            Expr::Prop(base, _) => self.degrade_str_expr(base),
            Expr::Unary(_op, a) => self.degrade_str_expr(a),
            Expr::Cast(a, _) => self.degrade_str_expr(a),
            Expr::Some(a) => self.degrade_str_expr(a),
            Expr::Ok(inner) => {
                if let Some(a) = inner {
                    self.degrade_str_expr(a);
                }
            }
            Expr::If(ifx) => {
                self.degrade_str_expr(&ifx.cond);
                self.degrade_block(&ifx.then_block);
                if let Some(b) = &ifx.else_block {
                    self.degrade_block(b);
                }
                for (c, b) in &ifx.elif {
                    self.degrade_str_expr(c);
                    self.degrade_block(b);
                }
            }
            Expr::Match(sub, arms) => {
                self.degrade_str_expr(sub);
                for (_p, b) in arms {
                    self.degrade_str_expr(b);
                }
            }
            _ => {}
        }
    }

    fn snapshot_owned(&self) -> std::collections::HashSet<String> {
        self.owns.keys().cloned().collect()
    }

    /// Drop ownership of any binding that was *created inside* a conditional
    /// arm. On other branch paths its alloca may be uninitialized, so freeing
    /// it at a later function exit (or `?`) would read a garbage/poison buffer.
    /// Such bindings leak instead (conservative but sound).
    fn after_branch(&mut self, before: &std::collections::HashSet<String>) {
        let cur: Vec<String> = self.owns.keys().cloned().collect();
        for k in cur {
            if !before.contains(&k) {
                self.owns.remove(&k);
            }
        }
    }

    fn degrade_block(&mut self, b: &Block) {
        for s in &b.stmts {
            match s {
                Stmt::Expr(e) => self.degrade_str_expr(e),
                Stmt::Decl(d) => {
                    if let Some(init) = &d.init {
                        self.degrade_str_expr(init);
                    }
                }
                Stmt::Assign(a) => self.degrade_str_expr(&a.value),
                _ => {}
            }
        }
    }

    /// Free every Str binding this function frame uniquely owns (fresh heap
    /// buffers that were never copied). Called just before each return path.
    /// Bindings whose shared buffers were copied or that came from aliased
    /// sources are deliberately left to leak (never freed).
    fn free_owned_bindings(&mut self) {
        if self.leak_strs {
            return;
        }
        let owned: Vec<String> = self.owns.keys().cloned().collect();
        for n in owned {
            if let Some((ptr, ty)) = self.scope.get(&n).map(|(p, t)| (*p, *t)) {
                if matches!(ty, BasicTypeEnum::StructType(st) if st == self.backend.types.xz_str) {
                    let v = self.build_load(ty, ptr, &n);
                    self.emit_str_free(v);
                }
            }
        }
        self.owns.clear();
    }

    fn concat_str(&mut self, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> GenResult<'ctx> {
        let (ap, al) = self.str_parts(a);
        let (bp, bl) = self.str_parts(b);
        let f = self.backend.module.get_function("xz_concat").ok_or("xz_concat missing")?;
        let call = self
            .backend
            .builder
            .build_direct_call(f, &[ap.into(), al.into(), bp.into(), bl.into()], "concat")
            .unwrap();
        Ok(call.try_as_basic_value().basic().unwrap())
    }

    // ------------------------------------------------------------------ //
    //  Statements / blocks
    // ------------------------------------------------------------------ //

    /// Generate a block's statements; return the final expression's value if
    /// the block ends in a value expression.
    fn gen_block(&mut self, block: &ast::Block) -> Option<BasicValueEnum<'ctx>> {
        let mut last: Option<BasicValueEnum<'ctx>> = None;
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            // After a `break`/`continue`, the current block is already
            // terminated; the remaining statements in this block (and any
            // fall-through value) are unreachable and must not be lowered.
            if self.block_terminated() {
                break;
            }
            match stmt {
                Stmt::Decl(d) => self.gen_decl(d),
                Stmt::Assign(a) => self.gen_assign(a),
                Stmt::Expr(e) => {
                    let v = self.gen_expr(e);
                    if i == n - 1 {
                        last = v.ok();
                    }
                }
                Stmt::Break => {
                    let _ = self.gen_break();
                }
                Stmt::Continue => {
                    let _ = self.gen_continue();
                }
            }
        }
        last
    }

    fn gen_decl(&mut self, d: &ast::Decl) {
        if d.recv {
            let _ = self.fail::<()>("channel recv (let x <- recv) is not supported in Phase 4");
            return;
        }
        let declared_kind: Option<Kind> = match &d.ty {
            Some(ty) => Some(kind_from_ast(ty, self.backend)),
            None => None,
        };
        match &d.init {
            Some(e) => {
                // Let the `none` literal know the declared Option type.
                let saved_hint = self.none_hint.clone();
                self.none_hint = declared_kind.clone();
                let r = self.gen_expr(e);
                self.none_hint = saved_hint;
                match r {
                    Ok(v) => match declared_kind {
                        Some(k) => {
                            let lty = self.backend.kind_to_llvm(&k);
                            self.bind_typed(&d.name, lty, v);
                        }
                        None => {
                            self.bind_value(&d.name, v);
                        }
                    },
                    Err(_) => {}
                }
                // Track Str ownership: a fresh buffer bound to `name` becomes
                // this binding's, freed at scope exit; an alias downgrades the
                // source binding so neither is freed.
                self.adopt_ownership(&d.name, e);
            }
            None => {
                // declared type, no initializer
                match &d.ty {
                    Some(ty) => {
                        let k = kind_from_ast(ty, self.backend);
                        let lty = self.backend.kind_to_llvm(&k);
                        let alloca = self.backend.builder.build_alloca(lty, &d.name).unwrap();
                        self.scope.insert(d.name.clone(), (alloca, lty));
                    }
                    None => {
                        let _ = self.fail::<()>(&format!("binding '{}' has no initializer or type", d.name));
                    }
                }
            }
        }
    }

    fn gen_assign(&mut self, a: &ast::Assign) {
        match &a.target {
            ast::AssignTarget::Name(n) => {
                match self.gen_expr(&a.value) {
                    Ok(v) => match self.scope.get(n) {
                        Some((ptr, ty)) => {
                            let ptr = *ptr;
                            let ty = *ty;
                            // Overwriting a binding that uniquely owns a heap
                            // buffer with a *fresh* Str: free the old buffer.
                            // If the new value is an alias (name/identity), the
                            // binding is being shared, so keep the old one
                            // alive (leak) rather than free underneath.
                            if self.owns.contains_key(n) && self.is_fresh_temp(&a.value) {
                                let old = self.build_load(ty, ptr, "old");
                                self.emit_str_free(old);
                            }
                            let _ = self.apply_assign_op(&a.op, ty, ptr, v);
                            self.adopt_ownership(n, &a.value);
                        }
                        None => {
                            let _ = self.fail::<()>(&format!("unknown name '{}' in assignment", n));
                        }
                    },
                    Err(_) => {}
                }
            }
            ast::AssignTarget::Field(base, _fname) => {
                let _ = self.gen_expr(base);
                let _ = self.fail::<()>("field assignment is not supported in Phase 4");
            }
        }
    }

    fn apply_assign_op(
        &mut self,
        op: &ast::AssignOp,
        ty: BasicTypeEnum<'ctx>,
        ptr: PointerValue<'ctx>,
        v: BasicValueEnum<'ctx>,
    ) -> GenResult<'ctx> {
        use crate::ast::AssignOp::*;
        match op {
            Set => {
                self.backend.builder.build_store(ptr, v).unwrap();
                Ok(v)
            }
            Add | Sub | Mul | Div => {
                let cur = self.build_load(ty, ptr, "lhs");
                let binop = match op {
                    Add => ast::BinOp::Add,
                    Sub => ast::BinOp::Sub,
                    Mul => ast::BinOp::Mul,
                    Div => ast::BinOp::Div,
                    _ => unreachable!(),
                };
                let r = self.binop(binop, cur, v)?;
                self.backend.builder.build_store(ptr, r).unwrap();
                Ok(r)
            }
        }
    }

    // ------------------------------------------------------------------ //
    //  Expressions
    // ------------------------------------------------------------------ //

    pub fn gen_expr(&mut self, e: &Expr) -> GenResult<'ctx> {
        match e {
            Expr::Int(v) => Ok(self.backend.types.int.const_int(*v, false).into()),
            Expr::Float(v) => Ok(self.backend.types.float.const_float(*v).into()),
            Expr::Char(c) => Ok(self.backend.types.char.const_int(*c as u64, false).into()),
            Expr::Str(s) | Expr::RawStr(s) => Ok(self.str_value(s)),
            Expr::Bool(b) => Ok(self.backend.types.bool.const_int(*b as u64, false).into()),
            Expr::None => Ok(self.none_value()),
            Expr::Name(n) => self.gen_name(n),
            Expr::Call(callee, args) => self.gen_call(callee, args),
            Expr::Field(base, fname) => self.gen_field(base, fname),
            Expr::Index(_, _) => self.fail("collection indexing is not supported in Phase 4"),
            Expr::Prop(base, _) => self.gen_prop(base),
            Expr::Unary(op, a) => self.gen_unary(op, a),
            Expr::Binary(op, a, b) => {
                let av = self.gen_expr(a)?;
                let bv = self.gen_expr(b)?;
                self.binop(op.clone(), av, bv)
            }
            Expr::Cast(a, ty) => {
                let v = self.gen_expr(a)?;
                self.gen_cast(v, ty)
            }
            Expr::Match(subject, arms) => self.gen_match(subject, arms),
            Expr::If(ifx) => self.gen_if(ifx),
            Expr::Loop(b) => self.gen_loop(b),
            Expr::For(name, iter, b) => self.gen_for(name, iter, b),
            Expr::Await(_) | Expr::Send(_, _) | Expr::Recv(_) => {
                self.fail("async/channel are not supported in Phase 4")
            }
            Expr::Transfer(a) => {
                // `transfer(x)` hands a handle to a callee. At runtime a handle
                // is passed by value, so the transfer is an identity move — the
                // ownership bookkeeping is a compile-time concern (docs/10).
                self.gen_expr(a)
            }
            Expr::Ok(inner) => self.gen_ok(inner),
            Expr::Err(a) => {
                // The error payload is never read (only the ok-flag matters), so
                // we skip evaluating it entirely.
                let _ = a;
                self.gen_err()
            }
            Expr::Some(a) => {
                self.degrade_str_expr(a);
                let v = self.gen_expr(a)?;
                self.result_value(v, true)
            }
        }
    }

    fn gen_name(&mut self, n: &str) -> GenResult<'ctx> {
        if let Some((ptr, ty)) = self.scope.get(n) {
            let ptr = *ptr;
            let ty = *ty;
            return Ok(self.build_load(ty, ptr, n));
        }
        match n {
            "PI" => Ok(self.backend.types.float.const_float(PI).into()),
            "E" => Ok(self.backend.types.float.const_float(E).into()),
            _ => self.fail(&format!("unknown name '{}'", n)),
        }
    }

    /// None / err / early-return zero aggregates for Result/Option.
    fn result_value(&mut self, payload: BasicValueEnum<'ctx>, ok: bool) -> GenResult<'ctx> {
        let pt = self.basic_type_of(payload);
        let st = self.backend.context.struct_type(&[pt.into(), self.backend.types.bool.into()], false);
        let mut agg = st.const_zero();
        agg = self.backend.builder.build_insert_value(agg, payload, 0, "p").unwrap().into_struct_value();
        agg = self
            .backend
            .builder
            .build_insert_value(agg, self.backend.types.bool.const_int(ok as u64, false), 1, "f")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// `none` is the absence literal. With a declared Option[T] (via none_hint)
    /// it is a zero of that Option's struct; otherwise it has no usable value.
    fn none_value(&mut self) -> BasicValueEnum<'ctx> {
        match &self.none_hint {
            Some(Kind::Option(inner)) => {
                let payload = self.backend.kind_to_llvm(inner);
                let st = self.backend.context.struct_type(&[payload.into(), self.backend.types.bool.into()], false);
                st.const_zero().into()
            }
            Some(Kind::Result(t, _)) => {
                let payload = self.backend.kind_to_llvm(t);
                let st = self.backend.context.struct_type(&[payload.into(), self.backend.types.bool.into()], false);
                st.const_zero().into()
            }
            _ => self.backend.types.unit.const_zero().into(),
        }
    }

    fn gen_ok(&mut self, inner: &Option<Box<Expr>>) -> GenResult<'ctx> {
        match inner {
            Some(e) => {
                self.degrade_str_expr(e);
                let v = self.gen_expr(e)?;
                self.result_value(v, true)
            }
            None => {
                // ok() with Unit payload
                let payload = self.backend.types.unit.const_zero().into();
                self.result_value(payload, true)
            }
        }
    }

    fn gen_err(&mut self) -> GenResult<'ctx> {
        // payload type comes from the enclosing Result return
        let payload_ty = match &self.ret_kind {
            Some(Kind::Result(t, _)) => self.backend.kind_to_llvm(t),
            Some(Kind::Option(t)) => self.backend.kind_to_llvm(t),
            _ => self.backend.types.unit.into(),
        };
        let st = self.backend.context.struct_type(&[payload_ty.into(), self.backend.types.bool.into()], false);
        let mut agg = st.const_zero();
        agg = self
            .backend
            .builder
            .build_insert_value(agg, self.backend.types.bool.const_int(0, false), 1, "f")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// `?` — on err, early-return a zero aggregate of the enclosing return.
    fn gen_prop(&mut self, base: &Expr) -> GenResult<'ctx> {
        let val = self.gen_expr(base)?;
        let st = val.into_struct_value();
        let ok = self.backend.builder.build_extract_value(st, 1, "prop.ok").unwrap().into_int_value();
        let fnv = self.cur_fn();
        let then_bb = self.backend.context.append_basic_block(fnv, "prop.then");
        let err_bb = self.backend.context.append_basic_block(fnv, "prop.err");
        self.backend.builder.build_conditional_branch(ok, then_bb, err_bb).unwrap();

        self.backend.builder.position_at_end(err_bb);
        self.free_owned_bindings();
        self.early_return()?;
        self.backend.builder.position_at_end(then_bb);
        let payload = self.backend.builder.build_extract_value(st, 0, "prop.val").unwrap();
        Ok(payload)
    }

    /// Emit a return for the enclosing function (used by `?`).
    fn early_return(&mut self) -> GenResult<'ctx> {
        match self.ret_llvm {
            Some(rt) => {
                let zero = rt.const_zero();
                self.backend.builder.build_return(Some(&zero)).unwrap();
            }
            None => {
                self.backend.builder.build_return(None).unwrap();
            }
        }
        Ok(self.backend.types.unit.const_zero().into())
    }

    fn cur_fn(&mut self) -> FunctionValue<'ctx> {
        // The current function is tracked by the builder's current function;
        // we reconstruct it from the insert block's parent.
        let bb = self.backend.builder.get_insert_block().unwrap();
        bb.get_parent().unwrap()
    }

    /// Whether the current insert block already ends in a terminator (a `break`
    /// / `continue` / `return` inside the block we just generated). If so, we
    /// must not append another branch to it.
    fn block_terminated(&self) -> bool {
        match self.backend.builder.get_insert_block() {
            Some(bb) => bb.get_terminator().is_some(),
            None => true,
        }
    }

    fn gen_unary(&mut self, op: &ast::UnaryOp, a: &Expr) -> GenResult<'ctx> {
        let v = self.gen_expr(a)?;
        match op {
            ast::UnaryOp::Neg => {
                if self.is_float(v) {
                    Ok(self.backend.builder.build_float_neg(v.into_float_value(), "neg").unwrap().into())
                } else {
                    Ok(self.backend.builder.build_int_neg(v.into_int_value(), "neg").unwrap().into())
                }
            }
            ast::UnaryOp::Not => {
                Ok(self.backend.builder.build_not(v.into_int_value(), "not").unwrap().into())
            }
        }
    }

    fn binop(&mut self, op: ast::BinOp, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> GenResult<'ctx> {
        use crate::ast::BinOp::*;
        match op {
            Add | Sub | Mul | Div | Mod => {
                if self.is_str(a) {
                    // Str concatenation (the one built-in overload)
                    if op == Add {
                        return self.concat_str(a, b);
                    }
                    return self.fail("non-add operator on Str");
                }
                if self.is_float(a) {
                    let af = a.into_float_value();
                    let bf = b.into_float_value();
                    let r = match op {
                        Add => self.backend.builder.build_float_add(af, bf, "add"),
                        Sub => self.backend.builder.build_float_sub(af, bf, "sub"),
                        Mul => self.backend.builder.build_float_mul(af, bf, "mul"),
                        Div => self.backend.builder.build_float_div(af, bf, "div"),
                        Mod => self.backend.builder.build_float_rem(af, bf, "rem"),
                        _ => unreachable!(),
                    }
                    .unwrap();
                    Ok(r.into())
                } else {
                    let ai = a.into_int_value();
                    let bi = b.into_int_value();
                    let r = match op {
                        Add => self.backend.builder.build_int_add(ai, bi, "add"),
                        Sub => self.backend.builder.build_int_sub(ai, bi, "sub"),
                        Mul => self.backend.builder.build_int_mul(ai, bi, "mul"),
                        Div => self.backend.builder.build_int_signed_div(ai, bi, "div"),
                        Mod => self.backend.builder.build_int_signed_rem(ai, bi, "rem"),
                        _ => unreachable!(),
                    }
                    .unwrap();
                    Ok(r.into())
                }
            }
            And | Or | Implies => {
                let ai = a.into_int_value();
                let bi = b.into_int_value();
                let r = match op {
                    And => self.backend.builder.build_and(ai, bi, "and"),
                    Or => self.backend.builder.build_or(ai, bi, "or"),
                    _ => {
                        // a implies b == !a or b
                        let na = self.backend.builder.build_not(ai, "nimpl").unwrap();
                        self.backend.builder.build_or(na, bi, "impl")
                    }
                }
                .unwrap();
                Ok(r.into())
            }
            Eq | Ne | Lt | Le | Gt | Ge => self.compare(op, a, b),
            IsOk | IsErr | IsNone | IsSome => self.gen_is(op, a),
        }
    }

    fn compare(&mut self, op: ast::BinOp, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> GenResult<'ctx> {
        use crate::ast::BinOp::*;
        let (pred_int, pred_float): (IntPredicate, FloatPredicate) = match op {
            Eq => (IntPredicate::EQ, FloatPredicate::OEQ),
            Ne => (IntPredicate::NE, FloatPredicate::ONE),
            Lt => (IntPredicate::SLT, FloatPredicate::OLT),
            Le => (IntPredicate::SLE, FloatPredicate::OLE),
            Gt => (IntPredicate::SGT, FloatPredicate::OGT),
            Ge => (IntPredicate::SGE, FloatPredicate::OGE),
            _ => unreachable!(),
        };
        if self.is_float(a) {
            let r = self
                .backend
                .builder
                .build_float_compare(pred_float, a.into_float_value(), b.into_float_value(), "cmp")
                .unwrap();
            Ok(r.into())
        } else if self.is_ptr(a) || self.is_ptr(b) {
            // Pointer comparison. The Phase 4 FFI pattern is a null check
            // (`p == 0` / `p != 0`); pointer-to-pointer equality is lowered by
            // comparing the addresses as integers.
            self.compare_ptrs(op, a, b)
        } else {
            let r = self
                .backend
                .builder
                .build_int_compare(pred_int, a.into_int_value(), b.into_int_value(), "cmp")
                .unwrap();
            Ok(r.into())
        }
    }

    /// Lower a comparison involving at least one pointer operand. `p == 0`
    /// (Int literal zero) becomes `is_null`; `p != 0` becomes `is_not_null`;
    /// pointer-to-pointer compares the addresses as integers.
    fn compare_ptrs(
        &mut self,
        op: ast::BinOp,
        a: BasicValueEnum<'ctx>,
        b: BasicValueEnum<'ctx>,
    ) -> GenResult<'ctx> {
        use crate::ast::BinOp::*;
        let (pv, other) = if self.is_ptr(a) { (a, b) } else { (b, a) };
        let pv = pv.into_pointer_value();
        // Is the non-pointer side an integer literal zero (the null check)?
        let other_is_zero = match other {
            BasicValueEnum::IntValue(iv) => iv.get_zero_extended_constant() == Some(0),
            _ => false,
        };
        if other_is_zero {
            let r = match op {
                Eq => self.backend.builder.build_is_null(pv, "isnull").unwrap(),
                Ne => self.backend.builder.build_is_not_null(pv, "notnull").unwrap(),
                _ => {
                    return self.fail("ordering comparison on a pointer is not supported in Phase 4");
                }
            };
            return Ok(r.into());
        }
        if self.is_ptr(other) {
            // ptr == ptr / ptr != ptr: compare addresses as integers.
            let addr_ty = self.backend.types.int;
            let ai = self
                .backend
                .builder
                .build_ptr_to_int(pv, addr_ty, "p2i")
                .unwrap();
            let bi = self
                .backend
                .builder
                .build_ptr_to_int(other.into_pointer_value(), addr_ty, "p2i")
                .unwrap();
            let (pred_int, _): (IntPredicate, FloatPredicate) = match op {
                Eq => (IntPredicate::EQ, FloatPredicate::OEQ),
                Ne => (IntPredicate::NE, FloatPredicate::ONE),
                _ => {
                    return self.fail("ordering comparison on a pointer is not supported in Phase 4");
                }
            };
            let r = self
                .backend
                .builder
                .build_int_compare(pred_int, ai, bi, "ptrcmp")
                .unwrap();
            return Ok(r.into());
        }
        self.fail("comparison between a pointer and a non-zero value is not supported in Phase 4")
    }

    fn gen_is(&mut self, op: ast::BinOp, a: BasicValueEnum<'ctx>) -> GenResult<'ctx> {
        use crate::ast::BinOp::*;
        let flag = self
            .backend
            .builder
            .build_extract_value(a.into_struct_value(), 1, "is.flag")
            .unwrap()
            .into_int_value();
        let r = match op {
            IsOk | IsSome => flag,
            IsErr | IsNone => self.backend.builder.build_not(flag, "isnot").unwrap(),
            _ => unreachable!(),
        };
        Ok(r.into())
    }

    fn gen_cast(&mut self, v: BasicValueEnum<'ctx>, ty: &Type) -> GenResult<'ctx> {
        let k = kind_from_ast(ty, self.backend);
        let to = self.backend.kind_to_llvm(&k);
        match (self.is_float(v), matches!(to, BasicTypeEnum::FloatType(_))) {
            (false, true) => {
                // int -> float
                let r = self
                    .backend
                    .builder
                    .build_signed_int_to_float(v.into_int_value(), self.backend.types.float, "cast")
                    .unwrap();
                Ok(r.into())
            }
            (true, false) => {
                // float -> int
                let r = self
                    .backend
                    .builder
                    .build_float_to_signed_int(v.into_float_value(), self.backend.types.int, "cast")
                    .unwrap();
                Ok(r.into())
            }
            // int <-> usize, Str <-> Bytes: same LLVM type, no-op
            _ => Ok(v),
        }
    }

    // ------------------------------------------------------------------ //
    //  Calls
    // ------------------------------------------------------------------ //

    fn gen_call(&mut self, callee: &Expr, args: &[Expr]) -> GenResult<'ctx> {
        match callee {
            Expr::Name(n) => self.gen_named_call(n, args),
            Expr::Field(receiver, method) => self.gen_method_call(receiver, method, args),
            _ => self.fail("unsupported callee"),
        }
    }

    fn gen_named_call(&mut self, name: &str, args: &[Expr]) -> GenResult<'ctx> {
        // record constructor
        if let Some(st) = self.backend.record_types.get(name).copied() {
            for a in args {
                self.degrade_str_expr(a);
            }
            let mut agg = st.const_zero();
            for (i, a) in args.iter().enumerate() {
                let v = self.gen_expr(a)?;
                agg = self
                    .backend
                    .builder
                    .build_insert_value(agg, v, i as u32, "rec")
                    .unwrap()
                    .into_struct_value();
            }
            return Ok(agg.into());
        }
        // enum variant constructor
        if let Some((_en, tag, fields)) = self.backend.variants.get(name).cloned() {
            return self.gen_enum_ctor(name, tag, fields, args);
        }
        // regular function / extern
        if let Some(fv) = self.backend.functions.get(name).copied() {
            let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
            for a in args {
                let v = self.gen_expr(a)?;
                call_args.push(v.into());
            }
            let call = self.backend.builder.build_direct_call(fv, &call_args, "call").unwrap();
            let out = call.try_as_basic_value().basic();
            return match out {
                Some(v) => Ok(v),
                None => Ok(self.backend.types.unit.const_zero().into()),
            };
        }
        // print is a special stdlib name not declared as a function
        if name == "print" {
            if args.len() != 1 {
                return self.fail("print takes one Str argument");
            }
            let v = self.gen_expr(&args[0])?;
            let (ptr, len) = self.str_parts(v);
            let f = self.backend.module.get_function("xz_print").ok_or("xz_print missing")?;
            let _ = self
                .backend
                .builder
                .build_direct_call(f, &[ptr.into(), len.into()], "print")
                .unwrap();
            // A fresh heap temp (concat/to_str result) passed directly to
            // print is consumed here: free it right after the call. A name or
            // literal is not a temp — the binding handles its own release.
            if self.is_fresh_temp(&args[0]) {
                self.emit_str_free(v);
            }
            return Ok(self.backend.types.unit.const_zero().into());
        }
        // stdlib approx_sqrt -> llvm.sqrt.f64 (native sqrt instruction)
        if name == "approx_sqrt" {
            if args.len() != 1 {
                return self.fail("approx_sqrt takes one Float");
            }
            let v = self.gen_expr(&args[0])?;
            return self.call_intrinsic("llvm.sqrt.f64", vec![self.backend.types.float.into()], &[v], "sqrt");
        }
        self.fail(&format!("unknown function '{}'", name))
    }

    fn gen_enum_ctor(
        &mut self,
        name: &str,
        tag: u32,
        fields: Vec<Kind>,
        args: &[Expr],
    ) -> GenResult<'ctx> {
        // allocate a box for the variant's field struct
        let field_tys: Vec<BasicTypeEnum<'ctx>> = fields.iter().map(|k| self.backend.kind_to_llvm(k)).collect();
        let vt = self.backend.context.struct_type(&field_tys, false);
        // Call libc malloc explicitly (i64 size) rather than inkwell's
        // `build_malloc`, whose builder data layout is empty and would emit an
        // i32-sized malloc that conflicts with the runtime's i64 declaration.
        let box_ptr = self.malloc_box(vt, name)?;
        for a in args {
            self.degrade_str_expr(a);
        }
        for (i, a) in args.iter().enumerate() {
            let v = self.gen_expr(a)?;
            let fptr = self.backend.builder.build_struct_gep(vt, box_ptr, i as u32, "fptr").unwrap();
            self.backend.builder.build_store(fptr, v).unwrap();
        }
        // build {box, tag}
        let enum_ty = self.backend.enum_struct(name);
        let mut agg = enum_ty.const_zero();
        agg = self
            .backend
            .builder
            .build_insert_value(agg, box_ptr, 0, "box")
            .unwrap()
            .into_struct_value();
        agg = self
            .backend
            .builder
            .build_insert_value(agg, self.backend.context.i32_type().const_int(tag as u64, false), 1, "tag")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    fn gen_method_call(&mut self, receiver: &Expr, method: &str, args: &[Expr]) -> GenResult<'ctx> {
        let rv = self.gen_expr(receiver)?;
        if method == "to_str" {
            return self.gen_to_str(rv);
        }
        if method == "len" {
            // Str.len() -> byte length (i64)
            if self.is_str(rv) {
                let (_, len) = self.str_parts(rv);
                return Ok(len.into());
            }
            return self.fail("len() only supported on Str in Phase 4");
        }
        if method == "is_empty" {
            let (_, len) = self.str_parts(rv);
            let zero = self.backend.types.int.const_int(0, false);
            let r = self
                .backend
                .builder
                .build_int_compare(IntPredicate::EQ, len, zero, "empty")
                .unwrap();
            return Ok(r.into());
        }
        if method == "abs" {
            // llvm.abs / llvm.fabs are native (fully inlineable) — no C ABI
            // round-trip like the host abs used to be.
            if self.is_float(rv) {
                return self.call_intrinsic("llvm.fabs.f64", vec![self.backend.types.float.into()], &[rv], "abs");
            }
            let iv = rv.into_int_value();
            // llvm.abs.i64(v, i1 is_int_min_poison=false)
            let poison = self.backend.types.bool.const_zero();
            let args: Vec<BasicValueEnum<'ctx>> = vec![iv.into(), poison.into()];
            return self.call_intrinsic("llvm.abs.i64", vec![self.backend.types.int.into()], &args, "abs");
        }
        // other methods take no args
        for a in args {
            let _ = self.gen_expr(a)?;
        }
        self.fail(&format!("method '{}' not supported in Phase 4", method))
    }

    fn gen_to_str(&mut self, v: BasicValueEnum<'ctx>) -> GenResult<'ctx> {
        let (name, arg): (&str, inkwell::values::BasicMetadataValueEnum) = match v.get_type() {
            inkwell::types::BasicTypeEnum::IntType(it) => {
                if it.get_bit_width() == 1 {
                    ("xz_bool_to_str", v.into())
                } else if it.get_bit_width() == 8 {
                    ("xz_char_to_str", v.into())
                } else {
                    ("xz_i64_to_str", v.into())
                }
            }
            inkwell::types::BasicTypeEnum::FloatType(_) => ("xz_f64_to_str", v.into()),
            inkwell::types::BasicTypeEnum::StructType(st) if st == self.backend.types.xz_str => {
                // Str.to_str() is identity
                return Ok(v);
            }
            _ => return self.fail("to_str() not supported for this type in Phase 4"),
        };
        let f = self
            .backend
            .module
            .get_function(name)
            .ok_or_else(|| format!("{} missing", name))?;
        let call = self.backend.builder.build_direct_call(f, &[arg], "tos").unwrap();
        Ok(call.try_as_basic_value().basic().unwrap())
    }

    // ------------------------------------------------------------------ //
    //  Fields (record access, Result.value)
    // ------------------------------------------------------------------ //

    fn gen_field(&mut self, base: &Expr, fname: &str) -> GenResult<'ctx> {
        let bv = self.gen_expr(base)?;
        if !self.is_struct(bv) {
            return self.fail(&format!("cannot access field '{}' of a non-struct", fname));
        }
        let st = bv.into_struct_value();
        let struct_ty = st.get_type();
        match struct_ty.get_name() {
            Some(_name) => {
                // named record — look up the field index
                let rec_name = struct_ty.get_name().unwrap().to_str().unwrap().to_string();
                match self.backend.record_field_index(&rec_name, fname) {
                    Some(i) => {
                        let v = self.backend.builder.build_extract_value(st, i, fname).unwrap();
                        Ok(v)
                    }
                    None => self.fail(&format!("record '{}' has no field '{}'", rec_name, fname)),
                }
            }
            None => {
                // Result / Option `.value`
                if fname == "value" || fname == "err" {
                    let v = self.backend.builder.build_extract_value(st, 0, "value").unwrap();
                    Ok(v)
                } else {
                    self.fail(&format!("unknown aggregate field '{}'", fname))
                }
            }
        }
    }

    // ------------------------------------------------------------------ //
    //  If / Match
    // ------------------------------------------------------------------ //

    fn gen_if(&mut self, ifx: &ast::IfExpr) -> GenResult<'ctx> {
        // Lower an if/elif/else chain into nested ifs so codegen only ever
        // needs the two-branch form. `if c1 {A} elif c2 {B} else {C}` becomes
        // `if c1 {A} else { if c2 {B} else {C} }`.
        match ifx.elif.first() {
            Some((c, b)) => {
                let inner_else = if ifx.else_block.is_some() { ifx.else_block.clone() } else { None };
                let inner = ast::IfExpr {
                    cond: (*c).clone(),
                    then_block: (*b).clone(),
                    elif: ifx.elif.iter().skip(1).map(|(c2, b2)| ((*c2).clone(), (*b2).clone())).collect(),
                    else_block: inner_else,
                };
                let outer = ast::IfExpr {
                    cond: ifx.cond.clone(),
                    then_block: ifx.then_block.clone(),
                    elif: vec![],
                    else_block: Some(Block { stmts: vec![Stmt::Expr(Expr::If(inner))], span: ifx.then_block.span.clone() }),
                };
                self.gen_if(&outer)
            }
            None => {
                let cond = self.gen_expr(&ifx.cond)?;
                let cond = cond.into_int_value();
                let fnv = self.cur_fn();

                let then_bb = self.backend.context.append_basic_block(fnv, "if.then");
                let else_bb = self.backend.context.append_basic_block(fnv, "if.else");
                let merge_bb = self.backend.context.append_basic_block(fnv, "if.merge");
                self.backend.builder.build_conditional_branch(cond, then_bb, else_bb).unwrap();

                // Flow typing: `x is some|ok` narrows x to its payload in the
                // then branch; `x is none|err` narrows it in the else branch.
                let narrow = Self::extract_narrow(&ifx.cond);

                // then
                let pre_then = self.snapshot_owned();
                self.backend.builder.position_at_end(then_bb);
                let saved_then = match narrow.clone() {
                    Some((n, true)) => self.narrow_scope(n.as_str()),
                    _ => None,
                };
                let then_val = self.gen_block(&ifx.then_block);
                self.after_branch(&pre_then);
                self.restore_scope(saved_then);
                let then_bb = self.backend.builder.get_insert_block().unwrap();
                if !self.block_terminated() {
                    self.backend.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                // else
                let pre_else = self.snapshot_owned();
                self.backend.builder.position_at_end(else_bb);
                let saved_else = match narrow.clone() {
                    Some((n, false)) => self.narrow_scope(n.as_str()),
                    _ => None,
                };
                let else_val = match &ifx.else_block {
                    Some(b) => self.gen_block(b),
                    None => None,
                };
                self.after_branch(&pre_else);
                self.restore_scope(saved_else);
                let else_bb = self.backend.builder.get_insert_block().unwrap();
                if !self.block_terminated() {
                    self.backend.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                self.backend.builder.position_at_end(merge_bb);

                match (then_val, else_val) {
                    (Some(t), Some(el)) => {
                        // The phi merges both arm values: any owned Str buffer
                        // the arms referenced is now potentially held by the
                        // merge result too, so downgrade those bindings.
                        self.degrade_block(&ifx.then_block);
                        if let Some(b) = &ifx.else_block {
                            self.degrade_block(b);
                        }
                        let ty = self.basic_type_of(t);
                        let phi = self.backend.builder.build_phi(ty, "if.phi").unwrap();
                        phi.add_incoming(&[(&t, then_bb), (&el, else_bb)]);
                        Ok(phi.as_basic_value())
                    }
                    (Some(t), None) => Ok(t),
                    (None, Some(el)) => Ok(el),
                    (None, None) => Ok(self.backend.types.unit.const_zero().into()),
                }
            }
        }
    }

    /// `break` — branch to the innermost enclosing loop's break target.
    fn gen_break(&mut self) -> GenResult<'ctx> {
        match self.loop_stack.last() {
            Some((_, brk)) => {
                self.backend.builder.build_unconditional_branch(*brk).unwrap();
            }
            None => {
                let _ = self.fail::<()>("break outside of a loop");
            }
        }
        Ok(self.backend.types.unit.const_zero().into())
    }

    /// `continue` — branch to the innermost enclosing loop's continue target.
    fn gen_continue(&mut self) -> GenResult<'ctx> {
        match self.loop_stack.last() {
            Some((cont, _)) => {
                self.backend.builder.build_unconditional_branch(*cont).unwrap();
            }
            None => {
                let _ = self.fail::<()>("continue outside of a loop");
            }
        }
        Ok(self.backend.types.unit.const_zero().into())
    }

    /// `loop { ... }` — an infinite loop; the body branches back to the header
    /// (after `continue` targets it), `break` exits to the after block. Like
    /// the type checker, the loop's value is never used (Kind::Never), so we
    /// return a Unit zero.
    fn gen_loop(&mut self, block: &ast::Block) -> GenResult<'ctx> {
        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "loop.header");
        let body_bb = self.backend.context.append_basic_block(fnv, "loop.body");
        let after = self.backend.context.append_basic_block(fnv, "loop.after");

        self.backend.builder.build_unconditional_branch(header).unwrap();

        // header: fall through to the body. `continue` jumps here (then falls
        // through again); `break` jumps to `after`.
        self.backend.builder.position_at_end(header);
        self.backend.builder.build_unconditional_branch(body_bb).unwrap();

        self.backend.builder.position_at_end(body_bb);
        self.loop_stack.push((header, after));
        let _ = self.gen_block(block);
        self.loop_stack.pop();
        if !self.block_terminated() {
            self.backend.builder.build_unconditional_branch(header).unwrap();
        }

        self.backend.builder.position_at_end(after);
        Ok(self.backend.types.unit.const_zero().into())
    }

    /// `for i in n { ... }` — Phase 4 range: iterate `i` over 0..n (n Int,
    /// exclusive). Lowered as an induction-variable loop with a header compare
    /// and increment; `continue` jumps to the increment, `break` to after.
    fn gen_for(&mut self, name: &str, iter: &Expr, block: &ast::Block) -> GenResult<'ctx> {
        let n = self.gen_expr(iter)?;
        let n = n.into_int_value();
        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "for.header");
        let body_bb = self.backend.context.append_basic_block(fnv, "for.body");
        let incr = self.backend.context.append_basic_block(fnv, "for.incr");
        let after = self.backend.context.append_basic_block(fnv, "for.after");

        // entry: i = 0; if n <= 0, skip the body
        let i_ty = self.backend.types.int;
        let i_ty_bt: BasicTypeEnum<'ctx> = i_ty.into();
        let i_ptr = self.backend.builder.build_alloca(i_ty, name).unwrap();
        self.backend.builder.build_store(i_ptr, i_ty.const_int(0, false)).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        // header: i < n ? body : after
        self.backend.builder.position_at_end(header);
        let i_cur = self.build_load(i_ty_bt, i_ptr, name);
        let cond = self
            .backend
            .builder
            .build_int_compare(IntPredicate::SLT, i_cur.into_int_value(), n, "for.cond")
            .unwrap();
        self.backend.builder.build_conditional_branch(cond, body_bb, after).unwrap();

        // body
        self.backend.builder.position_at_end(body_bb);
        let saved = self.scope.insert(name.to_string(), (i_ptr, i_ty_bt));
        self.loop_stack.push((incr, after));
        let _ = self.gen_block(block);
        self.loop_stack.pop();
        match saved {
            Some((p, t)) => {
                self.scope.insert(name.to_string(), (p, t));
            }
            None => {
                self.scope.remove(name);
            }
        }
        if !self.block_terminated() {
            self.backend.builder.build_unconditional_branch(incr).unwrap();
        }

        // incr: i += 1; jump back to header
        self.backend.builder.position_at_end(incr);
        let i_next = self
            .backend
            .builder
            .build_int_add(i_cur.into_int_value(), i_ty.const_int(1, false), "for.next")
            .unwrap();
        self.backend.builder.build_store(i_ptr, i_next).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(after);
        Ok(self.backend.types.unit.const_zero().into())
    }

    fn gen_match(&mut self, subject: &Expr, arms: &[(Pattern, Box<Expr>)]) -> GenResult<'ctx> {
        let subj = self.gen_expr(subject)?;
        let fnv = self.cur_fn();
        let merge_bb = self.backend.context.append_basic_block(fnv, "match.merge");

        // Determine mode: enum (tag) vs Result/Option (ok-flag), from the arms.
        let is_result = arms.iter().any(|(p, _)| matches!(p, Pattern::Variant(n, _) if n == "ok" || n == "err" || n == "some" || n == "none"));
        let _ = is_result;

        let mut incoming: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();
        let mut first_type: Option<BasicTypeEnum<'ctx>> = None;

        let mut dispatch = self.backend.builder.get_insert_block().unwrap();
        let mut idx = 0usize;
        let n_arms = arms.len();
        for (pat, body) in arms {
            let is_variant = matches!(pat, Pattern::Variant(..));
            if is_variant {
                let arm_bb = self.backend.context.append_basic_block(fnv, &format!("arm{}", idx));
                let is_last = idx == n_arms - 1;
                // If this is the last arm, branch to it directly (no further dispatch).
                let target = if is_last { arm_bb } else {
                    self.backend.context.append_basic_block(fnv, &format!("arm{}next", idx))
                };
                self.backend.builder.position_at_end(dispatch);
                let test = self.variant_test(subj, pat)?;
                self.backend.builder.build_conditional_branch(test, arm_bb, target).unwrap();

                self.backend.builder.position_at_end(arm_bb);
                let pre_arm = self.snapshot_owned();
                self.bind_variant(subj, pat)?;
                self.degrade_str_expr(body);
                let v = self.gen_expr(body)?;
                self.after_branch(&pre_arm);
                if first_type.is_none() {
                    first_type = Some(self.basic_type_of(v));
                }
                // An arm that `break`s/`continue`s/`return`s leaves the block
                // terminated — it contributes no incoming value to the phi.
                if !self.block_terminated() {
                    incoming.push((v, arm_bb));
                    self.backend.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                if is_last {
                    // no further arms; the dispatch chain ends here
                    dispatch = merge_bb;
                    self.backend.builder.position_at_end(dispatch);
                } else {
                    dispatch = target;
                    self.backend.builder.position_at_end(dispatch);
                }
            } else {
                // catch-all arm (Name / Wildcard / none)
                self.backend.builder.position_at_end(dispatch);
                let pre_arm = self.snapshot_owned();
                self.bind_catchall(subj, pat);
                self.degrade_str_expr(body);
                let v = self.gen_expr(body)?;
                self.after_branch(&pre_arm);
                if first_type.is_none() {
                    first_type = Some(self.basic_type_of(v));
                }
                if !self.block_terminated() {
                    incoming.push((v, dispatch));
                    self.backend.builder.build_unconditional_branch(merge_bb).unwrap();
                }
                dispatch = merge_bb;
                self.backend.builder.position_at_end(dispatch);
            }
            idx += 1;
        }

        self.backend.builder.position_at_end(merge_bb);
        match first_type {
            Some(ty) => {
                let phi = self.backend.builder.build_phi(ty, "match.phi").unwrap();
                let incoming_refs: Vec<(&dyn BasicValue<'ctx>, BasicBlock<'ctx>)> =
                    incoming.iter().map(|(v, bb)| (v as &dyn BasicValue<'ctx>, *bb)).collect();
                phi.add_incoming(&incoming_refs);
                Ok(phi.as_basic_value())
            }
            None => Ok(self.backend.types.unit.const_zero().into()),
        }
    }

    fn variant_test(&mut self, subj: BasicValueEnum<'ctx>, pat: &Pattern) -> Result<inkwell::values::IntValue<'ctx>, String> {
        match pat {
            Pattern::Variant(name, _) => {
                // is it a real enum variant?
                if let Some((_en, tag, _)) = self.backend.variants.get(name) {
                    let st = subj.into_struct_value();
                    let tagv = self.backend.builder.build_extract_value(st, 1, "tag").unwrap().into_int_value();
                    let expect = self.backend.context.i32_type().const_int(*tag as u64, false);
                    Ok(self
                        .backend
                        .builder
                        .build_int_compare(IntPredicate::EQ, tagv, expect, "armtest")
                        .unwrap())
                } else {
                    // Result/Option arm: ok/some -> true, err/none -> false
                    let st = subj.into_struct_value();
                    let flag = self.backend.builder.build_extract_value(st, 1, "flag").unwrap().into_int_value();
                    match name.as_str() {
                        "ok" | "some" => Ok(flag),
                        "err" | "none" => Ok(self.backend.builder.build_not(flag, "notflag").unwrap()),
                        _ => Err("unknown variant".to_string()),
                    }
                }
            }
            _ => Err("not a variant".to_string()),
        }
    }

    fn bind_variant(&mut self, subj: BasicValueEnum<'ctx>, pat: &Pattern) -> Result<(), String> {
        match pat {
            Pattern::Variant(name, names) => {
                if let Some((_en, _tag, fields)) = self.backend.variants.get(name).cloned() {
                    // enum: load fields from the box
                    let st = subj.into_struct_value();
                    let boxp = self.backend.builder.build_extract_value(st, 0, "box").unwrap().into_pointer_value();
                    let field_tys: Vec<BasicTypeEnum<'ctx>> = fields.iter().map(|k| self.backend.kind_to_llvm(k)).collect();
                    let vt = self.backend.context.struct_type(&field_tys, false);
                    for (i, nm) in names.iter().enumerate() {
                        if i < fields.len() {
                            let fptr = self.backend.builder.build_struct_gep(vt, boxp, i as u32, "vf").unwrap();
                            let v = self.build_load(field_tys[i], fptr, nm);
                            self.bind_value(nm, v);
                        }
                    }
                    Ok(())
                } else {
                    // Result/Option: payload is index 0; bind first name
                    let st = subj.into_struct_value();
                    let payload = self.backend.builder.build_extract_value(st, 0, "payload").unwrap();
                    if let Some(nm) = names.first() {
                        self.bind_value(nm, payload);
                    }
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    fn bind_catchall(&mut self, subj: BasicValueEnum<'ctx>, pat: &Pattern) {
        match pat {
            Pattern::Name(n) => {
                self.bind_value(n, subj);
            }
            _ => {}
        }
    }
}

// --------------------------------------------------------------------- //
//  Entry points
// --------------------------------------------------------------------- //

/// Generate a non-main function. Builds the entry block, allocas for params,
/// the body, and the return.
pub fn gen_function(backend: &mut LlvmBackend<'static>, f: &ast::FuncDecl) {
    let fv = match backend.functions.get(&f.name) {
        Some(fv) => *fv,
        None => return,
    };
    let ret_kind = backend.sigs.get(&f.name).and_then(|s| s.xz_ret.clone());
    let ret_llvm = backend.sigs.get(&f.name).and_then(|s| s.ret);
    let leak_strs = ret_kind.as_ref().map(|k| kind_contains_str(k, backend)).unwrap_or(false);

    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind,
        ret_llvm,
        str_count: 0,
        none_hint: None,
        owns: HashMap::new(),
        leak_strs,
        loop_stack: Vec::new(),
        is_main: false,
    };
    cg.gen_body_common(&f.name, fv, &f.params, &f.body);
}

/// Generate `main`. `main` is the C entry point: it is declared returning i32
/// (see `compile`) so the crt/linker is satisfied, and the body's value is
/// discarded; `gen_body_common` emits `ret i32 0`.
pub fn gen_main(backend: &mut LlvmBackend<'static>, f: &ast::FuncDecl) {
    let fv = match backend.functions.get(&f.name) {
        Some(fv) => *fv,
        None => return,
    };
    let ret_kind = f.ret.as_ref().map(|t| kind_from_ast(t, backend));
    let leak_strs = ret_kind.as_ref().map(|k| kind_contains_str(k, backend)).unwrap_or(false);
    let main_ret: BasicTypeEnum<'static> = backend.context.i32_type().into();
    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind,
        ret_llvm: Some(main_ret),
        str_count: 0,
        none_hint: None,
        owns: HashMap::new(),
        leak_strs,
        loop_stack: Vec::new(),
        is_main: true,
    };
    cg.gen_body_common(&f.name, fv, &f.params, &f.body);
}

impl<'ctx> Codegen<'_, 'ctx> {
    fn gen_body_common(
        &mut self,
        name: &str,
        fv: FunctionValue<'ctx>,
        params: &[ast::Param],
        body: &ast::Block,
    ) {
        let entry = self.backend.context.append_basic_block(fv, "entry");
        self.backend.builder.position_at_end(entry);
        for (i, p) in params.iter().enumerate() {
            let pv = fv.get_nth_param(i as u32);
            if let Some(pv) = pv {
                let ty = self.basic_type_of(pv);
                let alloca = self.backend.builder.build_alloca(ty, &p.name).unwrap();
                self.backend.builder.build_store(alloca, pv).unwrap();
                self.scope.insert(p.name.clone(), (alloca, ty));
            }
        }
        let val = self.gen_block(body);
        // Release any Str buffers this function's bindings uniquely own
        // (unless the return type can escape Str storage, in which case the
        // return value may alias a binding and those stay live for the caller).
        self.free_owned_bindings();
        // `main` is the C entry point returning i32; discard the body value.
        if self.is_main {
            if !self.block_terminated() {
                let zero = self.backend.context.i32_type().const_zero();
                let _ = self.backend.builder.build_return(Some(&zero));
            }
            let _ = name;
            return;
        }
        let ret_llvm = self.ret_llvm;
        match (ret_llvm, val) {
            (Some(_rt), Some(v)) => {
                let _ = self.backend.builder.build_return(Some(&v));
            }
            (Some(_rt), None) => {
                // body ended without a value expression; return zero
                let _ = self.backend.builder.build_return(Some(&self.backend.types.unit.const_zero()));
            }
            (None, _) => {
                let _ = self.backend.builder.build_return(None);
            }
        }
        let _ = name;
    }
}
