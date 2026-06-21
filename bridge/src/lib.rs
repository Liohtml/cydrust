// Library entry-point – exposes internal modules for integration tests.
// The binary (main.rs) uses these same modules directly via `mod` declarations.
pub mod collector;
pub mod collector_opencode;
pub mod collector_hermes;
pub mod hub;
pub mod metrics;
pub mod model;
pub mod state;
pub mod usage;
