//! QLStats — статистика игроков Quake Live по ZMQ-потоку сервера.
//!
//! Архитектура: docs/ARCHITECTURE.md, правила учёта: docs/REQUIREMENTS.md.

pub mod cli;
pub mod collector;
pub mod config;
pub mod live;
pub mod model;
pub mod replay;
pub mod stats;
pub mod store;
pub mod web;
