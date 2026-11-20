//! `tj bench`: how fast an app compiles, boots and runs, as numbers an agent can optimise.
//! `tj bench --budget <file>` fails when an app or a build artifact got slower or bigger
//! than its budget, so "fast and small" cannot drift unnoticed.
//!
//! The scenario is fixed: boot with every object local (server and UI in one runtime) at a
//! fixed clock and seed, then one minute of app time, one tick a second. The counters
//! (VM steps, patch ops, nodes) are exact and the same on every machine. Times are the best
//! of a few runs, so they are less noisy, but they still depend on the machine.

use serde_json::{json, Value as Json};
use std::path::Path;
use std::time::Instant;
use tj_core::{compile, Input, MemStore, Options, Output, Runtime};

const RUNS: usize = 5;
const START: f64 = 1_800_000_000_000.0;
const TICKS: usize = 60;

pub struct Bench {
    pub compile_ms: f64,
    pub boot_ms: f64,
    /// Average time to handle one tick (timers, re-render, diff) during the minute.
    pub tick_ms: f64,
    pub nodes: u64,
    pub first_render_ops: u64,
    pub renders: u64,
    /// Totals over boot and the minute.
    pub vm_steps: u64,
    pub patch_ops: u64,
}

impl Bench {
    pub fn to_json(&self) -> Json {
        json!({
            "compile_ms": round(self.compile_ms), "boot_ms": round(self.boot_ms), "tick_ms": round(self.tick_ms),
            "nodes": self.nodes, "first_render_ops": self.first_render_ops, "renders": self.renders,
            "vm_steps": self.vm_steps, "patch_ops": self.patch_ops,
        })
    }

    pub fn report(&self, name: &str) -> String {
        format!(
            "{name}\n  compile   {:>8.3} ms\n  boot      {:>8.3} ms   first render: {} nodes, {} patch ops\n  1 minute  {:>8.3} ms per tick ({TICKS} ticks, {} renders)\n  totals    {} vm steps, {} patch ops\n",
            self.compile_ms, self.boot_ms, self.nodes, self.first_render_ops, self.tick_ms, self.renders, self.vm_steps, self.patch_ops
        )
    }
}

fn round(ms: f64) -> f64 {
    (ms * 1000.0).round() / 1000.0
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// Fails on the first app error, so a benchmark never measures a broken app.
fn check(out: &[Output]) -> Result<u64, String> {
    let mut ops = 0;
    for o in out {
        match o {
            Output::Error { message, .. } => return Err(format!("the app failed: {message}")),
            Output::Patch { ops: p } => ops += p.len() as u64,
            _ => {}
        }
    }
    Ok(ops)
}

pub fn bench(name: &str, source: &str) -> Result<Bench, String> {
    let mut best: Option<Bench> = None;
    for _ in 0..RUNS {
        let t = Instant::now();
        let program = compile(source).map_err(|errs| errs.iter().map(|d| d.render(name, source)).collect::<Vec<_>>().join("\n\n"))?;
        let compile_ms = ms(t);
        let t = Instant::now();
        let opts = Options { server: true, ..Options::new(START, 1) };
        let (mut rt, out) = Runtime::boot(program, Box::new(MemStore::default()), opts).map_err(|e| format!("boot failed: {e}"))?;
        let boot_ms = ms(t);
        let first_render_ops = check(&out)?;
        let t = Instant::now();
        for i in 1..=TICKS {
            check(&rt.handle(START + i as f64 * 1000.0, Input::Tick))?;
        }
        let tick_ms = ms(t) / TICKS as f64;
        let inspect = rt.handle(START + (TICKS as f64 + 1.0) * 1000.0, Input::Inspect { reply: Some(1) });
        let metrics = inspect.iter().find_map(|o| match o {
            Output::Reply { ok: Some(snapshot), .. } => Some(snapshot["metrics"].clone()),
            _ => None,
        });
        let m = metrics.ok_or("the app did not answer inspect")?;
        let count = |k: &str| m[k].as_u64().unwrap_or(0);
        let run = Bench {
            compile_ms,
            boot_ms,
            tick_ms,
            nodes: count("nodes"),
            first_render_ops,
            renders: count("renders"),
            vm_steps: count("vmSteps"),
            patch_ops: count("patchOps"),
        };
        best = Some(match best {
            None => run,
            Some(b) => Bench { compile_ms: b.compile_ms.min(run.compile_ms), boot_ms: b.boot_ms.min(run.boot_ms), tick_ms: b.tick_ms.min(run.tick_ms), ..run },
        });
    }
    best.ok_or_else(|| "no runs".to_string())
}

/// Checks a budget file:
///
/// ```json
/// { "files": { "target/wasm32-unknown-unknown/small/tj_wasm.wasm": 700 },
///   "apps": { "examples/todo/app.tj": { "boot_ms": 5, "tick_ms": 1, "vm_steps": 9000, "patch_ops": 200 } } }
/// ```
///
/// File sizes are in KB. Paths are relative to the current directory; on Windows a missing
/// file is also looked for with `.exe`. Every listed limit is a maximum.
pub fn check_budget(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let budget: Json = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
    let mut over = Vec::new();
    for (file, limit) in budget["files"].as_object().into_iter().flatten() {
        let limit = limit.as_f64().ok_or(format!("budget for {file} must be a number of KB"))?;
        let exe = format!("{file}{}", std::env::consts::EXE_SUFFIX);
        let size = std::fs::metadata(file).or_else(|_| std::fs::metadata(&exe)).map(|m| m.len() as f64 / 1024.0);
        match size {
            Ok(kb) => {
                let ok = kb <= limit;
                println!("{} {file}: {kb:.0} KB (budget {limit:.0} KB)", if ok { "ok  " } else { "OVER" });
                if !ok {
                    over.push(file.clone());
                }
            }
            Err(_) => {
                println!("MISS {file}: not built");
                over.push(file.clone());
            }
        }
    }
    for (app, limits) in budget["apps"].as_object().into_iter().flatten() {
        let source = crate::app::read(Path::new(app))?.source;
        let measured = bench(app, &source)?.to_json();
        for (key, limit) in limits.as_object().into_iter().flatten() {
            let (Some(limit), Some(value)) = (limit.as_f64(), measured[key.as_str()].as_f64()) else {
                return Err(format!("{app}: unknown budget '{key}' (use one of {})", measured.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()));
            };
            let ok = value <= limit;
            println!("{} {app} {key}: {value} (budget {limit})", if ok { "ok  " } else { "OVER" });
            if !ok {
                over.push(format!("{app} {key}"));
            }
        }
    }
    if over.is_empty() {
        Ok(())
    } else {
        Err(format!("over budget: {}", over.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_exact_and_app_errors_fail_the_bench() {
        let src = "object Clock {\n  ticks: number = 0\n  @every(\"1s\") tick() { this.ticks += 1 }\n}\nfunction App() {\n  return <Text>{String(Clock.get(\"c\").ticks)}</Text>\n}\n";
        let a = bench("clock.tj", src).expect("runs");
        let b = bench("clock.tj", src).expect("runs");
        assert_eq!((a.vm_steps, a.patch_ops, a.renders), (b.vm_steps, b.patch_ops, b.renders), "the same on every run");
        assert_eq!(a.nodes, 1);
        assert!(a.renders >= TICKS as u64, "every tick re-renders: {}", a.renders);

        let broken = "function App() {\n  return <Text>{String(JSON.parse(\"{\"))}</Text>\n}\n";
        assert!(bench("broken.tj", broken).err().unwrap_or_default().contains("failed"));
    }
}
