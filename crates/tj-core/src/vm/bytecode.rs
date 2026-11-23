//! Ahead-of-time bytecode serialization (`TJB\x01`) and validation.
//!
//! `tj build` compiles a `.tj` app once into compact binary bytecode (`.tjb`) or a
//! UTF-8 bundle (`//# tjb:1:<base64>`) that every host (`tj-wasm`, `tj-android`, `tj-ffi`,
//! `tj serve`) boots directly without running the compiler.
//!
//! Encoding is gated to `check`/`server`/tests so device builds only carry the small decoder.

use super::builtins;
use super::program::{ArgTy, Capture, ElemDesc, Guard, MethodDef, ObjectDef, Op, Pii, Program, Proto, Retain, Source};
use super::value::Value;
use indexmap::IndexMap;
use std::rc::Rc;

/// Binary `.tjb` file header: magic `TJB` + format version `1`.
pub const MAGIC: &[u8; 4] = b"TJB\x01";

/// UTF-8 transport prefix accepted by `JsonHost::boot` and `tj serve` alongside `.tj` source.
pub const BUNDLE_PREFIX: &str = "//# tjb:1:";

const MAX_TYPE_DEPTH: usize = 32;

// ---------- decoder (compiled into all hosts, including devices) ----------

type Res<T> = Result<T, &'static str>;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn rem(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Res<&'a [u8]> {
        if self.rem() < n {
            return Err("unexpected end of bytecode");
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Res<u8> {
        Ok(self.take(1)?[0])
    }

    fn bool(&mut self) -> Res<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("invalid boolean in bytecode"),
        }
    }

    fn u16(&mut self) -> Res<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Res<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f64(&mut self) -> Res<f64> {
        let b = self.take(8)?;
        Ok(f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn count(&mut self, min_elem_bytes: usize) -> Res<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_elem_bytes) > self.rem() {
            return Err("bytecode section count exceeds remaining bytes");
        }
        Ok(n)
    }

    fn str(&mut self) -> Res<Rc<str>> {
        let len = self.count(1)?;
        let s = std::str::from_utf8(self.take(len)?).map_err(|_| "invalid UTF-8 in bytecode")?;
        Ok(Rc::from(s))
    }

    fn str_list(&mut self) -> Res<Rc<[Rc<str>]>> {
        let n = self.count(4)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.str()?);
        }
        Ok(Rc::from(v))
    }

    fn opt_str_list(&mut self) -> Res<Option<Rc<[Rc<str>]>>> {
        if self.bool()? {
            Ok(Some(self.str_list()?))
        } else {
            Ok(None)
        }
    }

    fn arg_ty(&mut self, depth: usize) -> Res<ArgTy> {
        if depth > MAX_TYPE_DEPTH {
            return Err("bytecode type nesting exceeds limit");
        }
        match self.u8()? {
            0 => Ok(ArgTy::Any),
            1 => Ok(ArgTy::Num),
            2 => Ok(ArgTy::Str),
            3 => Ok(ArgTy::Bool),
            4 => Ok(ArgTy::Null),
            5 => Ok(ArgTy::StrLit(self.str()?)),
            6 => Ok(ArgTy::NumLit(self.f64()?)),
            7 => Ok(ArgTy::BoolLit(self.bool()?)),
            8 => Ok(ArgTy::Arr(Box::new(self.arg_ty(depth + 1)?))),
            9 => {
                let n = self.count(6)?;
                let mut fields = Vec::with_capacity(n);
                for _ in 0..n {
                    fields.push((self.str()?, self.bool()?, self.arg_ty(depth + 1)?));
                }
                Ok(ArgTy::Rec(Rc::from(fields)))
            }
            10 => Ok(ArgTy::Dict(Box::new(self.arg_ty(depth + 1)?))),
            11 => {
                let n = self.count(1)?;
                let mut ts = Vec::with_capacity(n);
                for _ in 0..n {
                    ts.push(self.arg_ty(depth + 1)?);
                }
                Ok(ArgTy::Union(ts))
            }
            _ => Err("invalid ArgTy tag in bytecode"),
        }
    }

    fn op(&mut self) -> Res<Op> {
        Ok(match self.u8()? {
            0 => Op::NeedsConsent(self.u32()?),
            1 => Op::Const(self.u32()?),
            2 => Op::Null,
            3 => Op::True,
            4 => Op::False,
            5 => Op::Pop,
            6 => Op::Dup,
            7 => Op::Dup2,
            8 => Op::LoadLocal(self.u16()?),
            9 => Op::StoreLocal(self.u16()?),
            10 => Op::MakeCell(self.u16()?),
            11 => Op::LoadCell(self.u16()?),
            12 => Op::StoreCell(self.u16()?),
            13 => Op::Recell(self.u16()?),
            14 => Op::LoadUpval(self.u16()?),
            15 => Op::StoreUpval(self.u16()?),
            16 => Op::LoadGlobal(self.u32()?),
            17 => Op::StoreGlobal(self.u32()?),
            18 => Op::GetField(self.u32()?),
            19 => Op::SetField(self.u32()?),
            20 => Op::GetIndex,
            21 => Op::SetIndex,
            22 => Op::NewArr,
            23 => Op::ArrPush,
            24 => Op::ArrSpread,
            25 => Op::NewRec,
            26 => Op::RecSet(self.u32()?),
            27 => Op::RecSetDyn,
            28 => Op::RecSpread,
            29 => Op::Add,
            30 => Op::Sub,
            31 => Op::Mul,
            32 => Op::Div,
            33 => Op::Mod,
            34 => Op::Neg,
            35 => Op::Not,
            36 => Op::Eq,
            37 => Op::Ne,
            38 => Op::Lt,
            39 => Op::Le,
            40 => Op::Gt,
            41 => Op::Ge,
            42 => Op::Concat(self.u16()?),
            43 => Op::Len,
            44 => Op::Jump(self.u32()?),
            45 => Op::JumpIfFalse(self.u32()?),
            46 => Op::JumpIfFalseKeep(self.u32()?),
            47 => Op::JumpIfTrueKeep(self.u32()?),
            48 => Op::JumpIfNotNullKeep(self.u32()?),
            49 => Op::JumpIfNullKeep(self.u32()?),
            50 => Op::Call(self.u8()?),
            51 => Op::CallMethod(self.u32()?, self.u8()?),
            52 => Op::Return,
            53 => Op::Closure(self.u32()?),
            54 => Op::Elem(self.u32()?),
            55 => Op::Await,
            56 => Op::Caller,
            57 => Op::Throw,
            58 => Op::TryBegin(self.u32()?),
            59 => Op::TryEnd,
            _ => return Err("unknown opcode in bytecode"),
        })
    }
}

/// Validates every table reference and jump target so a corrupt `.tjb` cannot panic the VM.
fn validate(p: &Program) -> Res<()> {
    if p.globals.len() < builtins::GLOBALS.len() {
        return Err("bytecode globals table is incompatible with this runtime");
    }
    for (i, (name, _)) in builtins::GLOBALS.iter().enumerate() {
        if &*p.globals[i] != *name {
            return Err("bytecode builtin slot mismatch");
        }
    }
    let check_proto = |idx: u32| -> Res<()> {
        if (idx as usize) >= p.protos.len() {
            return Err("bytecode proto index out of range");
        }
        Ok(())
    };
    let check_str = |idx: u32| -> Res<()> {
        match p.consts.get(idx as usize) {
            Some(Value::Str(_)) => Ok(()),
            _ => Err("bytecode string constant out of range"),
        }
    };
    for &m in &p.main {
        check_proto(m)?;
    }
    if let Some(slot) = p.app {
        if (slot as usize) >= p.globals.len() {
            return Err("bytecode App global slot out of range");
        }
    }
    for e in &p.elems {
        if crate::ui::catalog::kind(&e.kind).is_none() {
            return Err("bytecode element kind is not in the catalog");
        }
    }
    for o in &p.objects {
        check_proto(o.init)?;
        for m in o.methods.values() {
            check_proto(m.proto)?;
        }
    }
    for proto in &p.protos {
        if proto.offsets.len() != proto.code.len() || proto.params > proto.locals {
            return Err("bytecode proto header mismatch");
        }
        let clen = proto.code.len();
        for &op in &proto.code {
            match op {
                Op::Const(i) if (i as usize) >= p.consts.len() => return Err("bytecode constant out of range"),
                Op::GetField(i) | Op::SetField(i) | Op::RecSet(i) | Op::CallMethod(i, _) | Op::NeedsConsent(i) => check_str(i)?,
                Op::LoadGlobal(i) | Op::StoreGlobal(i) if (i as usize) >= p.globals.len() => return Err("bytecode global out of range"),
                Op::LoadLocal(i) | Op::StoreLocal(i) | Op::MakeCell(i) | Op::LoadCell(i) | Op::StoreCell(i) | Op::Recell(i) if i >= proto.locals => {
                    return Err("bytecode local out of range");
                }
                Op::LoadUpval(i) | Op::StoreUpval(i) if (i as usize) >= proto.captures.len() => return Err("bytecode upval out of range"),
                Op::Closure(i) => check_proto(i)?,
                Op::Elem(i) if (i as usize) >= p.elems.len() => return Err("bytecode elem out of range"),
                Op::Jump(t)
                | Op::JumpIfFalse(t)
                | Op::JumpIfFalseKeep(t)
                | Op::JumpIfTrueKeep(t)
                | Op::JumpIfNotNullKeep(t)
                | Op::JumpIfNullKeep(t)
                | Op::TryBegin(t)
                    if (t as usize) > clen =>
                {
                    return Err("bytecode jump target out of range");
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Decodes and validates a binary `.tjb` bytecode image.
pub fn decode(bytes: &[u8]) -> Result<Program, String> {
    decode_inner(bytes).map_err(String::from)
}

fn decode_inner(bytes: &[u8]) -> Res<Program> {
    let mut r = Reader { bytes, pos: 0 };
    if r.take(4)? != MAGIC {
        return Err("invalid bytecode header (expected TJB v1)");
    }
    if r.u16()? as usize != builtins::GLOBALS.len() {
        return Err("bytecode builtin count mismatch; rebuild with this tj version");
    }

    let n_globals = r.count(4)?;
    let mut globals = Vec::with_capacity(n_globals);
    for _ in 0..n_globals {
        globals.push(r.str()?);
    }

    let n_consts = r.count(1)?;
    let mut consts = Vec::with_capacity(n_consts);
    for _ in 0..n_consts {
        let v = match r.u8()? {
            0 => Value::Null,
            1 => Value::Bool(r.bool()?),
            2 => Value::Num(r.f64()?),
            3 => Value::Str(r.str()?),
            4 => Value::Class(r.u16()?),
            _ => return Err("invalid constant tag in bytecode"),
        };
        consts.push(v);
    }

    let n_elems = r.count(10)?;
    let mut elems = Vec::with_capacity(n_elems);
    for _ in 0..n_elems {
        let kind = r.str()?;
        let n_props = r.count(4)?;
        let mut props = Vec::with_capacity(n_props);
        for _ in 0..n_props {
            props.push(r.str()?);
        }
        elems.push(ElemDesc { kind, props, children: r.u16()? });
    }

    let n_protos = r.count(17)?;
    let mut protos = Vec::with_capacity(n_protos);
    for _ in 0..n_protos {
        let name = r.str()?;
        let params = r.u16()?;
        let locals = r.u16()?;
        let source = match r.u8()? {
            0 => Source::User,
            1 => Source::Prelude,
            _ => return Err("invalid source tag in bytecode"),
        };
        let n_code = r.count(1)?;
        let mut code = Vec::with_capacity(n_code);
        for _ in 0..n_code {
            code.push(r.op()?);
        }
        let n_offsets = r.count(4)?;
        let mut offsets = Vec::with_capacity(n_offsets);
        for _ in 0..n_offsets {
            offsets.push(r.u32()?);
        }
        let n_caps = r.count(3)?;
        let mut captures = Vec::with_capacity(n_caps);
        for _ in 0..n_caps {
            captures.push(match r.u8()? {
                0 => Capture::Local(r.u16()?),
                1 => Capture::Upval(r.u16()?),
                _ => return Err("invalid capture tag in bytecode"),
            });
        }
        protos.push(Proto { name, params, locals, code, offsets, captures, source });
    }

    let n_objects = r.count(18)?;
    let mut objects = Vec::with_capacity(n_objects);
    for _ in 0..n_objects {
        let name = r.str()?;
        let n_fields = r.count(4)?;
        let mut fields = Vec::with_capacity(n_fields);
        for _ in 0..n_fields {
            fields.push(r.str()?);
        }
        let init = r.u32()?;
        let server = r.bool()?;
        let read = r.opt_str_list()?;
        let n_pii = r.count(13)?;
        let mut pii = Vec::with_capacity(n_pii);
        for _ in 0..n_pii {
            let field = r.str()?;
            let category = r.str()?;
            let purposes = r.str_list()?;
            let by = if r.bool()? {
                let person = r.str()?;
                let at = if r.bool()? { Some(r.str()?) } else { None };
                Some((person, at))
            } else {
                None
            };
            pii.push(Pii { field, category, purposes, by });
        }
        let n_guards = r.count(9)?;
        let mut guards = Vec::with_capacity(n_guards);
        for _ in 0..n_guards {
            guards.push(Guard { field: r.str()?, rule: r.str_list()?, pii: r.bool()? });
        }
        let retain = if r.bool()? { Some(Retain { ms: r.f64()?, from_created: r.bool()? }) } else { None };
        let hold = if r.bool()? { Some(r.f64()?) } else { None };
        let n_methods = r.count(12)?;
        let mut methods = IndexMap::with_capacity(n_methods);
        for _ in 0..n_methods {
            let mname = r.str()?;
            let proto = r.u32()?;
            let private = r.bool()?;
            let every = if r.bool()? { Some(r.f64()?) } else { None };
            let allow = r.opt_str_list()?;
            let exclusive = r.bool()?;
            let invariant = r.bool()?;
            let n_params = r.count(6)?;
            let mut params = Vec::with_capacity(n_params);
            for _ in 0..n_params {
                params.push((r.str()?, r.bool()?, r.arg_ty(0)?));
            }
            methods.insert(mname, MethodDef { proto, private, every, allow, exclusive, invariant, params });
        }
        objects.push(ObjectDef { name, fields, init, methods, server, read, pii, guards, retain, hold });
    }

    let n_main = r.count(4)?;
    let mut main = Vec::with_capacity(n_main);
    for _ in 0..n_main {
        main.push(r.u32()?);
    }
    let app = if r.bool()? { Some(r.u32()?) } else { None };
    if r.pos != bytes.len() {
        return Err("unexpected trailing bytes in bytecode");
    }
    for c in &consts {
        if let Value::Class(idx) = c {
            if (*idx as usize) >= objects.len() {
                return Err("bytecode class index out of range");
            }
        }
    }
    let program = Program { protos, consts, elems, objects, globals, main, app, user_source: Rc::from("") };
    validate(&program)?;
    Ok(program)
}

/// If `src` is a precompiled `//# tjb:1:<base64>` bundle, decodes and validates it.
pub fn decode_bundle(src: &str) -> Option<Result<Program, String>> {
    let payload = src.strip_prefix(BUNDLE_PREFIX)?.trim();
    Some(base64_decode(payload).and_then(|bytes| decode_inner(&bytes)).map_err(String::from))
}

fn base64_decode(s: &str) -> Res<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    let mut pad = 0u8;
    for b in s.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return Err("invalid base64 padding in bytecode bundle");
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("invalid base64 character in bytecode bundle"),
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    if pad > 2 || bits >= 6 {
        return Err("truncated base64 in bytecode bundle");
    }
    Ok(out)
}

// ---------- encoder (CLI / server / tests only; omitted from device builds) ----------

#[cfg(any(test, feature = "server", feature = "check"))]
mod enc {
    use super::*;

    struct Writer(Vec<u8>);

    impl Writer {
        fn u8(&mut self, v: u8) {
            self.0.push(v);
        }
        fn bool(&mut self, v: bool) {
            self.0.push(u8::from(v));
        }
        fn u16(&mut self, v: u16) {
            self.0.extend_from_slice(&v.to_le_bytes());
        }
        fn u32(&mut self, v: u32) {
            self.0.extend_from_slice(&v.to_le_bytes());
        }
        fn f64(&mut self, v: f64) {
            self.0.extend_from_slice(&v.to_le_bytes());
        }
        fn len(&mut self, n: usize) -> Result<(), String> {
            let n = u32::try_from(n).map_err(|_| "bytecode section exceeds u32 length")?;
            self.u32(n);
            Ok(())
        }
        fn str(&mut self, s: &str) -> Result<(), String> {
            self.len(s.len())?;
            self.0.extend_from_slice(s.as_bytes());
            Ok(())
        }
        fn str_list(&mut self, xs: &[Rc<str>]) -> Result<(), String> {
            self.len(xs.len())?;
            for s in xs {
                self.str(s)?;
            }
            Ok(())
        }
        fn opt_str_list(&mut self, xs: &Option<Rc<[Rc<str>]>>) -> Result<(), String> {
            match xs {
                Some(list) => {
                    self.bool(true);
                    self.str_list(list)
                }
                None => {
                    self.bool(false);
                    Ok(())
                }
            }
        }
        fn arg_ty(&mut self, t: &ArgTy) -> Result<(), String> {
            match t {
                ArgTy::Any => self.u8(0),
                ArgTy::Num => self.u8(1),
                ArgTy::Str => self.u8(2),
                ArgTy::Bool => self.u8(3),
                ArgTy::Null => self.u8(4),
                ArgTy::StrLit(s) => {
                    self.u8(5);
                    self.str(s)?;
                }
                ArgTy::NumLit(n) => {
                    self.u8(6);
                    self.f64(*n);
                }
                ArgTy::BoolLit(b) => {
                    self.u8(7);
                    self.bool(*b);
                }
                ArgTy::Arr(inner) => {
                    self.u8(8);
                    self.arg_ty(inner)?;
                }
                ArgTy::Rec(fields) => {
                    self.u8(9);
                    self.len(fields.len())?;
                    for (k, opt, ft) in fields.iter() {
                        self.str(k)?;
                        self.bool(*opt);
                        self.arg_ty(ft)?;
                    }
                }
                ArgTy::Dict(inner) => {
                    self.u8(10);
                    self.arg_ty(inner)?;
                }
                ArgTy::Union(ts) => {
                    self.u8(11);
                    self.len(ts.len())?;
                    for x in ts {
                        self.arg_ty(x)?;
                    }
                }
            }
            Ok(())
        }
        fn op(&mut self, op: Op) {
            match op {
                Op::NeedsConsent(a) => { self.u8(0); self.u32(a); }
                Op::Const(a) => { self.u8(1); self.u32(a); }
                Op::Null => self.u8(2),
                Op::True => self.u8(3),
                Op::False => self.u8(4),
                Op::Pop => self.u8(5),
                Op::Dup => self.u8(6),
                Op::Dup2 => self.u8(7),
                Op::LoadLocal(a) => { self.u8(8); self.u16(a); }
                Op::StoreLocal(a) => { self.u8(9); self.u16(a); }
                Op::MakeCell(a) => { self.u8(10); self.u16(a); }
                Op::LoadCell(a) => { self.u8(11); self.u16(a); }
                Op::StoreCell(a) => { self.u8(12); self.u16(a); }
                Op::Recell(a) => { self.u8(13); self.u16(a); }
                Op::LoadUpval(a) => { self.u8(14); self.u16(a); }
                Op::StoreUpval(a) => { self.u8(15); self.u16(a); }
                Op::LoadGlobal(a) => { self.u8(16); self.u32(a); }
                Op::StoreGlobal(a) => { self.u8(17); self.u32(a); }
                Op::GetField(a) => { self.u8(18); self.u32(a); }
                Op::SetField(a) => { self.u8(19); self.u32(a); }
                Op::GetIndex => self.u8(20),
                Op::SetIndex => self.u8(21),
                Op::NewArr => self.u8(22),
                Op::ArrPush => self.u8(23),
                Op::ArrSpread => self.u8(24),
                Op::NewRec => self.u8(25),
                Op::RecSet(a) => { self.u8(26); self.u32(a); }
                Op::RecSetDyn => self.u8(27),
                Op::RecSpread => self.u8(28),
                Op::Add => self.u8(29),
                Op::Sub => self.u8(30),
                Op::Mul => self.u8(31),
                Op::Div => self.u8(32),
                Op::Mod => self.u8(33),
                Op::Neg => self.u8(34),
                Op::Not => self.u8(35),
                Op::Eq => self.u8(36),
                Op::Ne => self.u8(37),
                Op::Lt => self.u8(38),
                Op::Le => self.u8(39),
                Op::Gt => self.u8(40),
                Op::Ge => self.u8(41),
                Op::Concat(a) => { self.u8(42); self.u16(a); }
                Op::Len => self.u8(43),
                Op::Jump(a) => { self.u8(44); self.u32(a); }
                Op::JumpIfFalse(a) => { self.u8(45); self.u32(a); }
                Op::JumpIfFalseKeep(a) => { self.u8(46); self.u32(a); }
                Op::JumpIfTrueKeep(a) => { self.u8(47); self.u32(a); }
                Op::JumpIfNotNullKeep(a) => { self.u8(48); self.u32(a); }
                Op::JumpIfNullKeep(a) => { self.u8(49); self.u32(a); }
                Op::Call(a) => { self.u8(50); self.u8(a); }
                Op::CallMethod(a, b) => { self.u8(51); self.u32(a); self.u8(b); }
                Op::Return => self.u8(52),
                Op::Closure(a) => { self.u8(53); self.u32(a); }
                Op::Elem(a) => { self.u8(54); self.u32(a); }
                Op::Await => self.u8(55),
                Op::Caller => self.u8(56),
                Op::Throw => self.u8(57),
                Op::TryBegin(a) => { self.u8(58); self.u32(a); }
                Op::TryEnd => self.u8(59),
            }
        }
    }

    pub fn encode(p: &Program) -> Result<Vec<u8>, String> {
        let mut w = Writer(Vec::with_capacity(4096));
        w.0.extend_from_slice(MAGIC);
        w.u16(builtins::GLOBALS.len() as u16);

        w.str_list(&p.globals)?;

        w.len(p.consts.len())?;
        for c in &p.consts {
            match c {
                Value::Null => w.u8(0),
                Value::Bool(b) => { w.u8(1); w.bool(*b); }
                Value::Num(n) => { w.u8(2); w.f64(*n); }
                Value::Str(s) => { w.u8(3); w.str(s)?; }
                Value::Class(id) => { w.u8(4); w.u16(*id); }
                other => return Err(format!("cannot serialize runtime constant of type {}", other.type_name())),
            }
        }

        w.len(p.elems.len())?;
        for e in &p.elems {
            w.str(&e.kind)?;
            w.str_list(&e.props)?;
            w.u16(e.children);
        }

        w.len(p.protos.len())?;
        for pr in &p.protos {
            w.str(&pr.name)?;
            w.u16(pr.params);
            w.u16(pr.locals);
            w.u8(match pr.source { Source::User => 0, Source::Prelude => 1 });
            w.len(pr.code.len())?;
            for &op in &pr.code {
                w.op(op);
            }
            w.len(pr.offsets.len())?;
            for &off in &pr.offsets {
                w.u32(off);
            }
            w.len(pr.captures.len())?;
            for &cap in &pr.captures {
                match cap {
                    Capture::Local(i) => { w.u8(0); w.u16(i); }
                    Capture::Upval(i) => { w.u8(1); w.u16(i); }
                }
            }
        }

        w.len(p.objects.len())?;
        for o in &p.objects {
            w.str(&o.name)?;
            w.str_list(&o.fields)?;
            w.u32(o.init);
            w.bool(o.server);
            w.opt_str_list(&o.read)?;
            w.len(o.pii.len())?;
            for pii in &o.pii {
                w.str(&pii.field)?;
                w.str(&pii.category)?;
                w.str_list(&pii.purposes)?;
                match &pii.by {
                    Some((person, at)) => {
                        w.bool(true);
                        w.str(person)?;
                        match at {
                            Some(a) => { w.bool(true); w.str(a)?; }
                            None => w.bool(false),
                        }
                    }
                    None => w.bool(false),
                }
            }
            w.len(o.guards.len())?;
            for g in &o.guards {
                w.str(&g.field)?;
                w.str_list(&g.rule)?;
                w.bool(g.pii);
            }
            match o.retain {
                Some(ret) => { w.bool(true); w.f64(ret.ms); w.bool(ret.from_created); }
                None => w.bool(false),
            }
            match o.hold {
                Some(h) => { w.bool(true); w.f64(h); }
                None => w.bool(false),
            }
            w.len(o.methods.len())?;
            for (mname, m) in &o.methods {
                w.str(mname)?;
                w.u32(m.proto);
                w.bool(m.private);
                match m.every {
                    Some(ms) => { w.bool(true); w.f64(ms); }
                    None => w.bool(false),
                }
                w.opt_str_list(&m.allow)?;
                w.bool(m.exclusive);
                w.bool(m.invariant);
                w.len(m.params.len())?;
                for (pname, opt, ty) in &m.params {
                    w.str(pname)?;
                    w.bool(*opt);
                    w.arg_ty(ty)?;
                }
            }
        }

        w.len(p.main.len())?;
        for &m in &p.main {
            w.u32(m);
        }
        match p.app {
            Some(slot) => { w.bool(true); w.u32(slot); }
            None => w.bool(false),
        }
        Ok(w.0)
    }

    pub fn base64_encode(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = if chunk.len() > 1 { u32::from(chunk[1]) } else { 0 };
            let b2 = if chunk.len() > 2 { u32::from(chunk[2]) } else { 0 };
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
        }
        out
    }
}

#[cfg(any(test, feature = "server", feature = "check"))]
pub fn encode(p: &Program) -> Result<Vec<u8>, String> {
    enc::encode(p)
}

#[cfg(any(test, feature = "server", feature = "check"))]
pub fn encode_bundle(p: &Program) -> Result<String, String> {
    Ok(format!("{BUNDLE_PREFIX}{}", enc::base64_encode(&encode(p)?)))
}

#[cfg(any(test, feature = "server", feature = "check"))]
pub fn bundle_from_bytes(bytes: &[u8]) -> String {
    format!("{BUNDLE_PREFIX}{}", enc::base64_encode(bytes))
}

impl Program {
    /// Decodes and validates a binary `.tjb` bytecode image.
    pub fn from_bytes(bytes: &[u8]) -> Result<Program, String> {
        decode(bytes)
    }

    /// If `src` starts with `//# tjb:1:`, decodes the precompiled bytecode bundle.
    pub fn from_bundle(src: &str) -> Option<Result<Program, String>> {
        decode_bundle(src)
    }

    #[cfg(any(test, feature = "server", feature = "check"))]
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        encode(self)
    }

    #[cfg(any(test, feature = "server", feature = "check"))]
    pub fn to_bundle(&self) -> Result<String, String> {
        encode_bundle(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile, Input, MemStore, Options, Output, Runtime, Target};
    use serde_json::json;

    #[test]
    fn bytecode_round_trips_and_runs_identically_without_compiler() {
        let src = r#"
@server @read("anyone") object Counter {
  n: number = 0
  @invariant nonNeg(): boolean { return this.n >= 0 }
  @allow("anyone") add(by: number): number { this.n += by; return this.n }
}

function App() {
  const c = Counter.get("main")
  return (
    <Column>
      <Text id="count">{c.n}</Text>
      <Button id="inc" onPress={() => c.add(2)}>Add</Button>
    </Column>
  )
}
"#;
        let prog = compile(src).unwrap();
        let bytes = prog.to_bytes().unwrap();
        assert_eq!(&bytes[..4], MAGIC);
        let decoded = Program::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.objects.len(), prog.objects.len());
        assert_eq!(decoded.protos.len(), prog.protos.len());
        assert_eq!(decoded.elems.len(), prog.elems.len());
        assert_eq!(decoded.app, prog.app);
        assert!(decoded.user_source.is_empty(), "AOT bytecode strips source text");

        // Run decoded program on a server runtime and verify invariant + method call.
        let (mut rt, _) = Runtime::boot(decoded, Box::new(MemStore::default()), Options { server: true, render: false, ..Options::new(1.0, 1) }).unwrap();
        let out = rt.handle(2.0, Input::Call { object: "Counter/main".into(), method: "add".into(), args: vec![json!(5)], reply: Some(1), call: None, caller: Some(crate::runtime::protocol::Caller::trusted()) });
        assert!(out.iter().any(|o| matches!(o, Output::Reply { reply: 1, ok: Some(v), .. } if *v == json!(5))), "{out:?}");
        let bad = rt.handle(3.0, Input::Call { object: "Counter/main".into(), method: "add".into(), args: vec![json!(-10)], reply: Some(2), call: None, caller: Some(crate::runtime::protocol::Caller::trusted()) });
        assert!(bad.iter().any(|o| matches!(o, Output::Reply { reply: 2, status: Some(422), .. })), "invariant must still enforce on AOT bytecode: {bad:?}");

        // Bundle round-trip via JsonHost::boot (web/Android/iOS path).
        let bundle = prog.to_bundle().unwrap();
        assert!(bundle.starts_with(BUNDLE_PREFIX));
        let mut host = crate::host::JsonHost::default();
        let (code, _) = host.boot(&bundle, Box::new(MemStore::default()), None, 1.0, 1, true);
        assert_eq!(code, crate::host::OK);
        let (code, body) = host.handle(2.0, &serde_json::to_string(&Input::Ui { target: Target::Id { id: "inc".into() }, event: "onPress".into(), value: serde_json::Value::Null, reply: Some(9) }).unwrap());
        assert_eq!(code, crate::host::OK, "{body}");
    }

    #[test]
    fn bytecode_decoder_rejects_corrupt_and_malformed_inputs() {
        assert!(Program::from_bytes(b"NOPE").is_err());
        assert!(Program::from_bytes(b"TJB\x01").is_err());
        let prog = compile("function f() { return 1 }").unwrap();
        let mut bytes = prog.to_bytes().unwrap();
        bytes.push(0);
        assert!(Program::from_bytes(&bytes).err().unwrap().contains("trailing bytes"));
        bytes.pop();
        // Flip builtin count to simulate mismatched runtime.
        bytes[4] ^= 0xff;
        assert!(Program::from_bytes(&bytes).err().unwrap().contains("builtin count"));
        assert!(Program::from_bundle("//# tjb:1:!!!not-base64!!!").unwrap().is_err());
        assert!(Program::from_bundle("function f() { return 1 }").is_none());
    }

    #[test]
    fn every_example_round_trips_through_aot_bytecode() {
        let cart = crate::modules::bundle(&[
            ("app.tj".into(), include_str!("../../../../examples/durable-cart/app.tj").into()),
            ("shelf.tj".into(), include_str!("../../../../examples/durable-cart/shelf.tj").into()),
            ("cart.tj".into(), include_str!("../../../../examples/durable-cart/cart.tj").into()),
            ("assistant.tj".into(), include_str!("../../../../examples/durable-cart/assistant.tj").into()),
        ]);
        for src in [
            include_str!("../../../../examples/todo/app.tj"),
            include_str!("../../../../examples/chat/app.tj"),
            cart.as_str(),
            include_str!("../../../../examples/smart-meter/app.tj"),
        ] {
            let prog = compile(src).unwrap();
            let bytes = prog.to_bytes().unwrap();
            let decoded = Program::from_bytes(&bytes).unwrap();
            let (mut rt, out) = Runtime::boot(decoded, Box::new(MemStore::default()), Options { server: true, ..Options::new(1_800_000_000_000.0, 1) }).unwrap();
            assert!(!out.iter().any(|o| matches!(o, Output::Error { .. })), "{out:?}");
            let tick = rt.handle(1_800_000_001_000.0, Input::Tick);
            assert!(!tick.iter().any(|o| matches!(o, Output::Error { .. })), "{tick:?}");
        }
    }
}
