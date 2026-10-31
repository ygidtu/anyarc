//! 统一后端抽象。
//!
//! `unarc-rs` 的 `UnifiedArchive<T>` 以 `T` 单态化(普通文件 / 分卷 Reader 类型不同),
//! 这里用 trait object 抹平,同时:
//! - 定义与任何库解耦的 `Entry`/`Format`(自写容器与 unarc 共用一套接口);
//! - 吸收 unarc 实测坑:目录/不读的条目必须显式 skip,否则 tar 类后端死循环;
//! - ACE/CAB 内部 `\` 分隔符 → 统一归一化为 `/`。

pub mod unarc;

use std::path::Path;

use unarc_rs::unified::ArchiveFormat;

/// 归档格式(unarc 支持的直接内嵌其枚举;自写容器单列)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Unarc(ArchiveFormat),
    /// ar 归档(`!<arch>`,含 Unix .a)
    Ar,
    /// Debian 包(ar 容器,只暴露 data.tar)
    Deb,
    /// RPM 包(暴露解压后的 payload 为单条 .cpio)
    Rpm,
    /// cpio(newc ASCII)
    Cpio,
    /// ISO 9660
    Iso,
}

impl Format {
    pub fn name(&self) -> String {
        match self {
            Format::Unarc(f) => f.name().to_string(),
            Format::Ar => "ar".into(),
            Format::Deb => "deb".into(),
            Format::Rpm => "rpm".into(),
            Format::Cpio => "cpio".into(),
            Format::Iso => "iso".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    HardLink,
    Special,
}

/// 与库解耦的条目快照;后端在 `next_entry` 时构造。
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String, // 已归一化分隔符(\ → /)
    pub kind: EntryKind,
    pub size: u64,
    pub compressed_size: u64,
    pub method: String,
    pub mtime_epoch: Option<u64>,
    pub encrypted: bool,
    pub link_target: Option<String>,
}

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
            max_entry_size: Some(4 << 30), // 4 GiB
            max_total_size: Some(32 << 30), // 32 GiB
            single_file_name: None,
        }
    }
}

/// 已打开归档的统一迭代接口。
/// 条目以"游标"语义操作:`read_current`/`skip_current` 作用于最近一次
/// `next_entry` 返回的条目(顺序流后端无法回读,故不提供按条目操作)。
pub trait ArchiveBackend {
    /// 实际检测出的格式(可能与请求不同,如分卷聚合)。
    fn format(&self) -> Format;
    /// 取下一个条目;读或 skip 后才前进。
    fn next_entry(&mut self) -> anyhow::Result<Option<Entry>>;
    /// 整条目读入内存(M1 条目大小受限额约束;solid 归档顺序读)。读后条目已消费。
    fn read_current(&mut self) -> anyhow::Result<Vec<u8>>;
    /// 跳过当前条目(目录/`--json` 列表模式用)。
    fn skip_current(&mut self) -> anyhow::Result<()>;
}

/// 打开一个已探测过的归档,返回后端实例。分卷(7z .001 / zip .z01)在此聚合。
pub fn open(path: &Path, det: &crate::detect::Detected, opts: &OpenOptions) -> anyhow::Result<Box<dyn ArchiveBackend>> {
    match det.kind {
        crate::detect::Known::Unarc(_) => unarc::open(path, det, opts),
        crate::detect::Known::Container(_) => crate::container::open(path, det, opts),
        crate::detect::Known::Backlog(_) => unarc::open(path, det, opts), // 由 unarc::open 统一报错
    }
}
