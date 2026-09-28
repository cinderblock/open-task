//! Platforms without an updater yet: every request says so.

use std::path::Path;
use std::sync::Arc;

use crate::{Installation, Platform, UpdateError};

pub fn native(_user_agent: &str) -> Arc<dyn Platform> {
    Arc::new(Unsupported)
}

pub fn installations() -> Vec<Installation> {
    Vec::new()
}

struct Unsupported;

impl Platform for Unsupported {
    fn fetch(
        &self,
        _url: &str,
        _limit: u64,
        _sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
        _progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<(), UpdateError> {
        Err(UpdateError::Unsupported)
    }

    fn run_installer(&self, _path: &Path, _args: &[&str]) -> Result<i32, UpdateError> {
        Err(UpdateError::Unsupported)
    }
}
