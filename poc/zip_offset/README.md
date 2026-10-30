# 偏移 zip 探针(2026-10-30)

结论:unarc-rs/zip-rs 后端对"前置数据 zip"与 WinZip AES-99/ZipCrypto 全部可解,
唯一缺口是 ArchiveFormat::detect() 无 EOCD 反向扫描。详见 docs/PLAN.md §3.5/§5/§6/§7.7。

复现:
    bash make_fixtures.sh          # 生成 normal.zip / prefixed.mp4 / aes.zip / zc.zip
    # 在 anyarc 仓库根建临时 bin 或:
    cargo script / rustc 方式均可;probe.rs 依赖 unarc-rs=0.8.1, zip=9
参照对照:
    python3 -c "import zipfile;print(zipfile.ZipFile('prefixed.mp4').namelist())"  # OK
    unzip -l prefixed.mp4   # FAIL(info-zip 也不行,勿作基准)
    file prefixed.mp4       # 只报 MP4 → patool 盲区
