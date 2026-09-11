use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use inkwell::module::Module;
use inkwell::OptimizationLevel;

/// The C ABI shape of an Xz `Str` / `Bytes` value: a byte pointer + length.
/// This must match the LLVM struct `{ i8*, i64 }` produced by codegen
/// (docs/13-codegen.md).
#[repr(C)]
pub struct XzStr {
    pub ptr: usize,
    pub len: usize,
}

/// Every heap-allocated Str buffer that has not yet been freed, keyed by its
/// byte pointer. `xz_str_free` only frees pointers in this registry, so the
/// compiler can conservatively emit `xz_str_free` for any Str slot: freeing a
/// string literal, an unknown-provenance pointer, or an already-freed buffer
/// is a sound no-op. This is the runtime backstop for the compiler's
/// conservative ownership rules (docs/13-codegen.md).
static LIVE_STR: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn copy_to_leaked(src: &[u8]) -> usize {
    let len = src.len().max(1);
    let layout = std::alloc::Layout::array::<u8>(len).unwrap();
    let dst = unsafe { std::alloc::alloc(layout) };
    for (i, b) in src.iter().enumerate() {
        unsafe { *((dst as usize + i) as *mut u8) = *b };
    }
    let ptr = dst as usize;
    let mut live = LIVE_STR.lock().unwrap();
    live.insert(ptr, len);
    ptr
}

fn write_stdout(ptr: usize, len: usize) {
    let mut buf: Vec<u8> = Vec::with_capacity(len);
    for i in 0..len {
        let b = unsafe { *((ptr + i) as *mut u8) };
        buf.push(b);
    }
    print!("{}", unsafe { String::from_utf8_unchecked(buf) });
}

#[unsafe(no_mangle)]
extern "C" fn xz_print(ptr: usize, len: usize) {
    write_stdout(ptr, len);
}

#[unsafe(no_mangle)]
extern "C" fn xz_concat(ap: usize, al: usize, bp: usize, bl: usize) -> XzStr {
    let mut buf: Vec<u8> = Vec::with_capacity(al + bl);
    for i in 0..al {
        buf.push(unsafe { *((ap + i) as *mut u8) });
    }
    for i in 0..bl {
        buf.push(unsafe { *((bp + i) as *mut u8) });
    }
    let ptr = copy_to_leaked(&buf);
    XzStr { ptr, len: buf.len() }
}

#[unsafe(no_mangle)]
extern "C" fn xz_i64_to_str(v: i64) -> XzStr {
    let s = v.to_string();
    let bytes: Vec<u8> = s.bytes().collect();
    XzStr { ptr: copy_to_leaked(&bytes), len: bytes.len() }
}

#[unsafe(no_mangle)]
extern "C" fn xz_f64_to_str(v: f64) -> XzStr {
    let s = v.to_string();
    let bytes: Vec<u8> = s.bytes().collect();
    XzStr { ptr: copy_to_leaked(&bytes), len: bytes.len() }
}

#[unsafe(no_mangle)]
extern "C" fn xz_bool_to_str(v: i8) -> XzStr {
    let s = if v != 0 { "true" } else { "false" };
    let bytes: Vec<u8> = s.bytes().collect();
    XzStr { ptr: copy_to_leaked(&bytes), len: bytes.len() }
}

#[unsafe(no_mangle)]
extern "C" fn xz_char_to_str(v: i8) -> XzStr {
    let bytes: Vec<u8> = vec![v as u8];
    XzStr { ptr: copy_to_leaked(&bytes), len: 1 }
}

#[unsafe(no_mangle)]
extern "C" fn xz_str_to_upper(ptr: usize, len: usize) -> XzStr {
    let mut bytes: Vec<u8> = Vec::with_capacity(len);
    for i in 0..len {
        bytes.push(unsafe { *((ptr + i) as *const u8) }.to_ascii_uppercase());
    }
    XzStr { ptr: copy_to_leaked(&bytes), len: bytes.len() }
}

#[unsafe(no_mangle)]
extern "C" fn xz_str_to_lower(ptr: usize, len: usize) -> XzStr {
    let mut bytes: Vec<u8> = Vec::with_capacity(len);
    for i in 0..len {
        bytes.push(unsafe { *((ptr + i) as *const u8) }.to_ascii_lowercase());
    }
    XzStr { ptr: copy_to_leaked(&bytes), len: bytes.len() }
}

/// Free a heap-allocated Str buffer. No-op unless the pointer is still live in
/// the registry, so it is safe to call on literals or already-freed buffers.
#[unsafe(no_mangle)]
extern "C" fn xz_str_free(ptr: usize, _len: usize) {
    let mut live = LIVE_STR.lock().unwrap();
    if let Some(len) = live.remove(&ptr) {
        let layout = std::alloc::Layout::array::<u8>(len).unwrap();
        unsafe { std::alloc::dealloc(ptr as *mut u8, layout) };
    }
}



/// Compile the module to a JIT engine, bind the host functions, and run `main`.
/// `main` is a no-arg C function (possibly declared `void`); a non-zero exit is
/// returned only when the host reports an execution problem.
pub fn run(module: Module) -> Result<i32, String> {
    // Optimize the module (inlining, mem2reg/SROA, DCE, constant folding, ...)
    // before JIT codegen, and let the engine compile at the aggressive level.
    crate::backend::llvm_backend::optimize(&module)?;
    let ee = module.create_jit_execution_engine(OptimizationLevel::Aggressive).map_err(|e| {
        e.to_str().map(|s| s.to_string()).unwrap_or_else(|_| "LLVM error".to_string())
    })?;

    // Bind every host function by name so the JIT can resolve them. Casting each
    // function item through a typed function pointer avoids the item→int lint.
    let p: unsafe extern "C" fn(usize, usize) -> () = xz_print;
    let c: unsafe extern "C" fn(usize, usize, usize, usize) -> XzStr = xz_concat;
    let i2s: unsafe extern "C" fn(i64) -> XzStr = xz_i64_to_str;
    let f2s: unsafe extern "C" fn(f64) -> XzStr = xz_f64_to_str;
    let b2s: unsafe extern "C" fn(i8) -> XzStr = xz_bool_to_str;
    let ch2s: unsafe extern "C" fn(i8) -> XzStr = xz_char_to_str;
    let upper: unsafe extern "C" fn(usize, usize) -> XzStr = xz_str_to_upper;
    let lower: unsafe extern "C" fn(usize, usize) -> XzStr = xz_str_to_lower;
    let sfree: unsafe extern "C" fn(usize, usize) -> () = xz_str_free;
    bind(&module, &ee, "xz_str_free", sfree as usize);
    bind(&module, &ee, "xz_print", p as usize);
    bind(&module, &ee, "xz_concat", c as usize);
    bind(&module, &ee, "xz_i64_to_str", i2s as usize);
    bind(&module, &ee, "xz_f64_to_str", f2s as usize);
    bind(&module, &ee, "xz_bool_to_str", b2s as usize);
    bind(&module, &ee, "xz_char_to_str", ch2s as usize);
    bind(&module, &ee, "xz_str_to_upper", upper as usize);
    bind(&module, &ee, "xz_str_to_lower", lower as usize);

    let main = ee.get_function_value("main").map_err(|_| "no 'main' function".to_string())?;
    let _ = unsafe { ee.run_function(main, &[]) };
    Ok(0)
}

fn bind<'ctx>(
    module: &Module<'ctx>,
    ee: &inkwell::execution_engine::ExecutionEngine<'ctx>,
    name: &str,
    addr: usize,
) {
    if let Some(fv) = module.get_function(name) {
        ee.add_global_mapping(&fv, addr);
    }
}
