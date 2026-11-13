# 航道 · Current 精修稿

这轮保留 `航道 · Current` 的核心：**实时速度带、当前文件、8 条连接的状态和单一主任务路径**。没有把它改成通用仪表盘，而是在同一语言下提供三种控制密度，并为每种补齐白色主底的浅色主题。

共 3 个变体，每个变体包含主窗口、设置窗口、新建下载窗口和浅/暗两套主题。

## 打开

- [交互总览](index.html)：切换变体、窗口和主题。
- [上一版 Current 基线](baseline-current.png)
- [主窗口双主题总览](main-overview.png)
- [设置窗口双主题总览](settings-overview.png)
- [新建下载双主题总览](add-overview.png)
- 单屏原图在 [`screenshots/`](screenshots/)

URL 参数：

```text
index.html?variant=kit&screen=main&theme=light
index.html?variant=air&screen=settings&theme=dark
index.html?variant=grid&screen=add&theme=light&export=1
```

## 三个变体

| 变体 | 结构 | 适合 | 组件路线 |
|---|---|---|---|
| **A · Current Kit** | 左侧队列、中央速度带与任务列表、右侧详情 | 默认方案，最接近现有产品 | GPUI Kit |
| **B · Current Air** | 顶部导航、无侧栏、宽度更大的速度带和扁平队列 | 喜欢更轻、更安静的界面 | Ely 视觉与 API |
| **C · Current Grid** | 图标窄轨、任务表、连接检查器 | 任务较多、需要快速扫视 | GPUI Kit + Ely 可替换组件 |

推荐 **A · Current Kit** 进入实际实现。它保留现有 `app.rs` 里的信息结构和 GPUI Kit 依赖，改造范围最小，也最不会破坏已有焦点、AccessKit 和窗口行为。

## 组件映射

### GPUI Kit 0.9.1

以下均来自当前锁定的 `gpui-component 0.9.1`：

- `TitleBar`：原生标题栏、窗口按钮和拖动区域。
- `Sidebar`、`SidebarMenu`、`SidebarHeader`、`SidebarFooter`：A 的队列导航。
- `Button`、`ButtonGroup`：新建下载、暂停、继续、打开目录。
- `Input`、`Switch`、`Select`、`Stepper`：设置和新建下载表单。
- `Progress`：真实任务进度；连接微轨由已有 GPUI 元素组合绘制。
- `DescriptionList`：右侧任务详情、保存位置和传输属性。
- `List` / `ListDelegate`：任务量增长后的队列实现。
- `Table`：C 的精修任务表；当前任务量不大，无需直接上 `DataTable`。
- `Tooltip`、`Dialog`、`Notification`：错误、重名、目录不可写和删除确认。
- `ResizablePanelGroup`：未来需要可拖动详情栏时使用。
- `ThemeColor` / `ThemeMode`：全部颜色来自语义 token，不在组件内写死。

### Ely GPUI Components

当前 Ely 源码提供了以下对应组件：

- `AppShell`、`Page`：B 的整体窗口结构。
- `Sidebar`、`NavigationMenu`、`NavItem`：筛选导航。
- `Button`、`IconButton`、`SegmentedControl`：操作和主题切换。
- `List`、`ListItem`、`SelectableList`：下载队列。
- `ProgressBar` 或 `UsageBar`：主进度和连接占用。
- `Input`、`PathInput`、`Switch`、`Stepper`、`Select`：设置与新建下载。
- `SettingsLayout`、`SettingsSection`、`SettingsRow`：设置窗口结构。
- `DescriptionList`、`PropertyGrid`：详情与保存路径。
- `ConnectionStatus`：浏览器捕获、网络和可续传状态。
- `Dialog`、`Fieldset`：新建下载和分组设置。
- `Theme`：浅色、深色、高对比和色彩无障碍模式。

Ely 的上游 README 标明当前主要验证 macOS；本项目当前是 Windows + GPUI Kit。因此不建议为了 B 直接并列引入第二套主题运行时。若采用 Ely，应单独建立分支并把 Ely 的 Windows 适配纳入迁移验证。

## 主题

### 浅色

- 主底：`#FFFFFF`
- 次表面：`#F3F5F7`
- 主文字：`#111417`
- 次文字：`#59626B`
- 分隔线：`#E1E4E7`
- 主按钮：`#14191D`
- 进行中：`#2F6BFF`
- 成功：`#16765A`
- 警告：`#94621C`
- 失败：`#B3423D`

### 暗色

- 主底：`#0E1319`
- 次表面：`#17212C`
- 主文字：`#EDF2F6`
- 次文字：`#A5AFB9`
- 分隔线：`#26313D`
- 主按钮：`#EDF2F6`
- 进行中：`#7DA3FF`
- 成功：`#66C9A5`
- 警告：`#E1B56D`
- 失败：`#FF8A84`

语义色只占小面积：进度线、能力柱、状态点与错误提示。两种主题的控件尺寸、选中规则和状态文案完全一致。

## 交互基线

1. 新建下载：粘贴链接 → 探测文件信息 → 确认保存位置 → 加入队列。
2. 当前任务：进度、速度、连接和剩余时间同时更新。
3. 暂停/继续：状态、连接微轨和主按钮文案同步变化。
4. 设置：即时应用，无额外保存步骤。
5. 错误：保留链接和输入位置，并说明可执行的恢复方式。

## 检查结果

- 18 个画面均在 1440×900 下实际渲染。
- A 主窗口浅色与 C 设置暗色额外检查了 2 倍像素密度。
- 已检查主窗口、设置和新建下载在浅/暗主题下的文字层级、输入宽度与按钮位置。
- 本轮没有连接真实 `Cmd` / `UiEvent`，未声称交互流程已经通过。
- 没有写 `DESIGN.md`，因为仍在三个变体之间选择；选定后再固化 token 与组件映射。
