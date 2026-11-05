pub mod client;
pub mod config;
pub mod glossary;
pub mod history;
pub mod protocol;
pub mod target;

pub use client::{Client, Error, EventStream};
pub use config::{Config, ConfigError, default_socket_path};
pub use glossary::{GlossaryError, merge_glossaries, parse_glossary};
pub use history::{
    HistoryError, Ineligible, RESERVED_TOKEN_PREFIX, append_history, check_history_eligible,
    drop_oldest_half, history_entry, parse_history,
};
pub use protocol::{
    ErrorCode, Event, FEATURE_HISTORY, GlossaryEntry, HistoryEntry, Status, TranslateRequest, Usage,
};
pub use target::resolve_target;
