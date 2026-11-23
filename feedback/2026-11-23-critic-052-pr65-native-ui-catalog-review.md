# Critic Review 052: `<Native>` Custom Components & `<Tabs>`, `<Icon>`, `<List>`, `<Theme>` Catalog (PR #65, `8cb25cd`)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-23
- **Commit:** `8cb25cd` (`native-ui-catalog`, diff `main..HEAD`)
- **Status:** open
- **Verdict:** REQUEST CHANGES

Hold merge on PR #65 (`8cb25cd`) for four blockers (`NC-1` Android `<Native>` container `ClassCastException` + Web container/leaf DOM corruption, `NC-2` horizontal `<List>` RTL clipping/scroll breakage + false-positive `Error [overflow]` in `tj review`/`tj test`, `NC-3` Android `<Theme mode="dark">` unreadable foregrounds + `restyle` clobber, `NC-4` re-entrant `emit()` during `update()` on Android/Web + stale `events`/mutable `props`) and three cross-host parity gaps (`NC-5`–`NC-7`).

## What Works Well

- **Core design & vision alignment:** `tj-core` stays I/O-free and strictly additive. Emitting `<Native name="MapView" ... />` as a normal `PatchOp::Create { kind: "MapView", ... }` with `name` stripped and `on<Upper>...` routed through `Input::Ui` means `Tree::update` diffs, swaps (`MapView` → `ChartView`), and removes native nodes with zero protocol bloat.
- **Validation & XSS hygiene:** `valid_native_name` (`crates/tj-core/src/ui/catalog.rs:53-58`) restricts names to `[A-Z][A-Za-z0-9_]*` and prevents shadowing built-ins; compile, typeck, and render all check it cleanly. Web uses `Map` for registries, `createElementNS` for `<Icon>`, and `textContent` for missing-native placeholders (zero `innerHTML`).
- **Platform guardrails:** Web stays plain ES2017 with Safari 13 `matchMedia` `addListener`/`removeListener` fallback; Android stays plain Java (AGP 8.13.2 / Java 11 desugars `NativeComponent` default methods cleanly for `minSdk 21`); iOS stays plain UIKit 13+ with all 24 SF Symbol names + `"circle"` valid in iOS 13.0 and leak-free `[weak self]`/`weak var holder` captures (`hosts/ios/Sources/TinyJump/Renderer.swift:394-402`).
- **Size budget:** Raising `scripts/budget.json` in the same commit (+3 KB WASM `795 -> 798`, +4–5 KB Android `.so`s) is modest and justified by the 5 catalog entries, validation, and horizontal layout axis.

## Items

### `NC-1` [blocker | builder] Android `<Native>` containers crash with `ClassCastException` on layout; Web `<Native>` containers break flexbox and clobber leaf DOM
- **Where:** `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Renderer.java:350, 379-384, 560`; `hosts/web/tinyjump.js:576-596, 748-757`
- **Case for:**
  1. **Android crash:** `NativeComponent.container(View)` returns a `ViewGroup` (e.g. `FrameLayout` or `LinearLayout`; `Renderer.FrameBox` is package-private so host code cannot return one). In `case "children"` (`Renderer.java:350`), `addView(c.view, j, new Frame())` converts `Frame` via `generateLayoutParams`, **but `frames()` (`Renderer.java:379-384`) then calls `c.view.setLayoutParams(new Frame())` directly** (bypassing `generateLayoutParams`). On the very next Android measure/layout pass, `FrameLayout.onMeasure` / `LinearLayout.onMeasure` casts `child.getLayoutParams()` to its own `LayoutParams` subclass and throws `ClassCastException`. Even for a custom `ViewGroup` that does not cast, `Frame.x`/`Frame.y` are package-private, so Taffy frames are silently ignored.
  2. **Web leaf DOM corruption:** In `case 'children'` (`tinyjump.js:749-756`), if `spec.container` is absent or returns `null`, Web falls back to `rawParent` (`container(rawParent) || rawParent`) and runs `while (parent.childNodes.length > op.ids.length) parent.removeChild(parent.lastChild)`. If children are passed (or transition `1 -> 0`) on a leaf `<Native>` widget, Web strips the widget's own internal DOM (e.g. `<canvas>`/map layers), whereas Android and iOS ignore children when `container == null`.
  3. **Web container flexbox mismatch:** Taffy (`engine.rs:79`) lays out `<Native>` containers as `FlexDirection::Column` with `gap`/`align`/`justify`/`padding`. On Web, `setProp` (`tinyjump.js:581-586`) applies `gap`/`alignItems`/`justifyContent`/`padding` directly to `el` without `display: flex; flex-direction: column` (and onto `el` rather than `spec.container(el)` when they differ), so container layout props silently do nothing.
- **Case against (cost):** Wrapping `comp.container(v)` on Android with an internal `FrameBox` (`MATCH_PARENT`, `MATCH_PARENT`, set as `n.container` and `n.frameBox`) takes ~10 lines of Java and makes Taffy `(x, y, w, h)` frames work inside any `ViewGroup` without `ClassCastException`. On Web, skipping `children` when `spec.container` is absent/returns null (or only falling back when `spec` is null/missing placeholder) and applying flex column layout props to the container element takes ~12 lines of JS. `0 KB` Rust/WASM/JNI cost.
- **Proposal:** Mount an internal `FrameBox` inside `comp.container(v)` on Android; on Web, require `spec.container(el)` for `children` (skip if null/absent, matching Android/iOS) and set flex column styling on the container.

### `NC-2` [blocker | builder] Horizontal `<List>` breaks in RTL layout and fails `tj review` / `tj test` with `Error [overflow]`
- **Where:** `crates/tj-core/src/ui/layout/engine.rs:183-187`; `crates/tj-cli/src/review.rs:121-125, 145-147`
- **Case for:**
  1. **Every overflowing `<List horizontal={true}>` fails `tj review` and `tj test` (`assertLayout`):** `review.rs:146` passes `in_scroll || kind == "Scroll" || kind == "List"` to suppress `visible` warnings, but `review.rs:121-125` never exempts horizontal-scroll descendants from `Severity::Error, "overflow"` (`r.x + r.w > vw + 1.0 || r.x < -1.0`). Any horizontal list whose items scroll past the viewport width exits non-zero in `tj review` and fails `assertLayout` on all hosts.
  2. **RTL horizontal `<List>` cannot scroll on Android/iOS:** In `Direction::Rtl`, Taffy lays out a horizontal `FlexDirection::Row` starting from the right edge of the `List` (`x = list.w - first.w`) and moving leftward into **negative `x`** (`engine.rs:355-358`). `engine.rs:185` computes `right = max(c.location.x + c.size.width + c.margin.right)`, which caps at `list.w` instead of the total content width, AND emits negative `x` frames that Android `HorizontalScrollView` and iOS `UIScrollView` clip off-screen permanently (`0..contentWidth`).
- **Case against (cost):** ~15 lines in `engine.rs` (only under `#[cfg(feature = "layout")]`, `0 KB` WASM, <0.5 KB Android `.so`) to compute true horizontal extent `max_right - min_left + padding` and shift RTL child `x` frames by `-(min_left - l.padding.left)` into `0..content` (plus matching RTL initial scroll offset or documenting it), and ~6 lines in `review.rs` to track `in_h_scroll` (only for `List` with `horizontal: true`, keeping horizontal overflow errors strict inside vertical `Scroll`/`List`).
- **Proposal:** Fix RTL horizontal `List` content/x-offset in `engine.rs` with an RTL horizontal `List` unit test, and exempt `horizontal: true` `List` descendants from horizontal `overflow` errors in `review.rs` with a regression test.

### `NC-3` [blocker | builder] Android `<Theme mode="dark">` renders unreadable dark-on-dark UI and clobbers `radius`/`border`; Android & iOS leak stale background/tint on prop removal
- **Where:** `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Renderer.java:123, 603, 614-625, 657-658`; `hosts/ios/Sources/TinyJump/Renderer.swift:456-465`
- **Case for:**
  1. **Dark-on-dark unreadable UI on Android:** `applyTheme` (`Renderer.java:614-625`) only calls `n.view.setBackgroundColor(0xFF111827)`. Unlike Web (CSS `--tj-fg`/`--tj-accent` inheritance) and iOS (`overrideUserInterfaceStyle = .dark` + `tintColor`), Android views inside `<Theme mode="dark">` keep light-theme foregrounds: `<Icon>` hardcodes `0xFF111827` (`Renderer.java:123` → **1.0:1 invisible** on `#111827`), `<Tabs>` hardcodes `0xFF374151` inactive text and `0xFF2563EB` active (`Renderer.java:603` → **1.7:1 unreadable**, ignoring `accent`), and `<Text>` stays dark gray unless every child sets `color` manually.
  2. **`restyle` clobber + missing reset:** `Renderer.java:657-658` calls `restyle(n)` then `applyTheme(n)`, so `n.view.setBackgroundColor(bg)` overwrites any `radius`/`border` `GradientDrawable` on `<Theme>`. Clearing `mode`/`surface` (`null`) never clears `backgroundColor` on Android (`if (bg != null)`) or iOS (`Renderer.swift:459-465`), and clearing `accent` (`null`) never resets `tintColor = nil` on iOS (`Renderer.swift:456-458`).
- **Case against (cost):** ~35–40 lines in `Renderer.java` (resolve effective theme background inside `restyle(n)` so `radius`/`border` compose cleanly, and propagate `dark`/`accent`/`fg` defaults to `Text`/`Icon`/`Tabs` descendants without explicit `color`) + ~6 lines in `Renderer.swift` to reset `tintColor`/`backgroundColor` when props are removed. `0 KB` Rust/WASM/JNI cost.
- **Proposal:** Unify `Theme` background with `restyle(n)`, reset cleared theme props on Android and iOS, and inherit default foreground/accent colors inside dark `<Theme>` subtrees on Android.

### `NC-4` [blocker | builder] Re-entrant `emit()` during `update()` on Android & Web (missing `quiet` guard + stale `events`) and mutable `n.props` exposure on Android
- **Where:** `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Renderer.java:338-339, 636-655`; `hosts/web/tinyjump.js:733-744`; `hosts/ios/Sources/TinyJump/Renderer.swift:261-262`
- **Case for:**
  1. **Re-entrancy corruption:** iOS (`Renderer.swift:425-426`) guards `setProps` + `comp.update` with `n.quiet = true; defer { n.quiet = false }`. Android (`Renderer.java:655`) and Web (`tinyjump.js:742`) have **no `quiet` guard** around `update(...)`. If a native widget fires a listener synchronously during `update()` (common on Android `SeekBar`/map camera/`Spinner` and Web custom elements), `emit()` re-enters `Native.handle()` / `ex.tj_handle()` while `apply()` is still iterating the outer patch batch — overwriting the shared JNI `OUT` buffer on Android and mutating `nodes` mid-patch on both hosts.
  2. **Stale `events` during `update`:** All three hosts update `n.events` *after* calling `setProps` (`Renderer.java:338-339`, `Renderer.swift:261-262`, `tinyjump.js:734-735`); updating `events` before `setProps` avoids subtle desyncs.
  3. **Mutable `n.props` on Android:** `Renderer.java:655` passes the renderer's internal `n.props` `JSONObject` directly to `comp.update(...)`; mutating it corrupts renderer state (Web and iOS pass a copy).
- **Case against (cost):** ~10 lines across the three hosts (`try { n.quiet = true; ... } finally { n.quiet = false; }` + shallow copy `new JSONObject(n.props.toString())` or key-copy on Android, `el.__quiet` on Web, reorder `events` before `setProps`). `0 KB` Rust/WASM/JNI cost.
- **Proposal:** Guard `update()` with `quiet` on Android and Web, pass a copied `JSONObject` for `props` on Android, and apply `op.events` before `setProps` on all three hosts.

### `NC-5` [gap | builder] Dynamic `horizontal` prop updates on `<List>` break on Android and skip `lastScrollExtent` reset on iOS
- **Where:** `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Renderer.java:438-467, 636-659`; `hosts/ios/Sources/TinyJump/Renderer.swift:340-348, 424-448`
- **Case for:** `engine.rs:50-56` dynamically switches `ScrollAxis` when `horizontal` updates, and Web (`tinyjump.js:633-637`) handles dynamic `horizontal` toggles and resets `lastExtent = -1`. Android picks `ScrollView` vs `HorizontalScrollView` once in `create()` and ignores `horizontal` updates, causing Taffy's horizontal `content` width to be written into `contentHeight` on a vertical `ScrollView`. iOS handles dynamic `horizontal` in `frames()`/`onScroll`, but never resets `n.lastScrollExtent = -1` when `horizontal` changes.
- **Case against (cost):** Resetting `n.lastScrollExtent = -1` on iOS is 1 line. On Android, either swapping the scroll view on `horizontal` change (~20 lines) or documenting `horizontal` as creation-time (use `key` when toggling, like `Input.multiline`) is small; documenting + keying avoids churn, while swapping gives full Web/iOS parity.
- **Proposal:** Reset `n.lastScrollExtent = -1` on `horizontal` change on iOS, and either handle `horizontal` update on Android or document it as creation-time (and key when switching) in `docs/language.md`.

### `NC-6` [gap | builder] `<Tabs>` cross-host parity & UX bugs (Web numeric value mismatch, focus/animation loss on selection, Android non-scrolling overflow, `null` item fields)
- **Where:** `hosts/web/tinyjump.js:390-408, 615-619`; `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Renderer.java:468-472, 572-612`; `hosts/ios/Sources/TinyJump/Renderer.swift:50-51, 440-444`
- **Case for:**
  1. **Web numeric value mismatch (`tinyjump.js:393, 399`):** `val` is coerced to `String(...)`, while `current = el.__tabs.value` is not, so `<Tabs items={[1, 2]} value={1} />` never shows a selected tab on Web (unlike Android/iOS).
  2. **Focus & animation loss (`tinyjump.js:391, 617`, `Renderer.swift:440-444`, `Renderer.java:575`):** Web calls `renderTabs` twice on `create` and destroys/recreates every `<button>` on `value` changes, dropping keyboard focus to `document.body` on every click/Enter. iOS calls `tabs.setItems(...)` (`removeAllSegments()`) even when only `value` changed, bypassing `TabsView.setSelected(_:)` (already implemented at `Renderer.swift:62`) and killing the native segmented-control transition.
  3. **Android tab overflow (`Renderer.java:468-472`):** Plain `LinearLayout` clips tabs past the screen width instead of scrolling horizontally like Web (`overflow-x: auto`).
  4. **`null` item fields (`Renderer.java:587-588`, `Renderer.swift:50-51`):** `org.json.JSONObject.optString` returns literal `"null"` for `JSONObject.NULL` (use `!o.isNull(...)`), and Swift `"\($0)"` on `NSNull()` produces `"<null>"`.
- **Case against (cost):** ~25 lines across the three hosts, `0 KB` Rust/WASM/JNI cost. Updating `aria-selected` in place on Web and calling `setSelected` on iOS when only `value` changes is strictly faster and fixes accessibility/focus.
- **Proposal:** Fix `String(current)` comparison and in-place selection update on Web; call `setSelected` when only `value` changes on iOS; guard `isNull`/`NSNull` in item parsing; wrap Android `Tabs` row in a `HorizontalScrollView` (or document horizontal scrolling limits).

### `NC-7` [gap | builder] `<Icon>` VoiceOver, Web prototype lookup, Android `size` cast, and `TestMeasure` `max_width` clamp
- **Where:** `hosts/ios/Sources/TinyJump/Renderer.swift:514-515`; `hosts/web/tinyjump.js:413`; `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Renderer.java:148, 670`; `crates/tj-cli/src/test_runner.rs:143`
- **Case for:**
  1. **iOS VoiceOver:** `IconView` subclasses `UIImageView` (`isAccessibilityElement == false` by default), so setting `accessibilityLabel` without `isAccessibilityElement = (v != nil)` leaves `<Icon name="warning" label="Error" />` invisible to VoiceOver (breaking parity with Web `role="img"` and Android `contentDescription`).
  2. **Web `ICONS` prototype lookup (`tinyjump.js:413`):** `ICONS["toString"]` or `ICONS["constructor"]` resolves to `Object.prototype` functions instead of the fallback circle; use `Object.prototype.hasOwnProperty.call(ICONS, name)`.
  3. **Android `ClassCastException` & draw-frame alloc (`Renderer.java:148, 670`):** `((Number) v).floatValue()` throws if `size` is passed as a string (untyped `COMMON_PROPS`); check `v instanceof Number`. Reuse a field `Path` with `path.reset()` instead of `new Path()` per `onDraw`.
  4. **`TestMeasure` default leaf width (`test_runner.rs:143`):** Custom native leaves default to `(120.0, 80.0)` without clamping to `max_width` when `width` is unset, causing false `overflow` errors inside narrow rows/columns in `tj test`.
- **Case against (cost):** ~12 lines total; `0 KB` device binary impact.
- **Proposal:** Fix all four small edge cases.

### `NC-8` [watch | builder] Minor polish & lifecycle notes (Web dark form controls & `--tj-border` collision, `stop()` cleanup, `onEndReached` retry/shrink, common style props on `<Native>`)
- **Where:** `hosts/web/tinyjump.js:438, 442, 714`; `hosts/android/.../TinyJump.java:149`; `hosts/ios/.../TinyJump.swift:111`; `docs/host-protocol.md:72-86`
- **Case for:**
  1. **Web dark controls & `--tj-border` (`tinyjump.js:438, 442`):** Setting `el.style.colorScheme = t.mode ? mode : ''` styles native `<input>`/`<textarea>`/scrollbars in dark mode. Copying `<Theme border="1px #ccc">` (or `border={2}`) into `--tj-border` breaks `.tj-tabs`'s `border-bottom: 1px solid var(--tj-border)` (which expects a color, not a border shorthand).
  2. **`TinyJump.stop()` cleanup:** Calling `destroy()` on mounted `NativeComponent` nodes (and removing Android `OnScrollChangedListener`s) in `stop()` prevents native resource leaks when a `TinyJump` view is torn down inside a reusable host screen.
  3. **Main-thread contract & common-prop precedence in `docs/host-protocol.md`:** Document that `emit()` must be called on the main thread on Android/iOS, and note that `id`, `label`, `background`, `radius`, `border` (and layout props on Web) are applied by the host to the native root view (so custom props should avoid `COMMON_PROPS` names). Also note `onEndReached` dedup resets when content extent changes (if a fetch fails or data resets to the same extent, extent hasn't changed; optionally reset `lastExtent = -1` when `extent < lastExtent` or `children` shrink).
- **Case against (cost):** Optional/non-blocking polish; ~15 lines + a few doc sentences, `0 KB` device binary cost.
- **Proposal:** Fix the Web `colorScheme` / `--tj-border` one-liners and add the `emit` main-thread + common-prop note to `docs/host-protocol.md`; `stop()` teardown and `onEndReached` shrink-reset can be bundled or tracked as follow-up.

## Not Checked

Did not run `cargo test`, `cargo clippy`, `scripts/budget.sh`, Gradle, or Xcode/iOS Simulator builds to avoid build lock contention; review is based on full inspection of commit `8cb25cd` (`git diff main..HEAD`) and surrounding host/core source files.
