# Builder Reply 049: Fixes for `IK-8` and `IK-9` (`idempotency-key`, PR #62)

Both findings on `e55956b` are real bugs — fixed immediately with **0 KB** added to device builds:

- **`IK-8` (Fixed):**
  - `crates/tj-core/src/runtime/mod.rs`: include **both** `ctx.object` (entry object) and `active_object` (innermost frame `this`) in `parts`, preventing cross-entry-object collisions when `Cart/c1` and `Cart/c2` reuse the same `Idempotency-Key` and invoke a shared `Item/sku-1` or `PaymentGateway/main` method.
  - `crates/tj-core/src/runtime/mod.rs` & `crates/tj-core/src/vm/interp.rs`: store `running_this: Option<Rc<ObjRef>>` (`fr.this.clone()`) instead of allocating `o.key()` on every `NativeFn` call.
  - `crates/tj-core/tests/runtime.rs`: replay `Cart/c1` (asserting equality) and also call `Cart/c2` with the same `call_id` (asserting distinct keys on `Item/sku-1` and `Item/sku-2`).
- **`IK-9` (Fixed):**
  - `crates/tj-core/src/runtime/objects.rs`: capture `(prev_saved, prev_seq)` alongside `saved_before`; on rollback restore `inst.seq = prev_seq` and re-insert `prev_obj` into `self.alarm_index` (updating `index_changed`) whenever the reverted instance still has pending alarms.
  - `crates/tj-core/tests/runtime.rs`: assert `mrt.changed_since(&["BoxObj/a".into()], 0).is_empty()` after multi-object flush failure.
