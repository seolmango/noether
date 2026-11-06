//! Lexer, parser and AST for Noether. See docs/spec.md, chapters 2 and 3.

use std::fmt;

#[derive(Debug, Clone)]
pub struct Error {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.msg)
        } else {
            write!(f, "line {}: {}", self.line, self.msg)
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn err<T>(line: usize, msg: impl Into<String>) -> Result<T> {
    Err(Error { line, msg: msg.into() })
}

// ------------------------------------------------------------------ AST

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Named(String),
    Elem(String, Box<Value>),
    Ptr(Box<Value>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Addr {
    Lit(i64),
    At(Target, i64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Read(Addr, u32),
    Intensity(Addr, u32),
    AddrOf(Addr),
    Call { line: usize, name: String, args: Vec<Value> },
    Absorb,
    Absorbc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub op: Cmp,
    pub left: Value,
    pub right: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Lit(i64),
    Rel(String, i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decl {
    pub fermion: bool,
    pub name: String,
    pub count: Option<u64>,
    pub at: Option<Place>,
    pub stride: Option<i64>,
    pub mass: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub line: usize,
    pub level: u32,
    pub kind: StmtKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// A value of None is the word `random`.
    Field(Vec<(String, Option<u64>)>),
    Decl(Decl),
    Path { name: String, params: Vec<String>, body: Vec<Stmt> },
    Write(Target, Value),
    Raise(Target),
    Lower(Target),
    Flip(Target),
    Swap(Target, Target),
    Fuse(Target, Target),
    If(Cond, Vec<Stmt>, Vec<Stmt>),
    While(Cond, Vec<Stmt>),
    SymTime(Vec<Stmt>),
    SymTranslate { index: String, lattice: String, body: Vec<Stmt> },
    Emit(Value),
    EmitC(Value),
    EmitN(Value),
    EmitS(Vec<u8>),
    Return(Value),
    CallStmt(Value),
    Create(bool, Value),
    Annihilate(Value),
}

// ------------------------------------------------------------------ lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Name(String),
    Int(u64),
    Str(Vec<u8>),
    Op(&'static str),
    Eol,
}

struct Line {
    no: usize,
    level: u32,
    toks: Vec<Tok>,
}

const OPS: [&str; 20] = [
    "<-", "<=", ">=", "==", "!=", "<", ">", "|", "~", "(", ")", "[", "]", ",", "@", "=", "+", "-",
    "", "",
];

fn lex_line(text: &str, no: usize) -> Result<Vec<Tok>> {
    let b = text.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    'scan: while i < b.len() {
        let c = b[i];
        if c == b' ' {
            i += 1;
        } else if c == b'#' {
            break;
        } else if c == b'"' {
            let mut out = Vec::new();
            i += 1;
            loop {
                match b.get(i) {
                    None => return err(no, "unterminated string"),
                    Some(b'"') => break,
                    Some(b'\\') => {
                        i += 1;
                        match b.get(i) {
                            Some(b'n') => out.push(b'\n'),
                            Some(b't') => out.push(b'\t'),
                            Some(b'r') => out.push(b'\r'),
                            Some(b'0') => out.push(0),
                            Some(b'\\') => out.push(b'\\'),
                            Some(b'"') => out.push(b'"'),
                            _ => return err(no, "unknown escape in string"),
                        }
                        i += 1;
                    }
                    Some(&x) => {
                        out.push(x);
                        i += 1;
                    }
                }
            }
            i += 1;
            toks.push(Tok::Str(out));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            match text[start..i].parse::<u64>() {
                Ok(n) if n <= 1 << 63 => toks.push(Tok::Int(n)),
                _ => return err(no, "integer does not fit in 64 bits"),
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(Tok::Name(text[start..i].to_string()));
        } else {
            for op in OPS.iter().filter(|o| !o.is_empty()) {
                if text[i..].starts_with(op) {
                    toks.push(Tok::Op(op));
                    i += op.len();
                    continue 'scan;
                }
            }
            return err(no, format!("bad character {:?}", text[i..].chars().next().unwrap()));
        }
    }
    Ok(toks)
}

fn indent_level(spaces: usize, no: usize) -> Result<u32> {
    let mut n = 0usize;
    while n * n < spaces {
        n += 1;
    }
    if n * n != spaces {
        return err(no, format!("forbidden energy level: {spaces} spaces is not n^2"));
    }
    Ok(n as u32)
}

fn lex(src: &str) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    for (idx, raw) in src.lines().enumerate() {
        let no = idx + 1;
        let body = raw.trim_end();
        let lead = body.len() - body.trim_start().len();
        if body[..lead].contains('\t') {
            return err(no, "tabs are not quantized");
        }
        let stripped = body.trim_start_matches(' ');
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        let level = indent_level(body.len() - stripped.len(), no)?;
        lines.push(Line { no, level, toks: lex_line(stripped, no)? });
    }
    Ok(lines)
}

// ------------------------------------------------------------------ parser

struct Parser {
    lines: Vec<Line>,
    i: usize,
    toks: Vec<Tok>,
    p: usize,
    ln: usize,
}

const EOL: Tok = Tok::Eol;

impl Parser {
    fn peek(&self, k: usize) -> &Tok {
        self.toks.get(self.p + k).unwrap_or(&EOL)
    }

    fn next(&mut self) -> Tok {
        let t = self.peek(0).clone();
        self.p += 1;
        t
    }

    fn at_op(&self, op: &str) -> bool {
        matches!(self.peek(0), Tok::Op(o) if *o == op)
    }

    fn at_word(&self, word: &str) -> bool {
        matches!(self.peek(0), Tok::Name(n) if n == word)
    }

    fn accept(&mut self, op: &str) -> bool {
        let hit = self.at_op(op);
        if hit {
            self.p += 1;
        }
        hit
    }

    fn expect(&mut self, op: &str) -> Result<()> {
        if self.accept(op) {
            Ok(())
        } else {
            self.fail(format!("expected {op:?}, found {:?}", self.peek(0)))
        }
    }

    fn expect_word(&mut self, word: &str) -> Result<()> {
        if self.at_word(word) {
            self.p += 1;
            Ok(())
        } else {
            self.fail(format!("expected {word:?}"))
        }
    }

    fn fail<T>(&self, msg: impl Into<String>) -> Result<T> {
        err(self.ln, msg)
    }

    fn name(&mut self) -> Result<String> {
        match self.next() {
            Tok::Name(n) => Ok(n),
            t => self.fail(format!("expected a name, found {t:?}")),
        }
    }

    fn uint(&mut self) -> Result<u64> {
        match self.next() {
            Tok::Int(n) => Ok(n),
            t => self.fail(format!("expected an integer, found {t:?}")),
        }
    }

    /// A non-negative literal that must fit in i64.
    fn int(&mut self) -> Result<i64> {
        let n = self.uint()?;
        i64::try_from(n).or_else(|_| self.fail("integer does not fit in 64 bits"))
    }

    fn eol(&self) -> Result<()> {
        if *self.peek(0) == Tok::Eol {
            Ok(())
        } else {
            self.fail(format!("unexpected {:?}", self.peek(0)))
        }
    }

    fn block(&mut self, level: u32) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        while self.i < self.lines.len() {
            let line = &self.lines[self.i];
            if line.level < level {
                break;
            }
            if line.level > level {
                return err(
                    line.no,
                    format!(
                        "forbidden transition: level {level} -> {} \
                         (indent must satisfy delta n = +1 after a block opener)",
                        line.level
                    ),
                );
            }
            self.ln = line.no;
            self.toks = std::mem::take(&mut self.lines[self.i].toks);
            self.p = 0;
            self.i += 1;
            stmts.push(self.stmt(level)?);
        }
        Ok(stmts)
    }

    fn body(&mut self, level: u32) -> Result<Vec<Stmt>> {
        if self.lines.get(self.i).map(|l| l.level) != Some(level + 1) {
            return self.fail(format!(
                "forbidden transition: block needs a body at level {}",
                level + 1
            ));
        }
        self.block(level + 1)
    }

    fn stmt(&mut self, lv: u32) -> Result<Stmt> {
        let line = self.ln;
        let kind = self.stmt_kind(lv)?;
        Ok(Stmt { line, level: lv, kind })
    }

    fn stmt_kind(&mut self, lv: u32) -> Result<StmtKind> {
        if self.at_op("|") {
            self.p += 1;
            let target = self.target()?;
            self.expect(">")?;
            self.expect("<-")?;
            let value = self.value()?;
            self.eol()?;
            return Ok(StmtKind::Write(target, value));
        }
        let word = match self.peek(0) {
            Tok::Name(n) => n.clone(),
            t => return self.fail(format!("cannot start a statement with {t:?}")),
        };
        match word.as_str() {
            "field" => {
                self.p += 1;
                let mut params = Vec::new();
                while *self.peek(0) != Tok::Eol {
                    let key = self.name()?;
                    self.expect("=")?;
                    let value = if self.at_word("random") {
                        self.p += 1;
                        None
                    } else {
                        Some(self.uint()?)
                    };
                    params.push((key, value));
                }
                Ok(StmtKind::Field(params))
            }
            "fermion" | "boson" => {
                self.p += 1;
                self.decl(word == "fermion")
            }
            "path" => {
                self.p += 1;
                let name = self.name()?;
                self.expect("(")?;
                let mut params = Vec::new();
                if !self.at_op(")") {
                    params.push(self.name()?);
                    while self.accept(",") {
                        params.push(self.name()?);
                    }
                }
                self.expect(")")?;
                self.eol()?;
                Ok(StmtKind::Path { name, params, body: self.body(lv)? })
            }
            "if" => {
                self.p += 1;
                let cond = self.cond()?;
                self.eol()?;
                let then = self.body(lv)?;
                let mut other = Vec::new();
                if let Some(next) = self.lines.get(self.i) {
                    if next.level == lv && next.toks.first() == Some(&Tok::Name("else".into())) {
                        self.ln = next.no;
                        self.toks = std::mem::take(&mut self.lines[self.i].toks);
                        self.p = 1;
                        self.i += 1;
                        self.eol()?;
                        other = self.body(lv)?;
                    }
                }
                Ok(StmtKind::If(cond, then, other))
            }
            "while" => {
                self.p += 1;
                let cond = self.cond()?;
                self.eol()?;
                Ok(StmtKind::While(cond, self.body(lv)?))
            }
            "symmetry" => {
                self.p += 1;
                let kind = self.name()?;
                match kind.as_str() {
                    "time" => {
                        self.eol()?;
                        Ok(StmtKind::SymTime(self.body(lv)?))
                    }
                    "translate" => {
                        let index = self.name()?;
                        self.expect_word("in")?;
                        let lattice = self.name()?;
                        self.eol()?;
                        Ok(StmtKind::SymTranslate { index, lattice, body: self.body(lv)? })
                    }
                    _ => self.fail(format!("unknown symmetry {kind:?}")),
                }
            }
            "raise" | "lower" | "flip" => {
                self.p += 1;
                let t = self.target()?;
                self.eol()?;
                Ok(match word.as_str() {
                    "raise" => StmtKind::Raise(t),
                    "lower" => StmtKind::Lower(t),
                    _ => StmtKind::Flip(t),
                })
            }
            "swap" | "fuse" => {
                self.p += 1;
                let a = self.target()?;
                let b = self.target()?;
                self.eol()?;
                Ok(if word == "swap" { StmtKind::Swap(a, b) } else { StmtKind::Fuse(a, b) })
            }
            "emit" | "emitc" | "emitn" => {
                self.p += 1;
                if word == "emit" {
                    if let Tok::Str(s) = self.peek(0).clone() {
                        self.p += 1;
                        self.eol()?;
                        return Ok(StmtKind::EmitS(s));
                    }
                }
                let v = self.value()?;
                self.eol()?;
                Ok(match word.as_str() {
                    "emit" => StmtKind::Emit(v),
                    "emitc" => StmtKind::EmitC(v),
                    _ => StmtKind::EmitN(v),
                })
            }
            "return" => {
                self.p += 1;
                let v = if *self.peek(0) == Tok::Eol { Value::Int(0) } else { self.value()? };
                self.eol()?;
                Ok(StmtKind::Return(v))
            }
            "create" => {
                self.p += 1;
                let kind = self.name()?;
                if kind != "fermion" && kind != "boson" {
                    return self.fail("create needs fermion or boson");
                }
                self.expect("@")?;
                let v = self.value()?;
                self.eol()?;
                Ok(StmtKind::Create(kind == "fermion", v))
            }
            "annihilate" => {
                self.p += 1;
                let v = self.value()?;
                self.eol()?;
                Ok(StmtKind::Annihilate(v))
            }
            _ if *self.peek(1) == Tok::Op("(") => {
                let v = self.value()?;
                self.eol()?;
                Ok(StmtKind::CallStmt(v))
            }
            _ => self.fail(format!("cannot start a statement with {word:?}")),
        }
    }

    fn decl(&mut self, fermion: bool) -> Result<StmtKind> {
        let name = self.name()?;
        let mut count = None;
        if self.accept("[") {
            count = Some(self.uint()?);
            self.expect("]")?;
        }
        let mut at = None;
        if self.accept("@") {
            at = Some(self.place()?);
        }
        let mut stride = None;
        let mut mass = 0;
        while *self.peek(0) != Tok::Eol {
            let key = self.name()?;
            match key.as_str() {
                "stride" => stride = Some(self.int()?),
                "mass" => {
                    mass = u32::try_from(self.uint()?).or_else(|_| self.fail("mass is too large"))?
                }
                _ => return self.fail(format!("unknown attribute {key:?}")),
            }
        }
        Ok(StmtKind::Decl(Decl { fermion, name, count, at, stride, mass }))
    }

    fn place(&mut self) -> Result<Place> {
        match self.next() {
            Tok::Int(n) => match i64::try_from(n) {
                Ok(v) => Ok(Place::Lit(v)),
                Err(_) => self.fail("integer does not fit in 64 bits"),
            },
            Tok::Name(name) => {
                let off = self.offset()?;
                Ok(Place::Rel(name, off))
            }
            _ => self.fail("bad placement"),
        }
    }

    fn offset(&mut self) -> Result<i64> {
        if self.accept("+") {
            self.int()
        } else if self.accept("-") {
            Ok(-self.int()?)
        } else {
            Ok(0)
        }
    }

    fn target(&mut self) -> Result<Target> {
        if self.accept("(") {
            let v = self.value()?;
            self.expect(")")?;
            return Ok(Target::Ptr(Box::new(v)));
        }
        let name = self.name()?;
        if self.accept("[") {
            let index = self.value()?;
            self.expect("]")?;
            return Ok(Target::Elem(name, Box::new(index)));
        }
        Ok(Target::Named(name))
    }

    fn cond(&mut self) -> Result<Cond> {
        let left = self.value()?;
        let op = match self.next() {
            Tok::Op("<") => Cmp::Lt,
            Tok::Op(">") => Cmp::Gt,
            Tok::Op("<=") => Cmp::Le,
            Tok::Op(">=") => Cmp::Ge,
            Tok::Op("==") => Cmp::Eq,
            Tok::Op("!=") => Cmp::Ne,
            _ => return self.fail("expected a comparison"),
        };
        Ok(Cond { op, left, right: self.value()? })
    }

    fn addr(&mut self) -> Result<Addr> {
        if let Tok::Int(_) = self.peek(0) {
            return Ok(Addr::Lit(self.int()?));
        }
        let base = self.target()?;
        Ok(Addr::At(base, self.offset()?))
    }

    fn window(&mut self) -> Result<u32> {
        if !self.accept("~") {
            return Ok(0);
        }
        let w = self.uint()?;
        if !w.is_power_of_two() || w > 1 << 30 {
            return self.fail(format!("window ~{w} is not a power of two up to 2^30"));
        }
        Ok(w.trailing_zeros())
    }

    fn value(&mut self) -> Result<Value> {
        if let Tok::Int(_) = self.peek(0) {
            return Ok(Value::Int(self.int()?));
        }
        if self.accept("-") {
            let n = self.uint()?;
            return Ok(Value::Int((n as i64).wrapping_neg()));
        }
        if self.accept("<") {
            let a = self.addr()?;
            self.expect("|")?;
            if matches!(self.peek(0), Tok::Name(_) | Tok::Int(_)) || self.at_op("(") {
                let b = self.addr()?;
                self.expect(">")?;
                if a != b {
                    return self.fail("intensity needs the same address on both sides");
                }
                return Ok(Value::Intensity(a, self.window()?));
            }
            return Ok(Value::Read(a, self.window()?));
        }
        if self.at_op("(") {
            return Ok(Value::AddrOf(self.addr()?));
        }
        if let Tok::Name(n) = self.peek(0).clone() {
            if n == "absorb" {
                self.p += 1;
                return Ok(Value::Absorb);
            }
            if n == "absorbc" {
                self.p += 1;
                return Ok(Value::Absorbc);
            }
            if *self.peek(1) == Tok::Op("(") {
                let line = self.ln;
                let name = self.name()?;
                self.expect("(")?;
                let mut args = Vec::new();
                if !self.at_op(")") {
                    args.push(self.value()?);
                    while self.accept(",") {
                        args.push(self.value()?);
                    }
                }
                self.expect(")")?;
                return Ok(Value::Call { line, name, args });
            }
            return Ok(Value::AddrOf(self.addr()?));
        }
        self.fail(format!("expected a value, found {:?}", self.peek(0)))
    }
}

pub fn parse(src: &str) -> Result<Vec<Stmt>> {
    let lines = lex(src)?;
    let mut p = Parser { lines, i: 0, toks: Vec::new(), p: 0, ln: 0 };
    p.block(0)
}
