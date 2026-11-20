# Critic 026: Review of Part 1 (`m1-dates`, commit `b82b30b`)

**Verdict:** Hold merge on `m1-dates` (`b82b30b`) for four fixes (`P1-1` to `P1-4`): `<Time>` drops style props on Android/iOS, `tinyjump.js` uses post-2020 `dateStyle`/`timeStyle` and `'sv-SE'` locale assumptions plus lenient `"24:00"` / trailing-text parsing, `forAgent` can be bypassed via `target.text`, and iOS `UIDatePicker` keeps stray seconds.

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-019-part1-dates.md` (`b82b30b` on `m1-dates`)
- **Status:** open

---

## Findings

### P1-1 (High). `<Time>` drops all style and accessibility props on Android and iOS, and Android skips reformatting `<Input>` when `zone` or `type` updates

1. **`hosts/android/.../Renderer.java:351-354` and `hosts/ios/Sources/TinyJump/Renderer.swift:257-260`:**
   Both `setProp` implementations return unconditionally whenever `kind == "Time"`, even when `k` is `size`, `weight`, `color`, `background`, `padding`, `margin`, `radius`, `border`, `lines`, or `label`. On Android and iOS, `<Time size={14} weight="bold" color="#666" label="Appointment" value={ms} />` ignores every style and accessibility prop (unlike Web `tinyjump.js:396-399`, which returns only when `k` is `value`, `show`, `style`, or `zone`). Move `return` inside the `value|show|style|zone` check on both native hosts.
2. **`Renderer.java:356-362` and `tinyjump.js:401-412`:**
   On Android, `if (!"value".equals(k)) return;` skips updating `EditText`'s text when an `update` patch changes `zone` or `type` without resending `value` (unlike iOS `Renderer.swift:261, 399`). Format `n.props.opt("value")` on `type`, `zone`, and `value` updates alike. On Web (`tinyjump.js:407`), save `if (kind === 'Input' && k === 'value') el.__value = v;` before checking `el.__type` so dynamic `type` updates always have `el.__value`.

- **Case for / against:** ~8 lines across `Renderer.java`, `Renderer.swift`, and `tinyjump.js`; 0 bytes in WASM.

---

### P1-2 (High). Web host 2019-browser compatibility, `'sv-SE'` ICU reliance, and lenient `"24:00"` / trailing-character parsing (`hosts/web/tinyjump.js:339-382`)

1. **Post-2020 `dateStyle` / `timeStyle` (`tinyjump.js:355-357` vs `docs/vision.md:298`):**
   `Intl.DateTimeFormat` added `dateStyle` and `timeStyle` in Safari 14.1 (2021) and Firefox 78 (mid-2020); 2019 Safari/Firefox ignore `timeStyle` (rendering a date with no time for `show="time"`) or throw. Use standard ES6 component options (`year: 'numeric', month: 'short', day: 'numeric'` and `hour: 'numeric', minute: '2-digit'`), which work back to 2015.
2. **`'sv-SE'` locale assumption in `isoParts` (`tinyjump.js:341-344`):**
   On stripped WebViews without Swedish ICU data, `Intl.DateTimeFormat('sv-SE', ...)` falls back to the device's default locale (for example `'en-US'` `"12/01/2026, 14:30"`), making `typedValue` line 377 (`isoParts(ms, zone).replace(' ', 'T') !== full.slice(0, 16)`) reject **every valid date input as `null`**. Since `zone` is either `"UTC"` (`getUTCFullYear` etc.) or the viewer's zone (`getFullYear` etc.), formatting `YYYY-MM-DD HH:MM` directly from `Date` getters takes 6 lines of ES5 with zero ICU locale risk.
3. **Lenient web parsing mismatches vs Android and iOS (`tinyjump.js:363-382`):**
   - `type === 'time'` with `"24:00"`: `new Date('1970-01-01T24:00Z').getTime()` returns `86400000` in JS (ISO next-day midnight), and `typedText` wraps `86400000` to `"00:00"`, whereas Android (`setLenient(false)`), iOS (`isLenient = false`), and `docs/language.md:323` reject `"24:00"` as `null`.
   - `type === 'datetime'` with `"2026-12-01 14:30:59"` or `"2026-12-01 14:30+05:00"`: passes `full.slice(0, 16)` on Web while Android (`at.getIndex() != v.length()`) and iOS return `null`.
   - Fix both by checking exact length (`10` for `date`, `5` for `time` with `ms >= 0 && ms < 86400000`, `16` for `datetime`) and verifying round-trip equality against `typedText`.

- **Case for / against:** ~12 lines in `tinyjump.js`; makes all three hosts agree on every edge case and keeps 2019 browser support intact.

---

### P1-3 (Medium). `forAgent` is bypassed when `act` targets an `<Input>` by `label` or `placeholder` (`target.text`)

Answering your check (`builder-019:43`): in `crates/tj-cli/src/mcp.rs:61-68`, any `act` target without `#` or a node number becomes `{ "text": "..." }` (for example `act("When", "onChange", "2026-12-01 14:30")` against `<Input id="when" label="When" type="datetime" />` in `examples/agenda/app.tj:42`).

- `tinyjump.js:515-522`, `Renderer.java:580-589`, and `Renderer.swift:410-417` only look up `target.node` and `target.id`. When `target.text` is used, `forAgent` leaves `input.value` as the raw string `"2026-12-01 14:30"`, bypassing conversion and storing a `string` into `agenda.at: number | null`.
- **Case for / against:** Match `target.text` against `label` and `placeholder` in `forAgent` on all three hosts (~3 lines per host).

---

### P1-4 (Low). iOS `UIDatePicker` keeps stray seconds when `value` starts `null`, and `Time.from` allows missing `year`/`month`/`day`

1. **`hosts/ios/Sources/TinyJump/Times.swift:65-80`:** When `<Input type="time" | "datetime">` starts with `value = null`, `UIDatePicker` defaults `p.date` to `Date()` (carrying current seconds/ms) and preserves those seconds when the wheel moves, whereas Web and Android (`Times.java:112-113`) zero seconds and milliseconds. Floor to whole minutes (`floor(p.date.timeIntervalSince1970 / 60) * 60_000`) in `Times.picked`.
2. **`crates/tj-core/src/typeck.rs:134` and `crates/tj-core/src/vm/time.rs:106-116`:** `Time.from` marks `year`, `month`, and `day` optional in `typeck.rs`, and `time.rs:108` defaults a missing `year` to `0` (`Time.from({ month: 1, day: 1 })` silently returns year 0 AD instead of `null` or a type error). Make `year`, `month`, and `day` required in `typeck.rs` and return `Value::Null` in `time.rs` when any of the three is missing or `null`.

- **Case for / against:** 5 lines total across `Times.swift`, `typeck.rs`, and `vm/time.rs`.
