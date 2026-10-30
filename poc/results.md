| fixture | 检测格式 | unarc-rs | sevenz-rust2 | 备注 |
|---|---|---|---|---|
| p .7z | 7z | MATCH | MATCH | 1项/1目录 |
| p.7z.foo | 7z | NOREF-OK | EXTRACTED 1项 | 1项/1目录 |
| p.arc | - | FAIL | - | read t.txt: CRC mismatch for 't.txt': expected 0x00001597, got 0x00000d8f |
| p.arc.foo | - | FAIL | - | read t.txt: CRC mismatch for 't.txt': expected 0x00001597, got 0x00000d8f |
| p.arj | ARJ | NOREF-OK | - | 1项/0目录 |
| p.arj.foo | ARJ | NOREF-OK | - | 1项/0目录 |
| p.cbz | ZIP | MATCH | - | 1项/1目录 |
| p.cbz.foo | ZIP | MATCH | - | 1项/1目录 |
| p.rar | RAR | MATCH | - | 1项/1目录 |
| p.rar.foo | RAR | MATCH | - | 1项/1目录 |
| p.zip | ZIP | MATCH | - | 1项/1目录 |
| p.zip.foo | ZIP | MATCH | - | 1项/1目录 |
| t .7z | 7z | MATCH | MATCH | 1项/1目录 |
| t .cb7 | 7z | MATCH | - | 1项/1目录 |
| t.7z.001 | 7z | MATCH | MATCH | 1项/1目录 |
| t.7z.foo | 7z | NOREF-OK | EXTRACTED 1项 | 1项/1目录 |
| t.ace | ACE | NOREF-OK | - | 1项/1目录 |
| t.ace.foo | ACE | NOREF-OK | - | 1项/1目录 |
| t.adf | ADF (Amiga floppy image) | NOREF-OK | - | 1项/0目录 |
| t.adf.foo | ADF (Amiga floppy image) | NOREF-OK | - | 1项/0目录 |
| t.alz | - | FAIL | - | detect: 未知格式 |
| t.alz.foo | - | FAIL | - | detect: 未知格式 |
| t.ape | - | FAIL | - | detect: 未知格式 |
| t.ape.foo | - | FAIL | - | detect: 未知格式 |
| t.apk | ZIP | MATCH | - | 7项/0目录 |
| t.apk.foo | ZIP | MATCH | - | 7项/0目录 |
| t.arc | ARC | MATCH | - | 2项/0目录 |
| t.arc.foo | ARC | MATCH | - | 2项/0目录 |
| t.arj | ARJ | MATCH | - | 1项/0目录 |
| t.arj.foo | ARJ | MATCH | - | 1项/0目录 |
| t.cab | CAB (Microsoft Cabinet) | MATCH | - | 1项/0目录 |
| t.cab.foo | CAB (Microsoft Cabinet) | MATCH | - | 1项/0目录 |
| t.cb7.foo | 7z | NOREF-OK | - | 1项/1目录 |
| t.cba | ACE | NOREF-OK | - | 1项/1目录 |
| t.cba.foo | ACE | NOREF-OK | - | 1项/1目录 |
| t.cbr | RAR | MATCH | - | 1项/1目录 |
| t.cbr.foo | RAR | MATCH | - | 1项/1目录 |
| t.cbt | TAR | MATCH | - | 1项/1目录 |
| t.cbt.foo | TAR | MATCH | - | 1项/1目录 |
| t.cbz | ZIP | MATCH | - | 1项/1目录 |
| t.cbz.foo | ZIP | MATCH | - | 1项/1目录 |
| t.chm | - | FAIL | - | detect: 未知格式 |
| t.chm.foo | - | FAIL | - | detect: 未知格式 |
| t.cpio | - | FAIL | - | detect: 未知格式 |
| t.cpio.foo | - | FAIL | - | detect: 未知格式 |
| t.deb | - | FAIL | - | detect: 未知格式 |
| t.deb.foo | - | FAIL | - | detect: 未知格式 |
| t.dms | DMS (Amiga DiskMasher) | NOREF-OK | - | 44项/7目录 |
| t.dms.foo | DMS (Amiga DiskMasher) | NOREF-OK | - | 44项/7目录 |
| t.epub | ZIP | MATCH | - | 7项/2目录 |
| t.epub.foo | ZIP | MATCH | - | 7项/2目录 |
| t.flac | - | FAIL | - | detect: 未知格式 |
| t.flac.flac | - | FAIL | - | detect: 未知格式 |
| t.flac.foo | - | FAIL | - | detect: 未知格式 |
| t.freearc.arc | - | FAIL | - | read }��Zp�J�� �: '}��Zp�J�� �' exceeds the size limit of 1073741824 bytes |
| t.freearc.arc.foo | - | FAIL | - | detect: 未知格式 |
| t.iso | - | FAIL | - | detect: 未知格式 |
| t.iso.foo | - | FAIL | - | detect: 未知格式 |
| t.jar | ZIP | MATCH | - | 2项/2目录 |
| t.jar.foo | ZIP | MATCH | - | 2项/2目录 |
| t.lha | LHA/LZH | MATCH | - | 1项/1目录 |
| t.lha.foo | LHA/LZH | MATCH | - | 1项/1目录 |
| t.lzh | LHA/LZH | MATCH | - | 1项/1目录 |
| t.rar | RAR | MATCH | - | 1项/1目录 |
| t.rar.foo | RAR | MATCH | - | 1项/1目录 |
| t.rar.gz | GZ (gzip) | MATCH | - | 1项/0目录 |
| t.rar.gz.foo | GZ (gzip) | MATCH | - | 1项/0目录 |
| t.rpm | - | FAIL | - | detect: 未知格式 |
| t.rpm.foo | - | FAIL | - | detect: 未知格式 |
| t.shar | - | FAIL | - | detect: 未知格式 |
| t.shn | - | FAIL | - | detect: 未知格式 |
| t.tar | TAR | MATCH | - | 1项/1目录 |
| t.tar.Z | TAR.Z (tar + Unix compress) | MATCH | - | 1项/1目录 |
| t.tar.Z.foo | Z (Unix compress) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t.tar] |
| t.tar.bz2 | TBZ (tar.bz2) | MATCH | - | 1项/1目录 |
| t.tar.bz2.foo | BZ2 (bzip2) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t.tar] |
| t.tar.foo | TAR | MATCH | - | 1项/1目录 |
| t.tar.gz | TGZ (tar.gz) | MATCH | - | 1项/1目录 |
| t.tar.gz.foo | GZ (gzip) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t.tar] |
| t.tar.lz | - | FAIL | - | detect: 未知格式 |
| t.tar.lz.foo | - | FAIL | - | detect: 未知格式 |
| t.tar.lzma | - | FAIL | - | detect: 未知格式 |
| t.tar.lzma.foo | - | FAIL | - | detect: 未知格式 |
| t.tar.xz | TXZ (tar.xz) | MATCH | - | 1项/1目录 |
| t.tar.xz.foo | XZ | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t.tar] |
| t.tar.zst | TZST (tar.zst) | MATCH | - | 1项/1目录 |
| t.tar.zst.foo | ZST (Zstandard) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t.tar] |
| t.taz | GZ (gzip) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t] |
| t.taz.foo | GZ (gzip) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t] |
| t.tbz2 | TBZ (tar.bz2) | MATCH | - | 1项/1目录 |
| t.tbz2.foo | BZ2 (bzip2) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t] |
| t.tgz | TGZ (tar.gz) | MATCH | - | 1项/1目录 |
| t.tgz.foo | GZ (gzip) | MISMATCH(2) | - | 1项/0目录 [缺:t/t.txt;多:t] |
| t.txt.Z | Z (Unix compress) | MATCH | - | 1项/0目录 |
| t.txt.Z.foo | Z (Unix compress) | MATCH | - | 1项/0目录 |
| t.txt.a | - | FAIL | - | detect: 未知格式 |
| t.txt.a.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.bz2 | BZ2 (bzip2) | MATCH | - | 1项/0目录 |
| t.txt.bz2.foo | BZ2 (bzip2) | MATCH | - | 1项/0目录 |
| t.txt.bz3 | - | FAIL | - | detect: 未知格式 |
| t.txt.bz3.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.gz | GZ (gzip) | MATCH | - | 1项/0目录 |
| t.txt.gz.foo | GZ (gzip) | MATCH | - | 1项/0目录 |
| t.txt.lrz | - | FAIL | - | detect: 未知格式 |
| t.txt.lrz.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.lz | - | FAIL | - | detect: 未知格式 |
| t.txt.lz.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.lz4 | - | FAIL | - | detect: 未知格式 |
| t.txt.lz4.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.lzma | - | FAIL | - | detect: 未知格式 |
| t.txt.lzma.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.lzo | - | FAIL | - | detect: 未知格式 |
| t.txt.lzo.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.rz | - | FAIL | - | detect: 未知格式 |
| t.txt.rz.foo | - | FAIL | - | detect: 未知格式 |
| t.txt.xz | XZ | MATCH | - | 1项/0目录 |
| t.txt.xz.foo | XZ | MATCH | - | 1项/0目录 |
| t.txt.zst | ZST (Zstandard) | MATCH | - | 1项/0目录 |
| t.txt.zst.foo | ZST (Zstandard) | MATCH | - | 1项/0目录 |
| t.udf | - | FAIL | - | detect: 未知格式 |
| t.vhd | - | FAIL | - | detect: 未知格式 |
| t.wav | - | FAIL | - | detect: 未知格式 |
| t.wim | - | FAIL | - | detect: 未知格式 |
| t.wim.foo | - | FAIL | - | detect: 未知格式 |
| t.zip | ZIP | MATCH | - | 1项/1目录 |
| t.zip.foo | ZIP | MATCH | - | 1项/1目录 |
| t.zoo | ZOO | MATCH | - | 2项/0目录 |
| t.zoo.foo | ZOO | MATCH | - | 2项/0目录 |
| t.zpaq | - | FAIL | - | detect: 未知格式 |
| t.zpaq.foo | - | FAIL | - | detect: 未知格式 |

总计 130 个 fixture: unarc-rs MATCH/NOREF-OK=71, MISMATCH=9, FAIL=50; sevenz-rust2: MATCH=3, FAIL=0
