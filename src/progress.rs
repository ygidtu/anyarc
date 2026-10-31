//! 进度条(indicatif):仅 TTY 且未 -q 时绘制,写 stderr,不污染 stdout。
//! 非 TTY 时返回 hidden bar,inc/set_message 为空操作。
//! 注:构造顺序固定为 new → set_style → set_prefix → tick,
//! 任何提前触发的重绘(如 set_prefix)都必须落在自定义样式上,
//! 否则 indicatif 默认样式的 `{bar} {pos}/{len}` 会闪现一帧。

use std::borrow::Cow;
use std::cell::Cell;
use std::io::Write;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

const ENTRY_TEMPLATE: &str = "{prefix:.bold} {spinner} {pos} 个条目 {wide_msg}";
const BYTES_TEMPLATE: &str = "{prefix:.bold} {spinner} {binary_bytes} {binary_bytes_per_sec} [{wide_msg}]";
/// pv 风格(总长已知):进度条 + 百分比 + 已解压字节 + 速率 + ETA + 已用时。
const PV_TEMPLATE: &str = "{prefix:.bold} {wide_bar} {percent:>3}% {binary_bytes}/{binary_total_bytes} {binary_bytes_per_sec} ETA {eta} [{elapsed}]";
/// pv 无大小模式(总长未知):字节 + 速率 + 已用时,无进度条/ETA。
const PV_STREAM_TEMPLATE: &str = "{prefix:.bold} {spinner} {binary_bytes} {binary_bytes_per_sec} [{elapsed}] {wide_msg}";

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

/// 解压字节流进度条(extract 多条目落盘:累计已写字节 + 当前条目)。
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
    /// 包装写入器:透写字节并分块推进进度条(流式解压 → 平滑进度)。
    pub fn wrap<W: Write>(&self, w: W) -> ProgressWriter<'_, W> {
        ProgressWriter { w, sink: self }
    }
    pub fn suspend<F: FnOnce() -> R, R>(&self, f: F) -> R {
        self.pb.suspend(f)
    }
    /// 结束绘制(摘要行由调用方输出)。
    pub fn finish(&self) {
        self.pb.finish_and_clear();
    }
}

/// 单文件落盘进度条(pv 风格):已解压字节 + 速率 + 进度条/ETA。
/// total 为解压总长估算(gz 尾 ISIZE / LZMA 头原始大小等);
/// None = 未知(退化为 pv 无大小模式);实际超出估算(多成员 gzip、
/// >4 GiB ISIZE 回绕)时自动降级,不显示错误百分比。
pub struct BytesBar {
    pb: ProgressBar,
    sized: Cell<bool>,
}

impl BytesBar {
    pub fn new(enabled: bool, prefix: &str, total: Option<u64>) -> Self {
        let sized = matches!(total, Some(t) if t > 0);
        let pb = if enabled {
            ProgressBar::new(if sized { total.unwrap() } else { u64::MAX })
        } else {
            ProgressBar::hidden()
        };
        pb.set_style(style(if sized { PV_TEMPLATE } else { PV_STREAM_TEMPLATE }));
        pb.set_prefix(prefix.to_string());
        pb.enable_steady_tick(Duration::from_millis(120));
        Self { pb, sized: Cell::new(sized) }
    }
    pub fn add_bytes(&self, n: usize) {
        if n == 0 {
            return;
        }
        self.pb.inc(n as u64);
        if self.sized.get() && self.pb.position() > self.pb.length().unwrap_or(u64::MAX) {
            // 估算偏小:降级为无总长模式(去进度条/百分比/ETA)。
            self.sized.set(false);
            self.pb.set_length(u64::MAX);
            self.pb.set_style(style(PV_STREAM_TEMPLATE));
        }
    }
    /// 包装写入器:透写字节并分块推进进度条。
    pub fn wrap<W: Write>(&self, w: W) -> ProgressWriter<'_, W> {
        ProgressWriter { w, sink: self }
    }
    /// 结束绘制(摘要行由调用方输出)。
    pub fn finish(&self) {
        self.pb.finish_and_clear();
    }
}

/// 字节推进回调(ExtractBar/BytesBar 统一实现,供 ProgressWriter 借用)。
pub trait ByteSink {
    fn add(&self, n: usize);
}

impl ByteSink for ExtractBar {
    fn add(&self, n: usize) {
        self.add_bytes(n);
    }
}

impl ByteSink for BytesBar {
    fn add(&self, n: usize) {
        self.add_bytes(n);
    }
}

/// 写入适配器:透写并回调已写字节数(drop 时尽力 flush)。
pub struct ProgressWriter<'a, W: Write> {
    w: W,
    sink: &'a dyn ByteSink,
}

impl<W: Write> Write for ProgressWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.w.write(buf)?;
        self.sink.add(n);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

impl<W: Write> Drop for ProgressWriter<'_, W> {
    fn drop(&mut self) {
        let _ = self.w.flush();
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
    pb.set_style(style(template));
    pb.set_prefix(prefix.to_string());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
}
