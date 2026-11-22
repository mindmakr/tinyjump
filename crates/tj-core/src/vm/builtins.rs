//! Built-in globals and the native methods of strings, numbers and arrays.
//! Higher-order array methods (`map`, `filter`, …) live in `prelude.tj`.

use super::value::{error_record, fmt_num, Exc, Native, NativeFn, Record, Value};
use crate::runtime::Runtime;
use std::rc::Rc;

pub type NativeMethod = fn(&mut Runtime, &Value, &[Value]) -> Result<Value, Exc>;

/// A global's name and the function that builds its value.
pub type Global = (&'static str, fn() -> Value);

/// Global names and constructors, in global-slot order. The compiler resolves names
/// against this list and the runtime builds the values from it.
pub const GLOBALS: &[Global] = &[
    ("console", || namespace(&[("log", console_log), ("warn", console_warn), ("error", console_error)])),
    ("Math", math),
    ("JSON", || namespace(&[("stringify", json_stringify), ("parse", json_parse)])),
    ("Object", || namespace(&[("keys", object_keys), ("values", object_values), ("entries", object_entries)])),
    ("Date", || namespace(&[("now", date_now)])),
    ("Time", || namespace(&[("add", super::time::add), ("startOf", super::time::start_of), ("parts", super::time::parts), ("from", super::time::from)])),
    // `reveal(value, purpose)`: personal data leaving on purpose. The compiler checks the
    // purpose (flow.rs); at run time it is the value.
    ("reveal", || native("reveal", |_, a| Ok(arg(a, 0).clone()))),
    ("crypto", || namespace(&[("hmac", crypto_hmac), ("timingSafeEqual", timing_safe_equal), ("randomId", random_id)])),
    ("Array", || namespace(&[("isArray", |_, a| Ok(Value::Bool(matches!(arg(a, 0), Value::Arr(_)))))])),
    ("String", || native("String", |_, a| Ok(Value::str(&arg(a, 0).display())))),
    ("Number", || native("Number", |_, a| Ok(Value::Num(to_number(arg(a, 0)))))),
    ("parseInt", || native("parseInt", parse_int)),
    ("parseFloat", || native("parseFloat", |_, a| Ok(Value::Num(parse_float_prefix(&arg(a, 0).display()))))),
    ("encodeURIComponent", || native("encodeURIComponent", |_, a| Ok(Value::Str(encode_uri_component(&arg(a, 0).display()).into())))),
    ("Error", || native("Error", |_, a| Ok(error_record(&arg(a, 0).display())))),
    // `check(ok, field, message)`: a rule on input, the same in UI code and on the server.
    ("check", || native("check", check)),
    ("__fetch", || native("fetch", fetch)),
    // Only plugin modules (`tj:<name>`) may call these; apps import their typed functions.
    ("plugin", || native("plugin", plugin)),
    ("invoke", || native("invoke", invoke)),
    ("__sort", || native("sort", sort)),
];

fn native(name: &'static str, f: NativeFn) -> Value {
    Value::Native(Native { name, f })
}

fn namespace(fns: &[(&'static str, NativeFn)]) -> Value {
    Value::rec(fns.iter().map(|(n, f)| (Rc::from(*n), native(n, *f))).collect())
}

fn arg(args: &[Value], i: usize) -> &Value {
    args.get(i).unwrap_or(&Value::Null)
}

fn num(args: &[Value], i: usize, what: &str) -> Result<f64, Exc> {
    match arg(args, i) {
        Value::Num(n) => Ok(*n),
        v => Err(Exc::msg(format!("{what} expects a number, got {}", v.type_name()))),
    }
}

fn opt_num(args: &[Value], i: usize, default: f64) -> f64 {
    match arg(args, i) {
        Value::Num(n) => *n,
        _ => default,
    }
}

fn text(args: &[Value], i: usize, what: &str) -> Result<Rc<str>, Exc> {
    match arg(args, i) {
        Value::Str(s) => Ok(s.clone()),
        v => Err(Exc::msg(format!("{what} expects a string, got {}", v.type_name()))),
    }
}

fn log_line(args: &[Value]) -> String {
    args.iter().map(Value::display).collect::<Vec<_>>().join(" ")
}

fn console_log(rt: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    rt.log("info", log_line(a));
    Ok(Value::Null)
}

fn console_warn(rt: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    rt.log("warn", log_line(a));
    Ok(Value::Null)
}

fn console_error(rt: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    rt.log("error", log_line(a));
    Ok(Value::Null)
}

fn math() -> Value {
    let Value::Rec(r) = namespace(&[
        ("floor", |_, a| Ok(Value::Num(num(a, 0, "Math.floor")?.floor()))),
        ("ceil", |_, a| Ok(Value::Num(num(a, 0, "Math.ceil")?.ceil()))),
        ("round", |_, a| Ok(Value::Num((num(a, 0, "Math.round")? + 0.5).floor()))),
        ("trunc", |_, a| Ok(Value::Num(num(a, 0, "Math.trunc")?.trunc()))),
        ("abs", |_, a| Ok(Value::Num(num(a, 0, "Math.abs")?.abs()))),
        ("sign", |_, a| Ok(Value::Num(num(a, 0, "Math.sign")?.signum()))),
        ("sqrt", |_, a| Ok(Value::Num(num(a, 0, "Math.sqrt")?.sqrt()))),
        ("pow", |_, a| Ok(Value::Num(num(a, 0, "Math.pow")?.powf(num(a, 1, "Math.pow")?)))),
        ("min", |_, a| fold_nums(a, f64::INFINITY, f64::min)),
        ("max", |_, a| fold_nums(a, f64::NEG_INFINITY, f64::max)),
        ("random", |rt, _| Ok(Value::Num(rt.random()))),
    ]) else {
        unreachable!()
    };
    r.borrow_mut().insert("PI".into(), Value::Num(std::f64::consts::PI));
    Value::Rec(r)
}

fn fold_nums(a: &[Value], init: f64, f: fn(f64, f64) -> f64) -> Result<Value, Exc> {
    let mut acc = init;
    for i in 0..a.len() {
        acc = f(acc, num(a, i, "Math.min/max")?);
    }
    Ok(Value::Num(acc))
}

fn json_stringify(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let j = arg(a, 0).to_json().map_err(|e| Exc::msg(format!("JSON.stringify: {e}")))?;
    let pretty = opt_num(a, 2, 0.0) > 0.0;
    let s = if pretty { serde_json::to_string_pretty(&j) } else { serde_json::to_string(&j) };
    Ok(Value::str(&s.unwrap_or_default()))
}

fn json_parse(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let s = text(a, 0, "JSON.parse")?;
    serde_json::from_str(&s).map(|j| Value::from_json(&j)).map_err(|e| Exc::msg(format!("JSON.parse: {e}")))
}

fn record_arg<'a>(a: &'a [Value], what: &str) -> Result<&'a super::value::Shared<Record>, Exc> {
    match arg(a, 0) {
        Value::Rec(r) => Ok(r),
        v => Err(Exc::msg(format!("{what} expects a record, got {}", v.type_name()))),
    }
}

fn object_keys(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let r = record_arg(a, "Object.keys")?;
    let keys = r.borrow().keys().map(|k| Value::Str(k.clone())).collect();
    Ok(Value::arr(keys))
}

fn object_values(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let r = record_arg(a, "Object.values")?;
    let values = r.borrow().values().map(|v| Value::inside(r.readonly, v.clone())).collect();
    Ok(Value::arr(values))
}

fn object_entries(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let r = record_arg(a, "Object.entries")?;
    let entries = r.borrow().iter().map(|(k, v)| Value::arr(vec![Value::Str(k.clone()), Value::inside(r.readonly, v.clone())])).collect();
    Ok(Value::arr(entries))
}

fn date_now(rt: &mut Runtime, _: &[Value]) -> Result<Value, Exc> {
    rt.read_clock = true;
    Ok(Value::Num(rt.now()))
}

/// As JavaScript's: every byte of the UTF-8 text is `%XX` except letters, digits and
/// `-_.!~*'()`.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn to_number(v: &Value) -> f64 {
    match v {
        Value::Num(n) => *n,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Null => 0.0,
        Value::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
}

fn parse_int(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let s = arg(a, 0).display();
    let radix = opt_num(a, 1, 10.0) as u32;
    if !(2..=36).contains(&radix) {
        return Ok(Value::Num(f64::NAN));
    }
    let t = s.trim_start();
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits: String = t.chars().take_while(|c| c.is_digit(radix)).collect();
    if digits.is_empty() {
        return Ok(Value::Num(f64::NAN));
    }
    let n = digits.chars().fold(0.0, |acc, c| acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0)));
    Ok(Value::Num(if neg { -n } else { n }))
}

fn parse_float_prefix(s: &str) -> f64 {
    let t = s.trim_start();
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_exp = false;
    let b = t.as_bytes();
    while end < b.len() {
        let c = b[end];
        let ok = c.is_ascii_digit()
            || ((c == b'-' || c == b'+') && (end == 0 || matches!(b[end - 1], b'e' | b'E')))
            || (c == b'.' && !seen_dot && !seen_exp)
            || ((c == b'e' || c == b'E') && !seen_exp && end > 0);
        if !ok {
            break;
        }
        seen_dot |= c == b'.';
        seen_exp |= c == b'e' || c == b'E';
        end += 1;
    }
    // Back off a dangling exponent or sign, e.g. "1e" or "2e-".
    let mut slice = &t[..end];
    while !slice.is_empty() && slice.parse::<f64>().is_err() {
        slice = &slice[..slice.len() - 1];
    }
    slice.parse().unwrap_or(f64::NAN)
}

fn fetch(rt: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let url = text(a, 0, "fetch")?;
    let init = match arg(a, 1) {
        Value::Null => serde_json::Value::Null,
        v => v.to_json().map_err(|e| Exc::msg(format!("fetch options: {e}")))?,
    };
    let method = init.get("method").and_then(|m| m.as_str()).unwrap_or("GET").to_uppercase();
    let headers = init.get("headers").cloned().unwrap_or_else(|| serde_json::json!({}));
    let body = match init.get("body") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    };
    Ok(Value::Promise(rt.fetch(url.to_string(), method, headers, body)))
}

/// `check(ok, field, message)`: unless `ok`, throws `{ name: "Error", message, field }`, so a
/// form shows `message` by the field that is wrong. Rules written once, in a function the UI
/// and the server both call.
fn check(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let ok = match arg(a, 0) {
        Value::Bool(b) => *b,
        v => return Err(Exc::msg(format!("check expects true or false first, got {}", v.type_name()))),
    };
    if ok {
        return Ok(Value::Null);
    }
    let (field, message) = (text(a, 1, "check field")?, text(a, 2, "check message")?);
    let e = error_record(&message);
    if let Value::Rec(r) = &e {
        r.borrow_mut().insert("field".into(), Value::Str(field));
    }
    Err(Exc(e, super::value::ExcKind::User))
}

/// `plugin(name, method, args)`: a call to a plugin, answered by the host (docs/plugins.md).
fn plugin(rt: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let name = text(a, 0, "plugin")?.to_string();
    let method = text(a, 1, "plugin")?.to_string();
    let mut args = arg(a, 2).to_json().map_err(|e| Exc::msg(format!("plugin arguments must be data: {e}")))?;
    // tj:privacy acts for the running call's caller, which code cannot choose.
    if name == "privacy" {
        let serde_json::Value::Object(fields) = &mut args else { return Err(Exc::msg("tj:privacy takes a record of arguments")) };
        fields.insert("caller".into(), rt.running_caller.record().to_json().map_err(Exc::msg)?);
    }
    Ok(Value::Promise(rt.plugin_call(name, method, args)))
}

/// `invoke(object, method, args)`: calls an app method as the current caller, under the
/// same access rules as a request from them (docs/plugins.md, `tj:agent`).
fn invoke(rt: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let object = text(a, 0, "invoke")?.to_string();
    let method = text(a, 1, "invoke")?.to_string();
    let args = match arg(a, 2) {
        Value::Arr(items) => items.borrow().clone(),
        Value::Null => Vec::new(),
        _ => return Err(Exc::msg("invoke(object, method, args) takes the arguments as an array")),
    };
    Ok(Value::Promise(rt.invoke_as_caller(object, method, args)?))
}

fn sort(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let Value::Arr(arr) = arg(a, 0) else { return Err(Exc::msg("sort expects an array")) };
    let mut items = arr.writable()?.borrow_mut();
    if items.iter().all(|v| matches!(v, Value::Num(_))) {
        items.sort_by(|x, y| match (x, y) {
            (Value::Num(x), Value::Num(y)) => x.total_cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
    } else if items.iter().all(|v| matches!(v, Value::Str(_))) {
        items.sort_by(|x, y| match (x, y) {
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
    } else {
        return Err(Exc::msg("sort() without a compare function needs all numbers or all strings; pass (a, b) => ..."));
    }
    drop(items);
    Ok(arg(a, 0).clone())
}

// ---------- methods on built-in types ----------

pub fn method(recv: &Value, name: &str) -> Option<NativeMethod> {
    match recv {
        Value::Arr(_) => array_method(name),
        Value::Str(_) => string_method(name),
        Value::Num(_) => number_method(name),
        _ => None,
    }
}

/// Clamps a JavaScript-style relative index (negative counts from the end).
fn rel_index(i: f64, len: usize) -> usize {
    if i.is_nan() {
        0
    } else if i < 0.0 {
        (len as f64 + i).max(0.0) as usize
    } else {
        (i as usize).min(len)
    }
}

fn the_arr(v: &Value) -> &super::value::Shared<Vec<Value>> {
    match v {
        Value::Arr(a) => a,
        _ => unreachable!("array method on non-array"),
    }
}

/// The array, to change in place: refused when it belongs to another object.
fn the_mut_arr(v: &Value) -> Result<&Rc<std::cell::RefCell<Vec<Value>>>, Exc> {
    the_arr(v).writable()
}

fn array_method(name: &str) -> Option<NativeMethod> {
    Some(match name {
        "push" => |_, r, a| {
            let mut v = the_mut_arr(r)?.borrow_mut();
            v.extend_from_slice(a);
            Ok(Value::Num(v.len() as f64))
        },
        "pop" => |_, r, _| Ok(the_mut_arr(r)?.borrow_mut().pop().unwrap_or(Value::Null)),
        "shift" => |_, r, _| {
            let mut v = the_mut_arr(r)?.borrow_mut();
            Ok(if v.is_empty() { Value::Null } else { v.remove(0) })
        },
        "unshift" => |_, r, a| {
            let mut v = the_mut_arr(r)?.borrow_mut();
            v.splice(0..0, a.iter().cloned());
            Ok(Value::Num(v.len() as f64))
        },
        "slice" => |_, r, a| {
            let arr = the_arr(r);
            let v = arr.borrow();
            let start = rel_index(opt_num(a, 0, 0.0), v.len());
            let end = rel_index(opt_num(a, 1, v.len() as f64), v.len());
            let items = if start < end { v[start..end].iter().map(|x| Value::inside(arr.readonly, x.clone())).collect() } else { Vec::new() };
            Ok(Value::arr(items))
        },
        "splice" => |_, r, a| {
            let mut v = the_mut_arr(r)?.borrow_mut();
            let start = rel_index(opt_num(a, 0, 0.0), v.len());
            let count = (opt_num(a, 1, (v.len() - start) as f64).max(0.0) as usize).min(v.len() - start);
            let removed: Vec<Value> = v.splice(start..start + count, a.iter().skip(2).cloned()).collect();
            Ok(Value::arr(removed))
        },
        "concat" => |_, r, a| {
            let arr = the_arr(r);
            let mut out: Vec<Value> = arr.borrow().iter().map(|x| Value::inside(arr.readonly, x.clone())).collect();
            for x in a {
                match x {
                    Value::Arr(other) => out.extend(other.borrow().iter().map(|x| Value::inside(other.readonly, x.clone()))),
                    other => out.push(other.clone()),
                }
            }
            Ok(Value::arr(out))
        },
        "indexOf" => |_, r, a| {
            let v = the_arr(r).borrow();
            Ok(Value::Num(v.iter().position(|x| x.strict_eq(arg(a, 0))).map_or(-1.0, |i| i as f64)))
        },
        "includes" => |_, r, a| Ok(Value::Bool(the_arr(r).borrow().iter().any(|x| x.strict_eq(arg(a, 0))))),
        "join" => |_, r, a| {
            let sep = match arg(a, 0) {
                Value::Str(s) => s.to_string(),
                _ => ",".into(),
            };
            let v = the_arr(r).borrow();
            Ok(Value::str(&v.iter().map(|x| if matches!(x, Value::Null) { String::new() } else { x.display() }).collect::<Vec<_>>().join(&sep)))
        },
        "reverse" => |_, r, _| {
            the_mut_arr(r)?.borrow_mut().reverse();
            Ok(r.clone())
        },
        "at" => |_, r, a| {
            let v = the_arr(r).borrow();
            let i = opt_num(a, 0, 0.0);
            let idx = if i < 0.0 { v.len() as f64 + i } else { i };
            Ok(Value::inside(the_arr(r).readonly, if idx >= 0.0 { v.get(idx as usize).cloned().unwrap_or(Value::Null) } else { Value::Null }))
        },
        _ => return None,
    })
}

fn the_str(v: &Value) -> &str {
    match v {
        Value::Str(s) => s,
        _ => unreachable!("string method on non-string"),
    }
}

fn char_slice(s: &str, start: usize, end: usize) -> String {
    s.chars().skip(start).take(end.saturating_sub(start)).collect()
}

fn string_method(name: &str) -> Option<NativeMethod> {
    Some(match name {
        "toUpperCase" => |_, r, _| Ok(Value::str(&the_str(r).to_uppercase())),
        "toLowerCase" => |_, r, _| Ok(Value::str(&the_str(r).to_lowercase())),
        "trim" => |_, r, _| Ok(Value::str(the_str(r).trim())),
        "trimStart" => |_, r, _| Ok(Value::str(the_str(r).trim_start())),
        "trimEnd" => |_, r, _| Ok(Value::str(the_str(r).trim_end())),
        "includes" => |_, r, a| Ok(Value::Bool(the_str(r).contains(&*text(a, 0, "includes")?))),
        "startsWith" => |_, r, a| Ok(Value::Bool(the_str(r).starts_with(&*text(a, 0, "startsWith")?))),
        "endsWith" => |_, r, a| Ok(Value::Bool(the_str(r).ends_with(&*text(a, 0, "endsWith")?))),
        "indexOf" => |_, r, a| {
            let s = the_str(r);
            let needle = text(a, 0, "indexOf")?;
            Ok(Value::Num(s.find(&*needle).map_or(-1.0, |byte| s[..byte].chars().count() as f64)))
        },
        "slice" => |_, r, a| {
            let s = the_str(r);
            let len = s.chars().count();
            let start = rel_index(opt_num(a, 0, 0.0), len);
            let end = rel_index(opt_num(a, 1, len as f64), len);
            Ok(Value::str(&char_slice(s, start, end)))
        },
        "substring" => |_, r, a| {
            let s = the_str(r);
            let len = s.chars().count();
            let clamp = |n: f64| if n.is_nan() { 0 } else { (n.max(0.0) as usize).min(len) };
            let (x, y) = (clamp(opt_num(a, 0, 0.0)), clamp(opt_num(a, 1, len as f64)));
            Ok(Value::str(&char_slice(s, x.min(y), x.max(y))))
        },
        "split" => |_, r, a| {
            let s = the_str(r);
            let parts: Vec<Value> = match arg(a, 0) {
                Value::Str(sep) if sep.is_empty() => s.chars().map(|c| Value::str(&c.to_string())).collect(),
                Value::Str(sep) => s.split(&**sep).map(Value::str).collect(),
                _ => vec![Value::str(s)],
            };
            Ok(Value::arr(parts))
        },
        "replace" => |_, r, a| Ok(Value::str(&the_str(r).replacen(&*text(a, 0, "replace")?, &text(a, 1, "replace")?, 1))),
        "replaceAll" => |_, r, a| Ok(Value::str(&the_str(r).replace(&*text(a, 0, "replaceAll")?, &text(a, 1, "replaceAll")?))),
        "padStart" => |_, r, a| pad(r, a, true),
        "padEnd" => |_, r, a| pad(r, a, false),
        "repeat" => |_, r, a| {
            let n = num(a, 0, "repeat")?;
            if !(0.0..=1e6).contains(&n) {
                return Err(Exc::msg("repeat count out of range"));
            }
            Ok(Value::str(&the_str(r).repeat(n as usize)))
        },
        "charAt" | "at" => |_, r, a| {
            let s = the_str(r);
            let i = opt_num(a, 0, 0.0);
            let idx = if i < 0.0 { s.chars().count() as f64 + i } else { i };
            Ok(if idx >= 0.0 { s.chars().nth(idx as usize).map_or(Value::str(""), |c| Value::str(&c.to_string())) } else { Value::str("") })
        },
        _ => return None,
    })
}

fn pad(r: &Value, a: &[Value], start: bool) -> Result<Value, Exc> {
    let s = the_str(r);
    let width = num(a, 0, "pad")? as usize;
    let fill = match arg(a, 1) {
        Value::Str(f) if !f.is_empty() => f.to_string(),
        _ => " ".into(),
    };
    let len = s.chars().count();
    if width <= len {
        return Ok(r.clone());
    }
    let padding: String = fill.chars().cycle().take(width - len).collect();
    Ok(Value::str(&if start { padding + s } else { s.to_string() + &padding }))
}

fn number_method(name: &str) -> Option<NativeMethod> {
    Some(match name {
        "toFixed" => |_, r, a| {
            let Value::Num(n) = r else { unreachable!() };
            let digits = opt_num(a, 0, 0.0).clamp(0.0, 20.0) as usize;
            Ok(Value::str(&format!("{n:.digits$}")))
        },
        "toString" => |_, r, _| {
            let Value::Num(n) = r else { unreachable!() };
            Ok(Value::str(&fmt_num(*n)))
        },
        _ => return None,
    })
}

/// `crypto.hmac(key, text)`: HMAC-SHA256 as hex. There is no plain hash: a hash of an email
/// or a phone number is easy to reverse by trying them all.
fn crypto_hmac(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let (key, msg) = (text(a, 0, "crypto.hmac key")?, text(a, 1, "crypto.hmac text")?);
    Ok(Value::str(&crate::crypto::hex(&crate::crypto::hmac(key.as_bytes(), &[msg.as_bytes()]))))
}

/// `crypto.timingSafeEqual(a, b)`: compares secrets without revealing, by the time taken,
/// how much of them matched.
fn timing_safe_equal(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let (x, y) = (text(a, 0, "crypto.timingSafeEqual")?, text(a, 1, "crypto.timingSafeEqual")?);
    Ok(Value::Bool(crate::crypto::same(x.as_bytes(), y.as_bytes())))
}

/// `crypto.randomId()`: 128 bits from the host's secure random source, as 32 hex characters.
fn random_id(rt: &mut Runtime, _: &[Value]) -> Result<Value, Exc> {
    let mut id = [0u8; 16];
    rt.secure_bytes(&mut id)?;
    Ok(Value::str(&crate::crypto::hex(&id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_float_prefixes() {
        assert_eq!(parse_float_prefix("3.5kg"), 3.5);
        assert_eq!(parse_float_prefix("1e3x"), 1000.0);
        assert_eq!(parse_float_prefix("2e"), 2.0);
        assert!(parse_float_prefix("x").is_nan());
    }

    #[test]
    fn relative_indexes() {
        assert_eq!(rel_index(-1.0, 5), 4);
        assert_eq!(rel_index(-10.0, 5), 0);
        assert_eq!(rel_index(9.0, 5), 5);
    }
}
