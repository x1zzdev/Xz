use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::intrinsics::Intrinsic;
use inkwell::module::Linkage;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::ast::{self, Block, Expr, Pattern, Stmt, StmtKind, Type};
use crate::backend::llvm_backend::{kind_from_ast, kind_from_ast_subst, kind_from_llvm, LlvmBackend};
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
    /// while evaluating a `let xs: List[T] = ...`, the declared element kind,
    /// so an empty `[]` literal can materialize a typed buffer.
    list_hint: Option<Kind>,
    /// while evaluating a `let s: Set[T] = {}`, the declared kind, so the
    /// empty brace literal materializes an empty set, not an empty map.
    set_hint: Option<Kind>,
    /// while evaluating a `let m: Map[K, V] = ...`, the declared kind, so a map
    /// literal whose keys are records/enums (whose LLVM type cannot name them)
    /// can hash and compare them structurally.
    map_hint: Option<Kind>,
    /// list-typed binding name -> element kind, so `for x in xs` and `xs[i]`
    /// can load the right element type (codegen carries no types otherwise).
    list_elems: HashMap<String, Kind>,
    /// set-typed binding name -> element kind, so `s.contains`/`insert` and
    /// `for e in s` can load the right element type.
    set_elems: HashMap<String, Kind>,
    /// map-typed binding name -> (key kind, value kind), so `m.get`/`insert`/
    /// `keys`/`values` can load the right key/value types.
    map_kvs: HashMap<String, (Kind, Kind)>,
    /// name -> owns a heap-allocated buffer, meaning this binding is the only
    /// reference to it (safe to release on overwrite / scope exit). A value is
    /// recorded here only when it was produced by concat/to_str in this function
    /// or returned by an extern `transfer` (with a `release` symbol), and has not
    /// been copied since. Absence means "static or shared — never free" (see
    /// docs/13-codegen.md § Str memory).
    owns: HashMap<String, FreeHow>,
    /// true when the enclosing function can escape an owned buffer through its
    /// return value (return type is or contains a pointer). Under that rule the
    /// function's owned bindings are never released at exit — conservative leak
    /// that keeps every returned buffer alive for the caller.
    leak_owned: bool,
    /// the innermost enclosing loop's continue-target and break-target blocks,
    /// for `break`/`continue` statements (nested loops push/pop).
    loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
    /// true for `main`, which is the C entry point and returns i32 (0), not
    /// its declared Xz return type.
    is_main: bool,
    /// concrete type-parameter substitution for a generic specialization
    /// (empty for ordinary functions); `kind_of` applies it to declared types.
    subst: HashMap<String, Kind>,
    /// For each `mut` parameter: the callee's own binding alloca (the copy-in),
    /// its value type, and the caller's out-pointer. The final value is copied
    /// back through the pointer before the function returns
    /// (copy-in/copy-out — docs/04-memory-model.md).
    mut_outs: Vec<(PointerValue<'ctx>, BasicTypeEnum<'ctx>, PointerValue<'ctx>)>,
}

type GenResult<'ctx> = Result<BasicValueEnum<'ctx>, String>;

/// How an owned buffer is released when its binding dies.
#[derive(Clone)]
enum FreeHow {
    /// A fresh `Str` buffer from `concat`/`to_str`: released through the
    /// runtime registry (`xz_str_free`), which no-ops on literals and
    /// already-freed buffers.
    Registry,
    /// A `transfer` return owned by the Xz caller: released through the named
    /// extern deallocator (`release <symbol>`, docs/10-ffi-interop.md).
    Symbol(String),
}

/// `true` when `k` is or transitively contains a `Str`/`Bytes`/`Ptr`. A function
/// whose return type is, or contains, a pointer can escape an owned buffer
/// through its return value, so its owned bindings are never released at exit
/// (the caller owns them — conservative leak that keeps the contract sound).
fn kind_carries_pointer(k: &Kind, backend: &LlvmBackend<'_>) -> bool {
    match k {
        Kind::Str | Kind::Bytes | Kind::Ptr => true,
        Kind::Option(t) | Kind::Result(t, _) => kind_carries_pointer(t, backend),
        Kind::List(t) => kind_carries_pointer(t, backend),
        Kind::Map(k, v) => kind_carries_pointer(k, backend) || kind_carries_pointer(v, backend),
        Kind::Set(t) => kind_carries_pointer(t, backend),
        Kind::Record(name) => backend
            .record_fields
            .get(name)
            .map(|fs| fs.iter().any(|f| kind_carries_pointer(f, backend)))
            .unwrap_or(false),
        Kind::Enum(name) => backend
            .variants
            .values()
            .any(|(en, _, fields)| en == name && fields.iter().any(|f| kind_carries_pointer(f, backend))),
        Kind::ErrUnion(members) => members.iter().any(|m| kind_carries_pointer(m, backend)),
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
    /// Re-derive a declared type as a `Kind`, applying any generic
    /// specialization substitution in scope.
    fn kind_of(&self, ty: &Type) -> Kind {
        if self.subst.is_empty() {
            kind_from_ast(ty, self.backend)
        } else {
            kind_from_ast_subst(ty, self.backend, &self.subst)
        }
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
    fn is_bytes(&self, v: BasicValueEnum<'ctx>) -> bool {
        match v.get_type() {
            inkwell::types::BasicTypeEnum::StructType(st) => st == self.backend.types.xz_bytes,
            _ => false,
        }
    }
    fn is_map(&self, v: BasicValueEnum<'ctx>) -> bool {
        match v.get_type() {
            inkwell::types::BasicTypeEnum::StructType(st) => st == self.backend.types.xz_map,
            _ => false,
        }
    }
    fn is_ptr(&self, v: BasicValueEnum<'ctx>) -> bool {
        matches!(v.get_type(), inkwell::types::BasicTypeEnum::PointerType(_))
    }

    /// The host ABI size of an LLVM value type, used to copy a channel message
    /// through a `(ptr, size)` byte range. `target_data` is set by `compile_impl`
    /// from the host target; 0 only if the host target was unavailable.
    fn abi_size(&self, ty: BasicTypeEnum<'ctx>) -> u64 {
        match &self.backend.target_data {
            Some(td) => td.get_abi_size(&ty),
            None => 0,
        }
    }

    fn build_load(&mut self, ty: BasicTypeEnum<'ctx>, ptr: PointerValue<'ctx>, name: &str) -> BasicValueEnum<'ctx> {
        self.backend.builder.build_load(ty, ptr, name).unwrap()
    }

    /// Match an integer value to the LLVM type of its storage slot. `Bool` is
    /// `i1` as a value but one byte (`i8`) as a record field (docs/13-codegen.md),
    /// so a record constructor zero-extends and a field read truncates. Other
    /// kinds share the value mapping and pass through unchanged.
    fn coerce_to(&mut self, ty: BasicTypeEnum<'ctx>, v: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match (ty, v) {
            (BasicTypeEnum::IntType(t), BasicValueEnum::IntValue(iv)) => {
                let want = t.get_bit_width();
                let have = iv.get_type().get_bit_width();
                if want > have {
                    self.backend.builder.build_int_z_extend(iv, t, "mem.zext").unwrap().into()
                } else if want < have {
                    self.backend.builder.build_int_truncate(iv, t, "mem.trunc").unwrap().into()
                } else {
                    v
                }
            }
            _ => v,
        }
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

    /// Allocate `count` elements of `elem` (at least one byte) via the
    /// module's libc `malloc`.
    fn alloc_buffer(&mut self, elem: BasicTypeEnum<'ctx>, count: u64, tag: &str) -> Result<PointerValue<'ctx>, String> {
        let elem_size = match &self.backend.target_data {
            Some(td) => td.get_abi_size(&elem),
            None => return self.fail("no target data for list allocation"),
        };
        let bytes = elem_size.saturating_mul(count).max(1);
        let f = self.backend.module.get_function("malloc").ok_or("malloc missing")?;
        let sz = self.backend.types.int.const_int(bytes, false);
        let call = self.backend.builder.build_direct_call(f, &[sz.into()], tag).unwrap();
        Ok(call.try_as_basic_value().basic().unwrap().into_pointer_value())
    }

    /// The element kind of a list/set-typed expression, determinable without
    /// a full type pass: a binding's recorded element kind, a literal's first
    /// element, a `List[T]`/`Set[T]`-returning call, or such a record field.
    /// Used by `for x in xs`, `xs[i]`, and `s.contains`.
    fn list_elem_kind(&self, e: &Expr) -> Option<Kind> {
        match e {
            Expr::Name(n) => self.list_elems.get(n).cloned().or_else(|| self.set_elems.get(n).cloned()),
            Expr::ListLit(elems) => elems.first().and_then(|x| self.scalar_kind(x)),
            Expr::SetLit(elems) => elems.first().and_then(|x| self.scalar_kind(x)),
            Expr::Call(callee, args) => match self.call_ret_kind(callee, args) {
                Some(Kind::List(t)) | Some(Kind::Set(t)) => Some(*t),
                _ => None,
            },
            Expr::Field(base, fname) => match self.record_field_kind(base, fname) {
                Some(Kind::List(t)) | Some(Kind::Set(t)) => Some(*t),
                _ => None,
            },
            _ => None,
        }
    }

    /// A kind for scalar literal expressions, used only to seed list element
    /// inference.
    fn scalar_kind(&self, e: &Expr) -> Option<Kind> {
        match e {
            Expr::Int(_) => Some(Kind::Int),
            Expr::Float(_) => Some(Kind::Float),
            Expr::Bool(_) => Some(Kind::Bool),
            Expr::Char(_) => Some(Kind::Char),
            Expr::Str(_) | Expr::RawStr(_) => Some(Kind::Str),
            _ => None,
        }
    }

    /// The (key, value) kinds of a map-typed expression, determinable without
    /// a full type pass: a binding's recorded kinds, a literal's first entry,
    /// a `Map[K, V]`-returning call, or such a record field. Used by
    /// `m.get`/`insert`/`keys`/`values`.
    fn map_kv_kind(&self, e: &Expr) -> Option<(Kind, Kind)> {
        match e {
            Expr::Name(n) => self.map_kvs.get(n).cloned(),
            Expr::MapLit(entries) => entries
                .first()
                .and_then(|(k, v)| Some((self.scalar_kind(k)?, self.scalar_kind(v)?))),
            Expr::Call(callee, args) => match self.call_ret_kind(callee, args) {
                Some(Kind::Map(k, v)) => Some((*k, *v)),
                _ => None,
            },
            Expr::Field(base, fname) => match self.record_field_kind(base, fname) {
                Some(Kind::Map(k, v)) => Some((*k, *v)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The element kind of a set-typed expression, determinable without a full
    /// type pass: a set binding's recorded element kind, a literal's first
    /// element, a `Set[T]`-returning call, or a `Set[T]` record field.
    fn set_elem_kind(&self, e: &Expr) -> Option<Kind> {
        match e {
            Expr::Name(n) => self.set_elems.get(n).cloned(),
            Expr::SetLit(elems) => elems.first().and_then(|x| self.scalar_kind(x)),
            Expr::Call(callee, args) => match self.call_ret_kind(callee, args) {
                Some(Kind::Set(t)) => Some(*t),
                _ => None,
            },
            Expr::Field(base, fname) => match self.record_field_kind(base, fname) {
                Some(Kind::Set(t)) => Some(*t),
                _ => None,
            },
            _ => None,
        }
    }

    /// The `Kind` of a call expression, without lowering. A named function
    /// reads its declared return kind; a collection method rebuilds one from
    /// the receiver's tracked kinds. Generic calls are left unresolved here
    /// because their declared return kind may mention a type parameter (the
    /// monomorphization path handles those at the call site).
    fn call_ret_kind(&self, callee: &Expr, args: &[Expr]) -> Option<Kind> {
        match callee {
            Expr::Name(f) => {
                if self.backend.generic_params.contains_key(f) {
                    return None;
                }
                self.backend.sigs.get(f).and_then(|s| s.xz_ret.clone())
            }
            Expr::Field(recv, m) => match m.as_str() {
                "append" => self.list_elem_kind(recv).map(|t| Kind::List(Box::new(t))),
                "keys" => self.map_kv_kind(recv).map(|(k, _)| Kind::List(Box::new(k))),
                "values" => self.map_kv_kind(recv).map(|(_, v)| Kind::List(Box::new(v))),
                "insert" if args.len() == 1 => self.set_elem_kind(recv).map(|t| Kind::Set(Box::new(t))),
                "insert" => self.map_kv_kind(recv).map(|(k, v)| Kind::Map(Box::new(k), Box::new(v))),
                "to_str" => Some(Kind::Str),
                _ => None,
            },
            _ => None,
        }
    }

    /// The declared `Kind` of a record field, when `base` resolves to a named
    /// record (a binding or a nested record field). Lets a field-typed
    /// collection receiver, e.g. `config.entries.get(k)`, know its element
    /// kinds without a full type pass.
    fn record_field_kind(&self, base: &Expr, fname: &str) -> Option<Kind> {
        let rec = self.record_kind_of(base)?;
        let idx = *self.backend.record_field_names.get(&rec)?.get(fname)? as usize;
        self.backend.record_fields.get(&rec)?.get(idx).cloned()
    }

    /// The record type name behind an expression, when it is a binding whose
    /// LLVM type is a named record struct or a nested record field.
    fn record_kind_of(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Name(n) => match self.scope.get(n) {
                Some((_, BasicTypeEnum::StructType(st))) => st.get_name().map(|s| s.to_str().unwrap().to_string()),
                _ => None,
            },
            Expr::Field(base, fname) => match self.record_field_kind(base, fname) {
                Some(Kind::Record(name)) => Some(name),
                _ => None,
            },
            _ => None,
        }
    }

    /// Split a `{ keys, vals, len }` Map value into its three fields.
    fn map_parts(
        &mut self,
        v: BasicValueEnum<'ctx>,
    ) -> (PointerValue<'ctx>, PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let st = v.into_struct_value();
        let keys = self.backend.builder.build_extract_value(st, 0, "mk").unwrap().into_pointer_value();
        let vals = self.backend.builder.build_extract_value(st, 1, "mv").unwrap().into_pointer_value();
        let len = self.backend.builder.build_extract_value(st, 2, "ml").unwrap().into_int_value();
        (keys, vals, len)
    }

    /// Split a Map value into its hash-index table and capacity.
    fn map_index_parts(
        &mut self,
        v: BasicValueEnum<'ctx>,
    ) -> (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let st = v.into_struct_value();
        let idx = self.backend.builder.build_extract_value(st, 3, "mi").unwrap().into_pointer_value();
        let cap = self.backend.builder.build_extract_value(st, 4, "mc").unwrap().into_int_value();
        (idx, cap)
    }

    /// Assemble a `{ keys, vals, len, idx, cap }` Map value.
    fn map_value(
        &mut self,
        keys: PointerValue<'ctx>,
        vals: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        idx: PointerValue<'ctx>,
        cap: inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let mut agg = self.backend.types.xz_map.const_zero();
        agg = self.backend.builder.build_insert_value(agg, keys, 0, "m.kp").unwrap().into_struct_value();
        agg = self.backend.builder.build_insert_value(agg, vals, 1, "m.vp").unwrap().into_struct_value();
        agg = self.backend.builder.build_insert_value(agg, len, 2, "m.len").unwrap().into_struct_value();
        agg = self.backend.builder.build_insert_value(agg, idx, 3, "m.ip").unwrap().into_struct_value();
        agg = self.backend.builder.build_insert_value(agg, cap, 4, "m.cp").unwrap().into_struct_value();
        agg.into()
    }

    /// Lower `{k1: v1, ...}`: allocate the key/value columns and the hash index
    /// once, then place each entry in order. A repeated key keeps its first
    /// position and the later value wins (docs/12), all in a single pass
    /// (docs/13-codegen.md § Map representation).
    fn gen_map_lit(&mut self, entries: &[(Expr, Expr)]) -> GenResult<'ctx> {
        // `{}` is Map or Set by the binding's declared type (docs/11).
        if entries.is_empty() {
            return self.gen_empty_brace();
        }
        let mut kvals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(entries.len());
        let mut vvals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(entries.len());
        for (k, v) in entries {
            kvals.push(self.gen_expr(k)?);
            vvals.push(self.gen_expr(v)?);
        }
        let key_bt = self.basic_type_of(kvals[0]);
        let val_bt = self.basic_type_of(vvals[0]);
        // A record/enum key's LLVM struct type cannot name it, so the declared
        // `Map[K, V]` hint supplies the key kind; primitives fall back to the
        // value type.
        let key_kind = match &self.map_hint {
            Some(Kind::Map(k, _)) => (**k).clone(),
            _ => kind_from_llvm(self.backend, key_bt),
        };
        let n = entries.len() as u64;
        let keys = self.alloc_buffer(key_bt, n, "map.keys")?;
        let vals = self.alloc_buffer(val_bt, n, "map.vals")?;
        // The table holds at most `n` entries, so a power of two at least twice
        // that keeps the load factor at or below one half.
        let mut cap_u = 1u64;
        while cap_u < n.saturating_mul(2).max(8) {
            cap_u <<= 1;
        }
        let int = self.backend.types.int;
        let cap = int.const_int(cap_u, false);
        let idx = self.alloc_map_column(8, cap, "map.idx")?;
        self.map_fill_index(idx, cap);
        let cur_ptr = self.backend.builder.build_alloca(int, "map.cur").unwrap();
        self.backend.builder.build_store(cur_ptr, int.const_zero()).unwrap();
        for (key, val) in kvals.iter().zip(vvals.iter()) {
            let cur = self.build_load(int.into(), cur_ptr, "map.curv").into_int_value();
            let found = self.map_probe_find_raw(keys, idx, cap, cur, *key, &key_kind)?;
            let is_found = self
                .backend
                .builder
                .build_int_compare(IntPredicate::SGE, found, int.const_zero(), "map.found")
                .unwrap();
            let fnv = self.cur_fn();
            let put = self.backend.context.append_basic_block(fnv, "map.put");
            let append = self.backend.context.append_basic_block(fnv, "map.append");
            let cont = self.backend.context.append_basic_block(fnv, "map.cont");
            self.backend.builder.build_conditional_branch(is_found, put, append).unwrap();

            self.backend.builder.position_at_end(put);
            let vslot = unsafe { self.backend.builder.build_in_bounds_gep(val_bt, vals, &[found], "map.vs").unwrap() };
            self.backend.builder.build_store(vslot, *val).unwrap();
            self.backend.builder.build_unconditional_branch(cont).unwrap();

            self.backend.builder.position_at_end(append);
            let kslot = unsafe { self.backend.builder.build_in_bounds_gep(key_bt, keys, &[cur], "map.ks").unwrap() };
            self.backend.builder.build_store(kslot, *key).unwrap();
            let vslot2 = unsafe { self.backend.builder.build_in_bounds_gep(val_bt, vals, &[cur], "map.vs2").unwrap() };
            self.backend.builder.build_store(vslot2, *val).unwrap();
            let h = self.map_hash(*key, &key_kind)?;
            let slot = self.map_probe_empty(idx, cap, h);
            let eslot = unsafe { self.backend.builder.build_in_bounds_gep(int, idx, &[slot], "map.es").unwrap() };
            self.backend.builder.build_store(eslot, cur).unwrap();
            let cur1 = self.backend.builder.build_int_add(cur, int.const_int(1, false), "map.cur1").unwrap();
            self.backend.builder.build_store(cur_ptr, cur1).unwrap();
            self.backend.builder.build_unconditional_branch(cont).unwrap();

            self.backend.builder.position_at_end(cont);
        }
        let len = self.build_load(int.into(), cur_ptr, "map.len").into_int_value();
        Ok(self.map_value(keys, vals, len, idx, cap))
    }

    /// Hash a Map key to an i64. Int/usize hash their value, Bool/Char are
    /// zero-extended first, Str hashes its bytes (FNV-1a), and a record/enum
    /// folds the hashes of its fields (docs/13-codegen.md § Map
    /// representation). The hash is a pure function of the key, so lowering is
    /// deterministic.
    fn map_hash(
        &mut self,
        key_val: BasicValueEnum<'ctx>,
        key_kind: &Kind,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let int = self.backend.types.int;
        let raw = match key_kind {
            Kind::Int | Kind::Usize => key_val.into_int_value(),
            Kind::Bool | Kind::Char => {
                let iv = key_val.into_int_value();
                self.backend.builder.build_int_z_extend(iv, int, "h.zext").unwrap()
            }
            Kind::Str => self.map_hash_str(key_val)?,
            Kind::Record(name) => self.map_hash_record(key_val, name)?,
            Kind::Enum(name) => self.map_hash_enum(key_val, name)?,
            _ => return Err(format!("Map key type {:?} has no hash lowering", key_kind)),
        };
        Ok(self.map_mix(raw))
    }

    /// Fold the field hashes of a record key with the same FNV-style combine the
    /// `Str` hash uses. Field kinds are the declared record fields.
    fn map_hash_record(
        &mut self,
        val: BasicValueEnum<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let fields = self
            .backend
            .record_fields
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown record '{}' as a Map key", name))?;
        let sv = val.into_struct_value();
        let int = self.backend.types.int;
        let mut acc = int.const_int(0xcbf2_9ce4_8422_2325, false);
        for (i, fk) in fields.iter().enumerate() {
            let fv = self.backend.builder.build_extract_value(sv, i as u32, "hr.f").unwrap();
            let fh = self.map_hash(fv, fk)?;
            acc = self.backend.builder.build_xor(acc, fh, "hr.x").unwrap();
            acc = self
                .backend
                .builder
                .build_int_mul(acc, int.const_int(0x0000_0100_0000_01b3, false), "hr.m")
                .unwrap();
        }
        Ok(acc)
    }

    /// Hash an enum key: fold the tag with the hashes of the active variant's
    /// payload fields, dispatched at runtime on the tag.
    fn map_hash_enum(
        &mut self,
        val: BasicValueEnum<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let variants = self.enum_variant_list(name)?;
        let sv = val.into_struct_value();
        let payload = self.backend.builder.build_extract_value(sv, 0, "he.box").unwrap().into_pointer_value();
        let tag_raw = self.backend.builder.build_extract_value(sv, 1, "he.tag").unwrap().into_int_value();
        let int = self.backend.types.int;
        let tag = self.backend.builder.build_int_z_extend(tag_raw, int, "he.tag64").unwrap();
        let mut seed = int.const_int(0xcbf2_9ce4_8422_2325, false);
        seed = self.backend.builder.build_xor(seed, tag, "he.seed").unwrap();

        let fnv = self.cur_fn();
        let merge = self.backend.context.append_basic_block(fnv, "he.merge");
        let default_bb = self.backend.context.append_basic_block(fnv, "he.default");
        let mut case_bbs = Vec::with_capacity(variants.len());
        let mut next_bbs = Vec::with_capacity(variants.len());
        for _ in 0..variants.len() {
            case_bbs.push(self.backend.context.append_basic_block(fnv, "he.case"));
            next_bbs.push(self.backend.context.append_basic_block(fnv, "he.next"));
        }
        for (i, (tagv, _)) in variants.iter().enumerate() {
            let cmp = self
                .backend
                .builder
                .build_int_compare(IntPredicate::EQ, tag, int.const_int(*tagv as u64, false), "he.cmp")
                .unwrap();
            self.backend.builder.build_conditional_branch(cmp, case_bbs[i], next_bbs[i]).unwrap();
            self.backend.builder.position_at_end(next_bbs[i]);
        }
        self.backend.builder.build_unconditional_branch(default_bb).unwrap();

        self.backend.builder.position_at_end(default_bb);
        self.backend.builder.build_unconditional_branch(merge).unwrap();

        let mut incoming: Vec<(inkwell::values::IntValue<'ctx>, BasicBlock<'ctx>)> = vec![(seed, default_bb)];
        for (i, (_, fields)) in variants.iter().enumerate() {
            self.backend.builder.position_at_end(case_bbs[i]);
            let vt = self.enum_payload_ty(fields);
            let mut acc = seed;
            for (j, fk) in fields.iter().enumerate() {
                let fptr = self.backend.builder.build_struct_gep(vt, payload, j as u32, "he.fp").unwrap();
                let fty = self.backend.kind_to_llvm(fk);
                let fv = self.build_load(fty, fptr, "he.fv");
                let fh = self.map_hash(fv, fk)?;
                acc = self.backend.builder.build_xor(acc, fh, "he.fx").unwrap();
                acc = self
                    .backend
                    .builder
                    .build_int_mul(acc, int.const_int(0x0000_0100_0000_01b3, false), "he.fm")
                    .unwrap();
            }
            let end = self.backend.builder.get_insert_block().unwrap();
            self.backend.builder.build_unconditional_branch(merge).unwrap();
            incoming.push((acc, end));
        }

        self.backend.builder.position_at_end(merge);
        let phi = self.backend.builder.build_phi(int, "he.phi").unwrap();
        let inc: Vec<(&dyn BasicValue<'ctx>, BasicBlock<'ctx>)> =
            incoming.iter().map(|(v, bb)| (v as &dyn BasicValue<'ctx>, *bb)).collect();
        phi.add_incoming(&inc);
        Ok(phi.as_basic_value().into_int_value())
    }

    /// The field kinds of each variant of `name`, in tag order.
    fn enum_variant_list(&self, name: &str) -> Result<Vec<(u32, Vec<Kind>)>, String> {
        let names = self
            .backend
            .enum_variants
            .get(name)
            .ok_or_else(|| format!("unknown enum '{}' as a Map key", name))?;
        let mut out = Vec::with_capacity(names.len());
        for vn in names {
            let (_, tag, fields) = self
                .backend
                .variants
                .get(vn)
                .ok_or_else(|| format!("unknown enum variant '{}'", vn))?;
            out.push((*tag, fields.clone()));
        }
        Ok(out)
    }

    /// The heap box type of a variant's fields, matching `gen_enum_ctor`.
    fn enum_payload_ty(&self, fields: &[Kind]) -> StructType<'ctx> {
        let tys: Vec<BasicTypeEnum<'ctx>> = fields.iter().map(|k| self.backend.kind_to_llvm(k)).collect();
        self.backend.context.struct_type(&tys, false)
    }

    /// FNV-1a over a `Str`'s bytes.
    fn map_hash_str(
        &mut self,
        key_val: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let (ptr, len) = self.str_parts(key_val);
        let int = self.backend.types.int;
        let i8t = self.backend.types.char;
        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "h.header");
        let body = self.backend.context.append_basic_block(fnv, "h.body");
        let done = self.backend.context.append_basic_block(fnv, "h.done");
        let h_ptr = self.backend.builder.build_alloca(int, "h.acc").unwrap();
        let i_ptr = self.backend.builder.build_alloca(int, "h.i").unwrap();
        self.backend.builder.build_store(h_ptr, int.const_int(0xcbf2_9ce4_8422_2325, false)).unwrap();
        self.backend.builder.build_store(i_ptr, int.const_zero()).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(header);
        let i_cur = self.build_load(int.into(), i_ptr, "h.ic").into_int_value();
        let cond = self.backend.builder.build_int_compare(IntPredicate::SLT, i_cur, len, "h.cond").unwrap();
        self.backend.builder.build_conditional_branch(cond, body, done).unwrap();

        self.backend.builder.position_at_end(body);
        let bslot = unsafe { self.backend.builder.build_in_bounds_gep(i8t, ptr, &[i_cur], "h.bs").unwrap() };
        let b = self.backend.builder.build_load(i8t, bslot, "h.b").unwrap();
        let b64 = self.backend.builder.build_int_z_extend(b.into_int_value(), int, "h.b64").unwrap();
        let h_cur = self.build_load(int.into(), h_ptr, "h.hc").into_int_value();
        let xored = self.backend.builder.build_xor(h_cur, b64, "h.xor").unwrap();
        let mul = self.backend.builder.build_int_mul(xored, int.const_int(0x0000_0100_0000_01b3, false), "h.mul").unwrap();
        self.backend.builder.build_store(h_ptr, mul).unwrap();
        let i_next = self.backend.builder.build_int_add(i_cur, int.const_int(1, false), "h.in").unwrap();
        self.backend.builder.build_store(i_ptr, i_next).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(done);
        Ok(self.build_load(int.into(), h_ptr, "h.ret").into_int_value())
    }

    /// SplitMix64 finalizer: spreads a raw key over the table index space.
    fn map_mix(&mut self, x: inkwell::values::IntValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let int = self.backend.types.int;
        let s30 = self.backend.builder.build_right_shift(x, int.const_int(30, false), false, "h.s30").unwrap();
        let x1 = self.backend.builder.build_xor(x, s30, "h.x1").unwrap();
        let x2 = self.backend.builder.build_int_mul(x1, int.const_int(0xbf58_476d_1ce4_e5b9, false), "h.x2").unwrap();
        let s27 = self.backend.builder.build_right_shift(x2, int.const_int(27, false), false, "h.s27").unwrap();
        let x3 = self.backend.builder.build_xor(x2, s27, "h.x3").unwrap();
        let x4 = self.backend.builder.build_int_mul(x3, int.const_int(0x94d0_49bb_1331_11eb, false), "h.x4").unwrap();
        let s31 = self.backend.builder.build_right_shift(x4, int.const_int(31, false), false, "h.s31").unwrap();
        self.backend.builder.build_xor(x4, s31, "h.x5").unwrap()
    }

    /// The smallest power of two `>= n` (n >= 1), by doubling.
    fn map_next_pow2(&mut self, n: inkwell::values::IntValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let int = self.backend.types.int;
        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "pow.header");
        let body = self.backend.context.append_basic_block(fnv, "pow.body");
        let done = self.backend.context.append_basic_block(fnv, "pow.done");
        let c_ptr = self.backend.builder.build_alloca(int, "pow.c").unwrap();
        self.backend.builder.build_store(c_ptr, int.const_int(1, false)).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(header);
        let c = self.build_load(int.into(), c_ptr, "pow.cc").into_int_value();
        let cond = self.backend.builder.build_int_compare(IntPredicate::SLT, c, n, "pow.cond").unwrap();
        self.backend.builder.build_conditional_branch(cond, body, done).unwrap();

        self.backend.builder.position_at_end(body);
        let c2 = self.backend.builder.build_left_shift(c, int.const_int(1, false), "pow.shl").unwrap();
        self.backend.builder.build_store(c_ptr, c2).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(done);
        self.build_load(int.into(), c_ptr, "pow.ret").into_int_value()
    }

    /// Probe the hash index for `key`; returns its column position or -1. An
    /// empty map (`len == 0`) has no table, so it cannot contain the key.
    fn map_probe_find_raw(
        &mut self,
        keys: PointerValue<'ctx>,
        idx: PointerValue<'ctx>,
        cap: inkwell::values::IntValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        key_val: BasicValueEnum<'ctx>,
        key_kind: &Kind,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let key_bt = self.basic_type_of(key_val);
        let int = self.backend.types.int;
        let h = self.map_hash(key_val, key_kind)?;
        let mask = self.backend.builder.build_int_sub(cap, int.const_int(1, false), "p.mask").unwrap();
        let fnv = self.cur_fn();
        let go = self.backend.context.append_basic_block(fnv, "p.go");
        let header = self.backend.context.append_basic_block(fnv, "p.header");
        let body = self.backend.context.append_basic_block(fnv, "p.body");
        let next = self.backend.context.append_basic_block(fnv, "p.next");
        let found = self.backend.context.append_basic_block(fnv, "p.found");
        let done = self.backend.context.append_basic_block(fnv, "p.done");
        let slot_ptr = self.backend.builder.build_alloca(int, "p.slot").unwrap();
        let out_ptr = self.backend.builder.build_alloca(int, "p.out").unwrap();
        self.backend.builder.build_store(out_ptr, int.const_int(u64::MAX, false)).unwrap();
        let empty = self.backend.builder.build_int_compare(IntPredicate::EQ, len, int.const_zero(), "p.empty").unwrap();
        self.backend.builder.build_conditional_branch(empty, done, go).unwrap();

        self.backend.builder.position_at_end(go);
        let start = self.backend.builder.build_and(h, mask, "p.start").unwrap();
        self.backend.builder.build_store(slot_ptr, start).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(header);
        let slot = self.build_load(int.into(), slot_ptr, "p.sc").into_int_value();
        let eslot = unsafe { self.backend.builder.build_in_bounds_gep(int, idx, &[slot], "p.es").unwrap() };
        let entry = self.backend.builder.build_load(int, eslot, "p.entry").unwrap().into_int_value();
        let is_empty = self.backend.builder.build_int_compare(IntPredicate::EQ, entry, int.const_int(u64::MAX, false), "p.isempty").unwrap();
        self.backend.builder.build_conditional_branch(is_empty, done, body).unwrap();

        self.backend.builder.position_at_end(body);
        let kslot = unsafe { self.backend.builder.build_in_bounds_gep(key_bt, keys, &[entry], "p.ks").unwrap() };
        let kcur = self.backend.builder.build_load(key_bt, kslot, "p.k").unwrap();
        let eq = self.map_key_eq(kcur, key_val, key_kind)?;
        self.backend.builder.build_conditional_branch(eq, found, next).unwrap();

        self.backend.builder.position_at_end(next);
        let slot1 = self.backend.builder.build_int_add(slot, int.const_int(1, false), "p.s1").unwrap();
        let slotm = self.backend.builder.build_and(slot1, mask, "p.sm").unwrap();
        self.backend.builder.build_store(slot_ptr, slotm).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(found);
        self.backend.builder.build_store(out_ptr, entry).unwrap();
        self.backend.builder.build_unconditional_branch(done).unwrap();

        self.backend.builder.position_at_end(done);
        Ok(self.build_load(int.into(), out_ptr, "p.ret").into_int_value())
    }

    /// The first empty slot a hash probes to. Used when the key is known absent,
    /// so no equality check is needed.
    fn map_probe_empty(
        &mut self,
        idx: PointerValue<'ctx>,
        cap: inkwell::values::IntValue<'ctx>,
        h: inkwell::values::IntValue<'ctx>,
    ) -> inkwell::values::IntValue<'ctx> {
        let int = self.backend.types.int;
        let mask = self.backend.builder.build_int_sub(cap, int.const_int(1, false), "pe.mask").unwrap();
        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "pe.header");
        let body = self.backend.context.append_basic_block(fnv, "pe.body");
        let done = self.backend.context.append_basic_block(fnv, "pe.done");
        let slot_ptr = self.backend.builder.build_alloca(int, "pe.slot").unwrap();
        let start = self.backend.builder.build_and(h, mask, "pe.start").unwrap();
        self.backend.builder.build_store(slot_ptr, start).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(header);
        let slot = self.build_load(int.into(), slot_ptr, "pe.sc").into_int_value();
        let eslot = unsafe { self.backend.builder.build_in_bounds_gep(int, idx, &[slot], "pe.es").unwrap() };
        let entry = self.backend.builder.build_load(int, eslot, "pe.entry").unwrap().into_int_value();
        let is_empty = self.backend.builder.build_int_compare(IntPredicate::EQ, entry, int.const_int(u64::MAX, false), "pe.isempty").unwrap();
        self.backend.builder.build_conditional_branch(is_empty, done, body).unwrap();

        self.backend.builder.position_at_end(body);
        let slot1 = self.backend.builder.build_int_add(slot, int.const_int(1, false), "pe.s1").unwrap();
        let slotm = self.backend.builder.build_and(slot1, mask, "pe.sm").unwrap();
        self.backend.builder.build_store(slot_ptr, slotm).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(done);
        self.build_load(int.into(), slot_ptr, "pe.ret").into_int_value()
    }

    /// Fill a fresh index table of `cap` i64 slots with -1 (empty).
    fn map_fill_index(&mut self, idx: PointerValue<'ctx>, cap: inkwell::values::IntValue<'ctx>) {
        let int = self.backend.types.int;
        let bytes = self.backend.builder.build_int_mul(cap, int.const_int(8, false), "fi.bytes").unwrap();
        let _ = self.backend.builder.build_memset(idx, 1, self.backend.types.char.const_int(0xff, false), bytes).unwrap();
    }

    /// Build a hash index for a key column of `len` entries: size the table to
    /// twice the load, fill it empty, then place each column position at its
    /// probed slot. Returns the table and its capacity.
    fn map_index_build(
        &mut self,
        keys: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        key_kind: &Kind,
    ) -> Result<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>), String> {
        let int = self.backend.types.int;
        let key_bt: BasicTypeEnum<'ctx> = self.backend.kind_to_llvm(key_kind);
        let doubled = self.backend.builder.build_int_mul(len, int.const_int(2, false), "ib.dbl").unwrap();
        let lt8 = self.backend.builder.build_int_compare(IntPredicate::SLT, doubled, int.const_int(8, false), "ib.lt8").unwrap();
        let want = self.backend.builder.build_select(lt8, int.const_int(8, false), doubled, "ib.want").unwrap().into_int_value();
        let cap = self.map_next_pow2(want);
        let idx = self.alloc_map_column(8, cap, "m.idx")?;
        self.map_fill_index(idx, cap);

        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "ib.header");
        let body = self.backend.context.append_basic_block(fnv, "ib.body");
        let done = self.backend.context.append_basic_block(fnv, "ib.done");
        let i_ptr = self.backend.builder.build_alloca(int, "ib.i").unwrap();
        self.backend.builder.build_store(i_ptr, int.const_zero()).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(header);
        let i_cur = self.build_load(int.into(), i_ptr, "ib.ic").into_int_value();
        let cond = self.backend.builder.build_int_compare(IntPredicate::SLT, i_cur, len, "ib.cond").unwrap();
        self.backend.builder.build_conditional_branch(cond, body, done).unwrap();

        self.backend.builder.position_at_end(body);
        let kslot = unsafe { self.backend.builder.build_in_bounds_gep(key_bt, keys, &[i_cur], "ib.ks").unwrap() };
        let kcur = self.backend.builder.build_load(key_bt, kslot, "ib.k").unwrap();
        let h = self.map_hash(kcur, key_kind)?;
        let slot = self.map_probe_empty(idx, cap, h);
        let eslot = unsafe { self.backend.builder.build_in_bounds_gep(int, idx, &[slot], "ib.es").unwrap() };
        self.backend.builder.build_store(eslot, i_cur).unwrap();
        let i_next = self.backend.builder.build_int_add(i_cur, int.const_int(1, false), "ib.in").unwrap();
        self.backend.builder.build_store(i_ptr, i_next).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(done);
        Ok((idx, cap))
    }

    /// Return the column position of `key`, or -1. An index probe (docs/13).
    fn map_find_key(
        &mut self,
        map_val: BasicValueEnum<'ctx>,
        key_val: BasicValueEnum<'ctx>,
        key_kind: &Kind,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let (keys, _, len) = self.map_parts(map_val);
        let (idx, cap) = self.map_index_parts(map_val);
        self.map_probe_find_raw(keys, idx, cap, len, key_val, key_kind)
    }

    /// Equality of two keys, dispatched on the key kind (docs/12): by value for
    /// Int/usize/Bool/Char, by content for Str, and structurally (field by
    /// field) for records and enums.
    fn map_key_eq(
        &mut self,
        a: BasicValueEnum<'ctx>,
        b: BasicValueEnum<'ctx>,
        kind: &Kind,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        match kind {
            Kind::Int | Kind::Usize | Kind::Bool | Kind::Char => {
                Ok(self.backend.builder.build_int_compare(IntPredicate::EQ, a.into_int_value(), b.into_int_value(), "k.eq").unwrap())
            }
            Kind::Str => {
                let (ap, al) = self.str_parts(a);
                let (bp, bl) = self.str_parts(b);
                let f = self.backend.module.get_function("xz_str_eq").ok_or("xz_str_eq missing")?;
                let call = self
                    .backend
                    .builder
                    .build_direct_call(f, &[ap.into(), al.into(), bp.into(), bl.into()], "k.streq")
                    .unwrap();
                Ok(call.try_as_basic_value().basic().unwrap().into_int_value())
            }
            Kind::Record(name) => self.map_key_eq_record(a, b, name),
            Kind::Enum(name) => self.map_key_eq_enum(a, b, name),
            _ => Err(format!("Map key type {:?} has no equality lowering", kind)),
        }
    }

    /// Structural equality of two record keys: conjunction of field equalities.
    fn map_key_eq_record(
        &mut self,
        a: BasicValueEnum<'ctx>,
        b: BasicValueEnum<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let fields = self
            .backend
            .record_fields
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown record '{}' as a Map key", name))?;
        let a_sv = a.into_struct_value();
        let b_sv = b.into_struct_value();
        let bool_ty = self.backend.types.bool;
        let mut acc = bool_ty.const_int(1, false);
        for (i, fk) in fields.iter().enumerate() {
            let av = self.backend.builder.build_extract_value(a_sv, i as u32, "kr.a").unwrap();
            let bv = self.backend.builder.build_extract_value(b_sv, i as u32, "kr.b").unwrap();
            let eq = self.map_key_eq(av, bv, fk)?;
            acc = self.backend.builder.build_and(acc, eq, "kr.and").unwrap();
        }
        Ok(acc)
    }

    /// Structural equality of two enum keys: the tags must match and, for the
    /// active variant, so must every payload field.
    fn map_key_eq_enum(
        &mut self,
        a: BasicValueEnum<'ctx>,
        b: BasicValueEnum<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let variants = self.enum_variant_list(name)?;
        let a_sv = a.into_struct_value();
        let b_sv = b.into_struct_value();
        let a_box = self.backend.builder.build_extract_value(a_sv, 0, "ke.abox").unwrap().into_pointer_value();
        let b_box = self.backend.builder.build_extract_value(b_sv, 0, "ke.bbox").unwrap().into_pointer_value();
        let a_tag = self.backend.builder.build_extract_value(a_sv, 1, "ke.atag").unwrap().into_int_value();
        let b_tag = self.backend.builder.build_extract_value(b_sv, 1, "ke.btag").unwrap().into_int_value();
        let i32t = self.backend.context.i32_type();
        let bool_ty = self.backend.types.bool;

        let fnv = self.cur_fn();
        let merge = self.backend.context.append_basic_block(fnv, "ke.merge");
        let default_bb = self.backend.context.append_basic_block(fnv, "ke.default");
        let mut case_bbs = Vec::with_capacity(variants.len());
        let mut next_bbs = Vec::with_capacity(variants.len());
        for _ in 0..variants.len() {
            case_bbs.push(self.backend.context.append_basic_block(fnv, "ke.case"));
            next_bbs.push(self.backend.context.append_basic_block(fnv, "ke.next"));
        }
        for (i, (tagv, _)) in variants.iter().enumerate() {
            let tconst = i32t.const_int(*tagv as u64, false);
            let c1 = self.backend.builder.build_int_compare(IntPredicate::EQ, a_tag, tconst, "ke.c1").unwrap();
            let c2 = self.backend.builder.build_int_compare(IntPredicate::EQ, b_tag, tconst, "ke.c2").unwrap();
            let both = self.backend.builder.build_and(c1, c2, "ke.both").unwrap();
            self.backend.builder.build_conditional_branch(both, case_bbs[i], next_bbs[i]).unwrap();
            self.backend.builder.position_at_end(next_bbs[i]);
        }
        self.backend.builder.build_unconditional_branch(default_bb).unwrap();

        self.backend.builder.position_at_end(default_bb);
        self.backend.builder.build_unconditional_branch(merge).unwrap();

        let mut incoming: Vec<(inkwell::values::IntValue<'ctx>, BasicBlock<'ctx>)> =
            vec![(bool_ty.const_zero(), default_bb)];
        for (i, (_, fields)) in variants.iter().enumerate() {
            self.backend.builder.position_at_end(case_bbs[i]);
            let vt = self.enum_payload_ty(fields);
            let mut acc = bool_ty.const_int(1, false);
            for (j, fk) in fields.iter().enumerate() {
                let fty = self.backend.kind_to_llvm(fk);
                let afp = self.backend.builder.build_struct_gep(vt, a_box, j as u32, "ke.afp").unwrap();
                let bfp = self.backend.builder.build_struct_gep(vt, b_box, j as u32, "ke.bfp").unwrap();
                let av = self.build_load(fty, afp, "ke.av");
                let bv = self.build_load(fty, bfp, "ke.bv");
                let eq = self.map_key_eq(av, bv, fk)?;
                acc = self.backend.builder.build_and(acc, eq, "ke.and").unwrap();
            }
            let end = self.backend.builder.get_insert_block().unwrap();
            self.backend.builder.build_unconditional_branch(merge).unwrap();
            incoming.push((acc, end));
        }

        self.backend.builder.position_at_end(merge);
        let phi = self.backend.builder.build_phi(bool_ty, "ke.phi").unwrap();
        let inc: Vec<(&dyn BasicValue<'ctx>, BasicBlock<'ctx>)> =
            incoming.iter().map(|(v, bb)| (v as &dyn BasicValue<'ctx>, *bb)).collect();
        phi.add_incoming(&inc);
        Ok(phi.as_basic_value().into_int_value())
    }

    /// Return a new Map with `key` set to `val`: replace in place when `key`
    /// already exists, else append (docs/12). The hash index is rebuilt for the
    /// new columns (docs/13-codegen.md § Map representation).
    fn map_insert_value(
        &mut self,
        map_val: BasicValueEnum<'ctx>,
        key_val: BasicValueEnum<'ctx>,
        val_val: BasicValueEnum<'ctx>,
        key_kind: &Kind,
    ) -> GenResult<'ctx> {
        let (keys, vals, len) = self.map_parts(map_val);
        let idx = self.map_find_key(map_val, key_val, key_kind)?;
        let key_bt = self.basic_type_of(key_val);
        let val_bt = self.basic_type_of(val_val);
        let key_size = self.abi_size(key_bt);
        let val_size = self.abi_size(val_bt);
        let int = self.backend.types.int;
        let zero = int.const_zero();
        let one = int.const_int(1, false);
        let found = self.backend.builder.build_int_compare(IntPredicate::SGE, idx, zero, "m.found").unwrap();
        let grow = self.backend.builder.build_select(found, zero, one, "m.grow").unwrap().into_int_value();
        let new_len = self.backend.builder.build_int_add(len, grow, "m.newlen").unwrap();
        let new_keys = self.alloc_map_column(key_size, new_len, "m.keys")?;
        let new_vals = self.alloc_map_column(val_size, new_len, "m.vals")?;
        let old_key_bytes = self.backend.builder.build_int_mul(len, int.const_int(key_size, false), "m.kb").unwrap();
        let old_val_bytes = self.backend.builder.build_int_mul(len, int.const_int(val_size, false), "m.vb").unwrap();
        let _ = self.backend.builder.build_memcpy(new_keys, 1, keys, 1, old_key_bytes).unwrap();
        let _ = self.backend.builder.build_memcpy(new_vals, 1, vals, 1, old_val_bytes).unwrap();
        // Store at the existing slot when found, else at the end. Overwriting a
        // found key with an equal key is a no-op in content.
        let slot = self.backend.builder.build_select(found, idx, len, "m.slot").unwrap().into_int_value();
        let kslot = unsafe { self.backend.builder.build_in_bounds_gep(key_bt, new_keys, &[slot], "m.kslot").unwrap() };
        self.backend.builder.build_store(kslot, key_val).unwrap();
        let vslot = unsafe { self.backend.builder.build_in_bounds_gep(val_bt, new_vals, &[slot], "m.vslot").unwrap() };
        self.backend.builder.build_store(vslot, val_val).unwrap();
        let (new_idx, cap) = self.map_index_build(new_keys, new_len, key_kind)?;
        Ok(self.map_value(new_keys, new_vals, new_len, new_idx, cap))
    }

    /// malloc a map column of `count` elements (at least one byte).
    fn alloc_map_column(
        &mut self,
        elem_size: u64,
        count: inkwell::values::IntValue<'ctx>,
        tag: &str,
    ) -> Result<PointerValue<'ctx>, String> {
        let int = self.backend.types.int;
        let bytes = self.backend.builder.build_int_mul(count, int.const_int(elem_size, false), "m.bytes").unwrap();
        let nonempty = self.backend.builder.build_int_compare(IntPredicate::SGT, bytes, int.const_zero(), "m.nonempty").unwrap();
        let size = self.backend.builder.build_select(nonempty, bytes, int.const_int(1, false), "m.size").unwrap().into_int_value();
        let f = self.backend.module.get_function("malloc").ok_or("malloc missing")?;
        let call = self.backend.builder.build_direct_call(f, &[size.into()], tag).unwrap();
        Ok(call.try_as_basic_value().basic().unwrap().into_pointer_value())
    }

    /// `m.get(k) -> Option[V]`: `some(v)` when present, else `none`.
    fn gen_map_get(
        &mut self,
        map_val: BasicValueEnum<'ctx>,
        key_val: BasicValueEnum<'ctx>,
        key_kind: &Kind,
        val_kind: &Kind,
    ) -> GenResult<'ctx> {
        let (_, vals, _) = self.map_parts(map_val);
        let idx = self.map_find_key(map_val, key_val, key_kind)?;
        let val_bt = self.backend.kind_to_llvm(val_kind);
        let int = self.backend.types.int;
        let found = self.backend.builder.build_int_compare(IntPredicate::SGE, idx, int.const_zero(), "get.found").unwrap();
        let fnv = self.cur_fn();
        let some_bb = self.backend.context.append_basic_block(fnv, "get.some");
        let none_bb = self.backend.context.append_basic_block(fnv, "get.none");
        let merge = self.backend.context.append_basic_block(fnv, "get.merge");
        self.backend.builder.build_conditional_branch(found, some_bb, none_bb).unwrap();

        self.backend.builder.position_at_end(some_bb);
        let slot = unsafe { self.backend.builder.build_in_bounds_gep(val_bt, vals, &[idx], "get.slot").unwrap() };
        let v = self.backend.builder.build_load(val_bt, slot, "get.val").unwrap();
        let some_agg = self.result_value(v, true)?;
        self.backend.builder.build_unconditional_branch(merge).unwrap();

        self.backend.builder.position_at_end(none_bb);
        let none_agg = self.result_value(val_bt.const_zero(), false)?;
        self.backend.builder.build_unconditional_branch(merge).unwrap();

        self.backend.builder.position_at_end(merge);
        let res_ty = self.basic_type_of(some_agg);
        let phi = self.backend.builder.build_phi(res_ty, "get.phi").unwrap();
        phi.add_incoming(&[(&some_agg, some_bb), (&none_agg, none_bb)]);
        Ok(phi.as_basic_value())
    }

    /// `m.keys()` / `m.values()`: a `List` snapshot of one column, in insertion
    /// order.
    fn gen_map_column_list(
        &mut self,
        map_val: BasicValueEnum<'ctx>,
        elem_kind: &Kind,
        want_keys: bool,
    ) -> GenResult<'ctx> {
        let (keys, vals, len) = self.map_parts(map_val);
        let elem_bt = self.backend.kind_to_llvm(elem_kind);
        let src = if want_keys { keys } else { vals };
        let elem_size = self.abi_size(elem_bt);
        let int = self.backend.types.int;
        let bytes = self.backend.builder.build_int_mul(len, int.const_int(elem_size, false), "col.bytes").unwrap();
        let nonempty = self.backend.builder.build_int_compare(IntPredicate::SGT, bytes, int.const_zero(), "col.nonempty").unwrap();
        let size = self.backend.builder.build_select(nonempty, bytes, int.const_int(1, false), "col.size").unwrap().into_int_value();
        let f = self.backend.module.get_function("malloc").ok_or("malloc missing")?;
        let buf = self
            .backend
            .builder
            .build_direct_call(f, &[size.into()], "col.buf")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let _ = self.backend.builder.build_memcpy(buf, 1, src, 1, bytes).unwrap();
        let mut agg = self.backend.types.xz_list.const_zero();
        agg = self.backend.builder.build_insert_value(agg, buf, 0, "l.ptr").unwrap().into_struct_value();
        agg = self.backend.builder.build_insert_value(agg, len, 1, "l.len").unwrap().into_struct_value();
        Ok(agg.into())
    }

    /// Lower an empty `{}` when the binding is a Set; otherwise an empty Map.
    fn gen_empty_brace(&mut self) -> GenResult<'ctx> {
        match &self.set_hint {
            Some(Kind::Set(_)) => Ok(self.backend.types.xz_list.const_zero().into()),
            _ => Ok(self.backend.types.xz_map.const_zero().into()),
        }
    }

    /// Lower `{e1, e2, ...}`: start from the empty set and `insert` each
    /// element in order, so a repeated element keeps its first position
    /// (docs/12).
    fn gen_set_lit(&mut self, elems: &[Expr]) -> GenResult<'ctx> {
        let mut agg: BasicValueEnum<'ctx> = self.backend.types.xz_list.const_zero().into();
        for e in elems {
            let ev = self.gen_expr(e)?;
            let kind = kind_from_llvm(self.backend, self.basic_type_of(ev));
            agg = self.set_insert_value(agg, ev, &kind)?;
        }
        Ok(agg)
    }

    /// Linear scan for `element` in the set's element buffer; returns the
    /// matching index or -1. Elements compare with the same rule as Map keys
    /// (`map_key_eq`: by value for Int/usize/Bool/Char, by content for Str).
    fn set_find(
        &mut self,
        set_val: BasicValueEnum<'ctx>,
        elem_val: BasicValueEnum<'ctx>,
        elem_kind: &Kind,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let (buf, len) = self.str_parts(set_val);
        let elem_bt = self.basic_type_of(elem_val);
        let int = self.backend.types.int;
        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "sfind.header");
        let body = self.backend.context.append_basic_block(fnv, "sfind.body");
        let next = self.backend.context.append_basic_block(fnv, "sfind.next");
        let found = self.backend.context.append_basic_block(fnv, "sfind.found");
        let done = self.backend.context.append_basic_block(fnv, "sfind.done");
        let i_ptr = self.backend.builder.build_alloca(int, "sfind.i").unwrap();
        let idx_ptr = self.backend.builder.build_alloca(int, "sfind.idx").unwrap();
        self.backend.builder.build_store(i_ptr, int.const_zero()).unwrap();
        self.backend.builder.build_store(idx_ptr, int.const_int(u64::MAX, false)).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(header);
        let i_cur = self.build_load(int.into(), i_ptr, "sfind.ic").into_int_value();
        let cond = self.backend.builder.build_int_compare(IntPredicate::SLT, i_cur, len, "sfind.cond").unwrap();
        self.backend.builder.build_conditional_branch(cond, body, done).unwrap();

        self.backend.builder.position_at_end(body);
        let slot = unsafe { self.backend.builder.build_in_bounds_gep(elem_bt, buf, &[i_cur], "sfind.es").unwrap() };
        let ecur = self.backend.builder.build_load(elem_bt, slot, "sfind.e").unwrap();
        let eq = self.map_key_eq(ecur, elem_val, elem_kind)?;
        self.backend.builder.build_conditional_branch(eq, found, next).unwrap();

        self.backend.builder.position_at_end(next);
        let i_next = self.backend.builder.build_int_add(i_cur, int.const_int(1, false), "sfind.in").unwrap();
        self.backend.builder.build_store(i_ptr, i_next).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        self.backend.builder.position_at_end(found);
        self.backend.builder.build_store(idx_ptr, i_cur).unwrap();
        self.backend.builder.build_unconditional_branch(done).unwrap();

        self.backend.builder.position_at_end(done);
        Ok(self.build_load(int.into(), idx_ptr, "sfind.ret").into_int_value())
    }

    /// Return a new Set with `element` added: when already present the same
    /// elements stay in the same positions; otherwise malloc `len+1`, copy the
    /// old buffer, and store the element at the end (docs/12).
    fn set_insert_value(
        &mut self,
        set_val: BasicValueEnum<'ctx>,
        elem_val: BasicValueEnum<'ctx>,
        elem_kind: &Kind,
    ) -> GenResult<'ctx> {
        let (buf, len) = self.str_parts(set_val);
        let idx = self.set_find(set_val, elem_val, elem_kind)?;
        let elem_bt = self.basic_type_of(elem_val);
        let elem_size = self.abi_size(elem_bt);
        let int = self.backend.types.int;
        let zero = int.const_zero();
        let one = int.const_int(1, false);
        let found = self.backend.builder.build_int_compare(IntPredicate::SGE, idx, zero, "s.found").unwrap();
        let grow = self.backend.builder.build_select(found, zero, one, "s.grow").unwrap().into_int_value();
        let new_len = self.backend.builder.build_int_add(len, grow, "s.newlen").unwrap();
        let new_buf = self.alloc_map_column(elem_size, new_len, "s.buf")?;
        let old_bytes = self.backend.builder.build_int_mul(len, int.const_int(elem_size, false), "s.bytes").unwrap();
        let _ = self.backend.builder.build_memcpy(new_buf, 1, buf, 1, old_bytes).unwrap();
        // Store at the existing slot when found, else at the end.
        let slot = self.backend.builder.build_select(found, idx, len, "s.slot").unwrap().into_int_value();
        let eslot = unsafe { self.backend.builder.build_in_bounds_gep(elem_bt, new_buf, &[slot], "s.eslot").unwrap() };
        self.backend.builder.build_store(eslot, elem_val).unwrap();
        let mut agg = self.backend.types.xz_list.const_zero();
        agg = self.backend.builder.build_insert_value(agg, new_buf, 0, "s.bp").unwrap().into_struct_value();
        agg = self.backend.builder.build_insert_value(agg, new_len, 1, "s.len").unwrap().into_struct_value();
        Ok(agg.into())
    }

    /// `s.contains(e) -> Bool`: membership by linear scan.
    fn gen_set_contains(
        &mut self,
        set_val: BasicValueEnum<'ctx>,
        elem_val: BasicValueEnum<'ctx>,
        elem_kind: &Kind,
    ) -> GenResult<'ctx> {
        let idx = self.set_find(set_val, elem_val, elem_kind)?;
        let int = self.backend.types.int;
        let r = self.backend.builder.build_int_compare(IntPredicate::SGE, idx, int.const_zero(), "s.has").unwrap();
        Ok(r.into())
    }

    /// Lower a `[e1, e2, ...]` literal: allocate an element buffer, store the
    /// elements, and build the `{ ptr, len }` List value.
    fn gen_list_lit(&mut self, elems: &[Expr]) -> GenResult<'ctx> {
        let mut vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(elems.len());
        for e in elems {
            vals.push(self.gen_expr(e)?);
        }
        let elem_bt: BasicTypeEnum<'ctx> = match vals.first() {
            Some(v) => self.basic_type_of(*v),
            None => match &self.list_hint {
                Some(Kind::List(t)) => self.backend.kind_to_llvm(t),
                _ => return self.fail("cannot infer the element type of an empty list literal; declare it, e.g. `let xs: List[Int] = []`"),
            },
        };
        let len = vals.len() as u64;
        let buf = self.alloc_buffer(elem_bt, len, "list.buf")?;
        for (i, v) in vals.iter().enumerate() {
            let idx = self.backend.types.int.const_int(i as u64, false);
            let slot = unsafe { self.backend.builder.build_in_bounds_gep(elem_bt, buf, &[idx], "list.elem").unwrap() };
            self.backend.builder.build_store(slot, *v).unwrap();
        }
        let mut agg = self.backend.types.xz_list.const_zero();
        agg = self.backend.builder.build_insert_value(agg, buf, 0, "l.ptr").unwrap().into_struct_value();
        let lenv = self.backend.types.int.const_int(len, false);
        agg = self.backend.builder.build_insert_value(agg, lenv, 1, "l.len").unwrap().into_struct_value();
        Ok(agg.into())
    }

    /// Lower bounds-checked `xs[i]` to `Result[T, IndexError]`.
    fn gen_list_index(
        &mut self,
        base_val: BasicValueEnum<'ctx>,
        idx_val: BasicValueEnum<'ctx>,
        base: &Expr,
    ) -> GenResult<'ctx> {
        let elem = match self.list_elem_kind(base) {
            Some(k) => k,
            None => return self.fail("cannot determine the list element type for indexing; bind the list with a declared `List[T]` type"),
        };
        let elem_bt = self.backend.kind_to_llvm(&elem);
        let (buf, len) = self.str_parts(base_val);
        let idx = idx_val.into_int_value();
        self.bounds_checked_index(buf, len, idx, elem_bt)
    }

    /// Build `Result[T, IndexError]` for `buf[idx]`, bounds-checked against
    /// `len` elements of `elem_bt`. Shared by `List[T]` indexing and `Str.at`.
    fn bounds_checked_index(
        &mut self,
        buf: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
        elem_bt: BasicTypeEnum<'ctx>,
    ) -> GenResult<'ctx> {
        let zero = self.backend.types.int.const_int(0, false);
        let ge = self.backend.builder.build_int_compare(IntPredicate::SGE, idx, zero, "idx.ge").unwrap();
        let lt = self.backend.builder.build_int_compare(IntPredicate::SLT, idx, len, "idx.lt").unwrap();
        let in_range = self.backend.builder.build_and(ge, lt, "idx.ok").unwrap();

        let fnv = self.cur_fn();
        let ok_bb = self.backend.context.append_basic_block(fnv, "index.ok");
        let err_bb = self.backend.context.append_basic_block(fnv, "index.err");
        let merge = self.backend.context.append_basic_block(fnv, "index.merge");
        self.backend.builder.build_conditional_branch(in_range, ok_bb, err_bb).unwrap();

        self.backend.builder.position_at_end(ok_bb);
        let slot = unsafe { self.backend.builder.build_in_bounds_gep(elem_bt, buf, &[idx], "index.slot").unwrap() };
        let v = self.backend.builder.build_load(elem_bt, slot, "index.val").unwrap();
        let ok_agg = self.result_value(v, true)?;
        self.backend.builder.build_unconditional_branch(merge).unwrap();

        self.backend.builder.position_at_end(err_bb);
        let zero_v = elem_bt.const_zero();
        let err_agg = self.result_value(zero_v, false)?;
        self.backend.builder.build_unconditional_branch(merge).unwrap();

        self.backend.builder.position_at_end(merge);
        let res_ty = self.basic_type_of(ok_agg);
        let phi = self.backend.builder.build_phi(res_ty, "index.phi").unwrap();
        phi.add_incoming(&[(&ok_agg, ok_bb), (&err_agg, err_bb)]);
        Ok(phi.as_basic_value())
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
            // A `?` unwraps a fresh heap `Str` payload out of a fallible call
            // (`read_file(path)?`), so the binding owns that buffer
            // (docs/12-stdlib.md, docs/13-codegen.md).
            Expr::Prop(inner, _) => self.is_fresh_str_result(inner),
            _ => false,
        }
    }

    /// Is `e` a call that, on success, yields a fresh heap `Str` this frame
    /// would own? `read_file` allocates a new buffer through the runtime
    /// (docs/12-stdlib.md); its `Result` payload is freed by the same rules as
    /// a `concat`/`to_str` result.
    fn is_fresh_str_result(&self, e: &Expr) -> bool {
        matches!(e, Expr::Call(callee, _) if matches!(&**callee, Expr::Name(n) if n == "read_file"))
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

    /// After binding `name` to `v`, record whether it is a unique owned binding,
    /// and mark any existing binding it copied as no longer (the buffer is now
    /// shared → never released).
    fn adopt_ownership(&mut self, name: &str, init: &Expr) {
        if let Some(base) = self.alias_source(init) {
            self.owns.remove(&base);
        }
        match self.fresh_free(init) {
            Some(how) => {
                self.owns.insert(name.to_string(), how);
            }
            None => {
                self.owns.remove(name);
            }
        }
    }

    /// The release strategy for an expression that produces a buffer this
    /// function would own, or `None` when the value is borrowed, static, or
    /// aliased. An extern call with a `transfer` return owns its buffer and
    /// releases it through the declaration's `release` symbol; a
    /// `concat`/`to_str` result is a fresh runtime allocation.
    fn fresh_free(&self, e: &Expr) -> Option<FreeHow> {
        if let Expr::Call(callee, _) = e
            && let Expr::Name(n) = &**callee
            && let Some(sym) = self.backend.release_syms.get(n)
        {
            return Some(FreeHow::Symbol(sym.clone()));
        }
        if self.is_fresh_temp(e) {
            Some(FreeHow::Registry)
        } else {
            None
        }
    }

    /// Release an owned buffer whose binding is dying: a runtime allocation
    /// through the registry, or a `transfer` return through its named symbol.
    fn release_value(&mut self, how: &FreeHow, v: BasicValueEnum<'ctx>) {
        match how {
            FreeHow::Registry => self.emit_str_free(v),
            FreeHow::Symbol(sym) => self.emit_symbol_release(sym, v),
        }
    }

    /// Call a `release` deallocator on the pointer an owned value carries:
    /// the `ptr` field of a `Str`/`Bytes`, or the value itself for `Ptr`.
    fn emit_symbol_release(&mut self, sym: &str, v: BasicValueEnum<'ctx>) {
        let fv = match self.backend.functions.get(sym) {
            Some(fv) => *fv,
            None => return,
        };
        let ptr = if self.is_str(v) || self.is_bytes(v) {
            self.backend
                .builder
                .build_extract_value(v.into_struct_value(), 0, "rel.ptr")
                .unwrap()
        } else {
            v
        };
        let _ = self.backend.builder.build_direct_call(fv, &[ptr.into()], "release").unwrap();
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
            match &s.kind {
                StmtKind::Expr(e) => self.degrade_str_expr(e),
                StmtKind::Decl(d) => {
                    if let Some(init) = &d.init {
                        self.degrade_str_expr(init);
                    }
                }
                StmtKind::Assign(a) => self.degrade_str_expr(&a.value),
                _ => {}
            }
        }
    }

    /// Release every buffer this function frame uniquely owns: fresh runtime
    /// allocations that were never copied, and `transfer` returns released
    /// through their declared symbol. Called just before each return path.
    /// Bindings whose buffers were copied, or that came from aliased sources,
    /// are deliberately left to leak (never released).
    fn free_owned_bindings(&mut self) {
        if self.leak_owned {
            return;
        }
        let owned: Vec<(String, FreeHow)> = self.owns.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (n, how) in owned {
            if let Some((ptr, ty)) = self.scope.get(&n).map(|(p, t)| (*p, *t)) {
                match &how {
                    FreeHow::Registry => {
                        if matches!(ty, BasicTypeEnum::StructType(st) if st == self.backend.types.xz_str) {
                            let v = self.build_load(ty, ptr, &n);
                            self.emit_str_free(v);
                        }
                    }
                    FreeHow::Symbol(_) => {
                        let v = self.build_load(ty, ptr, &n);
                        self.release_value(&how, v);
                    }
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
            match &stmt.kind {
                StmtKind::Decl(d) => self.gen_decl(d),
                StmtKind::Assign(a) => self.gen_assign(a),
                StmtKind::Expr(e) => {
                    let v = self.gen_expr(e);
                    if i == n - 1 {
                        last = v.ok();
                    }
                }
                StmtKind::Break => {
                    let _ = self.gen_break();
                }
                StmtKind::Continue => {
                    let _ = self.gen_continue();
                }
            }
        }
        last
    }

    fn gen_decl(&mut self, d: &ast::Decl) {
        if d.recv {
            self.gen_recv_decl(d);
            return;
        }
        let declared_kind: Option<Kind> = match &d.ty {
            Some(ty) => Some(self.kind_of(ty)),
            None => None,
        };
        match &d.init {
            Some(e) => {
                // Let `none`, an empty `[]`, and an empty `{}` see the declared type.
                let saved_hint = self.none_hint.clone();
                let saved_list_hint = self.list_hint.clone();
                let saved_set_hint = self.set_hint.clone();
                let saved_map_hint = self.map_hint.clone();
                self.none_hint = declared_kind.clone();
                self.list_hint = declared_kind.clone();
                self.set_hint = declared_kind.clone();
                self.map_hint = declared_kind.clone();
                let r = self.gen_expr(e);
                self.none_hint = saved_hint;
                self.list_hint = saved_list_hint;
                self.set_hint = saved_set_hint;
                self.map_hint = saved_map_hint;
                match r {
                    Ok(v) => match &declared_kind {
                        Some(k) => {
                            let lty = self.backend.kind_to_llvm(k);
                            self.bind_typed(&d.name, lty, v);
                        }
                        None => {
                            self.bind_value(&d.name, v);
                        }
                    },
                    Err(_) => {}
                }
                // Track List element kinds so `for x in xs` / `xs[i]` can load
                // the right element type. From the declared type, or inferred
                // from a list literal / list-producing expression.
                let elem = match &declared_kind {
                    Some(Kind::List(t)) => Some((**t).clone()),
                    _ => self.list_elem_kind(e),
                };
                if let Some(ek) = elem {
                    self.list_elems.insert(d.name.clone(), ek);
                }
                // Track Map key/value kinds so `m.get`/`insert`/`keys`/`values`
                // can load the right types. From the declared type, or inferred
                // from a map literal / insert-producing expression.
                let kv = match &declared_kind {
                    Some(Kind::Map(k, v)) => Some(((**k).clone(), (**v).clone())),
                    _ => self.map_kv_kind(e),
                };
                if let Some(kv) = kv {
                    self.map_kvs.insert(d.name.clone(), kv);
                }
                // Track Set element kinds so `s.contains`/`insert` and
                // `for e in s` can load the right element type. From the
                // declared type, or inferred from a set literal.
                let selem = match &declared_kind {
                    Some(Kind::Set(t)) => Some((**t).clone()),
                    _ => self.set_elem_kind(e),
                };
                if let Some(ek) = selem {
                    self.set_elems.insert(d.name.clone(), ek);
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
                        let k = self.kind_of(ty);
                        if let Kind::List(t) = &k {
                            self.list_elems.insert(d.name.clone(), (**t).clone());
                        }
                        if let Kind::Map(kk, vv) = &k {
                            self.map_kvs.insert(d.name.clone(), ((**kk).clone(), (**vv).clone()));
                        }
                        if let Kind::Set(t) = &k {
                            self.set_elems.insert(d.name.clone(), (**t).clone());
                        }
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
                            // Overwriting a binding that uniquely owns a buffer
                            // with another *fresh* value: release the old
                            // buffer through its own channel. If the new value
                            // is an alias (name/identity), the binding is being
                            // shared, so keep the old one alive (leak) rather
                            // than release underneath.
                            let old_how = self.owns.get(n).cloned();
                            if let Some(old_how) = old_how {
                                if self.fresh_free(&a.value).is_some() {
                                    let old = self.build_load(ty, ptr, "old");
                                    self.release_value(&old_how, old);
                                }
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
            ast::AssignTarget::Field(base, fname) => {
                match self.gen_expr(&a.value) {
                    Ok(v) => match self.field_lvalue(base, fname) {
                        Ok((fptr, fty)) => {
                            let _ = self.apply_assign_op(&a.op, fty, fptr, v);
                        }
                        Err(_) => {}
                    },
                    Err(_) => {}
                }
            }
        }
    }

    /// The address and LLVM type of a record field lvalue (`p.x`, `a.b.c`).
    /// The base must be a record binding or a nested record field.
    fn field_lvalue(
        &mut self,
        base: &Expr,
        fname: &str,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        let (base_ptr, base_ty) = match base {
            Expr::Name(n) => match self.scope.get(n) {
                Some((p, t)) => (*p, *t),
                None => return self.fail(&format!("unknown name '{}' in field assignment", n)),
            },
            Expr::Field(inner, iname) => self.field_lvalue(inner, iname)?,
            _ => return self.fail("field assignment target must be a record binding or field"),
        };
        let st = match base_ty {
            BasicTypeEnum::StructType(st) => st,
            _ => return self.fail("field assignment on a non-record"),
        };
        let rec_name = match st.get_name() {
            Some(n) => n.to_str().unwrap().to_string(),
            None => return self.fail("field assignment on an anonymous struct"),
        };
        let idx = match self.backend.record_field_index(&rec_name, fname) {
            Some(i) => i,
            None => return self.fail(&format!("record '{}' has no field '{}'", rec_name, fname)),
        };
        let fty = st.get_field_type_at_index(idx).ok_or("field type missing")?;
        let fptr = self.backend.builder.build_struct_gep(st, base_ptr, idx, fname).unwrap();
        Ok((fptr, fty))
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
                let v = self.coerce_to(ty, v);
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

    /// `let x <- recv(ch)`: allocate a slot of the channel's payload type and
    /// copy the next message into it (docs/13-codegen.md § Concurrency). Blocks
    /// (via the host scheduler) when the channel is empty.
    fn gen_recv_decl(&mut self, d: &ast::Decl) {
        let ch = match &d.chan {
            Some(c) => c.clone(),
            None => {
                let _ = self.fail::<()>(&format!("binding '{}' has a recv with no channel", d.name));
                return;
            }
        };
        let (id, payload) = match self.channel_target(&ch) {
            Ok(v) => v,
            Err(_) => return,
        };
        let lty = self.backend.kind_to_llvm(&payload);
        let slot = self.backend.builder.build_alloca(lty, &d.name).unwrap();
        self.scope.insert(d.name.clone(), (slot, lty));
        let _ = self.emit_chan_recv(id, slot, lty);
    }

    /// Resolve a channel name to its id and payload kind, or fail codegen.
    fn channel_target(&mut self, name: &str) -> Result<(u64, Kind), String> {
        let id = match self.backend.channel_ids.get(name).copied() {
            Some(id) => id,
            None => return self.fail(&format!("unknown channel '{}'", name)),
        };
        let payload = self.backend.channel_payloads.get(name).cloned().unwrap_or(Kind::Unit);
        Ok((id, payload))
    }

    /// Copy the next message on channel `id` into `slot`. The host scheduler
    /// suspends the current task when the channel is empty.
    fn emit_chan_recv(&mut self, id: u64, slot: PointerValue<'ctx>, lty: BasicTypeEnum<'ctx>) -> GenResult<'ctx> {
        let size = self.abi_size(lty);
        let f = self.backend.module.get_function("xz_chan_recv").ok_or("xz_chan_recv missing")?;
        let idc = self.backend.types.int.const_int(id, false);
        let szc = self.backend.types.int.const_int(size, false);
        self.backend
            .builder
            .build_direct_call(f, &[idc.into(), slot.into(), szc.into()], "recv")
            .unwrap();
        Ok(self.build_load(lty, slot, "recv.val"))
    }

    /// `send(ch, v)`: copy the value into the channel (docs/05). Never blocks.
    fn gen_send(&mut self, ch: &Expr, value: &Expr) -> GenResult<'ctx> {
        let name = match ch {
            Expr::Name(n) => n.clone(),
            _ => return self.fail("send target must be a channel name"),
        };
        let (id, _payload) = self.channel_target(&name)?;
        let v = self.gen_expr(value)?;
        let ty = self.basic_type_of(v);
        let slot = self.backend.builder.build_alloca(ty, "send.slot").unwrap();
        self.backend.builder.build_store(slot, v).unwrap();
        let size = self.abi_size(ty);
        let f = self.backend.module.get_function("xz_chan_send").ok_or("xz_chan_send missing")?;
        let idc = self.backend.types.int.const_int(id, false);
        let szc = self.backend.types.int.const_int(size, false);
        self.backend
            .builder
            .build_direct_call(f, &[idc.into(), slot.into(), szc.into()], "send")
            .unwrap();
        Ok(self.backend.types.unit.const_zero().into())
    }

    /// `recv(ch)` as an expression.
    fn gen_recv_value(&mut self, ch: &str) -> GenResult<'ctx> {
        let (id, payload) = self.channel_target(ch)?;
        let lty = self.backend.kind_to_llvm(&payload);
        let slot = self.backend.builder.build_alloca(lty, "recv.slot").unwrap();
        self.emit_chan_recv(id, slot, lty)
    }

    /// `await f(args)` — run the async callee as a scheduled child coroutine and
    /// suspend the caller until it completes (docs/05-concurrency.md rule 6).
    ///
    /// The caller evaluates the arguments into a stack struct, spawns an
    /// internal trampoline with that struct as its argument, then blocks on a
    /// synthetic completion channel. The trampoline calls `f` and sends the
    /// result on the channel, so the scheduler can run other ready tasks while
    /// the child is blocked. The caller's frame stays alive through the block,
    /// so the argument struct is valid when the child finally runs.
    fn gen_await(&mut self, inner: &Expr) -> GenResult<'ctx> {
        let (callee, args) = match inner {
            Expr::Call(c, a) => (&**c, a),
            _ => return self.fail("await applies to a call to an async function"),
        };
        let name = match callee {
            Expr::Name(n) => n.clone(),
            _ => return self.fail("await target must be a named function"),
        };
        // A generic async callee is monomorphized on the concrete argument
        // types first; the await then targets the specialization (docs/13).
        let generic = if self.backend.generic_params.contains_key(&name) {
            Some(self.gen_generic_await_args(&name, args)?)
        } else {
            None
        };
        let name = match &generic {
            Some((mangled, _)) => mangled.clone(),
            None => name,
        };
        let fv = match self.backend.functions.get(&name).copied() {
            Some(fv) => fv,
            None => return self.fail(&format!("await target '{}' is not a function", name)),
        };
        let (params, xz_ret, param_kinds) = match self.backend.sigs.get(&name) {
            Some(s) => (s.params.clone(), s.xz_ret.clone(), s.param_kinds.clone()),
            None => return self.fail(&format!("missing signature for '{}'", name)),
        };
        let ret_kind = xz_ret.unwrap_or(Kind::Unit);

        let env_ty = self.backend.context.struct_type(&params, false);
        let env = self.backend.builder.build_alloca(env_ty, "await.env").unwrap();
        // A generic callee's arguments were evaluated to infer its type
        // arguments already; a concrete callee's are evaluated here with its
        // declared types as hints (an empty `[]`/`none` argument materializes
        // its type from the parameter).
        let vals: Vec<BasicValueEnum<'ctx>> = match generic {
            Some((_, vals)) => vals,
            None => {
                let mut vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(args.len());
                for (i, a) in args.iter().enumerate() {
                    let hint = param_kinds.get(i).cloned();
                    let saved_list_hint = self.list_hint.clone();
                    let saved_none_hint = self.none_hint.clone();
                    let saved_set_hint = self.set_hint.clone();
                    let saved_map_hint = self.map_hint.clone();
                    self.list_hint = hint.clone();
                    self.none_hint = hint.clone();
                    self.set_hint = hint.clone();
                    self.map_hint = hint;
                    vals.push(self.gen_expr(a)?);
                    self.list_hint = saved_list_hint;
                    self.none_hint = saved_none_hint;
                    self.set_hint = saved_set_hint;
                    self.map_hint = saved_map_hint;
                }
                vals
            }
        };
        for (i, v) in vals.iter().enumerate() {
            let p = self.backend.builder.build_struct_gep(env_ty, env, i as u32, "await.arg").unwrap();
            self.backend.builder.build_store(p, *v).unwrap();
        }

        let aid = self.backend.next_channel_id();
        let tramp = self.make_await_trampoline(aid, fv, env_ty, &params, &ret_kind);
        let spawn = self
            .backend
            .module
            .get_function("xz_task_spawn_arg")
            .ok_or("xz_task_spawn_arg missing")?;
        let fp = tramp.as_global_value().as_pointer_value();
        let _ = self
            .backend
            .builder
            .build_direct_call(spawn, &[fp.into(), env.into()], "await.spawn")
            .unwrap();

        let ret_ty = self.backend.kind_to_llvm(&ret_kind);
        let slot = self.backend.builder.build_alloca(ret_ty, "await.ret").unwrap();
        self.emit_chan_recv(aid, slot, ret_ty)
    }

    /// Emit the child coroutine for one `await` site: `void (ptr)` where the
    /// pointer addresses the caller's argument struct. It calls the async
    /// function and sends the result on completion channel `aid`.
    fn make_await_trampoline(
        &mut self,
        aid: u64,
        callee: FunctionValue<'ctx>,
        env_ty: StructType<'ctx>,
        params: &[BasicTypeEnum<'ctx>],
        ret_kind: &Kind,
    ) -> FunctionValue<'ctx> {
        let ptr = self.backend.types.ptr;
        let fn_ty = self.backend.context.void_type().fn_type(&[ptr.into()], false);
        let tramp = self.backend.module.add_function(&format!("__await_{}", aid), fn_ty, None);
        tramp.set_linkage(Linkage::Internal);

        let saved = self.backend.builder.get_insert_block();
        let entry = self.backend.context.append_basic_block(tramp, "entry");
        self.backend.builder.position_at_end(entry);
        let env = tramp.get_nth_param(0).unwrap().into_pointer_value();
        let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::with_capacity(params.len());
        for (i, ty) in params.iter().enumerate() {
            let p = self.backend.builder.build_struct_gep(env_ty, env, i as u32, "arg").unwrap();
            let v = self.backend.builder.build_load(*ty, p, "arg.v").unwrap();
            call_args.push(v.into());
        }
        let call = self.backend.builder.build_direct_call(callee, &call_args, "call").unwrap();

        // Send the result on the completion channel. The slot always exists so
        // a `Unit` result is a zero-byte send (the parent's `recv` still blocks).
        let ret_ty = self.backend.kind_to_llvm(ret_kind);
        let slot = self.backend.builder.build_alloca(ret_ty, "ret").unwrap();
        if let Some(v) = call.try_as_basic_value().basic() {
            self.backend.builder.build_store(slot, v).unwrap();
        }
        let size = self.abi_size(ret_ty);
        if let Some(send) = self.backend.module.get_function("xz_chan_send") {
            let idc = self.backend.types.int.const_int(aid, false);
            let szc = self.backend.types.int.const_int(size, false);
            let _ = self
                .backend
                .builder
                .build_direct_call(send, &[idc.into(), slot.into(), szc.into()], "await.send")
                .unwrap();
        }
        let _ = self.backend.builder.build_return(None);
        if let Some(bb) = saved {
            self.backend.builder.position_at_end(bb);
        }
        tramp
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
            Expr::ListLit(elems) => self.gen_list_lit(elems),
            Expr::MapLit(entries) => self.gen_map_lit(entries),
            Expr::SetLit(elems) => self.gen_set_lit(elems),
            Expr::Index(base, idx) => {
                let bv = self.gen_expr(base)?;
                let iv = self.gen_expr(idx)?;
                self.gen_list_index(bv, iv, base)
            }
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
            Expr::Await(a) => self.gen_await(a),
            Expr::Send(ch, value) => self.gen_send(ch, value),
            Expr::Recv(ch) => self.gen_recv_value(ch),
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
        self.write_back_mut_params();
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

    /// Copy each `mut` parameter's current binding value back through the
    /// caller's out-pointer (copy-out). A no-op for functions without `mut`
    /// parameters.
    fn write_back_mut_params(&mut self) {
        let outs = self.mut_outs.clone();
        for (alloca, ty, out) in outs {
            let v = self.build_load(ty, alloca, "mut.out");
            self.backend.builder.build_store(out, v).unwrap();
        }
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
        } else if a.get_type().is_int_type() {
            let r = self
                .backend
                .builder
                .build_int_compare(pred_int, a.into_int_value(), b.into_int_value(), "cmp")
                .unwrap();
            Ok(r.into())
        } else {
            self.fail("comparison is only defined for Int, Float, and Ptr (and Str equality via `==` is not implemented)")
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
        let k = self.kind_of(ty);
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

    /// Produce the address the caller passes for a `mut` argument, plus the
    /// pointed-to value type. A name or field lvalue yields the caller's own
    /// storage; any other expression is materialized into a fresh temporary
    /// (copy-in only — the callee's copy-out is discarded).
    fn gen_mut_arg(&mut self, e: &Expr) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        match e {
            Expr::Name(n) => match self.scope.get(n) {
                Some((ptr, ty)) => Ok((*ptr, *ty)),
                None => self.fail(&format!("unknown name '{}' in mut argument", n)),
            },
            Expr::Field(base, fname) => self.field_lvalue(base, fname),
            _ => {
                let v = self.gen_expr(e)?;
                let ty = self.basic_type_of(v);
                let alloca = self.backend.builder.build_alloca(ty, "mut.tmp").unwrap();
                self.backend.builder.build_store(alloca, v).unwrap();
                Ok((alloca, ty))
            }
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
                let field_ty = st.get_field_type_at_index(i as u32).unwrap_or_else(|| v.get_type());
                let v = self.coerce_to(field_ty, v);
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
        // generic function: monomorphize on the concrete argument types
        if self.backend.generic_params.contains_key(name) {
            return self.gen_generic_call(name, args);
        }
        // regular function / extern
        if let Some(fv) = self.backend.functions.get(name).copied() {
            let param_kinds = self.backend.sigs.get(name).map(|s| s.param_kinds.clone()).unwrap_or_default();
            let param_muts = self.backend.sigs.get(name).map(|s| s.param_muts.clone()).unwrap_or_default();
            let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
            for (i, a) in args.iter().enumerate() {
                // A `mut` argument crosses as a pointer to the caller's storage
                // (copy-in/copy-out — docs/04-memory-model.md).
                if param_muts.get(i).copied().unwrap_or(false) {
                    let (ptr, _ty) = self.gen_mut_arg(a)?;
                    call_args.push(ptr.into());
                    continue;
                }
                // Give the argument the callee's declared param type as a hint
                // (so an empty `[]` argument can materialize its element type).
                let hint = param_kinds.get(i).cloned();
                let saved_list_hint = self.list_hint.clone();
                let saved_none_hint = self.none_hint.clone();
                let saved_set_hint = self.set_hint.clone();
                let saved_map_hint = self.map_hint.clone();
                self.list_hint = hint.clone();
                self.none_hint = hint.clone();
                self.set_hint = hint.clone();
                self.map_hint = hint;
                let v = self.gen_expr(a)?;
                self.list_hint = saved_list_hint;
                self.none_hint = saved_none_hint;
                self.set_hint = saved_set_hint;
                self.map_hint = saved_map_hint;
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
            // A fresh owned temp (a concat/to_str result, or a `transfer` return)
            // passed directly to print is consumed here: release it through its
            // own channel right after the call. A name or literal is not a temp
            // — the binding handles its own release.
            if let Some(how) = self.fresh_free(&args[0]) {
                self.release_value(&how, v);
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
        // stdlib now / monotonic -> host xz_time_now() / xz_time_monotonic()
        // returning f64 seconds (docs/12, docs/13).
        if name == "now" || name == "monotonic" {
            if !args.is_empty() {
                return self.fail(&format!("{} takes no arguments", name));
            }
            let host = if name == "now" { "xz_time_now" } else { "xz_time_monotonic" };
            let f = self.backend.module.get_function(host).ok_or(format!("{} missing", host))?;
            let call = self.backend.builder.build_direct_call(f, &[], "time").unwrap();
            return Ok(call.try_as_basic_value().basic().unwrap());
        }
        // stdlib read_file -> host xz_read_file(ptr, len, out) -> i1. The host
        // writes a fresh Str payload through `out` on success; codegen wraps it
        // in the Result[Str, Err] struct { payload, ok-flag } (docs/13).
        if name == "read_file" {
            if args.len() != 1 {
                return self.fail("read_file takes one Str argument");
            }
            self.degrade_str_expr(&args[0]);
            let v = self.gen_expr(&args[0])?;
            let (ptr, len) = self.str_parts(v);
            let str_ty = self.backend.types.xz_str;
            let out = self.backend.builder.build_alloca(str_ty, "read.out").unwrap();
            let f = self.backend.module.get_function("xz_read_file").ok_or("xz_read_file missing")?;
            let ok = self
                .backend
                .builder
                .build_direct_call(f, &[ptr.into(), len.into(), out.into()], "read")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap()
                .into_int_value();
            let payload = self.build_load(str_ty.into(), out, "read.str");
            let st = self.backend.context.struct_type(&[str_ty.into(), self.backend.types.bool.into()], false);
            let mut agg = st.const_zero();
            agg = self.backend.builder.build_insert_value(agg, payload, 0, "r.p").unwrap().into_struct_value();
            agg = self.backend.builder.build_insert_value(agg, ok, 1, "r.f").unwrap().into_struct_value();
            return Ok(agg.into());
        }
        self.fail(&format!("unknown function '{}'", name))
    }

    /// Lower a call to a generic function by monomorphizing on the concrete
    /// argument types: infer each bare type parameter from the matching
    /// argument's LLVM type, create/reuse the specialization, and call it.
    fn gen_generic_call(&mut self, name: &str, args: &[Expr]) -> GenResult<'ctx> {
        let f = self
            .backend
            .func_asts
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing AST for generic function '{}'", name))?;
        let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::with_capacity(args.len());
        let mut val_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            // A `mut` parameter is inferred from the pointed-to value type but
            // passed as an address (copy-in/copy-out — docs/04-memory-model.md).
            if f.params.get(i).map(|p| p.mutable).unwrap_or(false) {
                let (ptr, vty) = self.gen_mut_arg(a)?;
                call_args.push(ptr.into());
                val_tys.push(vty);
            } else {
                let v = self.gen_expr(a)?;
                val_tys.push(self.basic_type_of(v));
                call_args.push(v.into());
            }
        }
        let type_args = self.infer_type_args(name, &val_tys)?;
        let mangled = self.backend.specialize(name, &type_args)?;
        let fv = self.backend.functions.get(&mangled).copied().ok_or("specialization not declared")?;
        let call = self.backend.builder.build_direct_call(fv, &call_args, "call").unwrap();
        match call.try_as_basic_value().basic() {
            Some(v) => Ok(v),
            None => Ok(self.backend.types.unit.const_zero().into()),
        }
    }

    /// Evaluate a generic async call's arguments, infer the callee's type
    /// arguments from their concrete LLVM types, and request the
    /// specialization. Returns the mangled callee name and the evaluated
    /// argument values (a `mut` argument crosses as an address, like a plain
    /// call). The values are reused for the await trampoline's argument struct.
    fn gen_generic_await_args(
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> Result<(String, Vec<BasicValueEnum<'ctx>>), String> {
        let f = self
            .backend
            .func_asts
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing AST for generic function '{}'", name))?;
        let mut vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(args.len());
        let mut val_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            if f.params.get(i).map(|p| p.mutable).unwrap_or(false) {
                let (ptr, vty) = self.gen_mut_arg(a)?;
                vals.push(ptr.into());
                val_tys.push(vty);
            } else {
                let v = self.gen_expr(a)?;
                val_tys.push(self.basic_type_of(v));
                vals.push(v);
            }
        }
        let type_args = self.infer_type_args(name, &val_tys)?;
        let mangled = self.backend.specialize(name, &type_args)?;
        Ok((mangled, vals))
    }

    /// Infer a generic function's type arguments from the concrete LLVM types
    /// of a call's arguments. Only a parameter that *is* a bare type parameter
    /// (`x: T`) contributes a binding; a `List[T]`-style parameter needs
    /// structural inference and is rejected with a clear message.
    fn infer_type_args(&mut self, name: &str, val_tys: &[BasicTypeEnum<'ctx>]) -> Result<Vec<Kind>, String> {
        let f = self
            .backend
            .func_asts
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing AST for generic function '{}'", name))?;
        let tparams = self.backend.generic_params.get(name).cloned().unwrap_or_default();
        let mut bound: Vec<Option<Kind>> = vec![None; tparams.len()];
        for (i, p) in f.params.iter().enumerate() {
            if i >= val_tys.len() {
                break;
            }
            let bare = match &p.ty {
                Type::NamedPlain(n) => Some(n.clone()),
                Type::Named(n, a) if a.is_empty() => Some(n.clone()),
                _ => None,
            };
            if let Some(n) = bare {
                if let Some(idx) = tparams.iter().position(|t| *t == n) {
                    bound[idx] = Some(kind_from_llvm(self.backend, val_tys[i]));
                }
            }
        }
        if let Some(missing) = bound.iter().position(|b| b.is_none()) {
            let msg = format!(
                "cannot infer type parameter '{}' for call to '{}' (only parameters that are a bare type parameter are supported)",
                tparams[missing], name
            );
            self.backend.fail(&msg);
            return Err(msg);
        }
        Ok(bound.into_iter().map(|b| b.unwrap()).collect())
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
            if self.is_map(rv) {
                let (_, _, len) = self.map_parts(rv);
                return Ok(len.into());
            }
            // Str.len() / List.len() -> length (i64); both are `{ ptr, i64 }`.
            let (_, len) = self.str_parts(rv);
            return Ok(len.into());
        }
        if method == "is_empty" {
            if self.is_map(rv) {
                let (_, _, len) = self.map_parts(rv);
                let zero = self.backend.types.int.const_int(0, false);
                let r = self.backend.builder.build_int_compare(IntPredicate::EQ, len, zero, "empty").unwrap();
                return Ok(r.into());
            }
            let (_, len) = self.str_parts(rv);
            let zero = self.backend.types.int.const_int(0, false);
            let r = self
                .backend
                .builder
                .build_int_compare(IntPredicate::EQ, len, zero, "empty")
                .unwrap();
            return Ok(r.into());
        }
        if method == "get" {
            if args.len() != 1 {
                return self.fail("get takes one argument");
            }
            let (key_kind, val_kind) = match self.map_kv_kind(receiver) {
                Some(kv) => kv,
                None => return self.fail("cannot determine the map value type for get; bind the map with a declared `Map[K, V]` type"),
            };
            let kv = self.gen_expr(&args[0])?;
            return self.gen_map_get(rv, kv, &key_kind, &val_kind);
        }
        if method == "contains" {
            if args.len() != 1 {
                return self.fail("contains takes one argument");
            }
            let elem_kind = match self.set_elem_kind(receiver) {
                Some(k) => k,
                None => return self.fail("cannot determine the set element type for contains; bind the set with a declared `Set[T]` type"),
            };
            let ev = self.gen_expr(&args[0])?;
            return self.gen_set_contains(rv, ev, &elem_kind);
        }
        if method == "insert" {
            // `Set.insert(e)` takes one argument; `Map.insert(k, v)` takes two.
            if args.len() == 1 {
                let elem_kind = match self.set_elem_kind(receiver) {
                    Some(k) => k,
                    None => return self.fail("cannot determine the set element type for insert; bind the set with a declared `Set[T]` type"),
                };
                let ev = self.gen_expr(&args[0])?;
                return self.set_insert_value(rv, ev, &elem_kind);
            }
            if args.len() != 2 {
                return self.fail("insert takes one or two arguments");
            }
            let key_kind = match self.map_kv_kind(receiver) {
                Some((k, _)) => k,
                None => return self.fail("cannot determine the map key type for insert; bind the map with a declared `Map[K, V]` type"),
            };
            let kv = self.gen_expr(&args[0])?;
            let vv = self.gen_expr(&args[1])?;
            return self.map_insert_value(rv, kv, vv, &key_kind);
        }
        if method == "keys" || method == "values" {
            let (k, v) = match self.map_kv_kind(receiver) {
                Some(kv) => kv,
                None => return self.fail("cannot determine the map key/value type; bind the map with a declared `Map[K, V]` type"),
            };
            let want_keys = method == "keys";
            let elem = if want_keys { k } else { v };
            return self.gen_map_column_list(rv, &elem, want_keys);
        }
        if method == "append" {
            // Value-returning growth: allocate len+1 elements, copy the old
            // buffer, store the new element, return a fresh List (docs/12).
            if args.len() != 1 {
                return self.fail("append takes one argument");
            }
            let elem = match self.list_elem_kind(receiver) {
                Some(k) => k,
                None => return self.fail("cannot determine the list element type for append; bind the list with a declared `List[T]` type"),
            };
            let elem_bt = self.backend.kind_to_llvm(&elem);
            let elem_size = match &self.backend.target_data {
                Some(td) => td.get_abi_size(&elem_bt),
                None => return self.fail("no target data for list allocation"),
            };
            let xv = self.gen_expr(&args[0])?;
            let (buf, len) = self.str_parts(rv);
            let int = self.backend.types.int;
            let len1 = self.backend.builder.build_int_add(len, int.const_int(1, false), "len1").unwrap();
            let bytes = self.backend.builder.build_int_mul(len1, int.const_int(elem_size, false), "bytes").unwrap();
            let malloc = self.backend.module.get_function("malloc").ok_or("malloc missing")?;
            let newbuf = self
                .backend
                .builder
                .build_direct_call(malloc, &[bytes.into()], "list.appended")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap()
                .into_pointer_value();
            let old_bytes = self.backend.builder.build_int_mul(len, int.const_int(elem_size, false), "oldbytes").unwrap();
            let _ = self.backend.builder.build_memcpy(newbuf, 1, buf, 1, old_bytes).unwrap();
            let slot = unsafe { self.backend.builder.build_in_bounds_gep(elem_bt, newbuf, &[len], "list.slot").unwrap() };
            self.backend.builder.build_store(slot, xv).unwrap();
            let mut agg = self.backend.types.xz_list.const_zero();
            agg = self.backend.builder.build_insert_value(agg, newbuf, 0, "l.ptr").unwrap().into_struct_value();
            agg = self.backend.builder.build_insert_value(agg, len1, 1, "l.len").unwrap().into_struct_value();
            return Ok(agg.into());
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
        if method == "at" {
            // Str.at(i) -> Result[Char, IndexError], bounds-checked.
            if args.len() != 1 {
                return self.fail("at takes one argument");
            }
            let iv = self.gen_expr(&args[0])?;
            let (buf, len) = self.str_parts(rv);
            let char_bt: BasicTypeEnum<'ctx> = self.backend.types.char.into();
            return self.bounds_checked_index(buf, len, iv.into_int_value(), char_bt);
        }
        if method == "to_bytes" {
            // Str and Bytes share the { ptr, len } layout: no copy at runtime.
            return Ok(rv);
        }
        if method == "to_upper" || method == "to_lower" {
            let (ptr, len) = self.str_parts(rv);
            let fname = if method == "to_upper" { "xz_str_to_upper" } else { "xz_str_to_lower" };
            let f = self.backend.module.get_function(fname).ok_or(fname)?;
            let call = self
                .backend
                .builder
                .build_direct_call(f, &[ptr.into(), len.into()], "case")
                .unwrap();
            return Ok(call.try_as_basic_value().basic().unwrap());
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
                        // A Bool field is stored as one byte (`i8`); codegen uses
                        // `i1` for Bool values, so narrow it back (docs/13-codegen.md).
                        let is_bool = self
                            .backend
                            .record_fields
                            .get(&rec_name)
                            .and_then(|fs| fs.get(i as usize))
                            .is_some_and(|k| matches!(k, Kind::Bool));
                        if is_bool {
                            Ok(self.coerce_to(self.backend.types.bool.into(), v))
                        } else {
                            Ok(v)
                        }
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
                    else_block: Some(Block { stmts: vec![Stmt { kind: StmtKind::Expr(Expr::If(inner)), span: ifx.then_block.span.clone() }], span: ifx.then_block.span.clone() }),
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
        let iv = self.gen_expr(iter)?;
        // `for i in n` is an Int range; `for x in xs` is a List. The iterable's
        // LLVM shape selects the lowering (both are checked by the type checker).
        if matches!(iv.get_type(), BasicTypeEnum::IntType(_)) {
            self.gen_for_range(name, iv.into_int_value(), block)
        } else {
            self.gen_for_list(name, iv, iter, block)
        }
    }

    /// `for i in n` — iterate the Int range 0..n (exclusive).
    fn gen_for_range(
        &mut self,
        name: &str,
        n: inkwell::values::IntValue<'ctx>,
        block: &ast::Block,
    ) -> GenResult<'ctx> {
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

    /// `for x in xs` — iterate a `List[T]` in order, binding the element value.
    fn gen_for_list(
        &mut self,
        name: &str,
        list_val: BasicValueEnum<'ctx>,
        iter: &Expr,
        block: &ast::Block,
    ) -> GenResult<'ctx> {
        let elem = match self.list_elem_kind(iter) {
            Some(k) => k,
            None => return self.fail("cannot determine the list element type for iteration; bind the list with a declared `List[T]` type"),
        };
        let elem_bt = self.backend.kind_to_llvm(&elem);
        let (buf, len) = self.str_parts(list_val);

        let fnv = self.cur_fn();
        let header = self.backend.context.append_basic_block(fnv, "for.header");
        let body_bb = self.backend.context.append_basic_block(fnv, "for.body");
        let incr = self.backend.context.append_basic_block(fnv, "for.incr");
        let after = self.backend.context.append_basic_block(fnv, "for.after");

        let i_ty = self.backend.types.int;
        let i_ptr = self.backend.builder.build_alloca(i_ty, "for.idx").unwrap();
        self.backend.builder.build_store(i_ptr, i_ty.const_int(0, false)).unwrap();
        self.backend.builder.build_unconditional_branch(header).unwrap();

        // header: idx < len ? body : after
        self.backend.builder.position_at_end(header);
        let i_cur = self.build_load(i_ty.into(), i_ptr, "for.idx");
        let cond = self
            .backend
            .builder
            .build_int_compare(IntPredicate::SLT, i_cur.into_int_value(), len, "for.cond")
            .unwrap();
        self.backend.builder.build_conditional_branch(cond, body_bb, after).unwrap();

        // body: load the element at idx and bind `name` to it
        self.backend.builder.position_at_end(body_bb);
        let slot = unsafe { self.backend.builder.build_in_bounds_gep(elem_bt, buf, &[i_cur.into_int_value()], "for.slot").unwrap() };
        let ev = self.backend.builder.build_load(elem_bt, slot, "for.elem").unwrap();
        let e_ptr = self.backend.builder.build_alloca(elem_bt, name).unwrap();
        self.backend.builder.build_store(e_ptr, ev).unwrap();
        let saved = self.scope.insert(name.to_string(), (e_ptr, elem_bt));
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

        // incr
        self.backend.builder.position_at_end(incr);
        let next = self
            .backend
            .builder
            .build_int_add(i_cur.into_int_value(), i_ty.const_int(1, false), "for.next")
            .unwrap();
        self.backend.builder.build_store(i_ptr, next).unwrap();
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
    let leak_owned = ret_kind.as_ref().map(|k| kind_carries_pointer(k, backend)).unwrap_or(false);

    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind,
        ret_llvm,
        str_count: 0,
        none_hint: None,
        list_hint: None,
        set_hint: None,
        map_hint: None,
        list_elems: HashMap::new(),
        set_elems: HashMap::new(),
        map_kvs: HashMap::new(),
        owns: HashMap::new(),
        leak_owned,
        loop_stack: Vec::new(),
        is_main: false,
        subst: HashMap::new(),
        mut_outs: Vec::new(),
    };
    cg.gen_body_common(&f.name, fv, &f.params, &f.body);
}

/// Generate the body of a generic specialization: like `gen_function`, but the
/// declared types are resolved through `subst` (type parameter -> concrete
/// kind). The concrete `FunctionValue` was declared by `LlvmBackend::specialize`.
pub fn gen_specialized(
    backend: &mut LlvmBackend<'static>,
    f: &ast::FuncDecl,
    subst: HashMap<String, Kind>,
    fv: FunctionValue<'static>,
) {
    let ret_kind = f.ret.as_ref().map(|t| kind_from_ast_subst(t, backend, &subst));
    let ret_llvm = fv.get_type().get_return_type();
    let leak_owned = ret_kind.as_ref().map(|k| kind_carries_pointer(k, backend)).unwrap_or(false);
    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind,
        ret_llvm,
        str_count: 0,
        none_hint: None,
        list_hint: None,
        set_hint: None,
        map_hint: None,
        list_elems: HashMap::new(),
        set_elems: HashMap::new(),
        map_kvs: HashMap::new(),
        owns: HashMap::new(),
        leak_owned,
        loop_stack: Vec::new(),
        is_main: false,
        subst,
        mut_outs: Vec::new(),
    };
    cg.gen_body_common(&f.name, fv, &f.params, &f.body);
}

/// Generate a `task` body. A task is a no-arg, void function spawned by `main`
/// in declaration order; its body may block on `recv` (docs/05-concurrency.md).
pub fn gen_task(backend: &mut LlvmBackend<'static>, t: &ast::TaskDecl) {
    let fv = match backend.functions.get(&t.name) {
        Some(fv) => *fv,
        None => return,
    };
    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind: None,
        ret_llvm: None,
        str_count: 0,
        none_hint: None,
        list_hint: None,
        set_hint: None,
        map_hint: None,
        list_elems: HashMap::new(),
        set_elems: HashMap::new(),
        map_kvs: HashMap::new(),
        owns: HashMap::new(),
        leak_owned: false,
        loop_stack: Vec::new(),
        is_main: false,
        subst: HashMap::new(),
        mut_outs: Vec::new(),
    };
    cg.gen_body_common(&t.name, fv, &[], &t.body);
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
    let leak_owned = ret_kind.as_ref().map(|k| kind_carries_pointer(k, backend)).unwrap_or(false);
    let main_ret: BasicTypeEnum<'static> = backend.context.i32_type().into();
    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind,
        ret_llvm: Some(main_ret),
        str_count: 0,
        none_hint: None,
        list_hint: None,
        set_hint: None,
        map_hint: None,
        list_elems: HashMap::new(),
        set_elems: HashMap::new(),
        map_kvs: HashMap::new(),
        owns: HashMap::new(),
        leak_owned,
        loop_stack: Vec::new(),
        is_main: true,
        subst: HashMap::new(),
        mut_outs: Vec::new(),
    };
    cg.gen_body_common(&f.name, fv, &f.params, &f.body);
}

impl<'ctx> Codegen<'_, 'ctx> {
    /// At the top of `main`, initialize the scheduler and spawn each declared
    /// task in source order (docs/05-concurrency.md rule 1). `main` keeps the
    /// token, so the new tasks do not run until it first blocks.
    fn emit_scheduler_start(&mut self) {
        if let Some(init) = self.backend.module.get_function("xz_sched_init") {
            let _ = self.backend.builder.build_direct_call(init, &[], "");
        }
        for tname in self.backend.tasks.clone() {
            let fv = match self.backend.functions.get(&tname).copied() {
                Some(fv) => fv,
                None => continue,
            };
            let fp = fv.as_global_value().as_pointer_value();
            if let Some(spawn) = self.backend.module.get_function("xz_task_spawn") {
                let _ = self.backend.builder.build_direct_call(spawn, &[fp.into()], "");
            }
        }
    }

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
                let ty;
                let alloca;
                if p.mutable {
                    // A `mut` parameter arrives as a pointer to the caller's
                    // storage. Copy the value in to a fresh alloca; the final
                    // value is copied back through the pointer at return
                    // (copy-in/copy-out — docs/04-memory-model.md).
                    let vk = self.kind_of(&p.ty);
                    ty = self.backend.kind_to_llvm(&vk);
                    alloca = self.backend.builder.build_alloca(ty, &p.name).unwrap();
                    let in_ptr = pv.into_pointer_value();
                    let v = self.build_load(ty, in_ptr, &format!("{}.in", p.name));
                    self.backend.builder.build_store(alloca, v).unwrap();
                    self.mut_outs.push((alloca, ty, in_ptr));
                } else {
                    ty = self.basic_type_of(pv);
                    alloca = self.backend.builder.build_alloca(ty, &p.name).unwrap();
                    self.backend.builder.build_store(alloca, pv).unwrap();
                }
                self.scope.insert(p.name.clone(), (alloca, ty));
                // Record a List parameter's element kind for iteration/indexing.
                if let Kind::List(t) = self.kind_of(&p.ty) {
                    self.list_elems.insert(p.name.clone(), *t);
                }
                // Record a Map parameter's key/value kinds for get/insert/columns.
                if let Kind::Map(k, v) = self.kind_of(&p.ty) {
                    self.map_kvs.insert(p.name.clone(), (*k, *v));
                }
                // Record a Set parameter's element kind for contains/insert/iteration.
                if let Kind::Set(t) = self.kind_of(&p.ty) {
                    self.set_elems.insert(p.name.clone(), *t);
                }
            }
        }
        if self.is_main && !self.backend.tasks.is_empty() {
            self.emit_scheduler_start();
        }
        let val = self.gen_block(body);
        // Release any Str buffers this function's bindings uniquely own
        // (unless the return type can escape Str storage, in which case the
        // return value may alias a binding and those stay live for the caller).
        self.free_owned_bindings();
        // Copy each `mut` parameter's final value back to the caller before
        // returning (copy-out). `early_return` already handled `?` paths.
        if !self.block_terminated() {
            self.write_back_mut_params();
        }
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
