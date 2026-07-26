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

### `close_document`

- 参数：`{ documentId: string }`。
- 返回：`void`。
- 标签页关闭或工作区文件移入回收站后，移除后端保存与外部变更跟踪状态；恢复记录不会因此删除。关闭标签时前端会先将其移出可编辑界面并清理定时器，再等待既有保存和后端状态清理。删除工作区文件时，从用户确认后到保存、回收站移动和 `close_document` 全部完成前，相关编辑器保持只读，再统一移除标签。两条路径都不会在 IPC 等待窗口接收随后被丢弃的新输入。较早启动、稍后完成的重载会校验原基线，不能把已关闭文档重新登记回来。

### `save_document` / `save_document_as`

- 参数：`{ request: SaveDocumentRequest }`。
- 返回：`SaveOutcome`。

```ts
type SaveOutcome =
  | { status: "saved"; path: string; revision: DiskRevision; content: string | null }
  | { status: "conflict"; path: string; diskRevision: DiskRevision | null }
  | { status: "needsPath" };
```

保存前会比较 `expectedRevision`。Windows 条件保存通过 [`ReplaceFileW`](https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-replacefilew) 在原子替换发生的同一操作中保留当时的目标版本，再把该备份与 `expectedRevision` 核验；若外部修改恰好发生在最后一次预检查与提交之间，会把被替换版本恢复到原路径并返回冲突，而不是把它静默覆盖。“另存为”会在后端操作开始时记录目标的具体修订或“不存在”状态；即使用户已在文件对话框确认覆盖，随后发生的目标修改或创建仍返回冲突。恢复期间若目标又被外部更新，会继续以最新被移走版本为恢复候选；极端持续竞争或系统级回滚失败时保留临时版本路径并返回明确错误。核验成功且已无恢复用途的备份会先改名为带 UUID 的 `inkflow-cleanup` 文件；即时删除受杀毒软件、同步客户端或索引器阻塞时进入单一后台清理队列，每 5 秒重试。原子写入同级文件名只包含 16 位小写十六进制目标文件名哈希、操作类型和 UUID，避免有效的长目标文件名因内部后缀超过 NTFS 255 个 UTF-16 代码单元限制；首次保存、另存为和已有文件替换均使用该有界临时名称。目录清理也必须完整匹配这一格式，不会仅凭 `inkflow-*` 片段删除普通文件。进程崩溃遗留的 `inkflow-write` 文件超过一小时后才具备清理资格；为避免大型目录中的自动保存反复枚举所有同级文件，每个父目录最多每小时统一扫描一次，并在同一次枚举中识别所有目标的 `inkflow-cleanup` 与过期 `inkflow-write` 文件。扫描登记按目录保存，并在超过一小时后惰性淘汰，不会永久积累每个文档路径。已打开的目标文件若被外部删除，也返回 `conflict`，此时 `diskRevision` 为 `null`。后端对保存临界区加锁，前端再按文档串行发送请求，并在响应时核对请求内容，避免旧响应把新输入错误标为已保存。正常写入使用目标目录中的临时文件、刷盘和原子替换，失败最多重试三次；不降级为直接截断覆盖。`content` 仅在未命名资源迁移或“另存为”重写图片相对路径时返回新文本。前端按后端结果中每个实际发生变化的图片位置重建编辑历史，不会因代码块里出现同名示例而清空撤销栈；上传失败时删除占位符也使用同一历史重放机制。“另存为”可复制原文档目录内的图片；原文档属于当前工作区时，也可复制工作区内的相对图片（包括 `../images` 一类引用）。复制资源不要求原 Markdown 文件仍然存在；若其父目录也已消失，则跳过资源复制但仍保存当前编辑缓冲区。待迁移图片只会在文档写入并登记成功后从恢复区清理。

### `check_external_changes`

- 参数：无。
- 返回：`ExternalChange[]`。
- 先比较路径存在性、大小与修改时间；元数据未变化时，每个文档最多每 60 秒做一次完整内容哈希复核，避免短周期轮询反复读取大文件。前端自动重载前会再次确认标签页内容和修订未变化；轮询期间产生的新输入会保留并进入冲突流程。

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
- 只读取恢复区对应文档的临时资源，或文档目录内、最大 50MB 的受支持本地图片；仅当文档自身属于当前工作区时才扩展到工作区范围。渲染器生成的 URL 路径会先执行一次 UTF-8 百分号解码，因此 `<My Note.assets/image.png>` 输出的 `My%20Note.assets/image.png` 仍能映射到真实文件；InkFlow 生成 Markdown 资源链接时会先把文件名中的字面 `%` 编码为 `%25`，保证类似 `100%20done.md` 的合法 Windows 文件名经过一次解码后仍保持字面 `%20`。对旧版已经生成的未转义字面 `%xx` 资源路径，解码目标不存在时会回退查找原字面路径。解码和兼容候选都要重新校验协议、绝对路径和规范化作用域，拒绝越过允许根目录及远程 URL，包括缺少双斜杠的 `http:` / `https:` 变体。

## 恢复

### `checkpoint_document`

- 参数：`{ request: CheckpointRequest }`。
- 返回：`RecoveryEntry | null`。
- `draft` 最短间隔 2 秒，`history` 最短间隔 60 秒；相同内容不重复写入。记录以 zstd 压缩 JSON 保存。

### `list_recovery`

- 参数：无。
- 返回：按时间倒序的 `RecoveryEntry[]`。
- 清理按每文档 50 个、30 天和 500MB 全局上限执行；已成功删除的淘汰记录不再占用全局配额，因文件锁或权限问题暂时无法删除的记录仍按实际大小参与后续配额计算。

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
- 写出包含 UTF-8 元数据、打印样式和已内嵌本地图片的单文件 HTML；文档含公式时再内嵌 KaTeX 样式/字体，不依赖相邻 `fonts/` 目录。目标写入同样使用原子替换。

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
- 前端先把设置变更同步应用到内存，再将布局、最近文件、最近工作区和设置对话框产生的快照放入同一串行写入队列；只有队列中最新操作的规范化响应可以回写内存，避免旧响应或基于旧状态生成的快照覆盖新设置。成功打开文件或工作区后立即持久化对应最近记录。
- 首次读取设置失败时使用当前内存值完成 hydration 并继续应用初始化；持久化闸门同时解除，因此后续主题、布局和设置对话框操作仍可正常写入，不需要重启应用。

## 路径与权限原则

- WebView capability 只有核心窗口、显式窗口标题更新、打开/保存对话框和外部 URL 打开权限。
- 前端不接收通用 `readFile`/`writeFile` 能力。
- 工作区变更在 Rust 侧执行规范化和根目录包含检查。
- 不跟随工作区符号链接或目录联接；未知或越界资源返回错误。
- 任何保存冲突、只读、文件锁、磁盘空间或原子替换失败都会保留未保存状态，由恢复记录兜底。
