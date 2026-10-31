//! 安全落盘:Zip-slip 防护、符号链接逃逸检查、覆盖策略、进度输出。

use std::io::IsTerminal;
use std::path::{Component, Path, PathBuf};

use unarc_rs::unified::ArchiveEntryKind;

use crate::backend::EntryKind;

/// 把归档内条目名解析到 dest 下的安全路径。
/// 拒绝:绝对路径、盘符、`..` 逃逸。返回 None = 应跳过该条目(由调用方计数告警)。
pub fn safe_join(dest: &Path, name: &str) -> Option<PathBuf> {
    let rel = normalize_rel(name)?;
    let joined = dest.join(&rel);
    // 词法规范化后再确认不出 dest(Path::starts_with 按组件比较,不会误判前缀)
    let dest_n = lexically_normalize(dest);
    if !lexically_normalize(&joined).starts_with(&dest_n) {
        return None;
    }
    Some(joined)
}

/// 是否逃逸 dest(用于符号链接目标检查)。
pub fn link_target_escapes(dest: &Path, entry_dir: &Path, target: &str) -> bool {
    let base = if Path::new(target).is_absolute() {
        // 绝对链接:落在 dest 内才允许(参考 GNU tar --same-owner 的保守策略)
        PathBuf::from(target)
    } else {
        entry_dir.join(target)
    };
    let dest_n = lexically_normalize(dest);
    !lexically_normalize(&base).starts_with(&dest_n)
}

fn normalize_rel(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() || name.starts_with('/') || name.contains(':') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            // ParentDir / RootDir / Prefix 一律视为逃逸
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    Some(out)
}

/// 纯词法规范化(解析 `.`/`..`,不要求路径存在)。
fn lexically_normalize(p: &Path) -> PathBuf {
    let mut stack: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if matches!(stack.last(), Some(Component::Normal(_))) {
                    stack.pop();
                } else {
                    stack.push(c);
                }
            }
            Component::CurDir => {}
            _ => stack.push(c),
        }
    }
    stack.iter().collect()
}

pub fn is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// 条目种类 → 落盘动作分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Dir,
    File,
    Symlink,
    Hardlink,
    SkipSpecial,
}

pub fn classify(kind: ArchiveEntryKind) -> Action {
    match kind {
        EntryKind::Directory => Action::Dir,
        EntryKind::File => Action::File,
        EntryKind::SymbolicLink => Action::Symlink,
        EntryKind::HardLink => Action::Hardlink,
        EntryKind::Special | EntryKind::Unknown => Action::SkipSpecial,
    }
}
