//! Tests that drive the push pipeline directly: the normalizer, the tree
//! builder, the sequence collector and the event serializer. They were
//! integration tests until the pipeline became crate-internal.

mod invariants;
mod namespace_inheritance;
mod strip_copy_ids;
mod strip_copy_types;
