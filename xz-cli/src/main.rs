use xz_cli::lexer::{lex};
use xz_cli::token::{TokKind};
use xz_cli::parser::{parse};
use xz_cli::resolve::{resolve};
use xz_cli::typecheck::{typecheck};
use xz_cli::intent::{check_intent, check_intent_strict};
use xz_cli::diagnostic::{Diagnostic, Severity, Category, Span as DSpan, to_json_array};
use xz_cli::token::{Span, Token};
fn main() {
    let args = std::env::args();
    let mut argv: Vec<String> = vec![];
    for a in args {
        argv.push(a);
    }
    if argv.len() < 3 {
        println!("usage: xz <lex|parse|check|check-json|build|run|build-native> [--strict] <file.xz>");
        return;
    }
    let cmd = argv[1].clone();
    let mut strict = false;
    let mut path: String = "".to_string();
    for i in 2..argv.len() {
        let a = argv[i].clone();
        if a == "--strict" {
            strict = true;
        } else if path == "" {
            path = a;
        }
    }
    if path == "" {
        println!("usage: xz <lex|parse|check|check-json|build|run|build-native> [--strict] <file.xz>");
        return;
    }
    let source = std::fs::read_to_string(path.clone());
    match source {
        Err(e) => {
            println!("error: cannot read {}: {}", path, e);
            std::process::exit(1);
        }
        Ok(src) => {
            let result = lex(src, path.clone());
            match result {
                Err(e) => {
                    let (l, c) = e.span.start;
                    println!("error: {} at {}:{}:{}", e.message, e.span.file, l, c);
                    std::process::exit(1);
                }
                Ok(tokens) => {
                    if cmd == "lex" {
                        for tok in tokens {
                            println!("{}  {}", tok_name(&tok.kind), tok.text);
                        }
                    } else if cmd == "parse" {
                        let parsed = parse(tokens);
                        match parsed {
                            Err(e) => {
                                let (l, c) = e.span.start;
                                println!("error: {} at {}:{}:{}", e.message, e.span.file, l, c);
                                std::process::exit(1);
                            }
                            Ok(program) => {
                                println!("ok: parsed {} top-level declarations", program.items.len());
                            }
                        }
                    } else if cmd == "check" {
                        std::process::exit(run_check(tokens, strict, false));
                    } else if cmd == "check-json" {
                        std::process::exit(run_check(tokens, strict, true));
                    } else if cmd == "build" {
                        std::process::exit(run_backend(tokens, false));
                    } else if cmd == "run" {
                        std::process::exit(run_backend(tokens, true));
                    } else if cmd == "build-native" {
                        std::process::exit(run_native_build(tokens));
                    } else {
                        println!("unknown command: {}", cmd);
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

fn run_check(tokens: Vec<Token>, strict: bool, json: bool) -> i32 {
    let mut diags: Vec<Diagnostic> = vec![];
    let parsed = parse(tokens);
    match parsed {
        Err(e) => {
            diags.push(Diagnostic {
                version: 1,
                severity: Severity::Error,
                code: "P0001".to_string(),
                message: e.message.clone(),
                category: Category::Parse,
                span: dspan(e.span),
                suggestion: None,
            });
        }
        Ok(program) => {
            match resolve(&program) {
                Err(errors) => {
                    for err in errors {
                        diags.push(Diagnostic {
                            version: 1,
                            severity: Severity::Error,
                            code: "R0001".to_string(),
                            message: err.message.clone(),
                            category: Category::Resolve,
                            span: dspan(err.span),
                            suggestion: None,
                        });
                    }
                }
                Ok(_) => {
                    match typecheck(&program) {
                        Err(errors) => {
                            for err in errors {
                                diags.push(Diagnostic {
                                    version: 1,
                                    severity: Severity::Error,
                                    code: "T0001".to_string(),
                                    message: err.message.clone(),
                                    category: Category::Type,
                                    span: DSpan { file: err.file.clone(), start: (0, 0), end: (0, 0) },
                                    suggestion: None,
                                });
                            }
                        }
                        Ok(_) => {
                            let intent = if strict { check_intent_strict(&program) } else { check_intent(&program) };
                            match intent {
                                Err(errors) => {
                                    for err in errors {
                                        let suggestion: Option<xz_cli::diagnostic::Suggestion> = match &err.suggestion {
                                            Some(s) => Some(xz_cli::diagnostic::Suggestion { fix: s.fix.clone(), confidence: s.confidence }),
                                            None => None,
                                        };
                                        diags.push(Diagnostic {
                                            version: 1,
                                            severity: Severity::Error,
                                            code: err.code.clone(),
                                            message: err.message.clone(),
                                            category: Category::Intent,
                                            span: dspan(err.span),
                                            suggestion: suggestion,
                                        });
                                    }
                                }
                                Ok(_) => {}
                            }
                        }
                    }
                }
            }
        }
    }
    let failed = !diags.is_empty();
    if json {
        println!("{}", to_json_array(&diags));
    } else if failed {
        for d in diags {
            let (sl, sc) = d.span.start;
            println!("  [{}] {} at {}:{}:{}", d.code, d.message, d.span.file, sl, sc);
        }
    } else {
        println!("ok: all checks passed");
    }
    if failed { 1 } else { 0 }
}

fn dspan(s: Span) -> DSpan {
    DSpan { file: s.file, start: s.start, end: s.end }
}

/// Phase 4 backend: run the full front-end check, then lower to LLVM IR and
/// (for `xz run`) JIT-execute `main`.
fn run_backend(tokens: Vec<Token>, execute: bool) -> i32 {
    let parsed = parse(tokens);
    let program = match parsed {
        Err(e) => {
            println!("error: {} at {}:{}:{}", e.message, e.span.file, e.span.start.0, e.span.start.1);
            return 1;
        }
        Ok(p) => p,
    };
    if let Err(errors) = resolve(&program) {
        println!("error: {} resolution errors", errors.len());
        return 1;
    }
    if let Err(errors) = typecheck(&program) {
        for err in &errors {
            println!("type error: {}", err.message);
        }
        println!("error: {} type errors", errors.len());
        return 1;
    }
    if let Err(errors) = check_intent(&program) {
        println!("error: intent: {}", errors[0].code);
        return 1;
    }

    let compiled = xz_cli::backend::llvm_backend::compile(&program);
    match compiled {
        Err(e) => {
            println!("error: codegen failed: {}", e);
            1
        }
        Ok(backend) => {
            if execute {
                match xz_cli::backend::runtime::run(backend.module) {
                    Ok(code) => code,
                    Err(e) => {
                        println!("error: run failed: {}", e);
                        1
                    }
                }
            } else {
                // xz build: emit the (unoptimized) LLVM IR.
                println!("{}", backend.module.print_to_string().to_string());
                0
            }
        }
    }
}

/// Phase 5 native build: compile to LLVM IR, emit the native runtime bodies,
/// optimize, then lower to an object file with `llc` and link with `ld` into a
/// standalone executable (no Rust runtime). Requires `llc` (from the portable
/// LLVM) and `ld`/libc on the host.
fn run_native_build(tokens: Vec<Token>) -> i32 {
    let parsed = parse(tokens);
    let program = match parsed {
        Err(e) => {
            println!("error: {} at {}:{}:{}", e.message, e.span.file, e.span.start.0, e.span.start.1);
            return 1;
        }
        Ok(p) => p,
    };
    if let Err(errors) = resolve(&program) {
        println!("error: {} resolution errors", errors.len());
        return 1;
    }
    if let Err(errors) = typecheck(&program) {
        for err in &errors {
            println!("type error: {}", err.message);
        }
        println!("error: {} type errors", errors.len());
        return 1;
    }
    if let Err(errors) = check_intent(&program) {
        println!("error: intent: {}", errors[0].code);
        return 1;
    }

    let mut backend = match xz_cli::backend::llvm_backend::compile(&program) {
        Ok(b) => b,
        Err(e) => {
            println!("error: codegen failed: {}", e);
            return 1;
        }
    };
    if let Err(e) = xz_cli::backend::llvm_backend::emit_native_runtime(&mut backend) {
        println!("error: native runtime emission failed: {}", e);
        return 1;
    }
    if let Err(e) = xz_cli::backend::llvm_backend::optimize(&backend.module) {
        println!("error: optimization failed: {}", e);
        return 1;
    }
    let module = &backend.module;

    // Write the IR to a temp file.
    let dir = std::env::temp_dir().join(format!("xz_native_{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        println!("error: cannot create temp dir: {}", e);
        return 1;
    }
    let ir_path = dir.join("prog.ll");
    let obj_path = dir.join("prog.o");
    let out_path = dir.join("prog");
    if let Err(e) = std::fs::write(&ir_path, module.print_to_string().to_string()) {
        println!("error: cannot write IR: {}", e);
        return 1;
    }

    // llc: IR -> object file.
    let llc = std::env::var("LLC")
        .unwrap_or_else(|_| "/home/x1zz/.local/share/xz-llvm17/debroot/usr/lib/llvm-17/bin/llc".to_string());
    let st = std::process::Command::new(&llc)
        .arg(&ir_path)
        .arg("-filetype=obj")
        .arg("-O3")
        .arg("-o")
        .arg(&obj_path)
        .status();
    match st {
        Err(e) => {
            println!("error: llc not found ({:?}); set LLC to the portable llc path", e);
            return 1;
        }
        Ok(s) if !s.success() => {
            println!("error: llc failed");
            return 1;
        }
        _ => {}
    }

    // ld: link the object with the C runtime and libc into an executable.
    let ld = std::env::var("LD").unwrap_or_else(|_| "ld".to_string());
    let dyn_loader = "/lib64/ld-linux-x86-64.so.2";
    let st = std::process::Command::new(&ld)
        .args(["-o", out_path.to_str().unwrap(), "-dynamic-linker", dyn_loader])
        .args([obj_path.to_str().unwrap(), "/usr/lib/x86_64-linux-gnu/crt1.o",
               "/usr/lib/x86_64-linux-gnu/crti.o", "/usr/lib/x86_64-linux-gnu/crtn.o",
               "-lc", "-lm", "--as-needed"])
        .status();
    match st {
        Err(e) => {
            println!("error: ld not found ({:?})", e);
            return 1;
        }
        Ok(s) if !s.success() => {
            println!("error: ld failed");
            return 1;
        }
        _ => {}
    }

    // Copy the binary out to the current directory.
    if let Err(e) = std::fs::copy(&out_path, "xz_program") {
        println!("error: cannot write ./xz_program: {}", e);
        return 1;
    }
    println!("ok: wrote ./xz_program");
    0
}

fn tok_name(kind: &TokKind) -> String {
    match kind {
        TokKind::Int(v) => format!("Int({v})"),
        TokKind::Float(v) => format!("Float({v})"),
        TokKind::Char(c) => format!("Char('{c}')"),
        TokKind::Str(s) => format!("Str(\"{s}\")"),
        TokKind::RawStr(s) => format!("RawStr(\"{s}\")"),
        TokKind::Ident(s) => format!("Ident({s})"),
        _ => match kind {
            TokKind::True => "true".to_string(),
            TokKind::False => "false".to_string(),
            TokKind::None => "none".to_string(),
            TokKind::And => "and".to_string(),
            TokKind::As => "as".to_string(),
            TokKind::Async => "async".to_string(),
            TokKind::Await => "await".to_string(),
            TokKind::Break => "break".to_string(),
            TokKind::Chan => "chan".to_string(),
            TokKind::Continue => "continue".to_string(),
            TokKind::Elif => "elif".to_string(),
            TokKind::Else => "else".to_string(),
            TokKind::Enum => "enum".to_string(),
            TokKind::ErrKw => "err".to_string(),
            TokKind::Extern => "extern".to_string(),
            TokKind::For => "for".to_string(),
            TokKind::Func => "func".to_string(),
            TokKind::If => "if".to_string(),
            TokKind::Implies => "implies".to_string(),
            TokKind::In => "in".to_string(),
            TokKind::Invariant => "invariant".to_string(),
            TokKind::Is => "is".to_string(),
            TokKind::Let => "let".to_string(),
            TokKind::Loop => "loop".to_string(),
            TokKind::Match => "match".to_string(),
            TokKind::Mut => "mut".to_string(),
            TokKind::Not => "not".to_string(),
            TokKind::Ok => "ok".to_string(),
            TokKind::Or => "or".to_string(),
            TokKind::Post => "post".to_string(),
            TokKind::Pre => "pre".to_string(),
            TokKind::Recv => "recv".to_string(),
            TokKind::Record => "record".to_string(),
            TokKind::Send => "send".to_string(),
            TokKind::Some => "some".to_string(),
            TokKind::Task => "task".to_string(),
            TokKind::Transfer => "transfer".to_string(),
            TokKind::LParen => "(".to_string(),
            TokKind::RParen => ")".to_string(),
            TokKind::LBracket => "[".to_string(),
            TokKind::RBracket => "]".to_string(),
            TokKind::LBrace => "{".to_string(),
            TokKind::RBrace => "}".to_string(),
            TokKind::Comma => ",".to_string(),
            TokKind::Colon => ":".to_string(),
            TokKind::Dot => ".".to_string(),
            TokKind::Arrow => "->".to_string(),
            TokKind::LeftArrow => "<-".to_string(),
            TokKind::Question => "?".to_string(),
            TokKind::Plus => "+".to_string(),
            TokKind::Minus => "-".to_string(),
            TokKind::Star => "*".to_string(),
            TokKind::Slash => "/".to_string(),
            TokKind::Percent => "%".to_string(),
            TokKind::EqEq => "==".to_string(),
            TokKind::NotEq => "!=".to_string(),
            TokKind::Lt => "<".to_string(),
            TokKind::Le => "<=".to_string(),
            TokKind::Gt => ">".to_string(),
            TokKind::Ge => ">=".to_string(),
            TokKind::Assign => "=".to_string(),
            TokKind::PlusEq => "+=".to_string(),
            TokKind::MinusEq => "-=".to_string(),
            TokKind::StarEq => "*=".to_string(),
            TokKind::SlashEq => "/=".to_string(),
            TokKind::Pipe => "|".to_string(),
            TokKind::Underscore => "_".to_string(),
            TokKind::DocIntent => "DocIntent".to_string(),
            TokKind::DocRequires => "DocRequires".to_string(),
            TokKind::DocEnsures => "DocEnsures".to_string(),
            TokKind::DocEffects => "DocEffects".to_string(),
            TokKind::DocTrusted => "DocTrusted".to_string(),
            TokKind::Eof => "Eof".to_string(),
            _ => "".to_string(),
        }
    }
}