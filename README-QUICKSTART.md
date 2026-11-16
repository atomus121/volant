# Volant 快速开始

## 安装客户端

1. 解压 `volant-0.6.21-gpui-setup.zip`。
2. 双击 `install.cmd`。安装位置是 `%LOCALAPPDATA%\Programs\Volant`，不需要管理员权限。
3. 从开始菜单或桌面启动 Volant。程序图标会出现在 Windows 通知区域。
4. 关闭窗口时默认留在托盘；需要彻底结束时右键托盘图标，选择“退出 Volant”。

`设置 > 集成` 可以改为“关闭窗口即退出”，也可以打开或关闭“开机启动”。开机启动会静默进入托盘，不主动弹窗。

## 加载浏览器扩展

Chrome、Edge、Brave 等 Chromium 浏览器不允许安装器静默加入未上架商店的扩展，需要手动加载一次：

1. 打开浏览器的扩展管理页，例如 `chrome://extensions` 或 `edge://extensions`。
2. 打开“开发者模式”。
3. 选择“加载已解压的扩展程序”。
4. 选择 `%LOCALAPPDATA%\Programs\Volant\extension`。

扩展 ID 应显示为 `lehgpnldgfjhkebnbblialhkkhcgaohm`。如果不同，说明加载的不是随包提供的目录，原生消息连接会失败。

## 视频组件

视频解析依赖 `yt-dlp`，合并音视频并输出 MP4 依赖 `ffmpeg`。Volant 会优先使用系统 PATH 中已有的版本，也可以在 `设置 > 视频` 中一键下载到自己的应用数据目录。

- `yt-dlp` 负责读取视频页面和画质。
- `ffmpeg` 负责合并分离的视频流与音频流，并完成 MP4 输出和格式转换。
- Node、Deno、QuickJS 或 Bun 用于 YouTube 等需要 JavaScript 的站点；没有运行时不会阻止普通视频站点，但部分站点会缺少格式。

扩展弹窗会显示这三项的状态。任何一项缺少时，点击“在 Volant 中准备”会直接打开对应设置页。

## 下载位置

默认下载到 `%LOCALAPPDATA%\Volant\downloads`。可在 `设置 > 下载` 中修改。视频临时文件、yt-dlp 和 ffmpeg 位于 `%LOCALAPPDATA%\Volant\video`。
