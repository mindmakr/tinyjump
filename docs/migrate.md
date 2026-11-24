# Porting TypeScript & React Apps (`tj migrate`)

`tj migrate` translates TypeScript, JavaScript, and React components (`.ts`, `.tsx`, `.js`, `.jsx`) into `.tj` modules with clear fix hints for unsupported idioms, and `tj migrate compare` verifies UI and behaviour parity against traces, snapshots, live URLs, and PNG screenshots.

Everything lives in `crates/tj-cli` (**0 KB** added to `tj-core` or device builds).

## Converting files or directories (`tj migrate`)

```sh
# Migrate one React component
tj migrate src/Counter.tsx --out Counter.tj --report migration.md

# Migrate an entire directory into ./my-app-tj
tj migrate ./my-app --out ./my-app-tj --report ./my-app-tj/MIGRATION.md --json

# Fail CI if any manual conversion hints or compile errors remain
tj migrate ./my-app --out ./my-app-tj --strict
```

### Flags

| Flag | Description |
|---|---|
| `--out <path>` | Output `.tj` file (for single file) or directory (default: `<dir>-tj` or `<file>.tj`); refuses to overwrite/nest inside input |
| `--report <file.md>` | Write a per-file Markdown migration report with line numbers, severities (`manual`, `warn`, `info`), and fix hints |
| `--strict` | Exit non-zero if any `manual` diagnostics or `.tj` compile errors remain |
| `--json` | Emit structured JSON diagnostics, line counts, and compile status |

### Directory safety & filtering

- Walks up to `200` source files (`<= 2 MiB` each), stripping optional UTF-8 BOMs cleanly.
- Skips `node_modules/`, `.git/`, `dist/`, `build/`, `.next/`, `.turbo/`, `coverage/`, `.tj-data/`, `*.d.ts`, `*.test.*`, and `*.spec.*`.
- Rejects symlinks pointing outside the input root.

### What translates automatically

- **React & CSS imports:** Removes `import React ... from "react"` and stylesheet imports (`*.css`/`*.scss`); normalizes relative module imports by dropping `.ts`/`.tsx`/`.js`/`.jsx` extensions.
- **Entry & exports:** Maps `export default function` to `function App` (when entry or named `App`) or `export function ...`.
- **TS/JS dialect:** Maps `undefined` -> `null`, `==`/`!=` -> `===`/`!==`, `var` -> `let`, strips `: JSX.Element`, `: React.FC`, and React synthetic event parameter types (`React.ChangeEvent<...>`, etc.).
- **Local state (`useState`):** Lifts `[x, setX] = useState(...)` inside PascalCase components into an `object <Component>State` declaration with `const state = <Component>State.get("main")`, rewrites `setX(c => c + 1)` updaters to `state.setX(state.x + 1)`, and rewrites free state reads (`state.x`) while preserving JSX text interpolations and prop names.
- **JSX primitives & styling:**
  - `<div>`/`<section>`/`<main>`/`<form>`/`<ul>`/`<li>` -> `<Column>` (or `<Row>` when `flex-row`/`row`/`flexDirection: "row"` is detected).
  - `<h1>`..`<h6>`/`<p>`/`<span>`/`<label>`/`<strong>` -> `<Text>` (with heading `size`/`weight`).
  - `<button>` -> `<Button>` (`onClick` -> `onPress`).
  - `<input>` -> `<Input>` (`type="password"` -> `secure`, `textarea` -> `multiline`, `type="number|date|time|datetime-local"` preserved with numeric handler hint).
  - `<input type="checkbox">` -> `<Toggle>` (`checked` -> `value`, `.target.checked` unwrapped).
  - `<img>` -> `<Image>` (`alt` -> `label`), `aria-label` -> `label`.
  - Basic Tailwind utility classes (`p-2`/`p-4`/`p-6`, `gap-2`/`gap-4`, `rounded*`, `font-bold`, `flex-row`) and numeric/color inline `style={{ ... }}` properties map directly to TinyJump layout/style props.

### Patterns flagged with `[manual]` fix hints

Comments and string literals are masked so occurrences inside prose/comments do not trigger false positives:
- React lifecycle/advanced hooks: `useEffect` (with tailored `@every(...)`, `this.schedule(...)`, or `@server async` hints), `useReducer`, `useRef`, `useContext`, `useMemo`, `useCallback`.
- Browser APIs: `localStorage`, `sessionStorage`, `window.*`, `document.*`, `<a>` links.
- Unsupported syntax/builtins: `class`, `switch`, `enum`, `try ... finally`, `new Date/Map/Set`, external npm package imports, default/alias relative imports, and export lists.

---

## Verifying parity (`tj migrate compare`)

After porting (or fixing `[manual]` items), run `tj migrate compare` to verify layout, interaction traces, snapshot inventories/API calls, live HTTP text, and golden PNG screenshots against the original app:

```sh
tj migrate compare my-app-tj/App.tj \
  --trace tests/smoke.trace.jsonl \
  --old-snapshot old-snap.json \
  --old-url http://127.0.0.1:3000 \
  --old-screenshot old.png \
  --new-screenshot new.png \
  --threshold 1.0 \
  --diff diff.png \
  --json
```

### Options

| Flag | Default | Description |
|---|---|---|
| `--trace <file.trace.jsonl>` | none | Deterministic trace(s) to execute against `<migrated.tj>` (repeatable) |
| `--old-snapshot <file.json>` | none | Compare rendered UI `texts`, `buttons`, `inputs`, and optional `api` method calls (`object`, `method`, `args`, `expect`/`expectContains`/`expectError`) |
| `--old-url <http://...>` | none | Fetch HTTP HTML/JSON from a running legacy app and compare visible text parity (private/link-local non-loopback hosts blocked unless `TJ_ALLOW_PRIVATE_FETCH=1`; redirects disabled; `2 MiB` cap) |
| `--old-screenshot <a.png> --new-screenshot <b.png>` | none | Pixel-by-pixel RGBA PNG diff via `golden::compare` |
| `--threshold <pct>` | `0.1` | Allowed changed-pixel percentage (`0..=100`) |
| `--diff <out.png>` | none | Optional PNG visualizing pixel differences |
| `--width <n>` / `--height <n>` | `390` / `844` | Viewport used for boot/inspect and `tj review` layout error check |
| `--json` | `false` | Emit structured parity report |
