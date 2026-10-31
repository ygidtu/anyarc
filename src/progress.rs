//! 进度条(indicatif):仅 TTY 且未 -q 时绘制,写 stderr,不污染 stdout。
//! 非 TTY 时返回 hidden bar,inc/set_message 为空操作。
//! 注:构造顺序固定为 new → set_style → set_prefix → tick,
//! 任何提前触发的重绘(如 set_prefix)都必须落在自定义样式上,
//! 否则 indicatif 默认样式的 `{bar} {pos}/{len}` 会闪现一帧。

use std::borrow::Cow;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

const ENTRY_TEMPLATE: &str = "{prefix:.bold} {spinner} {pos} 个条目 {wide_msg}";
const BYTES_TEMPLATE: &str = "{prefix:.bold} {spinner} {pos:>12} B [{wide_msg}]";

/// 条目计数进度条(extract/test/repack 源端等未知总数循环)。
pub struct EntryBar {
    pb: ProgressBar,
}

impl EntryBar {
    pub fn new(enabled: bool, prefix: &str) -> Self {
        Self { pb: make_bar(enabled, prefix, ENTRY_TEMPLATE) }
    }
    pub fn inc(&self) {
        self.pb.inc(1);
    }
    pub fn message(&self, msg: impl Into<Cow<'static, str>>) {
        self.pb.set_message(msg.into());
    }
    /// 在进度条暂停状态下执行 f(用于穿插 eprintln 告警/摘要)。
    pub fn suspend<F: FnOnce() -> R, R>(&self, f: F) -> R {
        self.pb.suspend(f)
    }
    pub fn finish(&self) {
        self.pb.finish_and_clear();
    }
}

/// 解压字节流进度条(extract 落盘:已写字节 + 当前条目)。
pub struct ExtractBar {
    pb: ProgressBar,
}

impl ExtractBar {
    pub fn new(enabled: bool) -> Self {
        Self { pb: make_bar(enabled, "解压", BYTES_TEMPLATE) }
    }
    pub fn entry(&mut self, name: &str) {
        self.pb.set_message(name.to_string());
    }
    pub fn add_bytes(&self, n: usize) {
        self.pb.set_position(self.pb.position() + n as u64);
    }
    pub fn suspend<F: FnOnce() -> R, R>(&self, f: F) -> R {
        self.pb.suspend(f)
    }
    /// 结束绘制(摘要行由调用方输出)。
    pub fn finish(&self) {
        self.pb.finish_and_clear();
    }
}

fn make_bar(enabled: bool, prefix: &str, template: &str) -> ProgressBar {
    // 未知总数:长度设 MAX,但样式只用 spinner/pos(无 {bar}),不会出现
    // "0/18446744073709551615" 之类噪声。
    let pb = if enabled {
        ProgressBar::new(u64::MAX)
    } else {
        ProgressBar::hidden()
    };
    let style = ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_spinner());
    pb.set_style(style);
    pb.set_prefix(prefix.to_string());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}
