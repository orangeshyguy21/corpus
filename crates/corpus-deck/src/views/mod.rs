//! Screen modules (house rule, deck-flow chunk 0): one module per
//! screen; every corpus-core call routes through `DeckState`, widgets
//! only render state and request actions.

pub mod agents;
pub mod launch;
pub mod projects;
pub mod teams;