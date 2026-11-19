//! JNI entry points for `com.mindmakr.tinyjump.Native`, over [`tj_core::host::JsonHost`].
//! All calls come from the Android main thread; storage calls go back to the Java
//! `FileStore` on that same thread.
//!
//! Only meaningful on Android; on other targets this crate is empty so the workspace
//! still builds everywhere.
#![cfg(target_os = "android")]

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jdouble, jint, jlong, jstring, JNI_TRUE};
use jni::{JNIEnv, JavaVM};
use std::cell::RefCell;
use tj_core::host::{JsonHost, INVALID};
use tj_core::Store;

thread_local! {
    static HOST: RefCell<JsonHost> = RefCell::new(JsonHost::default());
    static OUT: RefCell<String> = const { RefCell::new(String::new()) };
}

const STRING_SIG: &str = "Ljava/lang/String;";

/// Storage backed by the Java `FileStore` (`load`, `save`, `keys`).
struct JavaStore {
    vm: JavaVM,
    store: GlobalRef,
}

enum Ret {
    Str(Option<String>),
    Bool(bool),
}

impl JavaStore {
    fn call(&self, name: &str, args: &[&str], returns_bool: bool) -> Option<Ret> {
        let mut env = self.vm.get_env().ok()?;
        let sig = format!("({}){}", STRING_SIG.repeat(args.len()), if returns_bool { "Z" } else { STRING_SIG });
        let result = env.with_local_frame(8, |env| -> jni::errors::Result<Ret> {
            let strings = args.iter().map(|a| env.new_string(a)).collect::<Result<Vec<_>, _>>()?;
            let values: Vec<JValue> = strings.iter().map(|s| JValue::Object(s)).collect();
            let v = env.call_method(&self.store, name, &sig, &values)?;
            if returns_bool {
                return Ok(Ret::Bool(v.z()?));
            }
            let obj = v.l()?;
            if obj.is_null() {
                return Ok(Ret::Str(None));
            }
            Ok(Ret::Str(Some(env.get_string(&JString::from(obj))?.into())))
        });
        match result {
            Ok(r) => Some(r),
            Err(_) => {
                // A Java exception must be cleared before any further JNI call.
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_describe();
                    let _ = env.exception_clear();
                }
                None
            }
        }
    }
}

impl Store for JavaStore {
    fn load(&mut self, key: &str) -> Option<String> {
        match self.call("load", &[key], false) {
            Some(Ret::Str(s)) => s,
            _ => None,
        }
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        match self.call("save", &[key, value], true) {
            Some(Ret::Bool(true)) => Ok(()),
            _ => Err(format!("could not write {key}")),
        }
    }

    fn keys(&mut self, prefix: &str) -> Vec<String> {
        match self.call("keys", &[prefix], false) {
            Some(Ret::Str(Some(json))) => tj_core::host::parse_keys(&json),
            _ => Vec::new(),
        }
    }
}

fn finish((code, body): (i32, String)) -> jint {
    OUT.with(|o| *o.borrow_mut() = body);
    code
}

fn invalid(message: &str) -> jint {
    finish((INVALID, format!("{{\"message\":{message:?}}}")))
}

#[no_mangle]
pub extern "system" fn Java_com_mindmakr_tinyjump_Native_boot<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    store: JObject<'l>,
    source: JString<'l>,
    now: jdouble,
    seed: jlong,
    render: jboolean,
) -> jint {
    let (Ok(vm), Ok(store), Ok(source)) = (env.get_java_vm(), env.new_global_ref(store), env.get_string(&source)) else {
        return invalid("bad boot arguments");
    };
    let source: String = source.into();
    let store = Box::new(JavaStore { vm, store });
    finish(HOST.with(|h| h.borrow_mut().boot(&source, store, now, seed as u64, render == JNI_TRUE)))
}

#[no_mangle]
pub extern "system" fn Java_com_mindmakr_tinyjump_Native_handle<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, now: jdouble, input: JString<'l>) -> jint {
    match env.get_string(&input) {
        Ok(input) => {
            let input: String = input.into();
            finish(HOST.with(|h| h.borrow_mut().handle(now, &input)))
        }
        Err(_) => invalid("bad input string"),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_mindmakr_tinyjump_Native_out<'l>(env: JNIEnv<'l>, _class: JClass<'l>) -> jstring {
    OUT.with(|o| env.new_string(o.borrow().as_str()).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut()))
}
