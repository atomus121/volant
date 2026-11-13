# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

Windows-first native desktop application. The current product is Windows-only; GPUI keeps a future cross-platform path open. This platform classification is inferred from the repository because the schema has no desktop value.

## Users

Desktop users who download files directly from browsers, HTTP/HTTPS servers, magnet links, or `.torrent` files. The original PRD leaves the release audience undecided between private use and public release; this design exploration therefore targets the concrete daily workflow rather than assuming a public-market audience.

## Product Purpose

Volant performs one job: move remote files to local disk, then help the user find and open them. It should be fast, predictable after interruption, and require fewer entry points than full-featured download managers.

## Positioning

Volant combines adaptive segmented HTTP downloads, host-aware connection limits, resumable state, browser capture, and a deliberately narrow BitTorrent integration in one minimal Windows downloader. Its UI should make the transfer itself understandable without turning the application into a network dashboard.

## Operating Context

- Users add HTTP/HTTPS, magnet, or `.torrent` sources.
- Accepted downloads appear in one queue with progress, speed, connection count, size, status, and destination.
- Common actions are add, pause/resume, open the saved file, and open its folder.
- Settings cover parallel task limits, connections per task, rate limiting, destination, clipboard capture, delete confirmation, seeding, and theme.
- The GPUI implementation currently supports keyboard focus, AccessKit, Chinese text, native window chrome, file drop, and browser capture.

## Capabilities and Constraints

- Current UI stack: Rust GPUI with GPUI Kit 0.9.1.
- Download engine is UI-framework independent.
- Preserve the current behavior boundary between `control::Cmd` and `control::UiEvent`.
- Do not introduce a second theme runtime alongside GPUI Kit.
- Keep the app usable at the existing 520×340 minimum window size; main design viewport is 1440×900.
- Support light and dark themes through semantic tokens.
- Light theme must use white as the dominant surface and black/gray for hierarchy; color may carry semantic status only.
- Do not invent upload, cloud-drive, video-download, remote-control, or account features.

## Brand Commitments

- Product name: Volant.
- Existing mark: a compact `V` symbol derived from `volant-icon.svg`.
- Chinese is the primary UI language.
- Product voice is direct, restrained, and specific; controls name the action they perform.

## Evidence on Hand

- `docs/PRD.md`: product positioning, scope, workflows, and acceptance constraints.
- `docs/GPUI_MIGRATION.md`: framework and migration boundaries.
- `crates/app-gpui/src/app.rs`: current implemented screens and content model.
- `docs/design-drafts/`: prior visual explorations.

No customer testimonials, production usage metrics, or public brand guidelines are available and none should be fabricated.

## Product Principles

1. The active download is the primary object; summaries exist only to support it.
2. Fewer entrances, but every necessary state remains visible and recoverable.
3. Speed claims must be demonstrated through real transfer state, not decorative dashboards.
4. Settings are low-frequency and should read like stable preferences, not a control room.
5. Light and dark are two renderings of one semantic system, not two separate products.

## Accessibility & Inclusion

Interactive controls must have visible keyboard focus and accessible names. Status cannot rely on color alone. Text must remain readable at system scaling and in the minimum supported desktop window.
