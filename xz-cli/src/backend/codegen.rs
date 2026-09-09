use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::ast::{self, Expr, Pattern, Stmt, Type};
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
    /// the enclosing function's declared return kind (for ok/err payloads)
    pub ret_kind: Option<Kind>,
    /// the enclosing function's LLVM return type; None for void (Unit)
    pub ret_llvm: Option<BasicTypeEnum<'ctx>>,
    str_count: usize,
}

type GenResult<'ctx> = Result<BasicValueEnum<'ctx>, String>;

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
            match stmt {
                Stmt::Decl(d) => self.gen_decl(d),
                Stmt::Assign(a) => self.gen_assign(a),
                Stmt::Expr(e) => {
                    let v = self.gen_expr(e);
                    if i == n - 1 {
                        last = v.ok();
                    }
                }
                Stmt::Break | Stmt::Continue => {
                    let _ = self.fail::<()>("break/continue (loops) are not supported in Phase 4");
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
        match &d.init {
            Some(e) => match self.gen_expr(e) {
                Ok(v) => {
                    self.bind_value(&d.name, v);
                }
                Err(_) => {}
            },
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
                            let _ = self.apply_assign_op(&a.op, ty, ptr, v);
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
            Expr::Loop(_) | Expr::For(_, _, _) => {
                self.fail("loop/for are not supported in Phase 4")
            }
            Expr::Await(_) | Expr::Send(_, _) | Expr::Recv(_) | Expr::Transfer(_) => {
                self.fail("async/channel/transfer are not supported in Phase 4")
            }
            Expr::Ok(inner) => self.gen_ok(inner),
            Expr::Err(a) => {
                // The error payload is never read (only the ok-flag matters), so
                // we skip evaluating it entirely.
                let _ = a;
                self.gen_err()
            }
            Expr::Some(a) => {
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

    /// A zero Result/Option whose payload is taken from the enclosing return.
    fn none_value(&mut self) -> BasicValueEnum<'ctx> {
        self.backend.types.unit.const_zero().into()
    }

    fn gen_ok(&mut self, inner: &Option<Box<Expr>>) -> GenResult<'ctx> {
        match inner {
            Some(e) => {
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
        } else {
            let r = self
                .backend
                .builder
                .build_int_compare(pred_int, a.into_int_value(), b.into_int_value(), "cmp")
                .unwrap();
            Ok(r.into())
        }
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
            return Ok(self.backend.types.unit.const_zero().into());
        }
        // stdlib approx_sqrt -> host xz_sqrt (libm sqrt)
        if name == "approx_sqrt" {
            if args.len() != 1 {
                return self.fail("approx_sqrt takes one Float");
            }
            let v = self.gen_expr(&args[0])?;
            let f = self.backend.module.get_function("xz_sqrt").ok_or("xz_sqrt missing")?;
            let call = self
                .backend
                .builder
                .build_direct_call(f, &[v.into()], "sqrt")
                .unwrap();
            return Ok(call.try_as_basic_value().basic().unwrap());
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
        let box_ptr = self.backend.builder.build_malloc(vt, &format!("{}.box", name)).unwrap();
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
            if self.is_float(rv) {
                let f = self.backend.module.get_function("xz_f64_abs").ok_or("xz_f64_abs missing")?;
                let call = self
                    .backend
                    .builder
                    .build_direct_call(f, &[rv.into()], "abs")
                    .unwrap();
                return Ok(call.try_as_basic_value().basic().unwrap());
            } else {
                let f = self.backend.module.get_function("xz_i64_abs").ok_or("xz_i64_abs missing")?;
                let call = self
                    .backend
                    .builder
                    .build_direct_call(f, &[rv.into()], "abs")
                    .unwrap();
                return Ok(call.try_as_basic_value().basic().unwrap());
            }
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
        let cond = self.gen_expr(&ifx.cond)?;
        let cond = cond.into_int_value();
        let fnv = self.cur_fn();

        let then_bb = self.backend.context.append_basic_block(fnv, "if.then");
        let else_bb = self.backend.context.append_basic_block(fnv, "if.else");
        let merge_bb = self.backend.context.append_basic_block(fnv, "if.merge");
        self.backend.builder.build_conditional_branch(cond, then_bb, else_bb).unwrap();

        // then
        self.backend.builder.position_at_end(then_bb);
        let then_val = self.gen_block(&ifx.then_block);
        let then_bb = self.backend.builder.get_insert_block().unwrap();
        self.backend.builder.build_unconditional_branch(merge_bb).unwrap();

        // else
        self.backend.builder.position_at_end(else_bb);
        let else_val = match &ifx.else_block {
            Some(b) => self.gen_block(b),
            None => None,
        };
        let else_bb = self.backend.builder.get_insert_block().unwrap();
        self.backend.builder.build_unconditional_branch(merge_bb).unwrap();

        self.backend.builder.position_at_end(merge_bb);

        match (then_val, else_val) {
            (Some(t), Some(el)) => {
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
                self.bind_variant(subj, pat)?;
                let v = self.gen_expr(body)?;
                if first_type.is_none() {
                    first_type = Some(self.basic_type_of(v));
                }
                incoming.push((v, arm_bb));
                self.backend.builder.build_unconditional_branch(merge_bb).unwrap();

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
                self.bind_catchall(subj, pat);
                let v = self.gen_expr(body)?;
                if first_type.is_none() {
                    first_type = Some(self.basic_type_of(v));
                }
                incoming.push((v, dispatch));
                self.backend.builder.build_unconditional_branch(merge_bb).unwrap();
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

    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind,
        ret_llvm,
        str_count: 0,
    };
    cg.gen_body_common(&f.name, fv, &f.params, &f.body);
}

/// Generate `main`. `main` is called by the runtime as a void, no-arg C
/// function regardless of its declared `Result[Unit, Err]` return.
pub fn gen_main(backend: &mut LlvmBackend<'static>, f: &ast::FuncDecl) {
    let fv = match backend.functions.get(&f.name) {
        Some(fv) => *fv,
        None => return,
    };
    let ret_kind = f.ret.as_ref().map(|t| kind_from_ast(t, backend));
    let mut cg = Codegen {
        backend,
        scope: HashMap::new(),
        ret_kind,
        ret_llvm: None,
        str_count: 0,
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
