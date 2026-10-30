# anyarc

单个静态二进制的归档文件管理工具,patool 的 Rust 重写 —— 运行时零外部依赖(无需 unrar/7z/unar 等系统工具)。

- 规划:`docs/PLAN.md`
- 可行性 PoC 报告:`docs/POC-REPORT.md`(对 patool 全部 fixture 的逐字节兼容性矩阵)

## 当前状态

PoC 完成:unarc-rs + sevenz-rust2 对 patool 69 个 fixture 的容器类格式通过率 ~85%,含 RAR(密码)、7z(AES/分卷)、ACE/ADF/DMS。正式实现等待立项指令。

```bash
bash poc/ref_extract.sh   # 生成参照 manifest
cargo run --release       # 复现兼容性矩阵
```
