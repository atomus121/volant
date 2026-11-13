# Volant GPUI 界面方向稿

打开 [`index.html`](index.html) 查看可交互设计稿。顶部切换三个方向，窗口栏切换主窗口、设置窗口与新建下载窗口。

真实 GPUI Kit 预览窗口：[gpui-actual-main.png](gpui-actual-main.png)。

## 预览

| 方向 | 主窗口 | 设置窗口 | 新建下载 |
|---|---|---|---|
| A 航道 | [flow-main.png](flow-main.png) | [flow-settings.png](flow-settings.png) | [flow-add.png](flow-add.png) |
| B 账本 | [ledger-main.png](ledger-main.png) | [ledger-settings.png](ledger-settings.png) | [ledger-add.png](ledger-add.png) |
| C 切片 | [slice-main.png](slice-main.png) | [slice-settings.png](slice-settings.png) | [slice-add.png](slice-add.png) |

## 三个方向

| 方向 | 产品主张 | 主要结构 | 最适合 |
|---|---|---|---|
| A 航道 Current | 让下载的流动状态成为第一信息 | 左侧队列、连续传输列表、8 连接微轨、右侧详情 | 日常下载主路径 |
| B 账本 Ledger | 完成后的文件仍然是可被找回的记录 | 顶部工具条、文件记录列表、行内展开、轻量筛选栏 | 低干扰和文件管理 |
| C 切片 Slice | 把分段和并发能力直接呈现出来 | 图标轨、数据表、分片检查器、趋势条 | 进阶设置与高密度任务 |

## 与两个组件库的关系

### GPUI Kit

- 窗口与基础交互直接使用 `TitleBar`、`Root`、`Dialog`、`Input`、`Button`、`Switch`、`Checkbox`、`Progress`。
- 任务数较少时使用普通 GPUI 列表；任务数增长后切换为 `List` / `VirtualList`。
- 方向 C 的将来版本可使用 `DataTable`，但不建议在 v1 的少量任务上过早引入表格交互和列管理。
- 使用语义主题 token，不把设计稿里的十六进制值散落到组件内部。
- 所有按钮、行选择、开关与输入依赖 GPUI Kit 的 focus / AccessKit 行为。

### Ely GPUI Components

- 借其“暖中性底 + 语义色而非装饰色”的判断，主要用于方向 B。
- 采用其 `Theme` 中的语义 token 思路：`bg`、`surface`、`fg`、`fg_muted`、`accent`、`border`、`focus`、`danger`。
- 控制尺寸与状态参考 Ely 的 24 / 28 / 32px 三档高度与 4 / 6 / 8 / 12px 圆角体系。
- 不建议同时接入两套全局主题运行时。两库能力有重叠，生产实现应二选一，或只借用 Ely 的视觉原则并在 GPUI Kit 主题系统内实现。

## 实现建议

1. 以方向 A 作为默认信息架构，保留现有 GPUI Kit 组件和商业应用级交互细节。
2. 从方向 B 吸收更安静的浅色主题、设置页的信息组织和“完成文件可被找回”的产品气质。
3. 从方向 C 只吸收分段可视化与吞吐趋势，放在可展开详情中，不让专业信息成为所有用户的常驻噪声。
4. 新建下载窗口保留现有业务顺序：URL → 探测 → 文件回执 → 加入队列；不要为了视觉节奏增加无意义的步骤。

## 设计稿中的真实产品约束

- 支持 HTTP、HTTPS、magnet 和 `.torrent`。
- 全局并发默认 3，单任务连接默认 8，上限 32。
- 文件可断点续传；取消和退出不能丢失恢复状态。
- 浏览器捕获、剪贴板监视、完成做种和跟随系统主题仍然是低频设置。
- 浅色 / 深色都应跟随同一套语义 token。
