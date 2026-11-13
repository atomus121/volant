"""从系统里的微软雅黑生成一份「去 hinting」的子集字体，给 Volant 的中文字族用。

为什么：msyh.ttc 有 19.7MB，装进 egui 后整份常驻内存。egui/skrifa 光栅化字形时不用
TrueType hinting，所以丢掉 hinting 和用不到的字符不掉画质，但能省 11MB 工作集
（19.7MB → 8.3MB）。IMPLEMENT §1.4 的 RSS 预算就是靠这个守住的。

用法（一次性，之后每次构建都会自动读到；没跑也能正常渲染，只是内存回到 19.7MB）：

    C:\\Users\\EvanShaw\\.dsh\\dsh-runtimes\\dsh-primary-runtime\\dependencies\\python\\python.exe tools\\subset-font.py

产物默认写到 %LOCALAPPDATA%\\volant\\cjk-subset.ttf。程序查找顺序：
    %VOLANT_CJK%  →  %LOCALAPPDATA%\\volant\\cjk-subset.ttf  →  C:\\Windows\\Fonts\\msyh.ttc

注意：这份子集是从本机已安装的字体生成的，不要提交进仓库、不要随安装包分发——
微软雅黑不允许再分发。换机器或换了雅黑版本后重跑一次即可。
"""

from __future__ import annotations

import argparse
import logging
import os
import sys

from fontTools import subset
from fontTools.ttLib import TTCollection


def charset(lo: int, hi: int) -> str:
    return "".join(chr(c) for c in range(lo, hi))


def repertoire(ext_a: bool) -> str:
    text = (
        charset(0x20, 0x7F)  # ASCII
        + charset(0xA0, 0x250)  # Latin-1 + 扩展 A/B
        + charset(0x370, 0x500)  # 希腊 + 西里尔
        + charset(0x2000, 0x2070)  # 常用标点（—、“”、…）
        + charset(0x2190, 0x21A0)  # 箭头
        + charset(0x25A0, 0x27C0)  # 几何图形 + 杂项符号（●、✓）
        + charset(0x3000, 0x3100)  # CJK 标点 + 假名
        + charset(0xFF00, 0xFFF0)  # 全角
    )
    if ext_a:
        text += charset(0x3400, 0x4DC0)  # CJK 扩展 A（生僻人名用字）
    text += charset(0x4E00, 0xA000)  # CJK 基本区
    return text


def main() -> int:
    default_out = os.path.join(
        os.environ.get("LOCALAPPDATA", os.path.expanduser("~")), "volant", "cjk-subset.ttf"
    )
    ap = argparse.ArgumentParser(description="生成 Volant 用的雅黑子集")
    ap.add_argument("--src", default=r"C:\Windows\Fonts\msyh.ttc", help="源字体（默认为系统中雅黑）")
    ap.add_argument("--out", default=default_out, help="输出路径（默认 %%LOCALAPPDATA%%\\volant\\cjk-subset.ttf）")
    ap.add_argument("--face", type=int, default=0, help="TTC 里的第几套字（0 = Microsoft YaHei）")
    ap.add_argument("--no-ext-a", action="store_true", help="不收 CJK 扩展 A，再省约 2MB")
    args = ap.parse_args()

    logging.disable(logging.WARNING)  # fontTools 会对 MERG/meta 这类表报「不支持，已丢弃」，无所谓

    if not os.path.exists(args.src):
        print(f"找不到源字体：{args.src}", file=sys.stderr)
        return 1

    text = repertoire(not args.no_ext_a)
    opts = subset.Options()
    opts.hinting = False  # 关键：-59% 体积，egui 不用 hinting
    opts.desubroutinize = False
    opts.layout_features = ["*"]
    opts.name_IDs = ["*"]
    opts.name_legacy = True
    opts.notdef_outline = True

    font = TTCollection(args.src, lazy=False).fonts[args.face]
    subsetter = subset.Subsetter(options=opts)
    subsetter.populate(text=text)
    subsetter.subset(font)

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    font.save(args.out)

    src_mb = os.path.getsize(args.src) / 1048576
    out_mb = os.path.getsize(args.out) / 1048576
    print(f"字形 {len(text)} 个：{args.src} {src_mb:.1f}MB → {args.out} {out_mb:.1f}MB（省 {src_mb - out_mb:.1f}MB）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
