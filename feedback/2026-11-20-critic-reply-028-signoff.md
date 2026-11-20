# Critic Reply 028: Sign-off on Part 3 (`m1-queries`, `eb029ff`)

**Verdict:** Sign-off on Part 3 (`4e3e523`..`eb029ff` on `m1-queries`). Ready to merge.

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-reply-028.md` (`4e3e523`, `eb029ff`)
- **Status:** closed / signed off

Verified all four fixes:
- **Q1 (`crates/tj-core/src/flow.rs:982-996`, `tests/runtime.rs:1053-1057`):** `reduce` binds `(acc, elem)` in the right order with `acc` joined with `whole`, and `concat` checks its argument.
- **Q2 (`hosts/web/tinyjump.js:234-237, 271-276`, `Sync.java:62-68, 173-179`, `Sync.swift:38-43, 98-104`, `objects.rs:547-548`):** Single `Class/*` watermark per class stream, API-21-safe `Iterator` removal on Android, and deduplicated `keys` in `changed_since`.
- **Q3 (`objects.rs:247, 584-595`, `tests/runtime.rs:2396-2409`):** `i.seq > 0` membership filter keeps live handles safe during renders while excluding blanked replicas after user switch, `class_streams` resets on `clear_replicas`, and `class_ids` loads offline-kept replicas on first `Class.all()`.
- **Q4 (`objects.rs:556-565`, `access.rs:70`, `tests/runtime.rs:2380-2395, 2410`):** `CLASS_STREAM_CAP` trims oldest class-stream changes only after `seq > since` while preserving explicitly listed objects, and `owns` rejects `"*"`.
