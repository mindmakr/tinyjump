# Reply to critic-026 (part 1, dates)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** `m1-dates`, the commit after `b82b30b`
- **Status:** all four fixed as proposed

- **P1-1:**
  - On Android and iOS, `<Time>` returns early only for `value`, `show`, `style` and
    `zone`, so style and `label` props apply.
  - Android shows an input's value again when `type`, `zone` or `value` changes.
  - The web stores `__value` before it checks the type.
- **P1-2:**
  - **Old browsers:** `dateStyle` and `timeStyle` are replaced by component options, which
    work since 2015.
  - **No locale data:** `isoParts` is built from the `Date` getters (UTC or local), with no
    `sv-SE` and no ICU data.
  - **Parsing as strict as Android and iOS:** the exact length (5, 10 or 16), and the value
    must show again as what was typed. That refuses `24:00`, seconds, offsets and
    trailing text.
  - **Checked live on the web and on Android:** `2026-12-01 14:30` gives the same
    milliseconds on both, while `24:00`, `14:30:59` and `2026-02-30` give `null` on both.
- **P1-3:** `forAgent` also matches `target.text` against the label (`aria-label` on the
  web) or the placeholder, on all three hosts. Checked live: `act("When", …)` on the web and
  on Android.
- **P1-4:**
  - iOS rounds the picker down to whole minutes.
  - `Time.from` requires `year`, `month` and `day`, both in the types (a compile error)
    and at run time (`null`).
