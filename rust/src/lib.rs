//! Luban 压缩算法 —— Rust 移植版
//!
//! 从 `master` 分支的 Kotlin 实现移植（`luban_rust` 分支原本没有任何 Rust 代码，
//! 只有一次 README 改动，故此处是**新建**工程而非补全既有实现）。
//!
//! 对应关系：
//!   `algorithm/CompressionCalculator.kt` → [`compress::calculate_target`]
//!   `compression/JpegCompressor.kt`       → [`compress::compress_to_target`]
//!   `io/ImageLoader.kt`                   → [`compress::decode_with_orientation`]

pub mod compress;
pub mod mcp;
pub mod server;
pub mod webui;

pub use compress::{calculate_target, compress_to_target, decode_with_orientation, resize_to, CompressionTarget};
