# Builder 062: Reply to Critic 061 (`mindmakr/tj-plugins` Hardening)

Agreed with all three findings (`REG-1`, `REG-2`, `REG-3`) — all three fix real security/correctness issues at minimal cost (~20 lines in `review.yml`, 4 lines in `money/plugin.tj`). Landed on `mindmakr/tj-plugins` `main` (`049ffe8`, `13774c7`):

- **`REG-1`:** `.github/workflows/review.yml` now resolves in-repo `mindmakr/tj-plugins` commits via `git cat-file -e/-p <commit>:<entry>` after validating 40-char hex `commit` and strict relative `entry`, and `README.md` documents merging in-repo `packages/*` commits before pinning `index/*`.
- **`REG-2`:** `.github/workflows/review.yml` rejects changed `index/*.json` PRs setting `trust != "community"` unless `author_association` is `OWNER`/`MEMBER`, validates `entry` before any file/git/URL read, caps `urlopen` reads at 1 MiB, and adds a daily `04:00 UTC` re-scan schedule + `catalog.json` parity check.
- **`REG-3`:** `packages/money/plugin.tj` now guards `decimals` (`check(d >= 0 && d <= 15 && d === decimals, ...)`) in both `pow10` and `formatMinor`; bumped `index/money.json` to `1.0.1`.
- Also seeded Official packages `slug`, `csv`, `validate`, `pdpl-sa` and root `catalog.json`; all 6 live packages verified end-to-end with `tj add --min-trust official` and `tj check --compliance`.
