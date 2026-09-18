use std::collections::{HashMap, HashSet};

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetData, TargetMachine};
use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::FunctionValue;
use inkwell::{AddressSpace, OptimizationLevel};

use crate::ast::{self, Item, Program, Type};
use crate::typecheck::Kind;

/// A concrete function signature with LLVM types, used to declare and call
/// functions. `llvm_ret` is None for a `Unit`-returning function (void).
pub struct LlvmSig<'ctx> {
    pub params: Vec<BasicTypeEnum<'ctx>>,
    pub ret: Option<BasicTypeEnum<'ctx>>,
    pub xz_ret: Option<Kind>,
    /// the param kinds (post-check), for call-argument type hints (e.g. an
    /// empty `[]` argument whose element type comes from the parameter).
    pub param_kinds: Vec<Kind>,
    /// per-parameter `mut` flag. A `mut` parameter lowers to a pointer to its
    /// value type (copy-in/copy-out — docs/04-memory-model.md), so call sites
    /// pass an address instead of a value.
    pub param_muts: Vec<bool>,
}

/// The lowered LLVM type of an Xz `Kind`. Only `BasicTypeEnum` (i.e. a value
/// type that can be a struct field, alloca element, or fn param). `Unit` maps
/// to an empty struct for values; `void` is handled at the function level.
pub struct TypeMap<'ctx> {
    pub int: inkwell::types::IntType<'ctx>,
    pub float: inkwell::types::FloatType<'ctx>,
    pub ptr: inkwell::types::PointerType<'ctx>,
    pub bool: inkwell::types::IntType<'ctx>,
    pub char: inkwell::types::IntType<'ctx>,
    pub xz_str: StructType<'ctx>,
    pub xz_bytes: StructType<'ctx>,
    pub xz_list: StructType<'ctx>,
    pub xz_map: StructType<'ctx>,
    pub unit: StructType<'ctx>,
}

impl<'ctx> TypeMap<'ctx> {
    pub fn new(context: &'ctx Context) -> TypeMap<'ctx> {
        let i64 = context.i64_type();
        let i8 = context.i8_type();
        let ptr = context.ptr_type(AddressSpace::default());
        let unit = context.struct_type(&[], false);
        let xz_str = context.struct_type(&[ptr.into(), i64.into()], false);
        let xz_bytes = context.struct_type(&[ptr.into(), i64.into()], false);
        let xz_list = context.struct_type(&[ptr.into(), i64.into()], false);
        // Map[K, V] is three words: a key buffer, a value buffer, and the entry
        // count. Entries are immutable, so copies share both buffers.
        let xz_map = context.struct_type(&[ptr.into(), ptr.into(), i64.into()], false);
        TypeMap {
            int: i64,
            float: context.f64_type(),
            ptr,
            bool: context.bool_type(),
            char: i8,
            xz_str,
            xz_bytes,
            xz_list,
            xz_map,
            unit,
        }
    }
}

/// The full backend state: the LLVM context/module/builder, the type map, and
/// the tables that map Xz names to LLVM entities.
pub struct LlvmBackend<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,
    pub types: TypeMap<'ctx>,
    /// record name -> LLVM struct type (declared on first use)
    pub record_types: HashMap<String, StructType<'ctx>>,
    /// record name -> ordered field types (Kind) in declaration order
    pub record_fields: HashMap<String, Vec<Kind>>,
    /// record name -> (field name -> index) for field access
    pub record_field_names: HashMap<String, HashMap<String, u32>>,
    /// enum name -> ordered variant names
    pub enum_variants: HashMap<String, Vec<String>>,
    /// variant name -> (enum name, index, field kinds)
    pub variants: HashMap<String, (String, u32, Vec<Kind>)>,
    /// function name -> FunctionValue (defined in this module)
    pub functions: HashMap<String, FunctionValue<'ctx>>,
    /// function name -> signature (params + ret), used by call sites
    pub sigs: HashMap<String, LlvmSig<'ctx>>,
    /// a shared runtime error string for codegen (not front-end diagnostics)
    pub error: Option<String>,
    /// host target data, used to compute aggregate sizes (enum box allocation)
    /// independently of the builder's (empty) data layout.
    pub target_data: Option<TargetData>,
    /// generic function name -> type-parameter names, in index order
    pub generic_params: HashMap<String, Vec<String>>,
    /// generic function name -> its AST, used to generate specializations
    pub func_asts: HashMap<String, ast::FuncDecl>,
    /// mangled specialization name -> the generated FunctionValue
    pub monos: HashMap<String, FunctionValue<'ctx>>,
    /// specializations whose bodies still need generating (name, substitution,
    /// function); processed after the concrete functions are lowered.
    pub pending_monos: Vec<(String, HashMap<String, Kind>, FunctionValue<'ctx>)>,
    /// channel name -> compiler-assigned id (the runtime keys channel state by
    /// it). Populated from `chan` declarations in `compile_impl`.
    pub channel_ids: HashMap<String, u64>,
    /// channel name -> its payload `Kind`, so `let x <- recv(ch)` and an
    /// `Expr::Recv` can materialize the right slot type (codegen carries no
    /// types otherwise).
    pub channel_payloads: HashMap<String, Kind>,
    /// task declaration names, in source order; `main` spawns them in this order
    /// (docs/05-concurrency.md deterministic scheduling rule 1).
    pub tasks: Vec<String>,
    /// names of functions marked `@export`: the only symbols a shared library
    /// makes public (docs/10-ffi-interop.md). Used by [`hide_runtime_symbols`]
    /// to internalize every other definition without a fixed `xz_*` list.
    pub exports: HashSet<String>,
    /// number of synthetic completion channels handed out to `await` sites.
    /// Their ids start after the declared channels (see `next_channel_id`).
    pub await_chan_count: u64,
}

impl<'ctx> LlvmBackend<'ctx> {
    pub fn new(context: &'ctx Context) -> LlvmBackend<'ctx> {
        let module = context.create_module("xz_program");
        let builder = context.create_builder();
        let types = TypeMap::new(&context);
        LlvmBackend {
            context,
            module,
            builder,
            types,
            record_types: HashMap::new(),
            record_fields: HashMap::new(),
            record_field_names: HashMap::new(),
            enum_variants: HashMap::new(),
            variants: HashMap::new(),
            functions: HashMap::new(),
            sigs: HashMap::new(),
            error: None,
            target_data: None,
            generic_params: HashMap::new(),
            func_asts: HashMap::new(),
            monos: HashMap::new(),
            pending_monos: Vec::new(),
            channel_ids: HashMap::new(),
            channel_payloads: HashMap::new(),
            tasks: Vec::new(),
            exports: HashSet::new(),
            await_chan_count: 0,
        }
    }

    pub fn fail(&mut self, msg: &str) {
        if self.error.is_none() {
            self.error = Some(msg.to_string());
        }
    }

    /// Reserve a runtime channel id for a synthetic `await` completion channel.
    /// Declared channels take ids `0..channel_ids.len()`; await channels follow,
    /// so no id collides with a user-visible `chan` (docs/13-codegen.md).
    pub fn next_channel_id(&mut self) -> u64 {
        let id = self.channel_ids.len() as u64 + self.await_chan_count;
        self.await_chan_count += 1;
        id
    }

    /// Convert a `Kind` to an LLVM basic type (value type). `Unit` becomes the
    /// empty struct; `Kind::Never`/`Unknown` are not expected post-check.
    pub fn kind_to_llvm(&self, kind: &Kind) -> BasicTypeEnum<'ctx> {
        match kind {
            Kind::Bool => self.types.bool.into(),
            Kind::Int | Kind::Usize => self.types.int.into(),
            Kind::Float => self.types.float.into(),
            Kind::Char => self.types.char.into(),
            Kind::Str => self.types.xz_str.into(),
            Kind::Bytes => self.types.xz_bytes.into(),
            Kind::Unit => self.types.unit.into(),
            Kind::Ptr => self.types.ptr.into(),
            Kind::Record(name) => self.record_types.get(name).copied().unwrap_or(self.types.unit).into(),
            Kind::Enum(name) => self.enum_struct(name).into(),
            Kind::Option(t) | Kind::Result(t, _) => {
                let payload = self.kind_to_llvm(t);
                self.context.struct_type(&[payload.into(), self.types.bool.into()], false).into()
            }
            // List[T] is { ptr, i64 } regardless of T: elements live in a
            // separately allocated buffer (opaque pointers erase T in IR).
            Kind::List(_) => self.types.xz_list.into(),
            // Map[K, V] is { K*, V*, i64 } regardless of K/V: entries live in
            // separately allocated key/value buffers (opaque pointers erase K/V).
            Kind::Map(_, _) => self.types.xz_map.into(),
            // Set[T] is also { ptr, i64 }: the element buffer + count. Same
            // shape as List[T], so it lowers to the same LLVM type; the front
            // end keeps Set and List apart (opaque pointers erase T).
            Kind::Set(_) => self.types.xz_list.into(),
            Kind::Err | Kind::ErrUnion(_) => self.types.unit.into(),
            // A channel name lowers to its integer id; `send`/`recv` pass the id
            // to the scheduler runtime (docs/13-codegen.md § Concurrency).
            Kind::Chan(_) => self.types.int.into(),
            Kind::TypeVar(_) | Kind::Unknown | Kind::Never => self.types.unit.into(),
        }
    }

    /// Convert a `Kind` to the LLVM type used when it is *stored in memory*.
    /// Only `Bool` differs from [`kind_to_llvm`]: a record/`@cstruct` field is
    /// one byte (`i8`), matching C's `bool` so the struct layout is identical
    /// to C; the register ABI still uses `i1`
    /// (docs/10-ffi-interop.md, docs/13-codegen.md).
    pub fn kind_to_llvm_mem(&self, kind: &Kind) -> BasicTypeEnum<'ctx> {
        match kind {
            Kind::Bool => self.context.i8_type().into(),
            _ => self.kind_to_llvm(kind),
        }
    }

    /// The LLVM enum representation: `struct { i8*, i32 }` (boxed payload + tag).
    pub fn enum_struct(&self, _name: &str) -> StructType<'ctx> {
        self.context.struct_type(&[self.types.ptr.into(), self.context.i32_type().into()], false)
    }

    /// The LLVM function return type for a `Kind`. `Unit` returns void.
    fn ret_type(&self, kind: &Option<Kind>) -> Option<BasicTypeEnum<'ctx>> {
        match kind {
            Some(Kind::Unit) => None,
            Some(k) => Some(self.kind_to_llvm(k)),
            None => None,
        }
    }

    /// Declare (or fetch) the LLVM struct type for a record, recursively
    /// resolving its fields. Returns the struct type.
    pub fn declare_record(&mut self, name: &str, fields: &[ast::Field]) -> StructType<'ctx> {
        if let Some(st) = self.record_types.get(name) {
            return *st;
        }
        // First pass: a named (opaque) struct type so recursive records work.
        let opaque = self.context.opaque_struct_type(name);
        self.record_types.insert(name.to_string(), opaque);

        let field_kinds: Vec<Kind> = fields.iter().map(|f| kind_from_ast(&f.ty, self)).collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = field_kinds.iter().map(|k| self.kind_to_llvm_mem(k)).collect();
        opaque.set_body(&field_tys, false);

        self.record_fields.insert(name.to_string(), field_kinds);
        let mut name_index = HashMap::new();
        for (i, f) in fields.iter().enumerate() {
            name_index.insert(f.name.clone(), i as u32);
        }
        self.record_field_names.insert(name.to_string(), name_index);
        opaque
    }

    /// The index of a record field by name, for `extract_value`.
    pub fn record_field_index(&self, record: &str, field: &str) -> Option<u32> {
        self.record_field_names.get(record).and_then(|m| m.get(field)).copied()
    }

    /// Declare an enum's variants. Each variant is an index; no LLVM struct is
    /// needed per-variant beyond the boxed payload (which is heap-allocated).
    pub fn declare_enum(&mut self, name: &str, variants: &[ast::Variant]) {
        let vnames: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
        let mut variant_map = HashMap::new();
        for (i, v) in variants.iter().enumerate() {
            let fk: Vec<Kind> = v.fields.iter().map(|f| kind_from_ast(&f.ty, self)).collect();
            variant_map.insert(v.name.clone(), (name.to_string(), i as u32, fk));
        }
        self.enum_variants.insert(name.to_string(), vnames);
        for (k, v) in variant_map {
            self.variants.insert(k, v);
        }
    }

    /// Declare a function with a C ABI. `explicit_ret` overrides the declared
    /// return (used by `main`, which returns `Result[Unit, Err]` but is called
    /// as `void`).
    pub fn declare_function(
        &mut self,
        name: &str,
        param_kinds: &[Kind],
        param_muts: &[bool],
        ret: &Option<Kind>,
        explicit_ret: Option<BasicTypeEnum<'ctx>>,
    ) -> FunctionValue<'ctx> {
        // A `mut` parameter crosses the ABI as a pointer to its value type: the
        // callee copies the pointed-to value in at entry and writes the final
        // value back before returning (copy-in/copy-out, docs/04-memory-model.md).
        let param_tys: Vec<BasicTypeEnum<'ctx>> = param_kinds
            .iter()
            .enumerate()
            .map(|(i, k)| {
                if param_muts.get(i).copied().unwrap_or(false) {
                    self.types.ptr.into()
                } else {
                    self.kind_to_llvm(k)
                }
            })
            .collect();
        let ret_ty = match explicit_ret {
            Some(t) => Some(t),
            None => self.ret_type(ret),
        };
        let sig = LlvmSig {
            params: param_tys.iter().map(|t| *t).collect(),
            ret: ret_ty,
            xz_ret: ret.clone(),
            param_kinds: param_kinds.to_vec(),
            param_muts: param_muts.to_vec(),
        };
        self.sigs.insert(name.to_string(), sig);

        let fn_type = self.make_fn_type(&param_tys, ret_ty);
        let fv = self.module.add_function(name, fn_type, None);
        self.functions.insert(name.to_string(), fv);
        fv
    }

    /// Build a C-ABI function type from LLVM param/return types.
    pub fn make_fn_type(
        &self,
        params: &[BasicTypeEnum<'ctx>],
        ret: Option<BasicTypeEnum<'ctx>>,
    ) -> inkwell::types::FunctionType<'ctx> {
        let pm: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = params.iter().map(|t| (*t).into()).collect();
        match ret {
            Some(rt) => rt.fn_type(&pm, false),
            None => self.context.void_type().fn_type(&pm, false),
        }
    }

    /// Declare an external (host or libm) function without a body. The runtime
    /// maps it via `add_global_mapping`.
    pub fn declare_extern(
        &mut self,
        name: &str,
        param_tys: &[BasicTypeEnum<'ctx>],
        ret: Option<BasicTypeEnum<'ctx>>,
    ) -> FunctionValue<'ctx> {
        let fn_type = self.make_fn_type(param_tys, ret);
        self.module.add_function(name, fn_type, None)
    }

    /// Create (or reuse) a concrete specialization of a generic function for
    /// the given type arguments. Declares the specialized LLVM function and
    /// queues its body for generation after the concrete functions are
    /// lowered. Returns the mangled name.
    pub fn specialize(&mut self, name: &str, type_args: &[Kind]) -> Result<String, String> {
        let tparams = self
            .generic_params
            .get(name)
            .cloned()
            .ok_or_else(|| format!("'{}' is not a generic function", name))?;
        let mut mangled = name.to_string();
        for t in type_args {
            mangled.push('$');
            mangled.push_str(&mangle_type_tag(t));
        }
        if self.monos.contains_key(&mangled) {
            return Ok(mangled);
        }
        let f = self
            .func_asts
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing AST for generic function '{}'", name))?;
        let subst: HashMap<String, Kind> =
            tparams.iter().cloned().zip(type_args.iter().cloned()).collect();
        let param_kinds: Vec<Kind> =
            f.params.iter().map(|p| kind_from_ast_subst(&p.ty, self, &subst)).collect();
        let param_muts: Vec<bool> = f.params.iter().map(|p| p.mutable).collect();
        let ret = f.ret.as_ref().map(|t| kind_from_ast_subst(t, self, &subst));
        let fv = self.declare_function(&mangled, &param_kinds, &param_muts, &ret, None);
        fv.set_linkage(Linkage::Internal);
        self.monos.insert(mangled.clone(), fv);
        self.pending_monos.push((name.to_string(), subst, fv));
        Ok(mangled)
    }
}

/// Convert an AST type to a `Kind`. The front end already validated it, so
/// this is a faithful re-derivation. Type variables become `Unknown`.
pub fn kind_from_ast(ty: &Type, backend: &LlvmBackend<'_>) -> Kind {
    match ty {
        Type::Named(name, args) => kind_from_ast_named(name, args, backend),
        Type::NamedPlain(name) => kind_from_ast_named(name, &vec![], backend),
        Type::Union(members) => {
            let kinds: Vec<Kind> = members.iter().map(|m| kind_from_ast(m, backend)).collect();
            Kind::ErrUnion(kinds)
        }
    }
}

fn kind_from_ast_named(name: &str, args: &[Type], backend: &LlvmBackend<'_>) -> Kind {
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
            let inner = kind_from_ast(&args[0], backend);
            Kind::Option(Box::new(inner))
        }
        "Result" => {
            let t = kind_from_ast(&args[0], backend);
            let e = kind_from_ast(&args[1], backend);
            Kind::Result(Box::new(t), Box::new(e))
        }
        "List" => {
            let inner = kind_from_ast(&args[0], backend);
            Kind::List(Box::new(inner))
        }
        "Map" => {
            let key = kind_from_ast(&args[0], backend);
            let val = kind_from_ast(&args[1], backend);
            Kind::Map(Box::new(key), Box::new(val))
        }
        "Set" => {
            let inner = kind_from_ast(&args[0], backend);
            Kind::Set(Box::new(inner))
        }
        "Err" => Kind::Err,
        _ => {
            // record or enum or (unlikely) type variable
            if backend.record_types.contains_key(name) {
                Kind::Record(name.to_string())
            } else if backend.enum_variants.contains_key(name) {
                Kind::Enum(name.to_string())
            } else {
                Kind::Unknown
            }
        }
    }
}

/// Like `kind_from_ast`, but maps a type-parameter name to a concrete `Kind`
/// from `subst` (used to lower a generic function's specialized body).
pub fn kind_from_ast_subst(ty: &Type, backend: &LlvmBackend<'_>, subst: &HashMap<String, Kind>) -> Kind {
    match ty {
        Type::NamedPlain(n) => kind_named_subst(n, &[], backend, subst),
        Type::Named(n, args) => kind_named_subst(n, args, backend, subst),
        Type::Union(ms) => Kind::ErrUnion(ms.iter().map(|m| kind_from_ast_subst(m, backend, subst)).collect()),
    }
}

fn kind_named_subst(name: &str, args: &[Type], backend: &LlvmBackend<'_>, subst: &HashMap<String, Kind>) -> Kind {
    if let Some(k) = subst.get(name) {
        return k.clone();
    }
    match name {
        "Option" => Kind::Option(Box::new(kind_from_ast_subst(&args[0], backend, subst))),
        "Result" => Kind::Result(
            Box::new(kind_from_ast_subst(&args[0], backend, subst)),
            Box::new(kind_from_ast_subst(&args[1], backend, subst)),
        ),
        "List" => Kind::List(Box::new(kind_from_ast_subst(&args[0], backend, subst))),
        "Map" => Kind::Map(
            Box::new(kind_from_ast_subst(&args[0], backend, subst)),
            Box::new(kind_from_ast_subst(&args[1], backend, subst)),
        ),
        "Chan" => Kind::Chan(Box::new(kind_from_ast_subst(&args[0], backend, subst))),
        "Set" => Kind::Set(Box::new(kind_from_ast_subst(&args[0], backend, subst))),
        _ => kind_from_ast_named(name, args, backend),
    }
}

/// Canonical `Kind` for an LLVM value type, used to monomorphize a generic
/// function from its concrete argument types. Structurally-equal LLVM types
/// (e.g. `Str`/`List[T]`, both `{ ptr, i64 }`) map to the same canonical kind,
/// which is harmless because the specialized LLVM signature is identical.
pub fn kind_from_llvm(backend: &LlvmBackend<'_>, t: BasicTypeEnum<'_>) -> Kind {
    match t {
        BasicTypeEnum::IntType(it) => match it.get_bit_width() {
            1 => Kind::Bool,
            8 => Kind::Char,
            _ => Kind::Int,
        },
        BasicTypeEnum::FloatType(_) => Kind::Float,
        BasicTypeEnum::PointerType(_) => Kind::Ptr,
        BasicTypeEnum::StructType(st) => {
            if st == backend.types.xz_str {
                Kind::Str
            } else if st.count_fields() == 2 && st.get_field_type_at_index(1) == Some(backend.types.bool.into()) {
                let payload = st.get_field_type_at_index(0).unwrap();
                Kind::Option(Box::new(kind_from_llvm(backend, payload)))
            } else if st.count_fields() == 0 {
                Kind::Unit
            } else {
                Kind::Unknown
            }
        }
        _ => Kind::Unknown,
    }
}

/// A short, LLVM-safe mangled type tag for specialization names.
pub fn mangle_type_tag(k: &Kind) -> String {
    match k {
        Kind::Bool => "b".into(),
        Kind::Int => "i".into(),
        Kind::Usize => "u".into(),
        Kind::Float => "f".into(),
        Kind::Char => "c".into(),
        Kind::Str => "s".into(),
        Kind::Bytes => "y".into(),
        Kind::Unit => "v".into(),
        Kind::Ptr => "p".into(),
        Kind::Option(t) => format!("O{}", mangle_type_tag(t)),
        Kind::Result(t, _) => format!("R{}", mangle_type_tag(t)),
        Kind::List(t) => format!("L{}", mangle_type_tag(t)),
        Kind::Set(t) => format!("S{}", mangle_type_tag(t)),
        Kind::Err => "e".into(),
        Kind::Record(n) => format!("r{}", n),
        Kind::Enum(n) => format!("E{}", n),
        _ => "x".into(),
    }
}

/// The entry point: declare every top-level type and function, then lower the
/// body of each function. `main` is declared with a `void` ABI regardless of
/// its declared `Result[Unit, Err]` return, since the runtime calls it with no
/// args and ignores the result.
pub fn compile(program: &Program) -> Result<LlvmBackend<'static>, String> {
    compile_impl(program, false)
}

/// Like [`compile`], but for the shared-library path: `main` (if present) is
/// internal (a library has no entry point) and functions marked `@export`
/// keep external linkage so they survive `globaldce` into the symbol table
/// (docs/10-ffi-interop.md).
pub fn compile_shared(program: &Program) -> Result<LlvmBackend<'static>, String> {
    compile_impl(program, true)
}

fn compile_impl(program: &Program, shared: bool) -> Result<LlvmBackend<'static>, String> {
    // The LLVM context lives for the whole compiler process. Leaking it gives
    // a 'static reference, so blocks/values don't borrow the backend (which is
    // what lets codegen hold block handles while mutating the symbol table).
    let context: &'static Context = Box::leak(Box::new(Context::create()));
    let mut backend = LlvmBackend::new(context);

    // Pin the target triple and data layout before lowering. Without a data
    // layout, LLVM assumes 32-bit pointers, so `build_malloc` (enum boxes)
    // emits `malloc(i32)` while the native runtime expects `malloc(i64)` — the
    // optimizer then splits them into `malloc`/`malloc.1` and linking fails.
    // Setting the host layout makes every size a pointer-sized i64.
    let _ = Target::initialize_native(&InitializationConfig::default());
    let triple = TargetMachine::get_default_triple();
    backend.module.set_triple(&triple);
    if let Ok(target) = Target::from_triple(&triple) {
        if let Some(machine) = target.create_target_machine(
            &triple,
            "",
            "",
            OptimizationLevel::Aggressive,
            RelocMode::Default,
            CodeModel::Default,
        ) {
            backend.module.set_data_layout(&machine.get_target_data().get_data_layout());
            backend.target_data = Some(machine.get_target_data());
        }
    }

    // Pass 1: declare records and enums (types first, so function sigs resolve).
    for item in &program.items {
        match item {
            Item::Record(rec) => {
                backend.declare_record(&rec.name, &rec.fields);
            }
            Item::Enum(en) => {
                backend.declare_enum(&en.name, &en.variants);
            }
            _ => {}
        }
    }

    // Pass 2: declare functions and externs.
    for item in &program.items {
        match item {
            Item::Func(f) => {
                // Generic functions are not declared directly; a concrete
                // specialization is created on demand at each call site
                // (monomorphization). Record the AST and its type parameters.
                if !f.type_params.is_empty() {
                    let tps: Vec<String> = f.type_params.iter().map(|tp| tp.name.clone()).collect();
                    backend.generic_params.insert(f.name.clone(), tps);
                    backend.func_asts.insert(f.name.clone(), f.clone());
                    continue;
                }
                let param_kinds: Vec<Kind> = f.params.iter().map(|p| kind_from_ast(&p.ty, &backend)).collect();
                let param_muts: Vec<bool> = f.params.iter().map(|p| p.mutable).collect();
                // `main` is the C entry point: the runtime calls it with no
                // args and the linker/crt expects `int main()`, so it is
                // declared returning i32 regardless of its declared
                // `Result[Unit, Err]` return (gen_main emits `ret i32 0`).
                if f.name == "main" {
                    let _ = backend.declare_function(
                        &f.name,
                        &param_kinds,
                        &param_muts,
                        &None,
                        Some(backend.context.i32_type().into()),
                    );
                    // A shared library has no entry point; keep `main` internal
                    // so it does not leak into the exported symbol table.
                    if shared {
                        if let Some(fv) = backend.functions.get(&f.name) {
                            fv.set_linkage(Linkage::Internal);
                        }
                    }
                } else {
                    let ret = f.ret.as_ref().map(|t| kind_from_ast(t, &backend));
                    let fv = backend.declare_function(&f.name, &param_kinds, &param_muts, &ret, None);
                    // Program functions are module-internal so the optimizer's
                    // global DCE can drop them when they become dead (e.g. after
                    // inlining). An `@export` function is the library's public
                    // surface, so it keeps external linkage.
                    if f.exported {
                        fv.set_linkage(Linkage::External);
                        backend.exports.insert(f.name.clone());
                    } else {
                        fv.set_linkage(Linkage::Internal);
                    }
                }
            }
            Item::Extern(e) => {
                let param_kinds: Vec<Kind> = e.params.iter().map(|p| kind_from_ast(&p.ty, &backend)).collect();
                let ret = e.ret.as_ref().map(|t| kind_from_ast(t, &backend));
                let param_tys: Vec<BasicTypeEnum> = param_kinds.iter().map(|k| backend.kind_to_llvm(k)).collect();
                let ret_ty = match &ret {
                    Some(Kind::Unit) => None,
                    Some(k) => Some(backend.kind_to_llvm(k)),
                    None => None,
                };
                let fv = backend.declare_extern(&e.name, &param_tys, ret_ty);
                backend.functions.insert(e.name.clone(), fv);
                backend.sigs.insert(
                    e.name.clone(),
                    LlvmSig {
                        params: param_tys,
                        ret: ret_ty,
                        xz_ret: ret,
                        param_kinds: param_kinds.clone(),
                        param_muts: vec![false; param_kinds.len()],
                    },
                );
            }
            Item::Chan(c) => {
                let id = backend.channel_ids.len() as u64;
                backend.channel_ids.insert(c.name.clone(), id);
                let payload = kind_from_ast(&c.payload, &backend);
                backend.channel_payloads.insert(c.name.clone(), payload);
            }
            Item::Task(t) => {
                // A task lowers to a no-arg, void function. It is declared
                // internal (a library does not export tasks); `main` takes its
                // address for `xz_task_spawn`, so the optimizer keeps it.
                let fv = backend.declare_function(&t.name, &[], &[], &None, None);
                fv.set_linkage(Linkage::Internal);
                backend.tasks.push(t.name.clone());
            }
            _ => {}
        }
    }

    // Host runtime functions: provided by runtime.rs, resolved via
    // add_global_mapping at run time (see docs/13-codegen.md).
    {
        let i64 = backend.types.int;
        let i8 = backend.types.char;
        let ptr = backend.types.ptr;
        let f64 = backend.types.float;
        let xstr = backend.types.xz_str;
        let i1 = backend.types.bool;
        // print(ptr, len) -> void
        backend.declare_extern("xz_print", &[ptr.into(), i64.into()], None);
        // str_free(ptr, len) -> void (registry-guarded; see runtime.rs)
        backend.declare_extern("xz_str_free", &[ptr.into(), i64.into()], None);
        // malloc(size) -> Ptr. Declared by us (not LLVM's `build_malloc`, whose
        // builder has an empty data layout and would emit an i32-sized malloc
        // conflicting with the i64 size used here). Used for enum boxes.
        backend.declare_extern("malloc", &[i64.into()], Some(ptr.into()));
        // concat(aptr, alen, bptr, blen) -> XzStr
        backend.declare_extern("xz_concat", &[ptr.into(), i64.into(), ptr.into(), i64.into()], Some(xstr.into()));
        // scalar -> XzStr
        backend.declare_extern("xz_i64_to_str", &[i64.into()], Some(xstr.into()));
        backend.declare_extern("xz_f64_to_str", &[f64.into()], Some(xstr.into()));
        backend.declare_extern("xz_bool_to_str", &[i1.into()], Some(xstr.into()));
        backend.declare_extern("xz_char_to_str", &[i8.into()], Some(xstr.into()));
        // case conversion (Str.to_upper / Str.to_lower) -> fresh XzStr
        backend.declare_extern("xz_str_to_upper", &[ptr.into(), i64.into()], Some(xstr.into()));
        backend.declare_extern("xz_str_to_lower", &[ptr.into(), i64.into()], Some(xstr.into()));
        // Str key equality for Map[Str, V] lookup/insert (JIT host in runtime.rs).
        backend.declare_extern("xz_str_eq", &[ptr.into(), i64.into(), ptr.into(), i64.into()], Some(i1.into()));
        // read_file(path_ptr, path_len, out) -> i1: reads the file and writes a
        // fresh {ptr,len} Str payload through `out` (JIT host in runtime.rs;
        // native body in emit_native_runtime). See docs/13-codegen.md.
        backend.declare_extern("xz_read_file", &[ptr.into(), i64.into(), ptr.into()], Some(i1.into()));
        // now() / monotonic() -> f64 seconds. JIT host in runtime.rs; native
        // body (gettimeofday / clock_gettime) in emit_native_runtime.
        backend.declare_extern("xz_time_now", &[], Some(f64.into()));
        backend.declare_extern("xz_time_monotonic", &[], Some(f64.into()));
        // Phase 6 scheduler + channels (JIT host in runtime.rs; see
        // docs/13-codegen.md § Concurrency). `id` is the compiler-assigned
        // channel id; the value is copied through a `(ptr, size)` byte range.
        backend.declare_extern("xz_sched_init", &[], None);
        backend.declare_extern("xz_task_spawn", &[ptr.into()], None);
        backend.declare_extern("xz_task_spawn_arg", &[ptr.into(), ptr.into()], None);
        backend.declare_extern("xz_chan_send", &[i64.into(), ptr.into(), i64.into()], None);
        backend.declare_extern("xz_chan_recv", &[i64.into(), ptr.into(), i64.into()], None);
    }

    // Map the stdlib function `approx_sqrt` and `print` to their host ABI.
    // `print` is special-cased in codegen; `approx_sqrt` is enabled by the
    // call site's `llvm.sqrt.f64` intrinsic (gen_named_call), so no extern is
    // declared — LLVM lowers the intrinsic natively at codegen time.
    backend
        .sigs
        .entry("approx_sqrt".to_string())
        .or_insert_with(|| LlvmSig {
            params: vec![backend.types.float.into()],
            ret: Some(backend.types.float.into()),
            xz_ret: Some(Kind::Float),
            param_kinds: vec![Kind::Float],
            param_muts: vec![false],
        });

    // Pass 3: lower bodies.
    for item in &program.items {
        match item {
            Item::Func(f) => {
                if f.name == "main" {
                    crate::backend::codegen::gen_main(&mut backend, f);
                } else {
                    crate::backend::codegen::gen_function(&mut backend, f);
                }
            }
            Item::Task(t) => {
                crate::backend::codegen::gen_task(&mut backend, t);
            }
            _ => {}
        }
    }

    // Generate the bodies of any generic specializations requested during pass
    // 3 (and, transitively, by later specializations).
    while let Some((gname, subst, fv)) = backend.pending_monos.pop() {
        if let Some(f) = backend.func_asts.get(&gname).cloned() {
            crate::backend::codegen::gen_specialized(&mut backend, &f, subst, fv);
        }
    }

    if let Some(e) = &backend.error {
        return Err(e.clone());
    }
    if backend.module.verify().is_err() {
        let detail = backend.module.verify();
        return Err(match detail {
            Err(s) => format!("LLVM module verification failed: {}", s.to_str().unwrap_or("").to_string()),
            Ok(_) => "LLVM module verification failed".to_string(),
        });
    }

    Ok(backend)
}

/// Run the LLVM optimization pipeline over a compiled module: the default pass
/// set at O3 (inlining, mem2reg/SROA, GVN, instcombine, DCE, tail-call
/// elimination, constant folding) followed by aggressive codegen in the JIT
/// engine. This is what turns the mechanical Phase 4 lowering into native
/// speed — an unoptimized LLVM backend is slower than plain C (see
/// docs/13-codegen.md). `xz build` emits the unoptimized IR; `xz run` and the
/// Phase 5 native path apply this pipeline first.
pub fn optimize(module: &Module<'_>) -> Result<(), String> {
    let _ = Target::initialize_native(&InitializationConfig::default())?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| {
        e.to_str().map(|s| s.to_string()).unwrap_or_else(|_| "unknown host target".to_string())
    })?;
    let machine = match target.create_target_machine(
        &triple,
        "",
        "",
        OptimizationLevel::Aggressive,
        RelocMode::Default,
        CodeModel::Default,
    ) {
        Some(m) => m,
        None => return Err("cannot create a target machine for the host triple".to_string()),
    };
    let options = PassBuilderOptions::create();
    options.set_verify_each(true);
    // Program functions are declared with internal linkage (compile()), so the
    // pipeline's global DCE drops any that become dead after inlining; host
    // declarations are left alone.
    module
        .run_passes("default<O3>,globaldce", &machine, options)
        .map_err(|e| {
            e.to_str().map(|s| s.to_string()).unwrap_or_else(|_| "LLVM pass error".to_string())
        })?;
    Ok(())
}

/// Enforce the shared library's visibility policy: a function is public iff it
/// is marked `@export`, so every other definition (`xz_*` runtime bodies,
/// private helpers, `main`) is internalized. Deriving the set from the module
/// and `backend.exports` instead of a fixed `xz_*` list means a newly added
/// runtime function cannot be forgotten. The JIT resolves all of them by name
/// via `add_global_mapping`; only the `@export` functions are a library's
/// public surface.
pub fn hide_runtime_symbols(backend: &LlvmBackend<'static>) {
    for f in backend.module.get_functions() {
        if f.get_first_basic_block().is_none() {
            continue;
        }
        if !backend.exports.contains(f.get_name().to_str().unwrap_or("")) {
            f.set_linkage(Linkage::Internal);
        }
    }
}

/// Emit the native runtime: define the `xz_*` host functions (currently only
/// declared as externs for the JIT) as real IR bodies that call libc. This is
/// what makes `xz build` output linkable into a standalone executable with
/// `llc` + `ld` (no Rust runtime needed). The JIT path is unaffected — it
/// still binds the Rust host functions via `add_global_mapping`, which
/// overrides these definitions.
pub fn emit_native_runtime(backend: &mut LlvmBackend<'static>) -> Result<(), String> {
    let i8 = backend.types.char;
    let i32 = backend.context.i32_type();
    let i64 = backend.types.int;
    let ptr = backend.types.ptr;
    let void = backend.context.void_type();

    // libc declarations. `malloc` may already exist (LLVM's `build_malloc`
    // inserts one for enum boxes during codegen); reuse that declaration
    // instead of adding a second one (which LLVM would rename to `malloc.1`,
    // then fail to resolve at link time).
    let malloc = match backend.module.get_function("malloc") {
        Some(f) => f,
        None => backend.module.add_function("malloc", ptr.fn_type(&[i64.into()], false), None),
    };
    let free = match backend.module.get_function("free") {
        Some(f) => f,
        None => backend.module.add_function("free", void.fn_type(&[ptr.into()], false), None),
    };
    let write = backend.module.add_function(
        "write",
        i64.fn_type(&[i32.into(), ptr.into(), i64.into()], false),
        None,
    );
    let snprintf = backend
        .module
        .add_function("snprintf", i32.fn_type(&[ptr.into(), i64.into(), ptr.into()], true), None);

    // Helper closures would fight the borrow checker; inline the bodies.
    let build_str_ret = |backend: &mut LlvmBackend<'static>, _f: FunctionValue<'static>, buf: inkwell::values::PointerValue<'static>, len: inkwell::values::IntValue<'static>| {
        let agg = backend.types.xz_str.const_zero();
        let agg = backend.builder.build_insert_value(agg, buf, 0, "s.ptr").unwrap().into_struct_value();
        let agg = backend.builder.build_insert_value(agg, len, 1, "s.len").unwrap().into_struct_value();
        backend.builder.build_return(Some(&agg)).unwrap();
    };

    // xz_print(ptr, len): write(1, ptr, len)
    if let Some(f) = backend.module.get_function("xz_print") {
        let entry = backend.context.append_basic_block(f, "entry");
        backend.builder.position_at_end(entry);
        let p = f.get_nth_param(0).unwrap().into_pointer_value();
        let l = f.get_nth_param(1).unwrap().into_int_value();
        let fd = i32.const_int(1, false);
        backend.builder.build_direct_call(write, &[fd.into(), p.into(), l.into()], "w").unwrap();
        backend.builder.build_return(None).unwrap();
    }

    // xz_str_free(ptr, len): free(ptr)
    if let Some(f) = backend.module.get_function("xz_str_free") {
        let entry = backend.context.append_basic_block(f, "entry");
        backend.builder.position_at_end(entry);
        let p = f.get_nth_param(0).unwrap().into_pointer_value();
        backend.builder.build_direct_call(free, &[p.into()], "free").unwrap();
        backend.builder.build_return(None).unwrap();
    }

    // xz_concat(ap, al, bp, bl): malloc(total), memcpy both halves
    if let Some(f) = backend.module.get_function("xz_concat") {
        let entry = backend.context.append_basic_block(f, "entry");
        backend.builder.position_at_end(entry);
        let ap = f.get_nth_param(0).unwrap().into_pointer_value();
        let al = f.get_nth_param(1).unwrap().into_int_value();
        let bp = f.get_nth_param(2).unwrap().into_pointer_value();
        let bl = f.get_nth_param(3).unwrap().into_int_value();
        let total = backend.builder.build_int_add(al, bl, "total").unwrap();
        let buf = backend
            .builder
            .build_direct_call(malloc, &[total.into()], "buf")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let _ = backend.builder.build_memcpy(buf, 1, ap, 1, al).unwrap();
        let dst = unsafe { backend.builder.build_in_bounds_gep(i8, buf, &[al], "dst").unwrap() };
        let _ = backend.builder.build_memcpy(dst, 1, bp, 1, bl).unwrap();
        build_str_ret(backend, f, buf, total);
    }

    // xz_i64_to_str(v): snprintf("%lld")
    if let Some(f) = backend.module.get_function("xz_i64_to_str") {
        let entry = backend.context.append_basic_block(f, "entry");
        backend.builder.position_at_end(entry);
        let v = f.get_nth_param(0).unwrap().into_int_value();
        let fmt = backend.builder.build_global_string_ptr("%lld", "fmt.lld").unwrap().as_pointer_value();
        let fmtp = unsafe { backend.builder.build_in_bounds_gep(i8, fmt, &[i64.const_zero()], "fmtp").unwrap() };
        let null = ptr.const_null();
        let len = backend
            .builder
            .build_direct_call(snprintf, &[null.into(), i64.const_zero().into(), fmtp.into(), v.into()], "len")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();
        let len64 = backend.builder.build_int_s_extend(len, i64, "len64").unwrap();
        let size = backend.builder.build_int_add(len64, i64.const_int(1, false), "size").unwrap();
        let buf = backend
            .builder
            .build_direct_call(malloc, &[size.into()], "buf")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        backend
            .builder
            .build_direct_call(snprintf, &[buf.into(), size.into(), fmtp.into(), v.into()], "fmt")
            .unwrap();
        build_str_ret(backend, f, buf, len64);
    }

    // xz_f64_to_str(v): snprintf("%.17g")
    if let Some(f) = backend.module.get_function("xz_f64_to_str") {
        let entry = backend.context.append_basic_block(f, "entry");
        backend.builder.position_at_end(entry);
        let v = f.get_nth_param(0).unwrap().into_float_value();
        let fmt = backend.builder.build_global_string_ptr("%.17g", "fmt.g").unwrap().as_pointer_value();
        let fmtp = unsafe { backend.builder.build_in_bounds_gep(i8, fmt, &[i64.const_zero()], "fmtp").unwrap() };
        let null = ptr.const_null();
        let len = backend
            .builder
            .build_direct_call(snprintf, &[null.into(), i64.const_zero().into(), fmtp.into(), v.into()], "len")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();
        let len64 = backend.builder.build_int_s_extend(len, i64, "len64").unwrap();
        let size = backend.builder.build_int_add(len64, i64.const_int(1, false), "size").unwrap();
        let buf = backend
            .builder
            .build_direct_call(malloc, &[size.into()], "buf")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        backend
            .builder
            .build_direct_call(snprintf, &[buf.into(), size.into(), fmtp.into(), v.into()], "fmt")
            .unwrap();
        build_str_ret(backend, f, buf, len64);
    }

    // xz_bool_to_str(v): select "true"/"false"
    if let Some(f) = backend.module.get_function("xz_bool_to_str") {
        let entry = backend.context.append_basic_block(f, "entry");
        backend.builder.position_at_end(entry);
        let v = f.get_nth_param(0).unwrap().into_int_value();
        let t = backend.builder.build_global_string_ptr("true", "s.true").unwrap().as_pointer_value();
        let fstr = backend.builder.build_global_string_ptr("false", "s.false").unwrap().as_pointer_value();
        let tp = unsafe { backend.builder.build_in_bounds_gep(i8, t, &[i64.const_zero()], "tp").unwrap() };
        let fp = unsafe { backend.builder.build_in_bounds_gep(i8, fstr, &[i64.const_zero()], "fp").unwrap() };
        let sel = backend.builder.build_select(v, tp, fp, "sel").unwrap();
        let selp = sel.into_pointer_value();
        let len = backend.builder.build_select(v, i64.const_int(4, false), i64.const_int(5, false), "len").unwrap();
        let leni = len.into_int_value();
        build_str_ret(backend, f, selp, leni);
    }

    // xz_char_to_str(v): malloc(1), store the byte
    if let Some(f) = backend.module.get_function("xz_char_to_str") {
        let entry = backend.context.append_basic_block(f, "entry");
        backend.builder.position_at_end(entry);
        let v = f.get_nth_param(0).unwrap().into_int_value();
        let buf = backend
            .builder
            .build_direct_call(malloc, &[i64.const_int(1, false).into()], "buf")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        backend.builder.build_store(buf, v).unwrap();
        build_str_ret(backend, f, buf, i64.const_int(1, false));
    }

    // xz_str_to_upper / xz_str_to_lower: allocate len bytes and map each byte
    // through libc toupper/tolower.
    let toupper = backend.module.add_function("toupper", i32.fn_type(&[i32.into()], false), None);
    let tolower = backend.module.add_function("tolower", i32.fn_type(&[i32.into()], false), None);
    for (fname, cfun) in [("xz_str_to_upper", toupper), ("xz_str_to_lower", tolower)] {
        if let Some(f) = backend.module.get_function(fname) {
            let ptr = f.get_nth_param(0).unwrap().into_pointer_value();
            let len = f.get_nth_param(1).unwrap().into_int_value();
            let entry = backend.context.append_basic_block(f, "entry");
            let loop_bb = backend.context.append_basic_block(f, "loop");
            let body_bb = backend.context.append_basic_block(f, "body");
            let done_bb = backend.context.append_basic_block(f, "done");
            let one = i64.const_int(1, false);
            let zero = i64.const_zero();

            backend.builder.position_at_end(entry);
            // malloc(max(len, 1)) so a zero-length string still gets a pointer.
            let nonempty = backend
                .builder
                .build_int_compare(inkwell::IntPredicate::SGT, len, zero, "nonempty")
                .unwrap();
            let size = backend.builder.build_select(nonempty, len, one, "sz").unwrap().into_int_value();
            let buf = backend
                .builder
                .build_direct_call(malloc, &[size.into()], "buf")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap()
                .into_pointer_value();
            backend.builder.build_unconditional_branch(loop_bb).unwrap();

            backend.builder.position_at_end(loop_bb);
            let i_phi = backend.builder.build_phi(i64, "i").unwrap();
            i_phi.add_incoming(&[(&zero, entry)]);
            let i_val = i_phi.as_basic_value().into_int_value();
            let cond = backend
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, i_val, len, "cond")
                .unwrap();
            backend.builder.build_conditional_branch(cond, body_bb, done_bb).unwrap();

            backend.builder.position_at_end(body_bb);
            let src = unsafe { backend.builder.build_in_bounds_gep(i8, ptr, &[i_val], "src").unwrap() };
            let ch = backend.builder.build_load(i8, src, "ch").unwrap().into_int_value();
            let chi = backend.builder.build_int_s_extend(ch, i32, "chi").unwrap();
            let mapped = backend
                .builder
                .build_direct_call(cfun, &[chi.into()], "case")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap()
                .into_int_value();
            let mapped8 = backend.builder.build_int_truncate(mapped, i8, "m8").unwrap();
            let dst = unsafe { backend.builder.build_in_bounds_gep(i8, buf, &[i_val], "dst").unwrap() };
            backend.builder.build_store(dst, mapped8).unwrap();
            let inext = backend.builder.build_int_add(i_val, one, "inext").unwrap();
            backend.builder.build_unconditional_branch(loop_bb).unwrap();
            i_phi.add_incoming(&[(&inext, body_bb)]);

            backend.builder.position_at_end(done_bb);
            build_str_ret(backend, f, buf, len);
        }
    }

    // xz_str_eq(ap, al, bp, bl) -> i1: 1 when the byte ranges are equal. The
    // native Map[Str, V] key comparison (JIT host lives in runtime.rs).
    if let Some(f) = backend.module.get_function("xz_str_eq") {
        let ap = f.get_nth_param(0).unwrap().into_pointer_value();
        let al = f.get_nth_param(1).unwrap().into_int_value();
        let bp = f.get_nth_param(2).unwrap().into_pointer_value();
        let bl = f.get_nth_param(3).unwrap().into_int_value();
        let i1 = backend.types.bool;
        let entry = backend.context.append_basic_block(f, "entry");
        let len_ok = backend.context.append_basic_block(f, "len.ok");
        let loop_bb = backend.context.append_basic_block(f, "loop");
        let body_bb = backend.context.append_basic_block(f, "body");
        let neq_bb = backend.context.append_basic_block(f, "neq");
        let eq_bb = backend.context.append_basic_block(f, "eq");
        let done_bb = backend.context.append_basic_block(f, "done");

        backend.builder.position_at_end(entry);
        let same_len = backend
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, al, bl, "samelen")
            .unwrap();
        backend.builder.build_conditional_branch(same_len, len_ok, neq_bb).unwrap();

        backend.builder.position_at_end(len_ok);
        backend.builder.build_unconditional_branch(loop_bb).unwrap();

        backend.builder.position_at_end(loop_bb);
        let i_phi = backend.builder.build_phi(i64, "i").unwrap();
        i_phi.add_incoming(&[(&i64.const_zero(), len_ok)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let cond = backend
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i_val, al, "cond")
            .unwrap();
        backend.builder.build_conditional_branch(cond, body_bb, eq_bb).unwrap();

        backend.builder.position_at_end(body_bb);
        let aptr = unsafe { backend.builder.build_in_bounds_gep(i8, ap, &[i_val], "ap").unwrap() };
        let bptr = unsafe { backend.builder.build_in_bounds_gep(i8, bp, &[i_val], "bp").unwrap() };
        let av = backend.builder.build_load(i8, aptr, "av").unwrap().into_int_value();
        let bv = backend.builder.build_load(i8, bptr, "bv").unwrap().into_int_value();
        let same = backend
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, av, bv, "b.eq")
            .unwrap();
        let inext = backend.builder.build_int_add(i_val, i64.const_int(1, false), "inext").unwrap();
        i_phi.add_incoming(&[(&inext, body_bb)]);
        backend.builder.build_conditional_branch(same, loop_bb, neq_bb).unwrap();

        backend.builder.position_at_end(neq_bb);
        backend.builder.build_unconditional_branch(done_bb).unwrap();
        backend.builder.position_at_end(eq_bb);
        backend.builder.build_unconditional_branch(done_bb).unwrap();

        backend.builder.position_at_end(done_bb);
        let result = backend.builder.build_phi(i1, "eq").unwrap();
        result.add_incoming(&[(&i1.const_zero(), neq_bb), (&i1.const_int(1, false), eq_bb)]);
        backend.builder.build_return(Some(&result.as_basic_value())).unwrap();
    }

    // xz_read_file(path_ptr, path_len, out) -> i1: read the whole file into a
    // fresh heap buffer and publish the {ptr, len} payload through `out`.
    // libc: fopen("rb") / fseek(END) / ftell / fseek(SET) / fread / fclose.
    if let Some(f) = backend.module.get_function("xz_read_file") {
        let i1 = backend.types.bool;
        let fopen = backend.module.add_function(
            "fopen",
            ptr.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        let fseek = backend.module.add_function(
            "fseek",
            i32.fn_type(&[ptr.into(), i64.into(), i32.into()], false),
            None,
        );
        let ftell = backend.module.add_function("ftell", i64.fn_type(&[ptr.into()], false), None);
        let fread = backend.module.add_function(
            "fread",
            i64.fn_type(&[ptr.into(), i64.into(), i64.into(), ptr.into()], false),
            None,
        );
        let fclose = backend.module.add_function("fclose", i32.fn_type(&[ptr.into()], false), None);

        let path_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        let path_len = f.get_nth_param(1).unwrap().into_int_value();
        let out = f.get_nth_param(2).unwrap().into_pointer_value();

        let entry = backend.context.append_basic_block(f, "entry");
        let open_bb = backend.context.append_basic_block(f, "open");
        let fail_bb = backend.context.append_basic_block(f, "fail");
        let ret_bb = backend.context.append_basic_block(f, "ret");

        backend.builder.position_at_end(entry);
        // libc paths are NUL-terminated: copy the byte path into malloc(len+1).
        let psize = backend.builder.build_int_add(path_len, i64.const_int(1, false), "psize").unwrap();
        let cpath = backend
            .builder
            .build_direct_call(malloc, &[psize.into()], "cpath")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let _ = backend.builder.build_memcpy(cpath, 1, path_ptr, 1, path_len).unwrap();
        let nul = unsafe { backend.builder.build_in_bounds_gep(i8, cpath, &[path_len], "nul").unwrap() };
        backend.builder.build_store(nul, i8.const_zero()).unwrap();
        let mode = backend.builder.build_global_string_ptr("rb", "mode.rb").unwrap().as_pointer_value();
        let handle = backend
            .builder
            .build_direct_call(fopen, &[cpath.into(), mode.into()], "fopen")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        backend.builder.build_direct_call(free, &[cpath.into()], "freepath").unwrap();
        let is_null = backend
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, handle, ptr.const_null(), "null")
            .unwrap();
        backend.builder.build_conditional_branch(is_null, fail_bb, open_bb).unwrap();

        backend.builder.position_at_end(open_bb);
        let _ = backend
            .builder
            .build_direct_call(fseek, &[handle.into(), i64.const_zero().into(), i32.const_int(2, false).into()], "seekend")
            .unwrap();
        let flen = backend
            .builder
            .build_direct_call(ftell, &[handle.into()], "ftell")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();
        let _ = backend
            .builder
            .build_direct_call(fseek, &[handle.into(), i64.const_zero().into(), i32.const_zero().into()], "seekstart")
            .unwrap();
        // malloc(max(len, 1)) so a zero-length file still gets a pointer.
        let nonempty = backend
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, flen, i64.const_zero(), "nonempty")
            .unwrap();
        let asize = backend.builder.build_select(nonempty, flen, i64.const_int(1, false), "asize").unwrap().into_int_value();
        let data = backend
            .builder
            .build_direct_call(malloc, &[asize.into()], "data")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let _ = backend
            .builder
            .build_direct_call(fread, &[data.into(), i64.const_int(1, false).into(), flen.into(), handle.into()], "fread")
            .unwrap();
        let _ = backend.builder.build_direct_call(fclose, &[handle.into()], "fclose").unwrap();
        let out_ptr = backend.builder.build_struct_gep(backend.types.xz_str, out, 0, "out.ptr").unwrap();
        backend.builder.build_store(out_ptr, data).unwrap();
        let out_len = backend.builder.build_struct_gep(backend.types.xz_str, out, 1, "out.len").unwrap();
        backend.builder.build_store(out_len, flen).unwrap();
        backend.builder.build_unconditional_branch(ret_bb).unwrap();

        backend.builder.position_at_end(fail_bb);
        backend.builder.build_unconditional_branch(ret_bb).unwrap();

        backend.builder.position_at_end(ret_bb);
        let flag = backend.builder.build_phi(i1, "read.ok").unwrap();
        flag.add_incoming(&[(&i1.const_zero(), fail_bb), (&i1.const_int(1, false), open_bb)]);
        backend.builder.build_return(Some(&flag.as_basic_value())).unwrap();
    }

    // xz_time_now / xz_time_monotonic -> f64 seconds. `now` reads the wall
    // clock via gettimeofday; `monotonic` reads CLOCK_MONOTONIC via
    // clock_gettime. Both libc calls fill a {sec, subsec} struct (timeval on
    // Linux is {i64, i64}, timespec likewise), which is scaled to f64 seconds.
    // The clock id is a libc macro, so `monotonic_clock_id` selects it per host.
    emit_time_body(backend, "xz_time_now", false);
    emit_time_body(backend, "xz_time_monotonic", true);

    Ok(())
}

/// Define one clock host function for the native runtime. `monotonic` selects
/// `clock_gettime(CLOCK_MONOTONIC, &ts)` and a nanosecond scale; otherwise
/// `gettimeofday(&tv, NULL)` and a microsecond scale. See `emit_native_runtime`.
fn emit_time_body(backend: &mut LlvmBackend<'static>, fname: &str, monotonic: bool) {
    let f = match backend.module.get_function(fname) {
        Some(f) => f,
        None => return,
    };
    let i32_ty = backend.context.i32_type();
    let i64_ty = backend.types.int;
    let f64_ty = backend.types.float;
    let ptr_ty = backend.types.ptr;
    let tv_ty = backend.context.struct_type(&[i64_ty.into(), i64_ty.into()], false);

    let (host, args_clock): (inkwell::values::FunctionValue<'static>, bool) = if monotonic {
        let f = match backend.module.get_function("clock_gettime") {
            Some(f) => f,
            None => backend
                .module
                .add_function("clock_gettime", i32_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false), None),
        };
        (f, true)
    } else {
        let f = match backend.module.get_function("gettimeofday") {
            Some(f) => f,
            None => backend
                .module
                .add_function("gettimeofday", i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false), None),
        };
        (f, false)
    };

    let entry = backend.context.append_basic_block(f, "entry");
    backend.builder.position_at_end(entry);
    let tv = backend.builder.build_alloca(tv_ty, "tv").unwrap();
    if args_clock {
        let clk = i32_ty.const_int(monotonic_clock_id(), false);
        backend.builder.build_direct_call(host, &[clk.into(), tv.into()], "clock").unwrap();
    } else {
        backend
            .builder
            .build_direct_call(host, &[tv.into(), ptr_ty.const_null().into()], "clock")
            .unwrap();
    }
    let sec_p = backend.builder.build_struct_gep(tv_ty, tv, 0, "sec.p").unwrap();
    let frac_p = backend.builder.build_struct_gep(tv_ty, tv, 1, "frac.p").unwrap();
    let sec = backend.builder.build_load(i64_ty, sec_p, "sec").unwrap().into_int_value();
    let frac = backend.builder.build_load(i64_ty, frac_p, "frac").unwrap().into_int_value();
    let sec_f = backend.builder.build_signed_int_to_float(sec, f64_ty, "sec.f").unwrap();
    let frac_f = backend.builder.build_signed_int_to_float(frac, f64_ty, "frac.f").unwrap();
    let scale = f64_ty.const_float(if monotonic { 1e-9 } else { 1e-6 });
    let frac_s = backend.builder.build_float_mul(frac_f, scale, "frac.s").unwrap();
    let total = backend.builder.build_float_add(sec_f, frac_s, "time").unwrap();
    backend.builder.build_return(Some(&total)).unwrap();
}

/// The host libc's `CLOCK_MONOTONIC` id for `clock_gettime`. The id is a libc
/// macro, not a shared ABI constant, so it is selected per target OS; the
/// native runtime is linked against the host libc (docs/13-codegen.md).
fn monotonic_clock_id() -> u64 {
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        6
    } else if cfg!(any(
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "solaris",
        target_os = "illumos"
    )) {
        4
    } else if cfg!(any(target_os = "netbsd", target_os = "openbsd")) {
        3
    } else {
        1
    }
}
