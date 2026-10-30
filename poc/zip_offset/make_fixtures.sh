#!/usr/bin/env bash
set -eu
cd "$(dirname "$0")"; rm -rf work; mkdir -p work/t
printf '42\n' > work/t/t.txt; printf '42\n' > work/t2.txt
(cd work && python3 - <<'PY'
import zipfile
z=zipfile.ZipFile('normal.zip','w')
z.write('t/t.txt','t/t.txt'); z.write('t2.txt','t2.txt'); z.close()
with open('prefixed.mp4','wb') as f:
    f.write(b'\x00\x00\x00\x1cftypisom'+b'\x11'*3068); f.write(open('normal.zip','rb').read())
PY
7z a -tzip -mem=AES256 -psecret -y aes.zip t/t.txt >/dev/null
7z a -tzip -mem=ZipCrypto -psecret -y zc.zip t/t.txt >/dev/null)
echo "fixtures: work/{normal.zip,prefixed.mp4,aes.zip,zc.zip}"
