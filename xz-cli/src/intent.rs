use crate::ast;
use crate::ast::{Program, Item, FuncDecl, Expr, Stmt};
use crate::token::{Span, DocTag};

pub struct IntentError {
    pub code: String,
    pub message: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectSet {
    pub io: bool,
    pub chan: bool,
    pub mut_: bool,
    pub extern_: bool,
}

impl EffectSet {
    fn empty() -> EffectSet {
        EffectSet { io: false, chan: false, mut_: false, extern_: false }
    }
    /// Returns the derived effect list, e.g. ["io", "chan"].
    fn labels(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = vec![];
        if self.mut_ { v.push("mut"); }
        if self.io { v.push("io"); }
        if self.chan { v.push("chan"); }
        if self.extern_ { v.push("extern"); }
        v
    }
}



pub struct IntentChecker {
    errors: Vec<IntentError>,
}

pub fn check_intent(program: &Program) -> Result<(), Vec<IntentError>> {
    let mut ic = IntentChecker { errors: vec![] };
    for item in &program.items {
        match item {
            Item::Func(f) => {
                if f.name == "main" { continue; }
                ic.check_func(f);
            }
            Item::Task(t) => {
                match &t.doc {
                    Some(d) => {
                        let mut derived = EffectSet::empty();
                        walk_block(&t.body, &mut derived);
                        ic.check_effects(&d.claims, &t.name, t.span.clone(), derived);
                    }
                    None => ic.error("I0022", format!("missing intent comment on task '{}'", t.name), t.span.clone()),
                }
            }
            _ => {}
        }
    }
    if ic.errors.len() > 0 { return Err(ic.errors); }
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
        // 1. pairing
        let nl_requires = doc.claims.iter().filter(|c| c.tag == DocTag::Requires).count();
        let nl_ensures = doc.claims.iter().filter(|c| c.tag == DocTag::Ensures).count();
        let formal_pre = f.contracts.iter().filter(|c| matches!(c, ast::Contract::Pre(_))).count();
        let formal_post = f.contracts.iter().filter(|c| matches!(c, ast::Contract::Post(_))).count();
        if nl_requires > formal_pre {
            self.error("I0021", format!("'@requires' claim without a paired 'pre' contract on '{}'", f.name), f.span.clone());
        }
        if nl_ensures > formal_post {
            self.error("I0021", format!("'@ensures' claim without a paired 'post' contract on '{}'", f.name), f.span.clone());
        }
        // 2. @trusted placement
        for c in doc.claims.iter() {
            if c.trusted && c.tag != DocTag::Ensures && c.tag != DocTag::Requires {
                self.error("I0003", format!("'@trusted' must attach to an @ensures or @requires claim on '{}'", f.name), c.span.clone());
            }
        }
        // 3. derived effects vs declared
        let mut derived = EffectSet::empty();
        if f.params.iter().any(|p| p.mutable) { derived.mut_ = true; }
        walk_block(&f.body, &mut derived);
        self.check_effects(&doc.claims, &f.name, f.span.clone(), derived);
    }

    fn check_effects(&mut self, claims: &Vec<ast::DocClaim>, name: &str, span: Span, derived: EffectSet) {
        let declared: Vec<String> = claims.iter()
            .filter(|c| c.tag == DocTag::Effects)
            .map(|c| c.text.clone())
            .collect();
        if declared.len() == 0 {
            self.error("I0023", format!("missing @effects declaration on '{}'", name), span);
            return;
        }
        // parse declared labels
        let raw = declared[0].clone();
        let mut declared_set: Vec<&str> = if raw.trim() == "none" {
            vec![]
        } else {
            raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect()
        };
        for p in declared_set.iter() {
            if !["none", "mut", "io", "chan", "extern"].contains(p) {
                self.error("I0024", format!("unknown effect '{}' in @effects on '{}'", p, name), span.clone());
            }
        }
        if declared_set.contains(&"none") {
            declared_set = vec![];
        }
        // compare derived vs declared
        let mut derived_set: Vec<&str> = derived.labels().clone();
        let declared_wo_none: Vec<&str> = declared_set.iter().map(|s| *s).collect();
        // normalize order and dedupe
        let mut decl_plus: Vec<&str> = declared_wo_none.clone();
        decl_plus.sort();
        derived_set.sort();
        if decl_plus != derived_set {
            let d_str = if derived_set.len() == 0 { "none".to_string() } else { derived_set.join(",") };
            let p_str = if decl_plus.len() == 0 { "none".to_string() } else { decl_plus.join(",") };
            self.error("I0020", format!("declared @effects '{p_str}' does not match derived effects '{d_str}' on '{name}'"), span.clone());
        }
    }
}

fn walk_block(block: &ast::Block, set: &mut EffectSet) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl(d) => {
                if d.mutable {
                    set.mut_ = true;
                }
                if d.recv {
                    set.chan = true;
                }
                match &d.init {
                    Some(e) => walk_expr(e, set),
                    None => {}
                }
            }
            Stmt::Assign(a) => {
                set.mut_ = true;
                walk_expr(&a.value, set);
            }
            Stmt::Expr(e) => walk_expr(e, set),
            _ => {}
        }
    }
}

fn walk_expr(e: &Expr, set: &mut EffectSet) {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Char(_) | Expr::Str(_)
        | Expr::RawStr(_) | Expr::Bool(_) | Expr::None => {}
        Expr::Name(_) => {}
        Expr::Call(callee, args) => {
            for a in args { walk_expr(a, set); }
            match &**callee {
                Expr::Name(n) => {
                    if n == "print" {
                        set.io = true;
                    } else if is_extern(n) {
                        set.extern_ = true;
                    }
                }
                Expr::Field(recv, method) => {
                    let _ = recv;
                    // method calls on values are pure unless known io (none in stdlib surface)
                    let _ = method;
                }
                _ => { walk_expr(callee, set); }
            }
        }
        Expr::Field(base, _) => walk_expr(base, set),
        Expr::Index(base, idx) => { walk_expr(base, set); walk_expr(idx, set); }
        Expr::Prop(base, _) => walk_expr(base, set),
        Expr::Unary(op, a) => {
            let _ = op;
            walk_expr(a, set);
        }
        Expr::Binary(op, a, b) => {
            walk_expr(a, set);
            walk_expr(b, set);
            let _ = op;
        }
        Expr::Cast(a, _) => walk_expr(a, set),
        Expr::Match(subject, arms) => {
            walk_expr(subject, set);
            for (_, body) in arms { walk_expr(body, set); }
        }
        Expr::If(ifx) => {
            walk_expr(&ifx.cond, set);
            walk_block(&ifx.then_block, set);
            match &ifx.elif {
                Some((c, b)) => { walk_expr(c, set); walk_block(b, set); }
                None => {}
            }
            match &ifx.else_block {
                Some(b) => walk_block(b, set),
                None => {}
            }
        }
        Expr::Loop(b) => walk_block(b, set),
        Expr::For(_, iter, b) => { walk_expr(iter, set); walk_block(b, set); }
        Expr::Await(a) => walk_expr(a, set),
        Expr::Send(_, _) => { set.chan = true; }
        Expr::Recv(_) => { set.chan = true; }
        Expr::Transfer(a) => walk_expr(a, set),
        Expr::Ok(inner) => { match inner { Some(v) => walk_expr(v, set), None => {} } }
        Expr::Err(a) => walk_expr(a, set),
        Expr::Some(a) => walk_expr(a, set),
    }
}

fn is_extern(name: &str) -> bool {
    ["malloc", "free", "strlen", "memcpy"].contains(&name)
}