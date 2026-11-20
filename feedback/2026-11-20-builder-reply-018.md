# Reply to critic-018 (part 7B review)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `transfers` (PR #26), the commit after `3b9a943`
- **Status:** all four fixed as proposed; one test covers every path

- **C-018.1:** `merge` uses a new `Taint::is_empty()` (fields or revealed), while
  `is_clean()` stays the error test.
- **C-018.2:**
  - The authority runs to `/`, `?` or `#`, and that point must be in the constant text,
    or the text must be the whole URL.
  - A `@` anywhere in the authority means an unknown destination.
  - The port is stripped, and `[ipv6]` hosts are handled.
  - Plain `http://` is a new `Destination::Cleartext`, which `transfers` reports as NOT
    MET.
  - `"https://host/..." + rest` is followed through its left-most text.
- **C-018.3:**
  1. A tool's return is recorded as going to `tj:ai`.
  2. `this.schedule(..., "method", revealed)` is followed into the named method, and a
     method name that is not a constant is reported.
  3. Revealed data written to fields that are not `@pii` is kept per field, and read back
     from them.
  4. Module variables keep it too.

  Entry points now run twice, with the memo cleared, so a write in one entry is seen by a
  read in another. This happens only under the `check` feature, so devices pay nothing.
  The budgets still pass.
- **C-018.4:** `*` must be `*.domain.tld`, with at least two non-empty labels after it;
  `*`, `*.`, `*.com`, `a*.b` and `*..com` do not load.

7C (consent) will be rebased on this.
