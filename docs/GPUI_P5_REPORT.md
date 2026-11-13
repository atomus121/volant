# GPUI P5 Preview Report

Date: 2026-11-14

## Scope

This report covers the GPUI Kit preview application, not the existing egui
release. Both are built from the same engine and `volant-app-core`.

Environment:

- Windows 11 Pro, build 26300
- Intel Core Ultra 9 285H, 16 GB-class integrated Arc 140T GPU plus RTX 5060
  Laptop GPU
- 200% display scaling
- Rust MSVC toolchain, `gpui-kit = =0.9.1`
- release profile: opt-level 3, fat LTO, one codegen unit, strip, abort on panic

## Reproduce

```powershell
.\tools\gpui-metrics.cmd
.\tools\package-gpui.cmd
```

The metrics script launches the real application with a fresh `VOLANT_HOME`,
keeps the window visible, waits 15 seconds for font, shader and GPU allocations
to settle, then samples for 10 seconds. Working Set is the Windows proxy for
RSS. `startup_window_ms` ends when Windows exposes a main window handle; this
is a repeatable proxy, not a measured first painted frame.

## Results

| Check | Old budget | GPUI preview | Result |
|---|---:|---:|---|
| Startup to window handle | <= 300 ms | 303, 317, 340, 412, 460 ms; median 340 ms | miss |
| Idle Working Set | <= 110 MB | 153.0, 153.7, 153.8, 166.0 MB; median 153.75 MB | miss |
| Idle Private Bytes | no legacy budget | 129.0, 129.8, 129.9, 137.3 MB; median 129.85 MB | reference |
| Idle CPU, one core | <= 0.5% | 0.16, 1.54, 2.15, 2.48% in 10-second runs; 1.01% and 2.76% in longer confirmations | miss / noisy |
| Eight-connection peak | <= 130 MB | 250.6 MB Working Set, 220.3 MB Private | miss |
| Three torrent peak | <= 150 MB | not yet measured | pending |
| Portable/setup zip | <= 12 MB | 9.36 MB each | pass |
| Portable plus setup | no legacy budget | 20.17 MB raw `volant.exe` + host | reference |

The 256 MiB local HTTP transfer used the default eight connections and finished
in 279 ms. A 20 MiB transfer and a two-file torrent were verified earlier on
the same preview path.

The final packaged build reproduced the baseline: startup to window handle
412 ms, 153.8 MB Working Set, 129.9 MB Private Bytes and 2.149% idle CPU.

## Same-method egui reference

Three runs of the rebuilt egui application through `gpui-metrics.ps1`:

| Check | egui |
|---|---:|
| Startup to window handle | 112.6, 118.3, 165.1 ms; median 118.3 ms |
| Idle Working Set after warm-up | 127.5-127.6 MB; median 127.6 MB |
| Idle Private Bytes after warm-up | 62.2-63.4 MB; median 63.1 MB |
| Idle CPU, 10-second runs | 0.15, 0.47, 8.07%; median 0.47% |

The 8.07% egui sample is an outlier. The earlier long egui test reported a
0.24% median. GPUI's idle samples remained higher across repeated runs.

## Interpretation

The package-size target is met with room to spare. Functional integration and
GPUI Kit component behavior are substantially complete. The unresolved issue
is not download throughput: it is the renderer/process baseline.

GPUI holds roughly 2x egui's Private Bytes while idle and repeatedly performs
GPU work while the window is untouched. That cost is visible in startup,
memory and idle CPU. It is not explained by an active transfer, torrent session,
clipboard change or capture event because fresh-home idle runs reproduce it.

Before replacing the egui release, choose one:

1. Keep GPUI as a preview and profile the render loop/driver path first.
2. Accept a new GPUI performance baseline and revise the PRD explicitly.
3. Keep egui as the default release until GPUI meets the existing budgets.

The current recommendation is option 1 plus option 3: continue the rewrite in
the existing GPUI preview branch, but do not make it the default release based
only on feature parity and package size.

## Remaining verification

- Three simultaneous torrents and peak memory.
- A download peak sampled after the 15-second warm-up, separate from startup.
- Actual IME composition and screen-reader speech, beyond the verified AccessKit
  tree and Unicode value.
- Explorer drag gesture, torrent file subsets, torrent pause/resume.
- Multi-monitor movement and live DPI changes.
- GPUI `VirtualList` for large task counts; the preview still renders all rows.
- Full HKCU native-host registration from a normal installer session. The
  packaged `--install-host` entry itself exits 0 without opening a window, and
  its generated manifest points at the packaged `volant-host.exe`; the
  automation process used for this report was not allowed to write HKCU.
