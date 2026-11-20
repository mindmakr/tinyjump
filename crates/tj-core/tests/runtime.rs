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

    let out = rt.handle(1.0, Input::EffectDone { id, ok: Some(json!({ "status": 200, "headers": {}, "body": "{\"temp\": 21.5}" })), error: None, unreachable: false });
    assert_eq!(reply(&out).unwrap(), json!(21.5));

    let out = rt.handle(2.0, Input::Call { object: "Weather/w".into(), method: "load".into(), args: vec![json!("X")], reply: Some(10), call: None, caller: Some(Caller::trusted()) });
    let id = out.iter().find_map(|o| if let Output::Effect { id, .. } = o { Some(*id) } else { None }).unwrap();
    let out = rt.handle(3.0, Input::EffectDone { id, ok: None, error: Some("network down".into()), unreachable: false });
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
    device.handle(3.0, Input::EffectDone { id, ok: Some(json!(1)), error: None, unreachable: false });
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
    Input::EffectDone { id, ok: Some(ok), error: None, unreachable: false }
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
    let out = device.handle(2000.0, Input::EffectDone { id: first[0].0, ok: None, error: Some("offline".into()), unreachable: true });
    assert!(server_calls(&out).is_empty() && !out.iter().any(|o| matches!(o, Output::Error { .. })), "{out:?}");
    let retry = server_calls(&device.handle(3000.0, Input::Tick));
    assert_eq!((retry.len(), &retry[0].1), (1, &first[0].1), "a retry keeps its call id");

    // Unreachable again; the backoff doubles, but the host's `online` sends it at once.
    device.handle(3100.0, Input::EffectDone { id: retry[0].0, ok: None, error: Some("offline".into()), unreachable: true });
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
    assert_eq!(rt.erase_subject("user:ana").err().map(|e| e.0), Some(409));
    rt.handle(3.0, answered(effect, json!({ "status": 200, "headers": {}, "body": "" })));

    // Erase: back to the defaults, alarms gone, and the blank state goes out as a change.
    let (erased, out) = rt.erase_subject("user:ana").unwrap();
    assert_eq!(erased, ["Account/user:ana"]);
    let change = out.iter().find_map(|o| if let Output::Changed { object, state, seq } = o { Some((object.clone(), state.clone(), *seq)) } else { None }).expect("replicas hear of it");
    assert_eq!((change.0.as_str(), &change.1), ("Account/user:ana", &json!({ "email": "", "notes": "", "plan": "free" })));
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
    assert_eq!(rt.erase_subject("user:ana").err().map(|e| e.0), Some(503));
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
    // Like any thrown error, it keeps the changes made before it failed (see
    // feedback/2026-11-20-builder-005-failed-calls-keep-changes.md).
    assert!(state(&mut rt, 3.0, "Spin/s")["n"].as_u64().unwrap() > 0);
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
    for method in ["direct", "alias", "nested", "element", "returned", "copyInside", "firstOne", "wrapper", "values", "entries", "sorted"] {
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
fn signing_in_as_someone_else_drops_their_queued_calls_and_replicas() {
    let store = Shared::default();
    let (mut device, _) = boot_as(BOARD, store.clone(), 0.0, false);
    device.handle(0.0, Input::Identity { user: Some("user:ana".into()) });
    device.handle(1.0, Input::Sync { object: "Board/b".into(), state: json!({ "notes": ["ana's"] }), seq: Some(5) });
    let sent = server_calls(&device.handle(2.0, note("from ana")));
    device.handle(3.0, Input::EffectDone { id: sent[0].0, ok: None, error: Some("offline".into()), unreachable: true });

    let out = device.handle(4.0, Input::Identity { user: Some("user:bo".into()) });
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
    assert!(msg("@server object A { @every(\"1s\") n: number = 0 }").contains("fields take one @pii"));
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
    out.iter().filter_map(|o| if let Output::Reply { reply, ok, error, status } = o { Some((*reply, ok.clone(), error.clone(), *status)) } else { None }).collect()
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