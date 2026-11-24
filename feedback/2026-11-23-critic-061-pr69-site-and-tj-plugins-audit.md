# Critic 061: Post-Merge Audit of PR #69 (`9c6bea5`) and Live `mindmakr/tj-plugins` Registry

Sign off on PR #69 (`9c6bea5` on `main` and `4c6b806` on `gh-pages`), with three hardening items for `mindmakr/tj-plugins` (`review.yml` and `packages/money/plugin.tj`).

## Verified End-to-End

- **Live `mindmakr/tj-plugins` install & check:** Ran `tj add money`, `tj add gdpr-eu`, `tj check app.tj --min-trust official --compliance .tj-packages/gdpr-eu/pack.json`, and `tj review app.tj` (`en` + `pseudo`) against `https://raw.githubusercontent.com/mindmakr/tj-plugins/main`; all passed with SHA-256 and commit verification.
- **Site & docs accuracy (`site/index.html`, `README.md`, `docs/vision.md`, `docs/registry.md`):** Binary sizes (`842 KB` / `540 KB` WASM, `1098 KB` / `845 KB` Android `arm64`, `782 KB` / `606 KB` `armv7`), shipped changelog bullets, and post-v1 items match `scripts/budget.json` and `gh-pages` (`4c6b806`).

---

## Hardening Items for `mindmakr/tj-plugins`

### 1. `REG-1` (`mindmakr/tj-plugins:.github/workflows/review.yml:51-68`): `git rev-parse HEAD` on `pull_request` never matches `data["commit"]`, and squash merges invalidate in-repo commit pins

On GitHub Actions `pull_request` runs, `HEAD` is a synthetic merge commit (`refs/pull/<N>/merge`), so `data.get("commit") == head_sha` (`line 55`) is always false, and `raw.githubusercontent.com/mindmakr/tj-plugins/<fork-commit>/...` returns 404 for fork PRs before merge. In addition, squash-merging an in-repo `packages/<name>/plugin.tj` PR rewrites its commit SHA on `main`, invalidating `data["commit"]`.

- **Case for fixing:** Resolve in-repo commits via `git cat-file -e <commit>:<entry>` (`fetch-depth: 0` is already set) instead of `head_sha`, and document/configure `mindmakr/tj-plugins` so in-repo `packages/*` PRs either pin an already-merged commit or disable squash-merging when both `packages/*` and `index/*` change together.
- **Case against / cost:** ~10 lines in `review.yml`.

### 2. `REG-2` (`mindmakr/tj-plugins:.github/workflows/review.yml:36-74`): Gate `"trust": "official" | "verified"` on community PRs and bound `urlopen` / `local_entry`

`tj registry check` validates manifest structure without knowing who opened the GitHub PR. An external PR setting `"trust": "official"` or `"trust": "verified"` in a new or modified `index/*.json` passes `review.yml` green unless a maintainer notices manually. `review.yml` also reads `local_entry` (`line 56`) before `tj registry check` rejects `..` traversal, and calls `urlopen(url)` (`line 64`) without a byte cap.

- **Case for fixing:** In `review.yml`, fail changed `index/*.json` manifests with `trust != "community"` unless `github.event.pull_request.author_association` is `OWNER` / `MEMBER` (or a maintainer label is present), resolve `local_entry` strictly inside repo root, and bound `resp.read(1_048_577)` to 1 MiB.
- **Case against / cost:** ~16 lines in `review.yml`.

### 3. `REG-3` (`mindmakr/tj-plugins:packages/money/plugin.tj:4-18, 53-68`): Guard non-integer or `> 15` `decimals` in `pow10` and `formatMinor`

`formatMinor(1250, "$", 2.9)` uses fractional `decimals` in `while (frac.length < decimals)`, producing `"$12.050"` instead of `"$12.50"`, and `decimals > 15` exceeds `f64` exact-integer precision (`2^53 - 1`), reintroducing the floating-point drift the module prevents.

- **Case for fixing:** Normalize `const d = Math.floor(decimals)` and `check(d >= 0 && d <= 15, "decimals", "decimals must be an integer between 0 and 15")` inside `pow10` / `formatMinor`.
- **Case against / cost:** 4 lines in `packages/money/plugin.tj` (plus updating `index/money.json` commit/sha256).

## Not Checked

Did not open live `https://mindmakr.github.io/tinyjump/demo/` in a browser; verified `gh-pages` commit `4c6b806` and ran `tj add` / `tj check` / `tj review` directly against the live GitHub registry.
