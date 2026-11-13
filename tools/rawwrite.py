"""裸写基准：顺序写 <size> 字节到 <path> 并 fsync，打印吞吐。

T1 的对照基线——判断「引擎慢」还是「盘就这样」时，必须有一个同机同条件下的裸写数字。
用 Python 而不是 Rust，是为了不依赖被测代码：基线要独立于被测对象。

    python tools/rawwrite.py D:\\...\\target\\bench\\raw.bin 10737418240
"""

import os
import sys
import time

path, size = sys.argv[1], int(sys.argv[2])
block = os.urandom(1 << 20)
with open(path, "wb") as f:
    start = time.perf_counter()
    left = size
    while left > 0:
        n = min(left, len(block))
        f.write(block[:n])
        left -= n
    f.flush()
    os.fsync(f.fileno())
    secs = time.perf_counter() - start
print(f"raw write {size / 2**30:.1f} GiB: {secs:.2f} s, {size / 1e6 / secs:.0f} MB/s")
os.remove(path)
