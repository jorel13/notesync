//! notesync server: accounts, vaults and change history for syncing Obsidian vaults.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod gc;
pub mod paths;
pub mod state;
pub mod storage;
pub mod sync;
pub mod users;

pub use config::Config;
pub use state::AppState;
