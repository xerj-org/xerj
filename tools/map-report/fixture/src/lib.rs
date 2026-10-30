mod store;

/// Start the parcel ledger with an explicitly bounded capacity.
pub fn startup(capacity: usize) -> store::ParcelStore {
    store::ParcelStore::new(capacity)
}

/// Validation happens before a record reaches the store.
pub fn validate_id(id: &str) -> bool {
    !id.trim().is_empty()
}
