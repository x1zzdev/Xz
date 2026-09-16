use std::collections::{HashMap, HashSet};

use crate::ast;
use crate::ast::{Program, Item, FuncDecl, Expr, Stmt};
use crate::token::{Span, DocTag};

pub struct IntentError {
    pub code: String,
    pub message: String,
    pub span: Span,
    pub suggestion: Option<IntentSuggestion>,
}

pub struct IntentSuggestion {
    pub fix: String,
    pub confidence: f64,
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

    fn merge(&mut self, other: &EffectSet) {
        self.io = self.io || other.io;
        self.chan = self.chan || other.chan;
        self.mut_ = self.mut_ || other.mut_;
        self.extern_ = self.extern_ || other.extern_;
    }

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
    /// callee effects: declared @effects profiles for transitively called
    /// functions, plus the declared `extern` names (which carry the `extern`
    /// effect regardless of their name)
    callees: CalleeEffects,
    /// strict mode: untrusted claims (trusted without a review note) block the build
    strict: bool,
}

#[derive(Default)]
struct CalleeEffects {
    declared: HashMap<String, EffectSet>,
    externs: HashSet<String>,
}

pub fn check_intent(program: &Program) -> Result<(), Vec<IntentError>> {
    run(program, false)
}

pub fn check_intent_strict(program: &Program) -> Result<(), Vec<IntentError>> {
    run(program, true)
}

fn run(program: &Program, strict: bool) -> Result<(), Vec<IntentError>> {
    let mut ic = IntentChecker { errors: vec![], callees: CalleeEffects::default(), strict: strict };
    ic.collect_declared(program);
    ic.collect_externs(program);
    for item in &program.items {
        match item {
            Item::Func(f) => {
                if f.name == "main" { continue; }
                ic.check_func(f);
            }
            Item::Task(t) => {
                ic.check_task(t);
            }
            _ => {}
        }
    }
    if ic.errors.len() > 0 { return Err(ic.errors); }
    Ok(())
}

impl IntentChecker {
    /// First pass: register each function's declared @effects so calls can
    /// propagate transitively (09: derived profile = self effects union
    /// callees').
    fn collect_declared(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::Func(f) = item {
                if let Some(d) = &f.doc {
                    let set = effects_from_doc(d);
                    self.callees.declared.insert(f.name.clone(), set);
                }
            }
        }
    }

    /// Every `extern` declaration carries the `extern` effect when called
    /// (docs/10): the C ABI boundary is a side-effect escape hatch. Derived
    /// effects therefore key on the declared signatures, not a name list.
    fn collect_externs(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::Extern(e) = item {
                self.callees.externs.insert(e.name.clone());
            }
        }
    }

    fn error(&mut self, code: &str, message: String, span: Span) {
        self.errors.push(IntentError { code: code.to_string(), message: message, span: span, suggestion: None });
    }

    fn error_suggest(&mut self, code: &str, message: String, span: Span, fix: String, confidence: f64) {
        self.errors.push(IntentError {
            code: code.to_string(),
            message: message,
            span: span,
            suggestion: Some(IntentSuggestion { fix: fix, confidence: confidence }),
        });
    }

    fn check_task(&mut self, t: &ast::TaskDecl) {
        let doc = match &t.doc {
            Some(d) => d,
            None => {
                self.error("I0022", format!("missing intent comment on task '{}'", t.name), t.span.clone());
                return;
            }
        };
        let mut derived = EffectSet::empty();
        walk_block(&t.body, &mut derived, &self.callees);
        self.check_effects(&doc.claims, &t.name, t.span.clone(), derived);
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
            if c.trusted {
                if c.tag != DocTag::Ensures && c.tag != DocTag::Requires {
                    self.error("I0003", format!("'@trusted' must attach to an @ensures or @requires claim on '{}'", f.name), c.span.clone());
                }
                if self.strict && !c.reviewed {
                    self.error("I0004", format!("@trusted claim on '{}' is untrusted: missing review note ('// reviewed by <who> on <date>')", f.name), c.span.clone());
                }
            }
        }
        // 3. derived effects (transitive) vs declared
        let mut derived = EffectSet::empty();
        if f.params.iter().any(|p| p.mutable) { derived.mut_ = true; }
        walk_block(&f.body, &mut derived, &self.callees);
        self.check_effects(&doc.claims, &f.name, f.span.clone(), derived);
    }

    fn check_effects(&mut self, claims: &Vec<ast::DocClaim>, name: &str, span: Span, derived: EffectSet) {
        let declared_text: Vec<String> = claims.iter()
            .filter(|c| c.tag == DocTag::Effects)
            .map(|c| c.text.clone())
            .collect();
        if declared_text.len() == 0 {
            self.error("I0023", format!("missing @effects declaration on '{}'", name), span);
            return;
        }
        let raw = declared_text[0].clone();
        let declared: EffectSet = effect_set_from_text(&raw);
        if !is_valid_effects(&raw) {
            self.error("I0024", format!("unknown effect in @effects on '{}' (allowed: none, mut, io, chan, extern)", name), span.clone());
        }
        let mut dl = declared.labels();
        let mut dd = derived.labels();
        dl.sort();
        dd.sort();
        if dl != dd {
            let d_str = if dd.len() == 0 { "none".to_string() } else { dd.join(",") };
            let p_str = if dl.len() == 0 { "none".to_string() } else { dl.join(",") };
            let fix = if dd.len() > dl.len() {
                format!("extend @effects on '{}' to include '{}'", name, d_str)
            } else {
                format!("narrow @effects on '{}' to '{}' (or remove the effect)", name, d_str)
            };
            self.error_suggest("I0020", format!("declared @effects '{p_str}' does not match derived effects '{d_str}' on '{name}'"), span, fix, 0.9);
        }
    }
}

/// Parse the labels out of a doc comment's @effects lines into a set.
fn effects_from_doc(d: &ast::DocComment) -> EffectSet {
    let mut set = EffectSet::empty();
    for c in d.claims.iter() {
        if c.tag == DocTag::Effects {
            set = effect_set_from_text(&c.text);
        }
    }
    set
}

fn effect_set_from_text(text: &str) -> EffectSet {
    let mut set = EffectSet::empty();
    let t = text.trim();
    if t == "none" || t == "" { return set; }
    for part in t.split(',') {
        match part.trim() {
            "mut" => set.mut_ = true,
            "io" => set.io = true,
            "chan" => set.chan = true,
            "extern" => set.extern_ = true,
            _ => {}
        }
    }
    set
}

fn is_valid_effects(text: &str) -> bool {
    let t = text.trim();
    if t == "none" || t == "" { return true; }
    t.split(',').all(|p| ["mut", "io", "chan", "extern"].contains(&p.trim()))
}

fn walk_block(block: &ast::Block, set: &mut EffectSet, callees: &CalleeEffects) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl(d) => {
                if d.mutable { set.mut_ = true; }
                if d.recv { set.chan = true; }
                match &d.init {
                    Some(e) => walk_expr(e, set, callees),
                    None => {}
                }
            }
            Stmt::Assign(a) => {
                set.mut_ = true;
                walk_expr(&a.value, set, callees);
            }
            Stmt::Expr(e) => walk_expr(e, set, callees),
            _ => {}
        }
    }
}

fn walk_expr(e: &Expr, set: &mut EffectSet, callees: &CalleeEffects) {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Char(_) | Expr::Str(_)
        | Expr::RawStr(_) | Expr::Bool(_) | Expr::None | Expr::Name(_) => {}
        Expr::Call(callee, args) => {
            for a in args { walk_expr(a, set, callees); }
            match &**callee {
                Expr::Name(n) => {
                    if n == "print" || n == "read_file" || n == "now" || n == "monotonic" {
                        set.io = true;
                    } else if let Some(callee_effects) = callees.declared.get(n) {
                        set.merge(callee_effects);
                    } else if callees.externs.contains(n) {
                        set.extern_ = true;
                    }
                }
                Expr::Field(_, _) => {
                    // method calls on values are pure in the stdlib surface
                }
                _ => { walk_expr(callee, set, callees); }
            }
        }
        Expr::Field(base, _) => walk_expr(base, set, callees),
        Expr::Index(base, idx) => { walk_expr(base, set, callees); walk_expr(idx, set, callees); }
        Expr::ListLit(elems) => { for e in elems { walk_expr(e, set, callees); } }
        Expr::MapLit(entries) => { for (k, v) in entries { walk_expr(k, set, callees); walk_expr(v, set, callees); } }
        Expr::SetLit(elems) => { for e in elems { walk_expr(e, set, callees); } }
        Expr::Prop(base, _) => walk_expr(base, set, callees),
        Expr::Unary(_, a) => walk_expr(a, set, callees),
        Expr::Binary(_, a, b) => { walk_expr(a, set, callees); walk_expr(b, set, callees); }
        Expr::Cast(a, _) => walk_expr(a, set, callees),
        Expr::Match(subject, arms) => {
            walk_expr(subject, set, callees);
            for (_, body) in arms { walk_expr(body, set, callees); }
        }
        Expr::If(ifx) => {
            walk_expr(&ifx.cond, set, callees);
            walk_block(&ifx.then_block, set, callees);
            for (c, b) in &ifx.elif {
                walk_expr(c, set, callees);
                walk_block(b, set, callees);
            }
            match &ifx.else_block {
                Some(b) => walk_block(b, set, callees),
                None => {}
            }
        }
        Expr::Loop(b) => walk_block(b, set, callees),
        Expr::For(_, iter, b) => { walk_expr(iter, set, callees); walk_block(b, set, callees); }
        Expr::Await(a) => walk_expr(a, set, callees),
        Expr::Send(_, _) => { set.chan = true; }
        Expr::Recv(_) => { set.chan = true; }
        Expr::Transfer(a) => walk_expr(a, set, callees),
        Expr::Ok(inner) => { match inner { Some(v) => walk_expr(v, set, callees), None => {} } }
        Expr::Err(a) => walk_expr(a, set, callees),
        Expr::Some(a) => walk_expr(a, set, callees),
    }
}