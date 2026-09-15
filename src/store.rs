//! The append-only trace store, with programs and tool descriptions stored
//! alongside by hash.

// Phase 2d stub

use std::path::Path;

use crate::trace::Trace;

pub struct TraceStore {
    _private: (),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl TraceStore {
    pub fn open(_dir: &Path) -> Result<Self, StoreError> {
        todo!("Phase 2d")
    }

    pub fn put_trace(&self, _trace: &Trace) -> Result<(), StoreError> {
        todo!("Phase 2d")
    }

    pub fn get_trace(&self, _trace_id: &str) -> Result<Trace, StoreError> {
        todo!("Phase 2d")
    }

    pub fn put_program(&self, _program_hash: &str, _source: &str) -> Result<(), StoreError> {
        todo!("Phase 2d")
    }

    pub fn get_program(&self, _program_hash: &str) -> Result<String, StoreError> {
        todo!("Phase 2d")
    }

    pub fn put_description(&self, _description_hash: &str, _text: &str) -> Result<(), StoreError> {
        todo!("Phase 2d")
    }

    pub fn get_description(&self, _description_hash: &str) -> Result<String, StoreError> {
        todo!("Phase 2d")
    }
}
