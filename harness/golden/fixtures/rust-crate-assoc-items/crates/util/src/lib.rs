mod auth;

pub use auth::{Validator, validator_kind};

/// Connection settings.
pub struct Config {
    pub retries: u32,
}

impl Config {
    /// Builds default settings.
    pub fn new() -> Config {
        Config { retries: 3 }
    }
}
