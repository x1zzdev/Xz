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
        println!("usage: xz <lex|parse|check> [--strict] <file.xz>");
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
        println!("usage: xz <lex|parse|check> [--strict] <file.xz>");
        return;
    }
    let source = std::fs::read_to_string(path.clone());
    match source {
        Err(e) => {
            println!("error: cannot read {}: {}", path, e);
        }
        Ok(src) => {
            let result = lex(src, path.clone());
            match result {
                Err(e) => {
                    let (l, c) = e.span.start;
                    println!("error: {} at {}:{}:{}", e.message, e.span.file, l, c);
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
                            }
                            Ok(program) => {
                                println!("ok: parsed {} top-level declarations", program.items.len());
                            }
                        }
                    } else if cmd == "check" {
                        run_check(tokens, strict, false);
                    } else if cmd == "check-json" {
                        run_check(tokens, strict, true);
                    } else {
                        println!("unknown command: {}", cmd);
                    }
                }
            }
        }
    }
}

fn run_check(tokens: Vec<Token>, strict: bool, json: bool) {
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
    if json {
        println!("{}", to_json_array(&diags));
    } else if diags.len() > 0 {
        for d in diags {
            let (sl, sc) = d.span.start;
            println!("  [{}] {} at {}:{}:{}", d.code, d.message, d.span.file, sl, sc);
        }
    } else {
        println!("ok: all checks passed");
    }
}

fn dspan(s: Span) -> DSpan {
    DSpan { file: s.file, start: s.start, end: s.end }
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