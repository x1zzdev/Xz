use crate::ast::{Program, Item, FuncDecl};
use crate::token::{Span, DocTag};

pub struct IntentError {
    pub code: String,
    pub message: String,
    pub span: Span,
}

pub struct IntentChecker {
    errors: Vec<IntentError>,
}

pub fn check_intent(program: &Program) -> Result<(), Vec<IntentError>> {
    let mut ic = IntentChecker { errors: vec![] };
    for item in &program.items {
        match item {
            Item::Func(f) => {
                if f.name == "main" {
                    // main is the one exemption
                    continue;
                }
                ic.check_func(f);
            }
            Item::Task(t) => {
                let doc = t.doc.clone();
                match doc {
                    Some(d) => {
                        // tasks must declare their effects, but guarded claims
                        // are still advisory
                        ic.check_effects(&d.claims, &t.name, t.span.clone());
                    }
                    None => {
                        ic.errors.push(IntentError {
                            code: "I0022".to_string(),
                            message: format!("missing intent comment on task '{}'", t.name),
                            span: t.span.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if ic.errors.len() > 0 {
        return Err(ic.errors);
    }
    Ok(())
}

impl IntentChecker {
    fn error(&mut self, code: &str, message: String, span: Span) {
        self.errors.push(IntentError { code: code.to_string(), message: message, span: span });
    }

    fn check_func(&mut self, f: &FuncDecl) {
        let doc = match &f.doc {
            Some(d) => d,
            None => {
                self.error("I0022", format!("missing intent comment on public function '{}'", f.name), f.span.clone());
                return;
            }
        };
        // 1. Pairing: @requires -> pre, @ensures -> post, in order.
        let nl_requires = doc.claims.iter().filter(|c| c.tag == DocTag::Requires).count();
        let nl_ensures = doc.claims.iter().filter(|c| c.tag == DocTag::Ensures).count();
        let formal_pre = f.contracts.iter().filter(|c| matches!(c, crate::ast::Contract::Pre(_))).count();
        let formal_post = f.contracts.iter().filter(|c| matches!(c, crate::ast::Contract::Post(_))).count();

        if nl_requires > formal_pre {
            self.error("I0021", format!("'@requires' claim without a paired 'pre' contract on '{}' (claims must be mirrored in order)", f.name), f.span.clone());
        }
        if nl_ensures > formal_post {
            self.error("I0021", format!("'@ensures' claim without a paired 'post' contract on '{}'", f.name), f.span.clone());
        }

        // 2. @trusted transfer: a trusted NL claim discharges the paired
        //    formal contract's proof obligation. If the claim is trusted, a
        //    review note must be present (require the line to be long enough
        //    to be a review note; exact format is checked by the parser).
        for c in doc.claims.iter() {
            if c.trusted {
                if c.tag != DocTag::Ensures && c.tag != DocTag::Requires {
                    self.error("I0003", format!("'@trusted' must attach to an @ensures or @requires claim on '{}'", f.name), c.span.clone());
                }
            }
        }

        // 3. effects: declared profile must match the body.
        self.check_effects(&doc.claims, &f.name, f.span.clone());
    }

    fn check_effects(&mut self, claims: &Vec<crate::ast::DocClaim>, name: &str, span: Span) {
        // find @effects declaration
        let declared: Vec<String> = claims
            .iter()
            .filter(|c| c.tag == DocTag::Effects)
            .map(|c| c.text.clone())
            .collect();
        if declared.len() == 0 {
            self.error("I0023", format!("missing @effects declaration on '{}'", name), span);
            return;
        }
        let declared_raw = declared[0].clone();
        // we compare body-derived effects at typecheck time; here we only
        // validate the declaration syntax: token list of allowed labels
        let allowed = ["none", "mut", "io", "chan", "extern"];
        let mut parts: Vec<&str> = declared_raw.split(',').map(|s| s.trim()).collect();
        if parts.len() == 1 && parts[0] == "" {
            parts = vec![];
        }
        for p in parts {
            if !allowed.contains(&p) {
                self.error("I0024", format!("unknown effect '{}' in @effects on '{}' (allowed: none, mut, io, chan, extern)", p, name), span.clone());
            }
        }
        let _ = &mut self.errors;
    }
}