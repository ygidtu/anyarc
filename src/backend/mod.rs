//! 统一后端抽象。
//!
//! `unarc-rs` 的 `UnifiedArchive<T>` 以 `T` 单态化(普通文件 / 分卷 Reader 类型不同),
//! 这里用 trait object 抹平,同时吸收 PoC 实测的两个坑:
//! - 目录/不读的条目必须显式 `skip()`,否则 tar 类后端死循环;
//! - ACE/CAB 内部 `\` 分隔符 → 统一归一化为 `/`。

pub mod unarc;

use std::path::Path;

use unarc_rs::EncryptionMethod;
use unarc_rs::unified::{ArchiveEntry, ArchiveFormat};

use crate::detect::Detected;

pub use unarc_rs::unified::ArchiveEntryKind as EntryKind;
pub use unarc::entry_name;

/// 打开归档的选项(限额、密码、CRC 校验)。
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub password: Option<String>,
    pub verify_crc: bool,
    /// 单条目解压后上限(字节),None = 不限
    pub max_entry_size: Option<u64>,
    /// 整档解压总量上限
    pub max_total_size: Option<u64>,
    /// 单流压缩格式(gz/bz2/…)解压出的目标文件名
    pub single_file_name: Option<String>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            password: None,
            verify_crc: true,
            max_entry_size: Some(4 << 30),      // 4 GiB
            max_total_size: Some(32 << 30),     // 32 GiB
            single_file_name: None,
        }
    }
}

/// 已打开归档的统一迭代接口。
pub trait ArchiveBackend {
    /// 实际检测出的格式(可能与请求不同,如分卷聚合)。
    fn format(&self) -> ArchiveFormat;
    /// 取下一个条目;读或 skip 后才前进。
    fn next_entry(&mut self) -> anyhow::Result<Option<ArchiveEntry>>;
    /// 整条目读入内存(M1 条目大小受限额约束;solid 归档顺序读)。
    fn read(&mut self, entry: &ArchiveEntry) -> anyhow::Result<Vec<u8>>;
    /// 跳过当前条目(目录/`--json` 列表模式用)。
    fn skip(&mut self, entry: &ArchiveEntry) -> anyhow::Result<()>;
    fn entries_encrypted(&self) -> bool {
        false // unarc 封装按条目 encryption 判断,默认 false
    }
}

/// 条目是否加密。
pub fn entry_encrypted(entry: &ArchiveEntry) -> bool {
    entry.encryption() != EncryptionMethod::None || entry.is_encrypted()
}

/// 打开一个已探测过的归档,返回后端实例。分卷(7z .001 / zip .z01)在此聚合。
pub fn open(path: &Path, det: &Detected, opts: &OpenOptions) -> anyhow::Result<Box<dyn ArchiveBackend>> {
    unarc::open(path, det, opts)
}
