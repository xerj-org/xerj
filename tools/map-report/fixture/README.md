# Parcel ledger

A small synthetic Rust project for map report verification.

The [parcel store](src/store.rs) owns in-memory records. The
[library](src/lib.rs) exposes startup and error handling. Sample
[events](data/events.csv) describe successful and refused writes.

Configuration selects the store capacity. Validation rejects empty parcel IDs.
No customer data, credentials, external model or production node is used.
