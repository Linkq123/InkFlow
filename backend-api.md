# InkFlow Backend API

本文记录前端可调用的 Tauri IPC 契约。Rust DTO 位于 `src-tauri/src/model.rs`；执行 `pnpm bindings` 会用 `ts-rs` 重建 `src/lib/api/generated/`。生成文件不得手工修改，UI 侧仅可在 `src/lib/api/types.ts` 中收窄已经由后端验证的字符串联合类型。

所有字段经 IPC 使用 `camelCase`。失败统一序列化为：

```ts
interface ApiError {
  code: string;
  message: string;
}
```

## 文档

### `take_startup_paths`

- 参数：无。
- 返回：`string[]`。
- 读取并清空首进程启动参数，用于 Windows 文件关联。后续实例通过 `app-open-paths` 事件把路径发送给主窗口。

### `open_paths`

- 参数：`{ paths: string[] }`。
- 返回：`DocumentSnapshot[]`。
- 只接受存在的普通文件；读取字节、识别编码/EOL/BOM、计算 BLAKE3 磁盘修订并加入最近文件。

### `reload_document`

- 参数：`{ documentId: string }`。
- 返回：`DocumentSnapshot`。
- 从磁盘重新读取已打开文档并更新后端修订基线。

### `save_document` / `save_document_as`

- 参数：`{ request: SaveDocumentRequest }`。
- 返回：`SaveOutcome`。

```ts
type SaveOutcome =
  | { status: "saved"; path: string; revision: DiskRevision; content: string | null }
  | { status: "conflict"; path: string; diskRevision: DiskRevision }
  | { status: "needsPath" };
```

保存前会比较 `expectedRevision`。不一致时只返回 `conflict`，不会写文件。后端对保存临界区加锁，前端再按文档串行发送请求，并在响应时核对请求内容，避免旧响应把新输入错误标为已保存。正常写入使用目标目录中的临时文件、刷盘和原子替换，失败最多重试三次；不降级为直接截断覆盖。`content` 仅在未命名资源迁移或“另存为”重写图片相对路径时返回新文本。

### `check_external_changes`

- 参数：无。
- 返回：`ExternalChange[]`。
- 比较路径存在性及大小、修改时间、内容哈希。前端对无本地编辑的文档自动重载；有本地编辑时显示冲突条。

## 工作区

### `open_workspace`

- 参数：`{ path: string }`。
- 返回：`WorkspaceSnapshot`。
- 规范化工作区根目录，返回扁平、带 `depth` 的安全文件树。

### `refresh_workspace`

- 参数：无。
- 返回：`WorkspaceSnapshot | null`。

### `search_workspace`

- 参数：`{ request: SearchRequest }`。
- 返回：`SearchHit[]`。
- 搜索限制在当前工作区；默认最多 500 条，硬上限 2000。遵循 `.gitignore`，不跟随链接，跳过 `.git`、`node_modules`、`target`、`.idea`、`.vscode` 和超过 20MB 的文件。
- 未打开工作区时搜索请求会被拒绝；不能通过 IPC 临时指定任意扫描根目录。

### `create_workspace_entry`

- 参数：`{ parent: string; name: string; isDir: boolean }`。
- 返回：更新后的 `WorkspaceSnapshot`。

### `rename_workspace_entry`

- 参数：`{ path: string; newName: string }`。
- 返回：更新后的 `WorkspaceSnapshot`。

### `trash_workspace_entry`

- 参数：`{ path: string }`。
- 返回：更新后的 `WorkspaceSnapshot`。
- 只允许工作区根目录以内的项目，根目录本身不可删除；操作进入 Windows 回收站。

## 图片资源

### `write_asset`

- 参数：`{ request: WriteAssetRequest }`。
- 返回：`WriteAssetResult`。
- 已保存文档写入 `<文档名>.assets`；未命名文档写入恢复区。文件名包含本地时间和 16 位内容哈希前缀，复用前会校验完整内容哈希。图片最大 50MB，Base64 输入在解码前后都会检查大小；临时资源的文档 ID 和文件名必须是单一路径组件。

### `load_resource`

- 参数：`{ documentId: string; resource: string }`。
- 返回：安全的 `data:` URL。
- 只读取恢复区对应文档的临时资源，或文档/当前工作区范围内、最大 50MB 的受支持本地图片；规范化后再次校验作用域，拒绝绝对路径、父目录穿越和远程 URL。

## 恢复

### `checkpoint_document`

- 参数：`{ request: CheckpointRequest }`。
- 返回：`RecoveryEntry | null`。
- `draft` 最短间隔 2 秒，`history` 最短间隔 60 秒；相同内容不重复写入。记录以 zstd 压缩 JSON 保存。

### `list_recovery`

- 参数：无。
- 返回：按时间倒序的 `RecoveryEntry[]`。

### `restore_revision`

- 参数：`{ id: string }`。
- 返回：`RecoverySnapshot`。

### `delete_recovery`

- 参数：`{ id: string }`。
- 返回：`void`。

## 导出

### `export_html`

- 参数：`{ request: ExportRequest }`，`outputPath` 必填。
- 返回：`ExportOutcome`。
- 写出包含 UTF-8 元数据、打印样式和已内嵌本地图片的单文件 HTML，目标写入同样使用原子替换。

### `export_pdf`

- 参数：`{ request: ExportRequest }`，`outputPath` 必填，`pageSize` 为 `A4` 或 `Letter`。
- 返回：`{ action: "saved"; path: string }`。
- Windows 下调用当前 WebView2 的 `PrintToPdf`。前端进入打印布局后最多等待字体 2 秒，避免损坏或不可达字体永久阻塞导出；后端先输出到目标目录内的随机临时 PDF，成功后再原子替换目标，WebView2 调用超时为 60 秒。超时后若 WebView2 仍迟到完成，完成回调会再次清理临时文件。

## 设置

### `get_settings`

- 参数：无。
- 返回：`SettingsV1`。

### `update_settings`

- 参数：`{ settings: SettingsV1 }`。
- 返回：规范化后的 `SettingsV1`。
- 页面宽度限制为 560–1400px，字号 12–32px，行高 1.2–2.4，自动保存延迟 250–10000ms；主题只接受 `system`、`light`、`dark`。

## 路径与权限原则

- WebView capability 只有核心窗口、显式窗口标题更新、打开/保存对话框和外部 URL 打开权限。
- 前端不接收通用 `readFile`/`writeFile` 能力。
- 工作区变更在 Rust 侧执行规范化和根目录包含检查。
- 不跟随工作区符号链接或目录联接；未知或越界资源返回错误。
- 任何保存冲突、只读、文件锁、磁盘空间或原子替换失败都会保留未保存状态，由恢复记录兜底。
