use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, LazyLock, Mutex};
use std::thread;

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

/// Byte equality for `Str`: the `Map[Str, V]` key comparison. Returns 1 when
/// the two byte ranges are the same length and content, else 0.
#[unsafe(no_mangle)]
extern "C" fn xz_str_eq(ap: usize, al: usize, bp: usize, bl: usize) -> i8 {
    if al != bl {
        return 0;
    }
    for i in 0..al {
        let a = unsafe { *((ap + i) as *const u8) };
        let b = unsafe { *((bp + i) as *const u8) };
        if a != b {
            return 0;
        }
    }
    1
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



/// Phase 6 deterministic scheduler (docs/05-concurrency.md). Tasks run one at a
/// time in a fixed order; the host serializes them with a single token even
/// though each task has its own OS thread. `send`/`recv` are the only
/// suspension points. This is the JIT host half of `task`/`chan`/`send`/`recv`.
struct Channel {
    queue: VecDeque<Vec<u8>>,
    receivers: VecDeque<u64>,
}

struct Sched {
    ready: VecDeque<u64>,
    running: Option<u64>,
    next_id: u64,
    channels: HashMap<u64, Channel>,
    pending: HashMap<u64, Vec<u8>>,
}

static SCHED: LazyLock<(Mutex<Sched>, Condvar)> = LazyLock::new(|| {
    (
        Mutex::new(Sched {
            ready: VecDeque::new(),
            running: Some(0),
            next_id: 1,
            channels: HashMap::new(),
            pending: HashMap::new(),
        }),
        Condvar::new(),
    )
});

thread_local! {
    static TASK_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn sched_lock() -> std::sync::MutexGuard<'static, Sched> {
    SCHED.0.lock().unwrap()
}

fn copy_bytes(ptr: usize, size: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(size);
    for i in 0..size {
        v.push(unsafe { *((ptr + i) as *const u8) });
    }
    v
}

fn copy_out(dst: usize, src: &[u8], size: usize) {
    for i in 0..size {
        let b = if i < src.len() { src[i] } else { 0 };
        unsafe { *((dst + i) as *mut u8) = b };
    }
}

/// `main` starts the scheduler; the calling thread becomes task 0.
#[unsafe(no_mangle)]
extern "C" fn xz_sched_init() {
    TASK_ID.with(|t| t.set(0));
    let (lock, cv) = &*SCHED;
    let mut s = lock.lock().unwrap();
    s.running = Some(0);
    cv.notify_all();
}

/// Spawn a task: assign an id, append it to the ready queue, and start its OS
/// thread. The thread waits for the token, so tasks run only when the scheduler
/// grants it. The token passes to the next ready task when the body returns.
fn spawn_task<F: FnOnce() + Send + 'static>(body: F) {
    let id;
    {
        let mut s = sched_lock();
        id = s.next_id;
        s.next_id += 1;
        s.ready.push_back(id);
    }
    thread::spawn(move || {
        TASK_ID.with(|t| t.set(id));
        {
            let (lock, cv) = &*SCHED;
            let mut s = lock.lock().unwrap();
            while s.running != Some(id) {
                s = cv.wait(s).unwrap();
            }
        }
        body();
        let (lock, cv) = &*SCHED;
        let mut s = lock.lock().unwrap();
        s.running = s.ready.pop_front();
        cv.notify_all();
    });
}

/// Spawn a `task` declaration: a no-arg entry.
#[unsafe(no_mangle)]
extern "C" fn xz_task_spawn(fp: usize) {
    let entry: extern "C" fn() = unsafe { std::mem::transmute(fp) };
    spawn_task(move || entry());
}

/// Spawn a coroutine with a single opaque environment pointer. `await` uses
/// this: the pointer addresses the caller's argument struct, valid because the
/// caller blocks (and keeps its frame) until the child finishes
/// (docs/05-concurrency.md, docs/13-codegen.md § async/await).
#[unsafe(no_mangle)]
extern "C" fn xz_task_spawn_arg(fp: usize, arg: usize) {
    let entry: extern "C" fn(usize) = unsafe { std::mem::transmute(fp) };
    spawn_task(move || entry(arg));
}

/// Hand a message to the earliest blocked receiver, else queue it. Never blocks
/// the sender (unbounded channels, docs/05).
#[unsafe(no_mangle)]
extern "C" fn xz_chan_send(id: i64, ptr: usize, size: usize) {
    let msg = copy_bytes(ptr, size);
    let (lock, cv) = &*SCHED;
    let mut s = lock.lock().unwrap();
    let cid = id as u64;
    let recv = {
        let ch = s
            .channels
            .entry(cid)
            .or_insert_with(|| Channel { queue: VecDeque::new(), receivers: VecDeque::new() });
        ch.receivers.pop_front()
    };
    if let Some(r) = recv {
        s.pending.insert(r, msg);
        s.ready.push_back(r);
        cv.notify_all();
    } else {
        s.channels.get_mut(&cid).unwrap().queue.push_back(msg);
    }
}

/// Receive the next message, blocking the current task when the channel is
/// empty. On a block, the token passes to the next ready task; this task
/// resumes when a `send` hands it a message and the scheduler grants the token.
#[unsafe(no_mangle)]
extern "C" fn xz_chan_recv(id: i64, out: usize, size: usize) {
    let me = TASK_ID.with(|t| t.get());
    let (lock, cv) = &*SCHED;
    let mut s = lock.lock().unwrap();
    let cid = id as u64;
    let queued = {
        let ch = s
            .channels
            .entry(cid)
            .or_insert_with(|| Channel { queue: VecDeque::new(), receivers: VecDeque::new() });
        ch.queue.pop_front()
    };
    if let Some(msg) = queued {
        copy_out(out, &msg, size);
        return;
    }
    s.channels.get_mut(&cid).unwrap().receivers.push_back(me);
    s.running = s.ready.pop_front();
    cv.notify_all();
    while s.running != Some(me) {
        s = cv.wait(s).unwrap();
    }
    let msg = s.pending.remove(&me).unwrap_or_default();
    copy_out(out, &msg, size);
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
    let streq: unsafe extern "C" fn(usize, usize, usize, usize) -> i8 = xz_str_eq;
    bind(&module, &ee, "xz_str_free", sfree as usize);
    bind(&module, &ee, "xz_print", p as usize);
    bind(&module, &ee, "xz_concat", c as usize);
    bind(&module, &ee, "xz_i64_to_str", i2s as usize);
    bind(&module, &ee, "xz_f64_to_str", f2s as usize);
    bind(&module, &ee, "xz_bool_to_str", b2s as usize);
    bind(&module, &ee, "xz_char_to_str", ch2s as usize);
    bind(&module, &ee, "xz_str_to_upper", upper as usize);
    bind(&module, &ee, "xz_str_to_lower", lower as usize);
    bind(&module, &ee, "xz_str_eq", streq as usize);
    let sched_init: unsafe extern "C" fn() = xz_sched_init;
    let task_spawn: unsafe extern "C" fn(usize) = xz_task_spawn;
    let task_spawn_arg: unsafe extern "C" fn(usize, usize) = xz_task_spawn_arg;
    let chan_send: unsafe extern "C" fn(i64, usize, usize) = xz_chan_send;
    let chan_recv: unsafe extern "C" fn(i64, usize, usize) = xz_chan_recv;
    bind(&module, &ee, "xz_sched_init", sched_init as usize);
    bind(&module, &ee, "xz_task_spawn", task_spawn as usize);
    bind(&module, &ee, "xz_task_spawn_arg", task_spawn_arg as usize);
    bind(&module, &ee, "xz_chan_send", chan_send as usize);
    bind(&module, &ee, "xz_chan_recv", chan_recv as usize);

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
