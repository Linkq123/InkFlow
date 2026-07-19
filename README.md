# InkFlow

InkFlow 是一款面向 Windows 的本地优先 Markdown 编辑器：界面克制、写作专注，同时把 Markdown 源码作为唯一事实来源。

当前版本：`0.1.0`（Windows x64 v1）。项目不包含账号、遥测、后台联网、云同步、AI、插件系统或 Git 操作。

## 已实现

- Tauri 2 + Rust + WebView2 桌面外壳，保留 Windows 原生窗口边框、缩放与 Snap。
- Svelte 5 + CodeMirror 6 编辑器，支持实时融合、源码和只读预览三种模式。
- 光标离开后渲染标题、强调、行内代码、任务框、图片、行内/块公式、表格、代码围栏和 Mermaid；点击复杂块回到原始 Markdown。
- 表格悬浮命令支持增加/删除行列；选区工具条、`/` 块命令和统一命令面板。
- 多标签页、文件树、大纲、文档查找、工作区全文搜索、快速打开和字数统计。
- UTF-8、UTF-8 BOM、UTF-16 LE/BE 与常见 Windows 旧编码识别；保留换行符、BOM 和末尾换行。
- 750ms 自动保存、按文档串行保存、同目录临时文件 + 刷盘 + 原子替换、外部修改检测、并排版本比较和恢复中心；保存期间的新输入会继续保持未保存状态并再次落盘。
- 图片粘贴/拖放使用文档级异步占位符，支持内容哈希去重、未命名文档临时资源迁移，以及“另存为”时复制 Markdown、引用式和 HTML 本地图片。
- 自包含 HTML 导出；Windows WebView2 `PrintToPdf` PDF 导出，先生成临时 PDF 再原子替换目标文件。
- 简体中文和英文界面、系统/浅色/深色主题、专注模式、打字机模式、高对比度与减少动态效果适配。
- NSIS 安装配置、Markdown 文件关联、WebView2 Evergreen 引导程序和便携 ZIP 脚本。

Markdown 语法边界见 [docs/markdown-compatibility.md](docs/markdown-compatibility.md)，IPC 契约见 [backend-api.md](backend-api.md)。

## 快捷键

| 快捷键 | 功能 |
|---|---|
| `Ctrl+N` | 新建文档 |
| `Ctrl+O` | 打开 Markdown 文件 |
| `Ctrl+S` | 立即保存 |
| `Ctrl+Shift+S` | 另存为 |
| `Ctrl+P` | 快速打开工作区/最近文件 |
| `Ctrl+Shift+P` | 命令面板 |
| `Ctrl+F` | 文档内查找替换 |
| `Ctrl+Shift+F` | 搜索工作区 |
| `Ctrl+B` / `Ctrl+I` | 加粗 / 斜体 |
| `Ctrl+K` | 插入链接 |
| `F11` | 专注模式 |

## 本地开发

前置条件：Windows 10/11、Node.js 22、pnpm 10、Rust stable、Microsoft C++ Build Tools，以及 WebView2 Evergreen Runtime。

```powershell
pnpm install
pnpm tauri:dev
```

浏览器内只验证前端布局可运行 `pnpm dev`；文件、导出、回收站等能力仅在 Tauri 桌面进程中开放。

完整验证：

```powershell
pnpm verify
```

该命令会重新生成 Rust → TypeScript IPC 类型，然后依次执行 Svelte 类型检查、前端测试、Rust 格式检查、Rust 测试和生产构建。

## 构建与发布

生成 Windows x64 NSIS 安装包：

```powershell
pnpm tauri:build
```

生成便携 ZIP：

```powershell
pnpm tauri build --no-bundle
pnpm portable
```

便携包写入项目内的 `release/`。公开发布前仍需使用可信代码签名证书对可执行文件和安装包签名。

生产构建使用 ES Module Worker，并按内容懒加载 Markdown 渲染、原始 HTML、KaTeX、Mermaid 和代码围栏语言包。发布产物不携带源码映射；首屏编辑代码与大型可选渲染依赖保持在不同分包中。

## 数据位置与安全边界

- 设置和恢复数据位于 `%LOCALAPPDATA%\InkFlow\InkFlow\data`（具体根目录由 Windows Known Folder API 决定）。
- 恢复记录按内容哈希去重；历史版本每分钟最多一个，每文档最多 50 个、最长 30 天，全局上限 500MB。
- 工作区不会写入 `.inkflow` 元数据，也不会跟随符号链接或目录联接。
- 删除工作区项目会先确认，再进入 Windows 回收站。
- 前端没有任意文件系统权限；所有路径都由 Rust 命令验证。
- 远程图片默认不加载；原始 HTML、导出内容和 Mermaid 均使用安全模式处理。

## v1 边界

首版只发布 Windows x64。ARM64、自动更新、跨平台构建、账号/云同步、多人协作、知识图谱、插件、AI、Git 集成、DOCX 导出和跨文件批量替换不在本版本范围内。

性能验收需在 4 核 x64、8GB 内存、SSD 与当前 WebView2 Evergreen 的基准机上执行；构建和单元测试通过并不等同于已取得 p95 启动、输入、滚动和内存数据。
