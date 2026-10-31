//! anyarc — 单二进制归档管理工具(patool 的 Rust 重写,零运行时外部依赖)。

pub mod backend;
pub mod cli;
pub mod container;
pub mod cp437;
pub mod create;
pub mod detect;
pub mod extract;
pub mod progress;
pub mod shn;
