//! End-to-end tests: source in, outputs out.

use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::rc::Rc;
use tj_core::runtime::protocol::Effect;
use tj_core::{compile, Input, MemStore, Options, Output, PatchOp, Runtime, Store, Target};

/// A store the test can still read after handing it to a runtime.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<MemStore>>);

impl Store for Shared {
    fn load(&mut self, key: &str) -> Option<String> {
        self.0.borrow_mut().load(key)
    }
    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.0.borrow_mut().save(key, value)
    }
    fn keys(&mut self, prefix: &str) -> Vec<String> {
        self.0.borrow_mut().keys(prefix)
    }
}

fn boot_with(src: &str, store: Shared, now: f64) -> (Runtime, Vec<Output>) {
    boot_as(src, store, now, false)
}

fn boot_as(src: &str, store: Shared, now: f64, server: bool) -> (Runtime, Vec<Output>) {
    let program = compile(src).unwrap_or_else(|errs| panic!("{}", errs.iter().map(|e| e.render("test.tj", src)).collect::<Vec<_>>().join("\n")));
    Runtime::boot(program, Box::new(store), Options { now, seed: 7, render: !server, server }).unwrap_or_else(|e| panic!("boot: {e}"))
}

fn boot(src: &str) -> Runtime {
    boot_with(src, Shared::default(), 0.0).0
}

fn reply(outputs: &[Output]) -> Result<Json, String> {
    for o in outputs {
        if let Output::Reply { ok, error, .. } = o {
            return match error {
                Some(e) => Err(e.clone()),
                None => Ok(ok.clone().unwrap_or(Json::Null)),
            };
        }
    }
    panic!("no reply in {outputs:?}")
}

fn call(rt: &mut Runtime, now: f64, object: &str, method: &str, args: Json) -> Result<Json, String> {
    let args = args.as_array().cloned().unwrap_or_default();
    reply(&rt.handle(now, Input::Call { object: object.into(), method: method.into(), args, reply: Some(1) }))
}

/// Runs `body` as a method and returns its result.
fn eval(body: &str) -> Result<Json, String> {
    let src = format!("object T {{\n run() {{\n{body}\n }}\n}}");
    let mut rt = boot(&src);
    call(&mut rt, 0.0, "T/t", "run", json!([]))
}

#[test]
fn arithmetic_strings_and_templates() {
    assert_eq!(eval("return 1 + 2 * 3 - 4 / 2").unwrap(), json!(5));
    assert_eq!(eval("const n = 3; return `n=${n} sq=${n * n}`").unwrap(), json!("n=3 sq=9"));
    assert_eq!(eval("return 'a' + 1 + 2").unwrap(), json!("a12"));
    assert_eq!(eval("return 7 % 3 === 1 && !(1 > 2)").unwrap(), json!(true));
    assert_eq!(eval("return null ?? 'x'").unwrap(), json!("x"));
    assert_eq!(eval("return 0 || 'y'").unwrap(), json!("y"));
}

#[test]
fn closures_capture_by_reference() {
    let body = r#"
        function makeCounter() { let n = 0; return () => { n += 1; return n } }
        const c = makeCounter(); c(); c()
        const fns = []
        for (let i = 0; i < 3; i++) { fns.push(() => i) }
        return [c(), fns.map(f => f())]
    "#;
    assert_eq!(eval(body).unwrap(), json!([3, [0, 1, 2]]));
}

#[test]
fn loops_destructuring_and_spread() {
    let body = r#"
        let total = 0
        for (const { price, qty = 1 } of [{ price: 2, qty: 3 }, { price: 5 }]) { total += price * qty }
        const [a, b] = [10, 20]
        const base = { x: 1, y: 2 }
        const r = { ...base, y: 3, [`k${a}`]: b }
        let w = 0
        while (true) { w++; if (w >= 4) { break } }
        return { total, r, all: [...[1, 2], 3], w }
    "#;
    assert_eq!(eval(body).unwrap(), json!({ "total": 11, "r": { "x": 1, "y": 3, "k10": 20 }, "all": [1, 2, 3], "w": 4 }));
}

#[test]
fn array_and_string_library() {
    let body = r#"
        const xs = [5, 3, 8, 1]
        const sorted = xs.slice(0).sort()
        const desc = xs.slice(0).sort((a, b) => b - a)
        return {
          sorted, desc,
          even: xs.filter(x => x % 2 === 0),
          sum: xs.reduce((s, x) => s + x, 0),
          found: xs.find(x => x > 4),
          idx: xs.indexOf(8),
          words: "a,b,c".split(",").map(s => s.toUpperCase()).join("-"),
          pad: "7".padStart(3, "0"),
          fixed: (2.5).toFixed(2),
          keys: Object.keys({ p: 1, q: 2 }),
          json: JSON.parse(JSON.stringify({ z: [1, "2"] })),
          rounded: Math.round(2.5),
        }
    "#;
    assert_eq!(
        eval(body).unwrap(),
        json!({
            "sorted": [1, 3, 5, 8], "desc": [8, 5, 3, 1], "even": [8], "sum": 17, "found": 5, "idx": 2,
            "words": "A-B-C", "pad": "007", "fixed": "2.50", "keys": ["p", "q"], "json": { "z": [1, "2"] }, "rounded": 3
        })
    );
}

#[test]
fn optional_chaining_and_errors() {
    assert_eq!(eval("const a = { b: null }; return a.b?.c.d ?? 'none'").unwrap(), json!("none"));
    assert_eq!(eval("try { throw new Error('boom') } catch (e) { return e.message }").unwrap(), json!("boom"));
    assert_eq!(eval("try { const x = null; return x.y } catch (e) { return 'caught' }").unwrap(), json!("caught"));
    let err = eval("const x = null; return x.y").unwrap_err();
    assert!(err.contains("cannot read 'y' of null"), "{err}");
    // The type checker rejects `1 - "a"`; values from outside (any) are still checked at runtime.
    assert!(eval("const a = JSON.parse('\"a\"'); return 1 - a").unwrap_err().contains("arithmetic needs numbers"));
}

#[test]
fn recursion_and_runaway_protection() {
    assert_eq!(eval("function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2) } return fib(15)").unwrap(), json!(610));
    assert!(eval("function f() { return f() } return f()").unwrap_err().contains("stack overflow"));
}

const COUNTER: &str = r#"
object Counter {
  count: number = 0
  history: number[] = []

  increment(by: number = 1) {
    this.count += by
    this.history.push(this.count)
    this.emit("changed", { count: this.count })
    return this.count
  }

  later(ms: number) { this.schedule(ms, "increment", 10) }

  @every("1s") tick() { this.count += 100 }

  private secret() { return 42 }
}
"#;

#[test]
fn objects_persist_across_reboots() {
    let store = Shared::default();
    let (mut rt, _) = boot_with(COUNTER, store.clone(), 0.0);
    assert_eq!(call(&mut rt, 1.0, "Counter/main", "increment", json!([2])).unwrap(), json!(2));
    let out = rt.handle(2.0, Input::Call { object: "Counter/main".into(), method: "increment".into(), args: vec![], reply: None });
    assert!(out.iter().any(|o| matches!(o, Output::Emit { name, data, .. } if name == "changed" && data == &json!({ "count": 3 }))));
    assert!(out.iter().any(|o| matches!(o, Output::Changed { object, .. } if object == "Counter/main")));
    drop(rt);

    let (mut rt, _) = boot_with(COUNTER, store, 3.0);
    let state = reply(&rt.handle(3.0, Input::Get { object: "Counter/main".into(), reply: Some(1) })).unwrap();
    assert_eq!(state, json!({ "count": 3, "history": [2, 3] }));
}

#[test]
fn object_encapsulation() {
    let mut rt = boot(COUNTER);
    assert!(call(&mut rt, 0.0, "Counter/a", "secret", json!([])).unwrap_err().contains("private"));
    assert!(call(&mut rt, 0.0, "Counter/a", "nope", json!([])).unwrap_err().contains("no method 'nope'"));
    assert!(call(&mut rt, 0.0, "Ghost/a", "x", json!([])).unwrap_err().contains("no object type named Ghost"));
    let src = format!("{COUNTER}\nobject Hacker {{ poke() {{ Counter.get(\"a\").count = 9 }} }}");
    let mut rt = boot(&src);
    assert!(call(&mut rt, 0.0, "Hacker/h", "poke", json!([])).unwrap_err().contains("only Counter's own methods"));
}

#[test]
fn every_timers_and_durable_alarms() {
    let store = Shared::default();
    let (mut rt, _) = boot_with(COUNTER, store.clone(), 0.0);
    call(&mut rt, 0.0, "Counter/c", "later", json!([5000])).unwrap();
    assert_eq!(rt.next_wake(), Some(1000.0));
    rt.handle(1000.0, Input::Tick);
    rt.handle(2000.0, Input::Tick);
    let state = reply(&rt.handle(2000.0, Input::Get { object: "Counter/c".into(), reply: Some(1) })).unwrap();
    assert_eq!(state["count"], json!(200));
    drop(rt);

    // After a restart the instance and its pending alarm come back.
    let (mut rt, _) = boot_with(COUNTER, store, 2500.0);
    for t in [3500.0, 4500.0, 5000.0] {
        rt.handle(t, Input::Tick);
    }
    let state = reply(&rt.handle(5000.0, Input::Get { object: "Counter/c".into(), reply: Some(1) })).unwrap();
    assert_eq!(state["count"], json!(200 + 200 + 10), "two more beats after restart plus the alarm");
}

const APP: &str = r#"
object Todos {
  items: { text: string, done: boolean }[] = []
  draft = ""
  setDraft(t: string) { this.draft = t }
  add() {
    if (this.draft.trim() === "") { return }
    this.items.push({ text: this.draft, done: false })
    this.draft = ""
  }
  toggle(i: number) { this.items[i].done = !this.items[i].done }
}

function Item({ item, onToggle }) {
  return <Row gap={8}>
    <Toggle value={item.done} onChange={onToggle}>{item.text}</Toggle>
  </Row>
}

function App() {
  const todos = Todos.get("main")
  return <Column padding={16}>
    <Text size={20}>Todos ({todos.items.length})</Text>
    <Input id="draft" value={todos.draft} placeholder="What next?" onChange={(t) => todos.setDraft(t)} />
    <Button id="add" onPress={() => todos.add()}>Add</Button>
    {todos.items.map((item, i) => <Item key={i} item={item} onToggle={() => todos.toggle(i)} />)}
    {todos.items.length === 0 && "Nothing yet"}
  </Column>
}
"#;

fn patches(out: &[Output]) -> Vec<PatchOp> {
    out.iter().filter_map(|o| if let Output::Patch { ops } = o { Some(ops.clone()) } else { None }).flatten().collect()
}

#[test]
fn renders_and_reacts_to_ui_events() {
    let (mut rt, boot_out) = boot_with(APP, Shared::default(), 0.0);
    let ops = patches(&boot_out);
    assert!(ops.iter().any(|op| matches!(op, PatchOp::Create { kind, props, .. } if kind == "Text" && props["text"] == json!("Todos (0)"))));
    assert!(ops.iter().any(|op| matches!(op, PatchOp::Create { kind, props, .. } if kind == "Text" && props["text"] == json!("Nothing yet"))));

    let ui = |target: Target, event: &str, value: Json| Input::Ui { target, event: event.into(), value, reply: Some(1) };
    reply(&rt.handle(1.0, ui(Target::Id { id: "draft".into() }, "onChange", json!("Buy milk")))).unwrap();
    let out = rt.handle(2.0, ui(Target::Id { id: "add".into() }, "onPress", Json::Null));
    reply(&out).unwrap();
    let ops = patches(&out);
    assert!(ops.iter().any(|op| matches!(op, PatchOp::Update { props, .. } if props.get("text") == Some(&json!("Todos (1)")))));
    assert!(ops.iter().any(|op| matches!(op, PatchOp::Create { kind, props, .. } if kind == "Toggle" && props["text"] == json!("Buy milk"))));

    let out = rt.handle(3.0, ui(Target::Text { text: "Buy milk".into() }, "onChange", json!(true)));
    reply(&out).unwrap();
    let snapshot = reply(&rt.handle(4.0, Input::Inspect { reply: Some(1) })).unwrap();
    assert_eq!(snapshot["objects"][0]["state"]["items"][0]["done"], json!(true));
    assert_eq!(snapshot["tree"][0]["kind"], json!("Column"));
}

#[test]
fn fetch_suspends_and_resumes() {
    let src = r#"
    object Weather {
      temp: number | null = null
      error = ""
      async load(city: string) {
        try {
          const res = await fetch(`https://api.example.com/w?city=${city}`, { headers: { accept: "application/json" } })
          if (!res.ok) { throw new Error(`status ${res.status}`) }
          const data = await res.json()
          this.temp = data.temp
          return this.temp
        } catch (e) {
          this.error = e.message
          return null
        }
      }
    }
    "#;
    let mut rt = boot(src);
    let out = rt.handle(0.0, Input::Call { object: "Weather/w".into(), method: "load".into(), args: vec![json!("Oslo")], reply: Some(9) });
    let (id, url) = out
        .iter()
        .find_map(|o| match o {
            Output::Effect { id, effect: Effect::Http { url, .. } } => Some((*id, url.clone())),
            _ => None,
        })
        .expect("an http effect");
    assert_eq!(url, "https://api.example.com/w?city=Oslo");
    assert!(!out.iter().any(|o| matches!(o, Output::Reply { .. })), "reply waits for the effect");

    let out = rt.handle(1.0, Input::EffectDone { id, ok: Some(json!({ "status": 200, "headers": {}, "body": "{\"temp\": 21.5}" })), error: None });
    assert_eq!(reply(&out).unwrap(), json!(21.5));

    let out = rt.handle(2.0, Input::Call { object: "Weather/w".into(), method: "load".into(), args: vec![json!("X")], reply: Some(10) });
    let id = out.iter().find_map(|o| if let Output::Effect { id, .. } = o { Some(*id) } else { None }).unwrap();
    let out = rt.handle(3.0, Input::EffectDone { id, ok: None, error: Some("network down".into()) });
    assert_eq!(reply(&out).unwrap(), Json::Null);
    let state = reply(&rt.handle(4.0, Input::Get { object: "Weather/w".into(), reply: Some(1) })).unwrap();
    assert_eq!(state["error"], json!("network down"));
}

#[test]
fn random_is_deterministic_per_seed() {
    let src = "object R { roll() { return Math.random() } }";
    let a = call(&mut boot(src), 0.0, "R/r", "roll", json!([])).unwrap();
    let b = call(&mut boot(src), 0.0, "R/r", "roll", json!([])).unwrap();
    assert_eq!(a, b);
}

const CHAT: &str = r#"
type Msg = { author: string, text: string }

@server object Room {
  messages: Msg[] = []
  post(author: string, text: string): number {
    this.messages.push({ author, text })
    return this.messages.length
  }
  private clear() { this.messages = [] }
}

object Me { name = "ana" }

function App() {
  const room = Room.get("lobby")
  return <Column>
    {room.messages.map((m, i) => <Text key={i}>{m.author}: {m.text}</Text>)}
    <Button id="send" onPress={() => room.post(Me.get("me").name, "hi")}>Send</Button>
  </Column>
}
"#;

#[test]
fn server_objects_replicate_to_devices() {
    let (mut server, _) = boot_as(CHAT, Shared::default(), 0.0, true);
    let device_store = Shared::default();
    let (mut device, out) = boot_as(CHAT, device_store.clone(), 0.0, false);
    assert!(out.iter().any(|o| matches!(o, Output::Subscribe { object } if object == "Room/lobby")));

    // Pressing the button asks the host to call the server instead of running the method here.
    let out = device.handle(1.0, Input::Ui { target: Target::Id { id: "send".into() }, event: "onPress".into(), value: Json::Null, reply: None });
    let (id, (object, method, args)) = out
        .iter()
        .find_map(|o| match o {
            Output::Effect { id, effect: Effect::Call { object, method, args } } => Some((*id, (object.clone(), method.clone(), args.clone()))),
            _ => None,
        })
        .expect("a server call");
    assert_eq!((object.as_str(), method.as_str(), &args), ("Room/lobby", "post", &vec![json!("ana"), json!("hi")]));

    // The server runs it, persists it, and reports the change.
    let out = server.handle(2.0, Input::Call { object, method, args, reply: Some(1) });
    assert_eq!(reply(&out).unwrap(), json!(1));
    let state = out
        .iter()
        .find_map(|o| match o {
            Output::Changed { object, state } if object == "Room/lobby" => Some(state.clone()),
            _ => None,
        })
        .expect("a change to broadcast");

    // The host relays the result and the change; the device re-renders from its replica.
    device.handle(3.0, Input::EffectDone { id, ok: Some(json!(1)), error: None });
    let out = device.handle(3.0, Input::Sync { object: "Room/lobby".into(), state });
    assert!(patches(&out).iter().any(|op| matches!(op, PatchOp::Create { props, .. } if props.get("text") == Some(&json!("ana: hi")))));

    // Replicas never run methods locally.
    assert!(call(&mut device, 4.0, "Room/lobby", "post", json!(["x", "y"])).unwrap_err().contains("lives on the server"));

    // The replica is cached, so the device shows it when it starts offline.
    let (mut again, _) = boot_as(CHAT, device_store, 5.0, false);
    let cached = reply(&again.handle(5.0, Input::Get { object: "Room/lobby".into(), reply: Some(1) })).unwrap();
    assert_eq!(cached["messages"][0]["text"], json!("hi"));
}
