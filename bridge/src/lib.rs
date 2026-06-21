// Library entry-point – exposes internal modules for integration tests.
// The binary (main.rs) uses these same modules directly via `mod` declarations.
pub mod collector;
pub mod hub;
pub mod model;
pub mod state;
