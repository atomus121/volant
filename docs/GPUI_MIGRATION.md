# GPUI Migration

Status: P4 core complete. The app now has a GPUI Kit title bar, DPI-aware
startup bounds, keyboard focus, AccessKit controls, Chinese text rendering,
native folder picking, browser capture verification, external torrent drop
wiring, and verified torrent selection/download.

## Decisions

- Rebuild the desktop UI with `gpui-kit`, pinned to one exact release at a time.
- Prefer GPUI Kit component styling; preserve the Volant name, palette and
  information architecture where they carry product identity.
- Keep the existing egui application buildable until GPUI reaches functional
  parity.
- Keep the download engine UI-framework independent. The boundary is
  `control::Cmd` in and `control::UiEvent` out.
- Develop the GPUI application on the MSVC Rust toolchain. The existing
  `windows-gnu` build remains available during migration.

## Architecture

```text
volant-engine / volant-torrent
              |
       volant-app-core
       model, control, capture, clipboard, native host
          |                 |
     volant-app        volant-app-gpui
       egui              GPUI Kit
```

`volant-app-core` must not depend on egui or GPUI. Background workers request a
UI wake-up through `notify::UiNotifier`; each GUI supplies its own adapter.

## Milestones

| Phase | Deliverable | Exit condition |
|---|---|---|
| P0 | app-core extraction, MSVC toolchain, blank GPUI window | complete: old GUI tests pass; GPUI and engine build and run on MSVC |
| P1 | shell, theme, fonts, icons, static list | complete: GPUI Kit dark theme, filters, summary cards and transfer rows render |
| P2 | live list and detail panel | complete for HTTP: add -> progress -> pause -> resume -> done and state persistence |
| P3 | dialogs, settings, torrent picker, capture, clipboard | implemented; HTTP dialog/settings verified, torrent/capture validation pending |
| P4 | DPI, CJK, IME, focus, accessibility, file drop, window chrome | core complete: title bar, DPI, Tab/AX tree, CJK, capture and torrent verified; native IME composition and OS drag gesture remain manual checks |
| P5 | tests, screenshots, metrics, packaging | budgets pass after dogfooding |

## Acceptance Budgets

The existing PRD budgets remain the target:

- cold start to interactive: <= 300 ms
- idle RSS: <= 110 MB
- eight-connection peak RSS: <= 130 MB
- three-torrent peak RSS: <= 150 MB
- package: <= 12 MB
- one-Gbps CPU: <= equivalent curl x 1.25

Measure the current egui application and the GPUI application on the same MSVC
toolchain before comparing them.

## Open Risks

- GPUI Kit currently requires MSVC, a Windows SDK and CMake.
- GPUI text shaping will require a CJK and DPI review despite the shared palette.
- Custom window resizing may be replaced by GPUI platform behavior, not copied.
- GPUI Kit default features may increase binary size; prune them after parity.
- GPUI and GPUI Kit are pre-1.0. Pin versions and upgrade deliberately.
