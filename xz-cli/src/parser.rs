use crate::ast::{Program, Item, FuncDecl, TaskDecl, ChanDecl, ExternDecl, RecordDecl, EnumDecl};
use crate::ast::{Param, Field, Variant, TypeParam, Contract, Block, Stmt, Decl, Assign, AssignTarget, AssignOp};
use crate::ast::{DocComment, DocClaim, Type, Expr, IfExpr, Pattern, UnaryOp, BinOp, PropKind};
use crate::token::{TokKind, Token, Span, DocTag};

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

pub fn parse(tokens: Vec<Token>) -> Result<Program, ParseError> {
    let mut p = Parser { tokens: tokens, i: 0 };
    p.run()
}

struct Parser {
    tokens: Vec<Token>,
    i: usize,
}

impl Parser {
    fn peek(&self, offset: usize) -> &TokKind {
        let mut idx = self.i + offset;
        if idx >= self.tokens.len() {
            idx = self.tokens.len() - 1;
        }
        &self.tokens[idx].kind
    }

    fn cur(&self) -> &TokKind {
        self.peek(0)
    }

    fn at(&self, kind: TokKind) -> bool {
        self.cur() == &kind
    }

    fn eat(&mut self, kind: TokKind) -> Option<Token> {
        if self.at(kind) {
            let tok = self.tokens[self.i].clone();
            self.i += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn expect(&mut self, kind: TokKind, what: String) -> Result<Token, ParseError> {
        let tok = self.eat(kind);
        match tok {
            Some(t) => Ok(t),
            None => {
                let cur = self.tokens[self.i].clone();
                Err(ParseError { message: format!("expected {what}, found {}", tok_show(&cur.kind)), span: cur.span })
            }
        }
    }

    fn expect_ident(&mut self) -> Result<Token, ParseError> {
        match self.peek(0) {
            TokKind::Ident(_) => {
                let tok = self.tokens[self.i].clone();
                self.i += 1;
                Ok(tok)
            }
            _ => {
                let cur = self.tokens[self.i].clone();
                Err(ParseError { message: format!("expected identifier, found {}", tok_show(&cur.kind)), span: cur.span })
            }
        }
    }

    fn run(&mut self) -> Result<Program, ParseError> {
        let mut items: Vec<Item> = vec![];
        while !self.at(TokKind::Eof) {
            let item = self.top_level();
            match item {
                Err(e) => return Err(e),
                Ok(it) => items.push(it),
            }
        }
        Ok(Program { items: items })
    }

    fn top_level(&mut self) -> Result<Item, ParseError> {
        // doc comment group precedes func/task
        let mut docs: Vec<DocClaim> = vec![];
        while self.is_doc_token() {
            let claim = self.doc_claim();
            match claim {
                Err(e) => return Err(e),
                Ok(c) => docs.push(c),
            }
        }

        match self.peek(0) {
            TokKind::Func => {
                let decl = self.func_decl(docs)?;
                Ok(Item::Func(decl))
            }
            TokKind::Async => {
                let decl = self.func_decl(docs)?;
                Ok(Item::Func(decl))
            }
            TokKind::Task => {
                let decl = self.task_decl(docs)?;
                Ok(Item::Task(decl))
            }
            TokKind::Chan => {
                let decl = self.chan_decl()?;
                Ok(Item::Chan(decl))
            }
            TokKind::Extern => {
                let decl = self.extern_decl()?;
                Ok(Item::Extern(decl))
            }
            TokKind::Record => {
                let decl = self.record_decl()?;
                Ok(Item::Record(decl))
            }
            TokKind::Enum => {
                let decl = self.enum_decl()?;
                Ok(Item::Enum(decl))
            }
            _ => {
                let cur = self.tokens[self.i].clone();
                Err(ParseError { message: format!("unexpected {} at top level", tok_show(&cur.kind)), span: cur.span })
            }
        }
    }

    fn is_doc_token(&self) -> bool {
        match self.peek(0) {
            TokKind::DocIntent => true,
            TokKind::DocRequires => true,
            TokKind::DocEnsures => true,
            TokKind::DocEffects => true,
            TokKind::DocTrusted => true,
            _ => false,
        }
    }

    fn doc_claim(&mut self) -> Result<DocClaim, ParseError> {
        let tok = self.tokens[self.i].clone();
        self.i += 1;
        let tag = match &tok.kind {
            TokKind::DocIntent => DocTag::Intent,
            TokKind::DocRequires => DocTag::Requires,
            TokKind::DocEnsures => DocTag::Ensures,
            TokKind::DocEffects => DocTag::Effects,
            TokKind::DocTrusted => DocTag::Trusted,
            _ => DocTag::Intent,
        };
        // A claim line may carry an inline `@trusted` stamp before the
// review note. The lexer folded the whole line into tok.text, so detect
// the marker here (it also keeps DocTrusted tokens for standalone use).
let mut trusted = false;
        let mut text = tok.text.clone();
        if text.ends_with("@trusted") || text.contains(" @trusted") {
            trusted = true;
            if let Some(pos) = text.find("@trusted") {
                text = String::from(text[..pos].trim());
            }
        }
        let reviewed = trusted && tok.text.contains("// reviewed by");
        Ok(DocClaim { tag: tag, text: text, trusted: trusted, reviewed: reviewed, span: tok.span })
    }

    fn func_decl(&mut self, docs: Vec<DocClaim>) -> Result<FuncDecl, ParseError> {
        let is_async = self.eat(TokKind::Async).is_some();
        self.expect(TokKind::Func, String::from("'func'"))?;
        let name_tok = self.expect_ident()?;
        let type_params = self.type_params()?;
        self.expect(TokKind::LParen, String::from("'('"))?;
        let params = self.params()?;
        self.expect(TokKind::RParen, String::from("')'"))?;
        let ret = if self.at(TokKind::Arrow) {
            self.eat(TokKind::Arrow);
            Some(self.ty()?)
        } else {
            None
        };
        let contracts = self.contracts()?;
        let body = self.block()?;
        let doc = if docs.len() > 0 {
            Some(DocComment { claims: docs, span: name_tok.span.clone() })
        } else {
            None
        };
        Ok(FuncDecl { is_async: is_async, name: name_tok.text.clone(), type_params: type_params, params: params, ret: ret, contracts: contracts, body: body, doc: doc, span: name_tok.span })
    }

    fn type_params(&mut self) -> Result<Vec<TypeParam>, ParseError> {
        let mut out: Vec<TypeParam> = vec![];
        if !self.at(TokKind::LBracket) {
            return Ok(out);
        }
        self.i += 1;
        loop {
            let name_tok = self.expect_ident()?;
            let mut constraint: Option<String> = None;
            if self.eat(TokKind::Colon).is_some() {
                let c_tok = self.expect_ident()?;
                constraint = Some(c_tok.text.clone());
            }
            out.push(TypeParam { name: name_tok.text.clone(), constraint: constraint, span: name_tok.span });
            if !self.eat(TokKind::Comma).is_some() {
                break;
            }
        }
        self.expect(TokKind::RBracket, String::from("']'"))?;
        Ok(out)
    }

    fn task_decl(&mut self, docs: Vec<DocClaim>) -> Result<TaskDecl, ParseError> {
        self.expect(TokKind::Task, String::from("'task'"))?;
        let name_tok = self.expect_ident()?;
        let body = self.block()?;
        let doc = if docs.len() > 0 {
            Some(DocComment { claims: docs, span: name_tok.span.clone() })
        } else {
            None
        };
        Ok(TaskDecl { name: name_tok.text.clone(), body: body, doc: doc, span: name_tok.span })
    }

    fn chan_decl(&mut self) -> Result<ChanDecl, ParseError> {
        let start = self.tokens[self.i].span.clone();
        self.expect(TokKind::Chan, String::from("'chan'"))?;
        let name_tok = self.expect_ident()?;
        self.expect(TokKind::Colon, String::from("':'"))?;
        let chan_name = self.expect_ident()?;   // 'Chan' — a type name, not a keyword
        if chan_name.text != "Chan" {
            let span = chan_name.span;
            return Err(ParseError { message: format!("expected 'Chan', found '{}'", chan_name.text), span: span });
        }
        self.expect(TokKind::LBracket, String::from("'['"))?;
        let payload = self.ty()?;
        self.expect(TokKind::RBracket, String::from("']'"))?;
        Ok(ChanDecl { name: name_tok.text.clone(), payload: payload, span: start })
    }

    fn extern_decl(&mut self) -> Result<ExternDecl, ParseError> {
        let start = self.tokens[self.i].span.clone();
        self.expect(TokKind::Extern, String::from("'extern'"))?;
        self.expect(TokKind::Func, String::from("'func'"))?;
        let name_tok = self.expect_ident()?;
        let type_params = self.type_params()?;
        self.expect(TokKind::LParen, String::from("'('"))?;
        let params = self.params()?;
        self.expect(TokKind::RParen, String::from("')'"))?;
        let ret = if self.at(TokKind::Arrow) {
            self.eat(TokKind::Arrow);
            Some(self.ty()?)
        } else {
            None
        };
        Ok(ExternDecl { name: name_tok.text.clone(), type_params: type_params, params: params, ret: ret, span: start })
    }

    fn record_decl(&mut self) -> Result<RecordDecl, ParseError> {
        let start = self.tokens[self.i].span.clone();
        self.expect(TokKind::Record, String::from("'record'"))?;
        let name_tok = self.expect_ident()?;
        self.expect(TokKind::LBrace, String::from("'{'"))?;
        let mut fields: Vec<Field> = vec![];
        while !self.at(TokKind::RBrace) {
            let f = self.field()?;
            fields.push(f);
        }
        self.expect(TokKind::RBrace, String::from("'}'"))?;
        Ok(RecordDecl { name: name_tok.text.clone(), fields: fields, span: start })
    }

    fn enum_decl(&mut self) -> Result<EnumDecl, ParseError> {
        let start = self.tokens[self.i].span.clone();
        self.expect(TokKind::Enum, String::from("'enum'"))?;
        let name_tok = self.expect_ident()?;
        self.expect(TokKind::LBrace, String::from("'{'"))?;
        let mut variants: Vec<Variant> = vec![];
        while !self.at(TokKind::RBrace) {
            let vtok = self.expect_ident()?;
            self.expect(TokKind::LParen, String::from("'('"))?;
            let mut fields: Vec<Field> = vec![];
            if !self.at(TokKind::RParen) {
                loop {
                    let f = self.field()?;
                    fields.push(f);
                    if !self.eat(TokKind::Comma).is_some() {
                        break;
                    }
                }
            }
            self.expect(TokKind::RParen, String::from("')'"))?;
            variants.push(Variant { name: vtok.text.clone(), fields: fields, span: vtok.span });
        }
        self.expect(TokKind::RBrace, String::from("'}'"))?;
        Ok(EnumDecl { name: name_tok.text.clone(), variants: variants, span: start })
    }

    fn params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut out: Vec<Param> = vec![];
        if self.at(TokKind::RParen) {
            return Ok(out);
        }
        loop {
            let mutable = self.eat(TokKind::Mut).is_some();
            let name_tok = self.expect_ident()?;
            self.expect(TokKind::Colon, String::from("':'"))?;
            let ty = self.ty()?;
            out.push(Param { mutable: mutable, name: name_tok.text.clone(), ty: ty, span: name_tok.span });
            if !self.eat(TokKind::Comma).is_some() {
                break;
            }
        }
        Ok(out)
    }

    fn field(&mut self) -> Result<Field, ParseError> {
        let name_tok = self.expect_ident()?;
        self.expect(TokKind::Colon, String::from("':'"))?;
        let ty = self.ty()?;
        Ok(Field { name: name_tok.text.clone(), ty: ty, span: name_tok.span })
    }

    fn contracts(&mut self) -> Result<Vec<Contract>, ParseError> {
        let mut out: Vec<Contract> = vec![];
        loop {
            if self.at(TokKind::Pre) {
                self.i += 1;
                let e = self.expr()?;
                out.push(Contract::Pre(e));
            } else if self.at(TokKind::Post) {
                self.i += 1;
                let e = self.expr()?;
                out.push(Contract::Post(e));
            } else if self.at(TokKind::Invariant) {
                self.i += 1;
                let e = self.expr()?;
                out.push(Contract::Invariant(e));
            } else {
                break;
            }
        }
        Ok(out)
    }

    fn block(&mut self) -> Result<Block, ParseError> {
        let start = self.tokens[self.i].span.clone();
        self.expect(TokKind::LBrace, String::from("'{'"))?;
        let mut stmts: Vec<Stmt> = vec![];
        while !self.at(TokKind::RBrace) {
            if self.at(TokKind::Eof) {
                let cur = self.tokens[self.i].clone();
                return Err(ParseError { message: String::from("unterminated block: missing '}'"), span: cur.span });
            }
            let stmt = self.statement()?;
            stmts.push(stmt);
        }
        self.expect(TokKind::RBrace, String::from("'}'"))?;
        Ok(Block { stmts: stmts, span: start })
    }

    fn statement(&mut self) -> Result<Stmt, ParseError> {
        match self.peek(0) {
            TokKind::Let => {
                let d = self.decl(true)?;
                Ok(Stmt::Decl(d))
            }
            TokKind::Mut => {
                let d = self.decl(false)?;
                Ok(Stmt::Decl(d))
            }
            TokKind::Break => {
                self.i += 1;
                Ok(Stmt::Break)
            }
            TokKind::Continue => {
                self.i += 1;
                Ok(Stmt::Continue)
            }
            _ => {
                // assignment or expression statement
                let e = self.expr()?;
                if self.at(TokKind::Assign) || self.at(TokKind::PlusEq) || self.at(TokKind::MinusEq) || self.at(TokKind::StarEq) || self.at(TokKind::SlashEq) {
                    let assign = self.assign_tail(e)?;
                    Ok(Stmt::Assign(assign))
                } else {
                    Ok(Stmt::Expr(e))
                }
            }
        }
    }

    fn decl(&mut self, is_let: bool) -> Result<Decl, ParseError> {
        let start = self.tokens[self.i].span.clone();
        self.i += 1; // let/mut
        let name_tok = self.expect_ident()?;
        let mut ty: Option<Type> = None;
        let mut init: Option<Box<Expr>> = None;
        let mut recv = false;
        let mut chan: Option<String> = None;
        if self.eat(TokKind::Colon).is_some() {
            ty = Some(self.ty()?);
        }
        if self.at(TokKind::LeftArrow) {
            self.i += 1;
            self.expect(TokKind::Recv, String::from("'recv'"))?;
            self.expect(TokKind::LParen, String::from("'('"))?;
            let chan_tok = self.expect_ident()?;
            self.expect(TokKind::RParen, String::from("')'"))?;
            recv = true;
            chan = Some(chan_tok.text.clone());
        } else if self.eat(TokKind::Assign).is_some() {
            let e = self.expr()?;
            init = Some(Box::new(e));
        }
        Ok(Decl { mutable: !is_let, name: name_tok.text.clone(), ty: ty, init: init, recv: recv, chan: chan, span: start })
    }

    fn assign_tail(&mut self, e: Expr) -> Result<Assign, ParseError> {
        let start = self.tokens[self.i].span.clone();
        let op_tok = self.tokens[self.i].kind.clone();
        self.i += 1;
        let value = self.expr()?;
        let target = match &e {
            Expr::Name(n) => AssignTarget::Name(n.clone()),
            Expr::Field(base, name) => AssignTarget::Field((*base).clone(), name.clone()),
            _ => return Err(ParseError { message: String::from("invalid assignment target"), span: start }),
        };
        let op = match &op_tok {
            TokKind::Assign => AssignOp::Set,
            TokKind::PlusEq => AssignOp::Add,
            TokKind::MinusEq => AssignOp::Sub,
            TokKind::StarEq => AssignOp::Mul,
            TokKind::SlashEq => AssignOp::Div,
            _ => AssignOp::Set,
        };
        Ok(Assign { target: target, op: op, value: Box::new(value), span: start })
    }

    // ---- types ----

    fn ty(&mut self) -> Result<Type, ParseError> {
        let first = self.nominal()?;
        if self.at(TokKind::Pipe) {
            let mut members: Vec<Type> = vec![first];
            while self.eat(TokKind::Pipe).is_some() {
                members.push(self.nominal()?);
            }
            Ok(Type::Union(members))
        } else {
            Ok(first)
        }
    }

    fn nominal(&mut self) -> Result<Type, ParseError> {
        let tok = self.tokens[self.i].clone();
        match &tok.kind {
            TokKind::Ident(name) => {
                self.i += 1;
                if self.at(TokKind::LBracket) {
                    self.i += 1;
                    let mut args: Vec<Type> = vec![];
                    loop {
                        args.push(self.ty()?);
                        if !self.eat(TokKind::Comma).is_some() {
                            break;
                        }
                    }
                    self.expect(TokKind::RBracket, String::from("']'"))?;
                    Ok(Type::Named(name.clone(), args))
                } else {
                    Ok(Type::Named(name.clone(), vec![]))
                }
            }
            _ => Err(ParseError { message: format!("expected type name, found {}", tok_show(&tok.kind)), span: tok.span }),
        }
    }

    // ---- expressions (precedence climbing per 11-grammar) ----

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.contract_expr()
    }

    fn contract_expr(&mut self) -> Result<Expr, ParseError> {
        let left = self.or_expr()?;
        if self.at(TokKind::Implies) {
            self.i += 1;
            let right = self.contract_expr()?;
            Ok(Expr::Binary(BinOp::Implies, Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.and_expr()?;
        while self.at(TokKind::Or) {
            self.i += 1;
            let right = self.and_expr()?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.not_expr()?;
        while self.at(TokKind::And) {
            self.i += 1;
            let right = self.not_expr()?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> Result<Expr, ParseError> {
        if self.at(TokKind::Not) {
            self.i += 1;
            let e = self.not_expr()?;
            Ok(Expr::Unary(UnaryOp::Not, Box::new(e)))
        } else {
            self.cast_expr()
        }
    }

    fn cast_expr(&mut self) -> Result<Expr, ParseError> {
        let e = self.comparison()?;
        if self.at(TokKind::As) {
            self.i += 1;
            let ty = self.ty()?;
            Ok(Expr::Cast(Box::new(e), ty))
        } else {
            Ok(e)
        }
    }

    fn comparison(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.additive()?;
        loop {
            if self.at(TokKind::EqEq) {
                self.i += 1;
                let right = self.additive()?;
                left = Expr::Binary(BinOp::Eq, Box::new(left), Box::new(right));
            } else if self.at(TokKind::NotEq) {
                self.i += 1;
                let right = self.additive()?;
                left = Expr::Binary(BinOp::Ne, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Lt) {
                self.i += 1;
                let right = self.additive()?;
                left = Expr::Binary(BinOp::Lt, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Le) {
                self.i += 1;
                let right = self.additive()?;
                left = Expr::Binary(BinOp::Le, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Gt) {
                self.i += 1;
                let right = self.additive()?;
                left = Expr::Binary(BinOp::Gt, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Ge) {
                self.i += 1;
                let right = self.additive()?;
                left = Expr::Binary(BinOp::Ge, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Is) {
                self.i += 1;
                let tag_tok = self.tokens[self.i].kind.clone();
                self.i += 1;
                let op = match &tag_tok {
                    TokKind::Ok => BinOp::IsOk,
                    TokKind::ErrKw => BinOp::IsErr,
                    TokKind::None => BinOp::IsNone,
                    TokKind::Some => BinOp::IsSome,
                    _ => BinOp::IsOk,
                };
                left = Expr::Binary(op, Box::new(left), Box::new(Expr::Bool(true)));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.multiplicative()?;
        loop {
            if self.at(TokKind::Plus) {
                self.i += 1;
                let right = self.multiplicative()?;
                left = Expr::Binary(BinOp::Add, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Minus) {
                self.i += 1;
                let right = self.multiplicative()?;
                left = Expr::Binary(BinOp::Sub, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.unary()?;
        loop {
            if self.at(TokKind::Star) {
                self.i += 1;
                let right = self.unary()?;
                left = Expr::Binary(BinOp::Mul, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Slash) {
                self.i += 1;
                let right = self.unary()?;
                left = Expr::Binary(BinOp::Div, Box::new(left), Box::new(right));
            } else if self.at(TokKind::Percent) {
                self.i += 1;
                let right = self.unary()?;
                left = Expr::Binary(BinOp::Mod, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.at(TokKind::Minus) {
            self.i += 1;
            let e = self.unary()?;
            Ok(Expr::Unary(UnaryOp::Neg, Box::new(e)))
        } else {
            self.postfix()
        }
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.primary()?;
        loop {
            if self.at(TokKind::LParen) {
                let args = self.args()?;
                e = Expr::Call(Box::new(e), args);
            } else if self.at(TokKind::Dot) {
                self.i += 1;
                let name_tok = self.expect_ident()?;
                e = Expr::Field(Box::new(e), name_tok.text.clone());
            } else if self.at(TokKind::LBracket) {
                self.i += 1;
                let index = self.expr()?;
                self.expect(TokKind::RBracket, String::from("']'"))?;
                e = Expr::Index(Box::new(e), Box::new(index));
            } else if self.at(TokKind::Question) {
                self.i += 1;
                e = Expr::Prop(Box::new(e), PropKind::Fallible);
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn args(&mut self) -> Result<Vec<Expr>, ParseError> {
        self.expect(TokKind::LParen, String::from("'('"))?;
        let mut out: Vec<Expr> = vec![];
        if !self.at(TokKind::RParen) {
            loop {
                out.push(self.expr()?);
                if !self.eat(TokKind::Comma).is_some() {
                    break;
                }
            }
        }
        self.expect(TokKind::RParen, String::from("')'"))?;
        Ok(out)
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek(0) {
            TokKind::Int(v) => {
                let val = *v;
                self.i += 1;
                Ok(Expr::Int(val))
            }
            TokKind::Float(v) => {
                let val = *v;
                self.i += 1;
                Ok(Expr::Float(val))
            }
            TokKind::Char(c) => {
                let val = *c;
                self.i += 1;
                Ok(Expr::Char(val))
            }
            TokKind::Str(s) => {
                let val = s.clone();
                self.i += 1;
                Ok(Expr::Str(val))
            }
            TokKind::RawStr(s) => {
                let val = s.clone();
                self.i += 1;
                Ok(Expr::RawStr(val))
            }
            TokKind::True => {
                self.i += 1;
                Ok(Expr::Bool(true))
            }
            TokKind::False => {
                self.i += 1;
                Ok(Expr::Bool(false))
            }
            TokKind::None => {
                self.i += 1;
                Ok(Expr::None)
            }
            TokKind::Ident(_) => {
                let name_tok = self.expect_ident()?;
                Ok(Expr::Name(name_tok.text.clone()))
            }
            TokKind::LBracket => {
                self.i += 1;
                let mut elems: Vec<Expr> = vec![];
                if !self.at(TokKind::RBracket) {
                    loop {
                        elems.push(self.expr()?);
                        if self.eat(TokKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                self.expect(TokKind::RBracket, String::from("']'"))?;
                Ok(Expr::ListLit(elems))
            }
            TokKind::LParen => {
                self.i += 1;
                let e = self.expr()?;
                self.expect(TokKind::RParen, String::from("')'"))?;
                Ok(e)
            }
            TokKind::Ok => {
                self.i += 1;
                self.expect(TokKind::LParen, String::from("'('"))?;
                let inner = if self.at(TokKind::RParen) {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                self.expect(TokKind::RParen, String::from("')'"))?;
                Ok(Expr::Ok(inner))
            }
            TokKind::ErrKw => {
                self.i += 1;
                let e = self.args()?;
                let val = if e.len() > 0 { e[0].clone() } else { Expr::None };
                Ok(Expr::Err(Box::new(val)))
            }
            TokKind::Some => {
                self.i += 1;
                let e = self.args()?;
                let val = if e.len() > 0 { e[0].clone() } else { Expr::None };
                Ok(Expr::Some(Box::new(val)))
            }
            TokKind::Send => {
                self.i += 1;
                self.expect(TokKind::LParen, String::from("'('"))?;
                let ch = self.expr()?;
                self.expect(TokKind::Comma, String::from("','"))?;
                let value = self.expr()?;
                self.expect(TokKind::RParen, String::from("')'"))?;
                Ok(Expr::Send(Box::new(ch), Box::new(value)))
            }
            TokKind::Transfer => {
                self.i += 1;
                let args = self.args()?;
                let val = if args.len() > 0 { args[0].clone() } else { Expr::None };
                Ok(Expr::Transfer(Box::new(val)))
            }
            TokKind::Await => {
                self.i += 1;
                let e = self.postfix()?;
                Ok(Expr::Await(Box::new(e)))
            }
            TokKind::Match => {
                self.i += 1;
                let subject = self.expr()?;
                self.expect(TokKind::LBrace, String::from("'{'"))?;
                let mut arms: Vec<(Pattern, Box<Expr>)> = vec![];
                while !self.at(TokKind::RBrace) {
                    let pat = self.pattern()?;
                    self.expect(TokKind::Arrow, String::from("'->'"))?;
                    let e = self.expr()?;
                    arms.push((pat, Box::new(e)));
                }
                self.expect(TokKind::RBrace, String::from("'}'"))?;
                Ok(Expr::Match(Box::new(subject), arms))
            }
            TokKind::If => {
                let e = self.if_expr()?;
                Ok(e)
            }
            TokKind::Loop => {
                self.i += 1;
                let b = self.block()?;
                Ok(Expr::Loop(b))
            }
            TokKind::For => {
                self.i += 1;
                let name_tok = self.expect_ident()?;
                self.expect(TokKind::In, String::from("'in'"))?;
                let iter = self.expr()?;
                let b = self.block()?;
                Ok(Expr::For(name_tok.text.clone(), Box::new(iter), b))
            }
            _ => {
                let cur = self.tokens[self.i].clone();
                Err(ParseError { message: format!("unexpected {} in expression", tok_show(&cur.kind)), span: cur.span })
            }
        }
    }

    fn if_expr(&mut self) -> Result<Expr, ParseError> {
        self.expect(TokKind::If, String::from("'if'"))?;
        let cond = self.expr()?;
        let then_block = self.block()?;
        let mut elif: Vec<(Box<Expr>, Block)> = vec![];
        let mut else_block: Option<Block> = None;
        while self.at(TokKind::Elif) {
            self.i += 1;
            let c = self.expr()?;
            let b = self.block()?;
            elif.push((Box::new(c), b));
        }
        if self.at(TokKind::Else) {
            self.i += 1;
            else_block = Some(self.block()?);
        }
        Ok(Expr::If(IfExpr { cond: Box::new(cond), then_block: then_block, elif: elif, else_block: else_block }))
    }

    fn pattern(&mut self) -> Result<Pattern, ParseError> {
        match self.peek(0) {
            TokKind::Underscore => {
                self.i += 1;
                Ok(Pattern::Wildcard)
            }
            TokKind::None => {
                self.i += 1;
                Ok(Pattern::NoneLit)
            }
            TokKind::Ok => {
                self.i += 1;
                let names = self.pattern_names()?;
                Ok(Pattern::Variant("ok".to_string(), names))
            }
            TokKind::ErrKw => {
                self.i += 1;
                let names = self.pattern_names()?;
                Ok(Pattern::Variant("err".to_string(), names))
            }
            TokKind::Some => {
                self.i += 1;
                let names = self.pattern_names()?;
                Ok(Pattern::Variant("some".to_string(), names))
            }
            TokKind::Ident(_) => {
                let name_tok = self.expect_ident()?;
                if self.at(TokKind::LParen) {
                    let names = self.pattern_names()?;
                    Ok(Pattern::Variant(name_tok.text.clone(), names))
                } else {
                    Ok(Pattern::Name(name_tok.text.clone()))
                }
            }
            _ => {
                let cur = self.tokens[self.i].clone();
                Err(ParseError { message: format!("invalid match pattern, found {}", tok_show(&cur.kind)), span: cur.span })
            }
        }
    }

    fn pattern_names(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect(TokKind::LParen, String::from("'('"))?;
        let mut names: Vec<String> = vec![];
        if !self.at(TokKind::RParen) {
            loop {
                let n = self.expect_ident()?;
                names.push(n.text.clone());
                if !self.eat(TokKind::Comma).is_some() {
                    break;
                }
            }
        }
        self.expect(TokKind::RParen, String::from("')'"))?;
        Ok(names)
    }
}

fn tok_show(kind: &TokKind) -> String {
    match kind {
        TokKind::Int(v) => format!("integer {v}"),
        TokKind::Float(v) => format!("number {v}"),
        TokKind::Char(c) => format!("char '{c}'"),
        TokKind::Str(_) => "string".to_string(),
        TokKind::RawStr(_) => "raw string".to_string(),
        TokKind::Ident(s) => format!("'{s}'"),
        TokKind::Eof => "end of file".to_string(),
        _ => "token".to_string(),
    }
}