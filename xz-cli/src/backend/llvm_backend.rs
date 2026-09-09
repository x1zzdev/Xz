use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::FunctionValue;
use inkwell::AddressSpace;

use crate::ast::{self, Item, Program, Type};
use crate::typecheck::Kind;

/// A concrete function signature with LLVM types, used to declare and call
/// functions. `llvm_ret` is None for a `Unit`-returning function (void).
pub struct LlvmSig<'ctx> {
    pub params: Vec<BasicTypeEnum<'ctx>>,
    pub ret: Option<BasicTypeEnum<'ctx>>,
    pub xz_ret: Option<Kind>,
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
        TypeMap {
            int: i64,
            float: context.f64_type(),
            ptr,
            bool: context.bool_type(),
            char: i8,
            xz_str,
            xz_bytes,
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
        }
    }

    pub fn fail(&mut self, msg: &str) {
        if self.error.is_none() {
            self.error = Some(msg.to_string());
        }
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
            Kind::Err | Kind::ErrUnion(_) => self.types.unit.into(),
            Kind::Chan(_) | Kind::TypeVar(_) | Kind::Unknown | Kind::Never => self.types.unit.into(),
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
        let field_tys: Vec<BasicTypeEnum<'ctx>> = field_kinds.iter().map(|k| self.kind_to_llvm(k)).collect();
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
        ret: &Option<Kind>,
        explicit_ret: Option<BasicTypeEnum<'ctx>>,
    ) -> FunctionValue<'ctx> {
        let param_tys: Vec<BasicTypeEnum<'ctx>> = param_kinds.iter().map(|k| self.kind_to_llvm(k)).collect();
        let ret_ty = match explicit_ret {
            Some(t) => Some(t),
            None => self.ret_type(ret),
        };
        let sig = LlvmSig {
            params: param_tys.iter().map(|t| *t).collect(),
            ret: ret_ty,
            xz_ret: ret.clone(),
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

/// The entry point: declare every top-level type and function, then lower the
/// body of each function. `main` is declared with a `void` ABI regardless of
/// its declared `Result[Unit, Err]` return, since the runtime calls it with no
/// args and ignores the result.
pub fn compile(program: &Program) -> Result<LlvmBackend<'static>, String> {
    // The LLVM context lives for the whole compiler process. Leaking it gives
    // a 'static reference, so blocks/values don't borrow the backend (which is
    // what lets codegen hold block handles while mutating the symbol table).
    let context: &'static Context = Box::leak(Box::new(Context::create()));
    let mut backend = LlvmBackend::new(context);

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
                let param_kinds: Vec<Kind> = f.params.iter().map(|p| kind_from_ast(&p.ty, &backend)).collect();
                // `main` is called by the runtime as a void, no-arg C function
                // regardless of its declared `Result[Unit, Err]` return.
                if f.name == "main" {
                    backend.declare_function(&f.name, &param_kinds, &None, None);
                } else {
                    let ret = f.ret.as_ref().map(|t| kind_from_ast(t, &backend));
                    backend.declare_function(&f.name, &param_kinds, &ret, None);
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
                    },
                );
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
        // concat(aptr, alen, bptr, blen) -> XzStr
        backend.declare_extern("xz_concat", &[ptr.into(), i64.into(), ptr.into(), i64.into()], Some(xstr.into()));
        // scalar -> XzStr
        backend.declare_extern("xz_i64_to_str", &[i64.into()], Some(xstr.into()));
        backend.declare_extern("xz_f64_to_str", &[f64.into()], Some(xstr.into()));
        backend.declare_extern("xz_bool_to_str", &[i1.into()], Some(xstr.into()));
        backend.declare_extern("xz_char_to_str", &[i8.into()], Some(xstr.into()));
        // abs
        backend.declare_extern("xz_i64_abs", &[i64.into()], Some(i64.into()));
        backend.declare_extern("xz_f64_abs", &[f64.into()], Some(f64.into()));
        // sqrt (approx_sqrt)
        backend.declare_extern("xz_sqrt", &[f64.into()], Some(f64.into()));
    }

    // Map the stdlib function `approx_sqrt` and `print` to their host ABI.
    // `print` is special-cased in codegen; `approx_sqrt` is a declared call.
    if let Some(fv) = backend.functions.get("approx_sqrt").copied() {
        let _ = fv;
    }
    // Extern-declared `approx_sqrt` resolves to xz_sqrt at the call site if the
    // user wrote an extern; stdlib approx_sqrt is handled in gen_named_call.
    backend
        .sigs
        .entry("approx_sqrt".to_string())
        .or_insert_with(|| LlvmSig {
            params: vec![backend.types.float.into()],
            ret: Some(backend.types.float.into()),
            xz_ret: Some(Kind::Float),
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
            _ => {}
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
