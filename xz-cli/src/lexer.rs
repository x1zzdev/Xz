use crate::token::{TokKind, Token, Span, LexError};

pub fn lex(source: String, file: String) -> Result<Vec<Token>, LexError> {
    let mut src: Vec<char> = vec![];
    for ch in source.chars() {
        src.push(ch);
    }
    let mut lx = Lexer { src: src, i: 0, line: 1, col: 1, file: file, tokens: vec![] };
    match lx.run() {
        Err(e) => return Err(e),
        _ => {}
    }
    Ok(lx.tokens)
}

struct Lexer {
    src: Vec<char>,
    i: usize,
    line: usize,
    col: usize,
    file: String,
    tokens: Vec<Token>,
}

impl Lexer {
    fn peek(&self, offset: usize) -> char {
        if self.i + offset < self.src.len() {
            self.src[self.i + offset]
        } else {
            '\0'
        }
    }

    fn advance(&mut self, n: usize) {
        for _ in 0..n {
            if self.i < self.src.len() {
                if self.src[self.i] == '\n' {
                    self.line += 1;
                    self.col = 1;
                } else {
                    self.col += 1;
                }
                self.i += 1;
            }
        }
    }

    fn pos(&self) -> (usize, usize) {
        (self.line, self.col)
    }

    fn err(&self, message: String, start: (usize, usize)) -> LexError {
        LexError { message: message, span: Span { file: self.file.clone(), start: start, end: self.pos() } }
    }

    fn push(&mut self, kind: TokKind, start: (usize, usize), text: String) {
        self.tokens.push(Token { kind: kind, span: Span { file: self.file.clone(), start: start, end: self.pos() }, text: text });
    }

    fn push_tok(&mut self, kind: TokKind) {
        self.push(kind, self.pos(), "".to_string());
    }

    fn run(&mut self) -> Result<(), LexError> {
        while self.i < self.src.len() {
            let c = self.src[self.i];

            // whitespace
            if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
                self.advance(1);
                continue;
            }

            // comments and doc comments
            if c == '/' && self.peek(1) == '/' {
                let _ = self.pos();
                self.advance(2);
                if self.peek(0) == '/' {
                    // doc comment "///"
                    self.advance(1);
                    while self.peek(0) == ' ' {
                        self.advance(1);
                    }
                    let tag_start = self.pos();
                    if self.peek(0) == '@' {
                        self.advance(1);
                        let mut name: String = "".to_string();
                        while is_ident_char(self.peek(0)) {
                            name.push(self.peek(0));
                            self.advance(1);
                        }
                        let kind = doc_tag_kind(name.clone());
                        if kind.is_none() {
                            return Err(self.err(format!("unknown intent tag @{name}"), tag_start));
                        }
                        let mut text: String = "".to_string();
                        while self.i < self.src.len() && self.src[self.i] != '\n' {
                            text.push(self.src[self.i]);
                            self.i += 1;
                            self.col += 1;
                        }
                        self.push(kind.unwrap(), tag_start, String::from(text.trim()));
                        continue;
                    }
                    // plain prose line
                    let mut text: String = "".to_string();
                    while self.i < self.src.len() && self.src[self.i] != '\n' {
                        text.push(self.src[self.i]);
                        self.i += 1;
                        self.col += 1;
                    }
                    self.push(TokKind::DocIntent, tag_start, String::from(text.trim()));
                    continue;
                }
                // normal line comment
                while self.i < self.src.len() && self.src[self.i] != '\n' {
                    self.advance(1);
                }
                continue;
            }
            if c == '/' && self.peek(1) == '*' {
                let start = self.pos();
                self.advance(2);
                let mut closed = false;
                while self.i < self.src.len() {
                    if self.src[self.i] == '*' && self.peek(1) == '/' {
                        self.advance(2);
                        closed = true;
                        break;
                    }
                    self.advance(1);
                }
                if !closed {
                    return Err(self.err(String::from("unterminated block comment"), start));
                }
                continue;
            }

            // identifiers / keywords
            if is_ident_start(c) {
                let start = self.pos();
                let mut name: String = "".to_string();
                while is_ident_char(self.peek(0)) {
                    name.push(self.peek(0));
                    self.advance(1);
                }
                let kind = keyword_kind(name.clone()).unwrap_or_else(|| TokKind::Ident(name.clone()));
                self.push(kind, start, name);
                continue;
            }

            // numbers
            if is_ascii_digit(c) {
                let _ = self.lex_number();
                continue;
            }

            // strings
            if c == '"' {
                let _ = self.lex_string();
                continue;
            }
            if c == 'r' && self.peek(1) == '"' {
                let _ = self.lex_raw_string();
                continue;
            }
            if c == '\'' {
                let _ = self.lex_char();
                continue;
            }

            // operators
            let start = self.pos();
            let two = format!("{c}{}", self.peek(1));
            if two == "==" {
                self.advance(2); self.push_tok(TokKind::EqEq); continue;
            }
            if two == "!=" {
                self.advance(2); self.push_tok(TokKind::NotEq); continue;
            }
            if two == "<=" {
                self.advance(2); self.push_tok(TokKind::Le); continue;
            }
            if two == ">=" {
                self.advance(2); self.push_tok(TokKind::Ge); continue;
            }
            if two == "+=" {
                self.advance(2); self.push_tok(TokKind::PlusEq); continue;
            }
            if two == "-=" {
                self.advance(2); self.push_tok(TokKind::MinusEq); continue;
            }
            if two == "*=" {
                self.advance(2); self.push_tok(TokKind::StarEq); continue;
            }
            if two == "/=" {
                self.advance(2); self.push_tok(TokKind::SlashEq); continue;
            }
            if two == "->" {
                self.advance(2); self.push_tok(TokKind::Arrow); continue;
            }
            if two == "<-" {
                self.advance(2); self.push_tok(TokKind::LeftArrow); continue;
            }
            match c {
                '(' => { self.advance(1); self.push_tok(TokKind::LParen); continue; }
                ')' => { self.advance(1); self.push_tok(TokKind::RParen); continue; }
                '[' => { self.advance(1); self.push_tok(TokKind::LBracket); continue; }
                ']' => { self.advance(1); self.push_tok(TokKind::RBracket); continue; }
                '{' => { self.advance(1); self.push_tok(TokKind::LBrace); continue; }
                '}' => { self.advance(1); self.push_tok(TokKind::RBrace); continue; }
                ',' => { self.advance(1); self.push_tok(TokKind::Comma); continue; }
                ':' => { self.advance(1); self.push_tok(TokKind::Colon); continue; }
                '.' => { self.advance(1); self.push_tok(TokKind::Dot); continue; }
                '?' => { self.advance(1); self.push_tok(TokKind::Question); continue; }
                '+' => { self.advance(1); self.push_tok(TokKind::Plus); continue; }
                '-' => { self.advance(1); self.push_tok(TokKind::Minus); continue; }
                '*' => { self.advance(1); self.push_tok(TokKind::Star); continue; }
                '/' => { self.advance(1); self.push_tok(TokKind::Slash); continue; }
                '%' => { self.advance(1); self.push_tok(TokKind::Percent); continue; }
                '<' => { self.advance(1); self.push_tok(TokKind::Lt); continue; }
                '>' => { self.advance(1); self.push_tok(TokKind::Gt); continue; }
                '=' => { self.advance(1); self.push_tok(TokKind::Assign); continue; }
                '|' => { self.advance(1); self.push_tok(TokKind::Pipe); continue; }
                '_' => { self.advance(1); self.push_tok(TokKind::Underscore); continue; }
                _ => return Err(self.err(format!("unexpected character {c}"), start)),
            }
        }

        self.push_tok(TokKind::Eof);
        Ok(())
    }

    fn lex_number(&mut self) -> Result<(), LexError> {
        let start = self.pos();
        let mut text: String = "".to_string();
        let mut digits: String = "".to_string();
        let c = self.src[self.i];
        if c == '0' && (self.peek(1) == 'x' || self.peek(1) == 'X') {
            text.push(c);
            self.advance(1);
            text.push(self.peek(0));
            self.advance(1);
            while is_hex_digit(self.peek(0)) || self.peek(0) == '_' {
                text.push(self.peek(0));
                if self.peek(0) != '_' {
                    digits.push(self.peek(0));
                }
                self.advance(1);
            }
            let value = parse_hex(digits);
            self.push(TokKind::Int(value), start, text);
            return Ok(());
        }
        while is_ascii_digit(self.peek(0)) || self.peek(0) == '_' {
            text.push(self.peek(0));
            if self.peek(0) != '_' {
                digits.push(self.peek(0));
            }
            self.advance(1);
        }
        let mut is_float = false;
        if self.peek(0) == '.' && is_ascii_digit(self.peek(1)) {
            is_float = true;
            text.push(self.peek(0));
            self.advance(1);
            while is_ascii_digit(self.peek(0)) || self.peek(0) == '_' {
                text.push(self.peek(0));
                if self.peek(0) != '_' {
                    digits.push(self.peek(0));
                }
                self.advance(1);
            }
        }
        if self.peek(0) == 'e' || self.peek(0) == 'E' {
            let mut j: usize = 1;
            if self.peek(1) == '+' || self.peek(1) == '-' {
                j += 1;
            }
            if is_ascii_digit(self.peek(j)) {
                is_float = true;
                text.push(self.peek(0));
                self.advance(1);
                if self.peek(0) == '+' || self.peek(0) == '-' {
                    text.push(self.peek(0));
                    self.advance(1);
                }
                while is_ascii_digit(self.peek(0)) || self.peek(0) == '_' {
                    text.push(self.peek(0));
                    if self.peek(0) != '_' {
                        digits.push(self.peek(0));
                    }
                    self.advance(1);
                }
            }
        }
        if is_float {
            let value = digits.parse::<f64>().unwrap();
            self.push(TokKind::Float(value), start, text);
        } else {
            let value = digits.parse::<u64>().unwrap();
            self.push(TokKind::Int(value), start, text);
        }
        Ok(())
    }

    fn lex_string(&mut self) -> Result<(), LexError> {
        let start = self.pos();
        self.advance(1);
        let mut s: String = "".to_string();
        let mut closed = false;
        while self.i < self.src.len() {
            let ch = self.src[self.i];
            if ch == '"' {
                self.advance(1);
                closed = true;
                break;
            }
            if ch == '\n' {
                break;
            }
            if ch == '\\' {
                self.advance(1);
                let e = self.src[self.i];
                match e {
                    'n' => { s.push('\n'); self.advance(1); }
                    't' => { s.push('\t'); self.advance(1); }
                    'r' => { s.push('\r'); self.advance(1); }
                    '\\' => { s.push('\\'); self.advance(1); }
                    '"' => { s.push('"'); self.advance(1); }
                    '\'' => { s.push('\''); self.advance(1); }
                    'u' => {
                        if self.peek(1) != '{' {
                            return Err(self.err(String::from("invalid unicode escape"), self.pos()));
                        }
                        self.advance(2);
                        let mut hex: String = "".to_string();
                        while self.peek(0) != '}' && self.i < self.src.len() {
                            hex.push(self.peek(0));
                            self.advance(1);
                        }
                        if self.peek(0) != '}' {
                            return Err(self.err(String::from("unterminated unicode escape"), self.pos()));
                        }
                        self.advance(1);
                        let cp = parse_hex(hex);
                        s.push(char::from_u32(cp as u32).unwrap());
                    }
                    _ => {
                        return Err(self.err(format!("unknown escape \\{e}"), self.pos()));
                    }
                }
                continue;
            }
            s.push(ch);
            self.advance(1);
        }
        if !closed {
            return Err(self.err(String::from("unterminated string literal"), start));
        }
        self.push(TokKind::Str(s), start, "".to_string());
        Ok(())
    }

    fn lex_raw_string(&mut self) -> Result<(), LexError> {
        let start = self.pos();
        self.advance(2);
        let mut s: String = "".to_string();
        let mut closed = false;
        while self.i < self.src.len() {
            let ch = self.src[self.i];
            if ch == '"' {
                self.advance(1);
                closed = true;
                break;
            }
            if ch == '\n' {
                break;
            }
            s.push(ch);
            self.advance(1);
        }
        if !closed {
            return Err(self.err(String::from("unterminated raw string"), start));
        }
        self.push(TokKind::RawStr(s), start, "".to_string());
        Ok(())
    }

    fn lex_char(&mut self) -> Result<(), LexError> {
        let start = self.pos();
        self.advance(1);
        let ch = if self.src[self.i] == '\\' {
            self.advance(1);
            let e = self.src[self.i];
            let result = match e {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                '\'' => '\'',
                '"' => '"',
                _ => return Err(self.err(format!("unknown escape \\{e}"), self.pos())),
            };
            self.advance(1);
            result
        } else {
            let result = self.src[self.i];
            self.advance(1);
            result
        };
        if self.src[self.i] != '\'' {
            return Err(self.err(String::from("unterminated char literal"), start));
        }
        self.advance(1);
        self.push(TokKind::Char(ch), start, "".to_string());
        Ok(())
    }
}

fn doc_tag_kind(name: String) -> Option<TokKind> {
    if name == "intent" {
        return Some(TokKind::DocIntent);
    }
    if name == "requires" {
        return Some(TokKind::DocRequires);
    }
    if name == "ensures" {
        return Some(TokKind::DocEnsures);
    }
    if name == "effects" {
        return Some(TokKind::DocEffects);
    }
    if name == "trusted" {
        return Some(TokKind::DocTrusted);
    }
    None
}

fn keyword_kind(name: String) -> Option<TokKind> {
    if name == "and" { return Some(TokKind::And); }
    if name == "as" { return Some(TokKind::As); }
    if name == "async" { return Some(TokKind::Async); }
    if name == "await" { return Some(TokKind::Await); }
    if name == "break" { return Some(TokKind::Break); }
    if name == "chan" { return Some(TokKind::Chan); }
    if name == "continue" { return Some(TokKind::Continue); }
    if name == "elif" { return Some(TokKind::Elif); }
    if name == "else" { return Some(TokKind::Else); }
    if name == "enum" { return Some(TokKind::Enum); }
    if name == "err" { return Some(TokKind::ErrKw); }
    if name == "extern" { return Some(TokKind::Extern); }
    if name == "false" { return Some(TokKind::False); }
    if name == "for" { return Some(TokKind::For); }
    if name == "func" { return Some(TokKind::Func); }
    if name == "if" { return Some(TokKind::If); }
    if name == "implies" { return Some(TokKind::Implies); }
    if name == "in" { return Some(TokKind::In); }
    if name == "invariant" { return Some(TokKind::Invariant); }
    if name == "is" { return Some(TokKind::Is); }
    if name == "let" { return Some(TokKind::Let); }
    if name == "loop" { return Some(TokKind::Loop); }
    if name == "match" { return Some(TokKind::Match); }
    if name == "mut" { return Some(TokKind::Mut); }
    if name == "none" { return Some(TokKind::None); }
    if name == "not" { return Some(TokKind::Not); }
    if name == "ok" { return Some(TokKind::Ok); }
    if name == "or" { return Some(TokKind::Or); }
    if name == "post" { return Some(TokKind::Post); }
    if name == "pre" { return Some(TokKind::Pre); }
    if name == "recv" { return Some(TokKind::Recv); }
    if name == "record" { return Some(TokKind::Record); }
    if name == "send" { return Some(TokKind::Send); }
    if name == "some" { return Some(TokKind::Some); }
    if name == "task" { return Some(TokKind::Task); }
    if name == "transfer" { return Some(TokKind::Transfer); }
    if name == "true" { return Some(TokKind::True); }
    None
}

fn is_ident_start(c: char) -> bool {
    (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_'
}

fn is_ident_char(c: char) -> bool {
    is_ident_start(c) || is_ascii_digit(c)
}

fn is_ascii_digit(c: char) -> bool {
    c >= '0' && c <= '9'
}

fn is_hex_digit(c: char) -> bool {
    is_ascii_digit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')
}

fn hex_value(c: char) -> u64 {
    if c >= '0' && c <= '9' {
        (c as u64) - ('0' as u64)
    } else if c >= 'a' && c <= 'f' {
        (c as u64) - ('a' as u64) + 10
    } else {
        (c as u64) - ('A' as u64) + 10
    }
}

fn parse_hex(digits: String) -> u64 {
    let mut value: u64 = 0;
    for ch in digits.chars() {
        value = value * 16 + hex_value(ch);
    }
    value
}