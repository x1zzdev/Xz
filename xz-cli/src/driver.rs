//! Compiler driver: run the front-end pipeline over source text and collect
//! structured diagnostics. Shared by `xz check`/`xz check-json` and the
//! language server (`xz lsp`), so both report exactly the same errors.
use crate::diagnostic::{Category, Diagnostic, Severity, Suggestion, Span as DSpan};
use crate::intent::{check_intent, check_intent_strict};
use crate::lexer::{lex};
use crate::parser::{parse};
use crate::resolve::{resolve};
use crate::token::{Span, Token};
use crate::typecheck::{typecheck};

/// Lex + parse + resolve + typecheck + intent-check a source string. Returns
/// every diagnostic found in pipeline order (a lexer error short-circuits).
pub fn check(source: &str, file: &str, strict: bool) -> Vec<Diagnostic> {
    match lex(source.to_string(), file.to_string()) {
        Err(e) => vec![error("L0001", e.message, Category::Lex, e.span)],
        Ok(tokens) => check_tokens(tokens, strict),
    }
}

/// The pipeline stages from parsing onward, for callers that already lexed
/// (e.g. `xz check`, which reports a lexer error in its own format first).
pub fn check_tokens(tokens: Vec<Token>, strict: bool) -> Vec<Diagnostic> {
    let mut diags: Vec<Diagnostic> = vec![];
    match parse(tokens) {
        Err(e) => diags.push(error("P0001", e.message, Category::Parse, e.span)),
        Ok(program) => match resolve(&program) {
            Err(errors) => {
                for err in errors {
                    diags.push(error("R0001", err.message, Category::Resolve, err.span));
                }
            }
            Ok(_) => match typecheck(&program) {
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
                    if let Err(errors) = intent {
                        for err in errors {
                            let suggestion: Option<Suggestion> = err
                                .suggestion
                                .as_ref()
                                .map(|s| Suggestion { fix: s.fix.clone(), confidence: s.confidence });
                            diags.push(Diagnostic {
                                version: 1,
                                severity: Severity::Error,
                                code: err.code.clone(),
                                message: err.message.clone(),
                                category: Category::Intent,
                                span: dspan(err.span),
                                suggestion,
                            });
                        }
                    }
                }
            },
        },
    }
    diags
}

fn error(code: &str, message: String, category: Category, span: Span) -> Diagnostic {
    Diagnostic {
        version: 1,
        severity: Severity::Error,
        code: code.to_string(),
        message,
        category,
        span: dspan(span),
        suggestion: None,
    }
}

fn dspan(s: Span) -> DSpan {
    DSpan { file: s.file, start: s.start, end: s.end }
}
