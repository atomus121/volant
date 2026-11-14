---
name: "Volant · 航道 · Current"
description: "A fast, low-noise Windows downloader built around the visible flow of transfer."
colors:
  paper: "#FFFFFF"
  paper-subtle: "#F7F8FA"
  paper-sunken: "#F3F5F7"
  paper-hover: "#F2F4F6"
  selected-surface: "#EDF2FF"
  selected-border: "#B9CAF4"
  text-primary: "#111417"
  text-muted: "#59626B"
  text-faint: "#657079"
  border: "#E1E4E7"
  border-strong: "#CDD3D8"
  action-primary: "#151A1E"
  action-primary-text: "#FFFFFF"
  current: "#2F6BFF"
  current-soft: "#EDF2FF"
  flow-bar-from: "#1F54D8"
  flow-bar-to: "#8CAEFF"
  success: "#16765A"
  warning: "#94621C"
  danger: "#B3423D"
  dark-canvas: "#101318"
  dark-titlebar: "#0D1014"
  dark-rail: "#12161B"
  dark-surface: "#15191F"
  dark-sunken: "#171C22"
  dark-hover: "#1B2128"
  dark-selected: "#202832"
  dark-selected-border: "#334A72"
  dark-text: "#EDF1F5"
  dark-text-muted: "#A2ACB6"
  dark-text-faint: "#87929C"
  dark-border: "#222830"
  dark-border-strong: "#34404C"
  dark-action-primary: "#7299FF"
  dark-action-primary-text: "#0D1422"
  dark-success: "#52D3A3"
  dark-warning: "#E3A94F"
  dark-danger: "#FF8078"
  dark-flow-bar-from: "#496EBD"
  dark-flow-bar-to: "#82A5FF"
  switch-thumb-shadow: "rgba(0,0,0,.18)"
typography:
  page-title:
    fontFamily: "Segoe UI Variable Display, Segoe UI, Microsoft YaHei UI, sans-serif"
    fontSize: "24px"
    fontWeight: 630
    lineHeight: 1.15
    letterSpacing: "-0.025em"
  section-title:
    fontFamily: "Segoe UI Variable Text, Segoe UI, Microsoft YaHei UI, sans-serif"
    fontSize: "21px"
    fontWeight: 620
    lineHeight: 1.2
  file-name:
    fontFamily: "Segoe UI Variable Text, Segoe UI, Microsoft YaHei UI, sans-serif"
    fontSize: "13px"
    fontWeight: 580
    lineHeight: 1.35
  body:
    fontFamily: "Segoe UI Variable Text, Segoe UI, Microsoft YaHei UI, sans-serif"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.45
  meta:
    fontFamily: "Segoe UI Variable Text, Segoe UI, Microsoft YaHei UI, sans-serif"
    fontSize: "11.5px"
    fontWeight: 400
    lineHeight: 1.3
  data:
    fontFamily: "Cascadia Mono, Consolas, monospace"
    fontSize: "11.5px"
    fontWeight: 400
    fontFeature: "tnum"
  mark:
    fontFamily: "Segoe UI Variable Text, Segoe UI, Microsoft YaHei UI, sans-serif"
    fontSize: "8px"
    fontWeight: 800
rounded:
  hairline: "1px"
  xs: "3px"
  sm: "5px"
  selection: "6px"
  md: "7px"
  band: "8px"
  lg: "10px"
  xl: "11px"
  pill: "999px"
spacing:
  xxs: "3px"
  xs: "5px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "22px"
  xxl: "32px"
components:
  button-primary:
    backgroundColor: "{colors.action-primary}"
    textColor: "{colors.action-primary-text}"
    rounded: "{rounded.md}"
    padding: "0 10px"
    height: "32px"
  button-primary-dark:
    backgroundColor: "{colors.dark-action-primary}"
    textColor: "{colors.dark-action-primary-text}"
    rounded: "{rounded.md}"
    padding: "0 10px"
    height: "32px"
  button-secondary:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.text-primary}"
    rounded: "{rounded.md}"
    padding: "0 10px"
    height: "32px"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.text-muted}"
    rounded: "{rounded.md}"
    padding: "0 8px"
    height: "28px"
  nav-item-selected:
    backgroundColor: "{colors.selected-surface}"
    textColor: "{colors.text-primary}"
    rounded: "{rounded.md}"
    height: "32px"
  input:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.text-primary}"
    rounded: "{rounded.sm}"
    height: "32px"
  switch-on:
    backgroundColor: "{colors.action-primary}"
    rounded: "{rounded.pill}"
    width: "32px"
    height: "18px"
  speed-band:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.text-primary}"
    rounded: "{rounded.md}"
    height: "70px"
  selected-row:
    backgroundColor: "{colors.selected-surface}"
    textColor: "{colors.text-primary}"
    rounded: "{rounded.md}"
    height: "59px"
  new-download-dialog:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.text-primary}"
    rounded: "{rounded.lg}"
    width: "620px"
    height: "350px"
  settings-dialog:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.text-primary}"
    rounded: "{rounded.lg}"
    width: "760px"
    height: "610px"
---

# Design System: Volant · 航道 · Current

## Overview

**Creative North Star: "航道 · Current"**

Volant should look like work in motion, not like a dashboard about work. The active download is the largest and most legible object; live throughput and connection activity visibly support it. The queue stays dense enough for desktop use, while the inspector gives one selected task room to explain itself.

This is an Operate surface. Familiar desktop patterns are deliberate: native window controls, a left navigation rail, a data-like queue, standard buttons, fields, switches and dialogs. Expression comes from the current-flow band, restrained blue activity states and precise alignment, never from decorative chrome.

**Key Characteristics:**

- One active task owns attention.
- Live flow is visible as a total speed and a smooth sampled area graph.
- The main window has a queue rail and work area; its inspector appears after a
  task is selected.
- Light theme is white-first; dark theme preserves the original graphite hierarchy.
- Borders and tonal surfaces create structure; shadows are reserved for floating windows and the primary action.
- Semantic color is used only for state.

## Canonical References

These references are normative. Implementers must reproduce their structure and content, not reinterpret them:

- [Main window, light](docs/design-drafts/current-canonical/reference/main-light.png)
- [Main window, dark](docs/design-drafts/current-canonical/reference/main-dark.png)
- [Settings, light](docs/design-drafts/current-canonical/reference/settings-light.png)
- [Settings, dark](docs/design-drafts/current-canonical/reference/settings-dark.png)
- [New download, light](docs/design-drafts/current-canonical/reference/new-download-light.png)
- [New download, dark](docs/design-drafts/current-canonical/reference/new-download-dark.png)

The main window and settings are fixed visual contracts for the full UI implementation. Do not add a second summary card row, a new sidebar section, a new inspector, a settings preview column, or any alternative new-download layout.

## Colors

The palette is a neutral desktop ground with one blue live-transfer signal. Light mode is intentionally white-dominant. Dark mode reuses the original Current graphite palette.

### Primary

- **Live Current** (`{colors.current}`): active progress, the speed line, focus,
  selected-row border and active state dots.
- **Live Current Soft** (`{colors.current-soft}`): the selected row background in light mode and restrained active-state fill.
- **Primary Action** (`{colors.action-primary}`): the filled new-download button in light mode.

### Secondary

- **Success Signal** (`{colors.success}`): completed and resumable confirmation only.
- **Warning Signal** (`{colors.warning}`): paused or waiting-for-action state only.
- **Danger Signal** (`{colors.danger}`): failed state and destructive confirmation only.

### Neutral

- **Paper** (`{colors.paper}`): main content and floating window background in light mode.
- **Paper Subtle** (`{colors.paper-subtle}`): navigation rail and window chrome.
- **Paper Sunken** (`{colors.paper-sunken}`): low-emphasis controls and file icon wells.
- **Primary Ink** (`{colors.text-primary}`): headings, filenames and primary values.
- **Muted Ink** (`{colors.text-muted}`): secondary labels and descriptions.
- **Faint Ink** (`{colors.text-faint}`): table heads and non-actionable metadata.
- **Rule** (`{colors.border}`): 1px internal dividers.
- **Strong Rule** (`{colors.border-strong}`): input borders, outline buttons and selected-row boundary.

### Dark Mode

- **Canvas** (`{colors.dark-canvas}`): main background.
- **Rail** (`{colors.dark-rail}`): navigation and inspector.
- **Surface** (`{colors.dark-surface}`): cards, bands and popovers.
- **Sunken** (`{colors.dark-sunken}`): icon wells and quiet inputs.
- **Selected** (`{colors.dark-selected}`): selected task row.
- **Text** (`{colors.dark-text}`): headings and filenames.
- **Muted Text** (`{colors.dark-text-muted}`): secondary data.
- **Faint Text** (`{colors.dark-text-faint}`): table headers and labels.

**The One Current Rule.** Blue is reserved for active transfer, focus and selection. It must not decorate inactive rows, headings or containers.

**The White Ground Rule.** In light mode, white must remain at least 75% of the visible application surface. Rail and sunken surfaces may be gray; content and dialogs remain white.

**The Status Pair Rule.** Every colored status also has a text label and a dot. Never communicate status with color alone.

## Typography

**Display Font:** Segoe UI Variable Display, with Segoe and Microsoft YaHei UI fallback  
**Body Font:** Segoe UI Variable Text, with Segoe UI and Microsoft YaHei UI fallback  
**Label/Mono Font:** Cascadia Mono, with Consolas fallback

**Character:** Native Windows UI typography, compact but calm. Chinese and Latin must share the same visual weight. Data uses tabular numerals; monospace is never used for prose.

### Hierarchy

- **Page title** (630, `{typography.page-title.fontSize}`, 1.15): `全部下载`, `下载` and dialog titles.
- **Section title** (620, `{typography.section-title.fontSize}`, 1.2): settings page and dialog headings.
- **File name** (580, `{typography.file-name.fontSize}`, 1.35): queue rows and file receipt.
- **Body** (400, `{typography.body.fontSize}`, 1.45): settings descriptions and dialog copy.
- **Metadata** (400, `{typography.meta.fontSize}`, 1.3): status, helper text, table headers and notes.
- **Data** (400, `{typography.data.fontSize}`, tabular): speed, percentage, size, time and paths.
- **Mark** (800, `{typography.mark.fontSize}`): the small `V` inside window chrome only.

**The No Display Type Rule.** Do not introduce a marketing or display typeface. Desktop clarity outranks typographic novelty.

**The Tabular Data Rule.** Any number that changes over time uses tabular numerals.

## Layout

### Main window

- Reference viewport: `1440×900`.
- Native title bar: `34px`.
- Left queue rail: `196px`.
- Right inspector: `260px`.
- Default window: `1180×760`; minimum: `960×560`.
- Main padding: `22px`.
- Main vertical gap: `16px`.
- Speed band: minimum `70px`.
- Queue header: `31px`.
- Task row: minimum `59px`.
- Queue columns: flexible file column, `190px` progress, `82px` size,
  `78px` status, `88px` row actions.
- Queue rows and header share a `12px` column gap and `11px` horizontal padding.

The main area sits between the rail and inspector when a task is selected. Do
not move filters into the main header, stack the inspector below the table, or
turn the speed band into three separate cards.

### Settings

- Window: `700×610` at the reference viewport.
- Left category rail: `176px`.
- Content padding: `22px 25px 16px`.
- Footer: `41px`.
- All settings rows remain in one right-side form. No search, preview pane or second settings column.
- `同时下载` and `每任务连接` use `136px` dropdowns showing five rows before
  vertical scrolling.

### New download

- Window: `560×350`.
- Single-column form. There is no persistent explanation column.
- Outer content padding: `17px 24px 0`.
- Input row height: `38px`; URL field and compact `添加` action share the row.
- Footer: `48px`.
- The URL field is the first interactive element after the heading.
- Empty and loading states keep the same dialog height; the primary action remains disabled until preview succeeds.

### Smaller windows

- Below `1000px` width, collapse the queue rail to a `64px` icon rail.
- The inspector is hidden by default. Selecting a task does not open it; the
  expand-details row action opens the inspector and expands the window to preserve
  the task workspace width and make room for the `260px` inspector.
- Close the inspector with its `X`; an automatically expanded window returns to
  its previous width.
- The task list remains the primary scroll area.
- Do not collapse the speed band or remove table columns.
- At the project minimum `960×560`, the center queue must remain untruncated;
  further narrowing is blocked by the native minimum size.

**The Three-Region Rule.** Main window structure is rail → task workspace → inspector. Do not replace it with a different navigation model.

**The One Scroll Rule.** The task rows scroll. The title bar, speed band and inspector actions remain fixed.

## Elevation & Depth

The application is flat by default. Hierarchy comes from white/gray tonal separation and 1px rules. Internal panels do not use shadows.

- Floating settings and new-download windows use `0 18px 52px rgba(14,19,24,.16), 0 2px 8px rgba(14,19,24,.08)` over a neutral scrim.
- The primary new-download button may use `0 4px 12px rgba(21,26,30,.12)` in light mode and `0 7px 18px rgba(46,88,185,.22)` in dark mode.
- No shadow is used for queue rows, the speed band or the inspector.

**The Flat Inspector Rule.** Details are separated by rules and background, not by a floating card.

## Shapes

- Window radius: `10px`.
- Floating dialog radius: `10px`.
- Standard controls: `7px`.
- Inputs and steppers: `5px`.
- Main speed band: `8px`.
- Selected task row: `6px`.
- File well: `7px`.
- Large file icon well in inspector: `11px`.
- Status pills: fully rounded.
- Progress tracks: fully rounded.

Edges remain quiet. Do not mix sharp industrial controls and heavily rounded consumer controls in the same screen.

## Components

### Window Title Bar

- Height `34px`.
- Background: paper-subtle in light; dark-titlebar in dark.
- A `17px` square mark with the `V`, followed by the window title at `10.5px / 600`.
- Native Windows minimize, maximize and close controls sit at the far right.
- The drag region excludes interactive controls.

### Queue Navigation

- Width `196px`; background paper-subtle in light and dark-rail in dark.
- Brand block at top, followed by a full-width `新建下载` button.
- Filter item height `32px`, radius `7px`, icon `13px`, text `11.5px`.
- Selected item uses selected-surface with a 1px selected-border.
- Count is right-aligned and uses faint text.
- Bottom utilities: `下载目录` and `设置`.

### Page Header

- Left: `全部下载` at page-title size, then `5 个任务 · 1 个正在传输` at meta size.
- Right: filled `添加链接` button with plus icon.
- The sidebar button says `新建下载`; the main-page action says `添加链接`. Preserve this distinction.

### Speed Band

- Height minimum `70px`.
- 1px border and `8px` radius.
- Three zones: total speed at left (`18.7 MB/s`), a sampled line graph in the
  center, connection/remaining values at right.
- The total speed uses tabular data type at `22px / 620`; its unit is `10px`.
- The smooth line and translucent area fill use `current`; no graph is drawn
  when there is no active flow. A small filled endpoint marks the latest sample.

### Task Queue

- Header labels: `文件`, `进度`, `大小`, `状态`.
- File cell: `30px` file well, file name, then one metadata line.
- Progress cell: status text and percentage above one `3px` progress rail. Do not
  add a second segmented connection rail.
- Row actions after `状态`: start/pause, delete, expand details.
- Selected row uses selected-surface, a 1px selected-border and `6px` radius.
- Hover uses paper-hover in light and dark-hover in dark.
- The three row-action icons remain visible; row activation only selects.

### Status

| State | Label | Semantic color |
|---|---|---|
| Active | `下载中` | current |
| Queued | `等待` | faint text |
| Paused | `已暂停` | warning |
| Done | `已完成` | success |
| Failed | `失败` | danger |

Each status has a 6px filled dot followed by the label. Pills appear only in inspector and dialog status contexts.

### Detail Inspector

- Width `260px`, padding `20px 18px`, left border 1px.
- Header label `任务详情` at meta size, close-detail `X` button at right.
- File area: `48px` file icon well, filename at `15px / 610`, active status pill.
- `传输` section: downloaded, speed, connections and remaining time.
- `保存位置` section: path and resumable value.
- Actions pinned to the bottom: `暂停下载`, `打开所在位置`, then `删除`.
- No extra tabs, metadata cards or file preview imagery.

### Task Interaction

- `↑` / `↓` move selection through the current visible queue.
- Left click selects a row without opening its inspector.
- The expand-details icon opens the inspector and follows subsequent selection.
- Right click opens a native menu with `开始`, `暂停`, `删除`, `彻底删除`,
  `复制下载链接`, `打开所在位置`.
- Double-click behavior follows status: active pauses, paused starts, completed
  opens the saved file, failed retries.

### Delete Confirmation

- Width `360px`. The `不再显示` checkbox and all actions share one horizontal row.
- Actions are `取消`, `删除`, and `彻底删除`.
- `彻底删除` uses danger text with an equal-color outline and no filled background.

### Buttons

- Height `32px`; radius `7px`; horizontal padding `10px`.
- Primary light: action-primary background, white text.
- Primary dark: current background, dark-action-primary-text.
- Secondary: white/surface background, 1px strong border, primary text.
- Ghost: transparent background, muted text; hover gains tonal surface.
- Disabled: 40% opacity and no hover; preserve layout dimensions.
- Focus: 2px current outline with 2px offset.

### Inputs and Steppers

- Height `32px`; radius `5px`; 1px strong border.
- Background white/surface; text primary; placeholder muted.
- Focus changes border to current and adds no glow.
- Error keeps the entered value, switches border to danger and places an actionable message below.
- Inputs and their trailing icon buttons use the same `32px` height.
- Numeric dropdown menus show five rows by default and scroll for longer lists.

### Switches

- Size `32×18px`; radius fully rounded.
- Off: strong rule track and surface thumb.
- On: primary action track in light, current track in dark.
- The entire 32×18 area is clickable; accessible label is mandatory.

### Settings Window

- Title bar remains native.
- Left nav: `设置`, then `下载`, `连接`, `代理`, `集成`, `外观`.
- Right content: one page at a time. The `代理` page supports direct, system and
  manual HTTP/HTTPS proxy modes.
- Rows: min height `44px`, label left, control right.
- Footer: green dot + `所有更改自动保存`; right action `打开配置目录`.

### New Download Window

- Dialog is `560×350px`, single column, with the native `34px` title bar and `48px` footer.
- Heading is `新建下载`; subcopy is limited to `粘贴链接，确认后立即开始。`
- URL input and `添加` action sit on one `38px` row. The action opens a native
  `.torrent` file picker; copied links, magnet links and dropped torrents probe
  automatically.
- Empty state shows only `粘贴链接后显示文件名、大小和保存位置。`
- Loading state replaces the empty hint with a small inline spinner and `正在读取文件信息…`.
- Error state keeps the entered URL, marks the field with the danger border,
  shows one inline error line, and places a retry icon at the field's trailing
  edge.
- Ready state shows one compact file row:
  - `29×33px` file icon
  - file name
  - source domain and `支持断点续传`
  - file size
  - `可下载`
- A single destination row follows the file row: folder icon, path, `更改`.
- The success summary has no surrounding card and no repeated metadata labels.
- Footer note is `Enter 开始下载`; actions are `取消` and `立即下载`.
- Do not show a second explanation column, stepper, progress steps, auto-start checkbox, connection suggestion, or Content-Disposition metadata.

### Theme Switching

- Use one semantic token map with `light` and `dark` values.
- Switching theme changes color values only. It must not change geometry, content order, control size or state.
- The settings control uses a three-part segmented control: `系统`, `浅色`, `深色`.
- Respect the OS theme when `系统` is selected.

## Do's and Don'ts

### Do:

- **Do** preserve the exact three-region main window: queue rail, task workspace, inspector.
- **Do** keep the live transfer band between the page header and the task table.
- **Do** keep exactly one progress rail per task row.
- **Do** use white as the primary background in light mode and keep colored area below 15%.
- **Do** use 1px rules and tonal surfaces before reaching for shadows.
- **Do** retain the same component size and placement between light and dark themes.
- **Do** use real Chinese labels and the existing product copy from the design reference.
- **Do** keep all primary actions in their designed locations.
- **Do** test keyboard focus, disabled states, errors, empty queue and minimum window width.

### Don't:

- **Don't** add a new sidebar section, top navigation bar or permanent extra inspector.
- **Don't** replace the live transfer band with three equal KPI cards.
- **Don't** display separate progress-window dialogs for individual tasks.
- **Don't** turn the settings page into a dashboard or add a preview pane.
- **Don't** add steps, tabs, wizards or an extra confirmation to new download.
- **Don't** use gradients, glass, blurred translucent panels or colored glow as decoration.
- **Don't** change the queue columns or move status into a separate actions column.
- **Don't** use icon-only controls without an accessible name and tooltip.
- **Don't** implement both GPUI Kit and Ely theme runtimes in the same executable.
