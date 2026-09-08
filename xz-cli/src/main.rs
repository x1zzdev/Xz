use xz_cli::lexer::{lex};
use xz_cli::token::{TokKind};
use xz_cli::parser::{parse};
use xz_cli::resolve::{resolve};

fn main() {
    let args = std::env::args();
    let mut argv: Vec<String> = vec![];
    for a in args {
        argv.push(a);
    }
    if argv.len() < 3 {
        println!("usage: xz <lex|parse|check> <file.xz>");
        return;
    }
    let cmd = argv[1].clone();
    let path = argv[2].clone();
    let source = std::fs::read_to_string(path.clone());
    match source {
        Err(e) => {
            println!("error: cannot read {}: {}", path, e);
        }
        Ok(src) => {
            let result = lex(src, path);
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
                        let parsed = parse(tokens);
                        match parsed {
                            Err(e) => {
                                let (l, c) = e.span.start;
                                println!("error: {} at {}:{}:{}", e.message, e.span.file, l, c);
                            }
                            Ok(program) => {
                                let resolved = resolve(&program);
                                match resolved {
                                    Err(errors) => {
                                        for err in errors {
                                            let (l, c) = err.span.start;
                                            println!("error: {} at {}:{}:{}", err.message, err.span.file, l, c);
                                        }
                                    }
                                    Ok(_) => {
                                        println!("ok: {} top-level declarations, all names resolve", program.items.len());
                                    }
                                }
                            }
                        }
                    } else {
                        println!("unknown command: {}", cmd);
                    }
                }
            }
        }
    }
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