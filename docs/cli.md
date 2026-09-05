# InkFlow CLI v1

`inkflow-cli.exe` 为 Agent、脚本和终端提供与 InkFlow 桌面端共用的文档、编码、安全写入、搜索、资源、恢复、设置、会话及导出能力。公共契约版本为 `inkflow.cli/v1`。

CLI 不控制已打开窗口。`app open` 只启动 InkFlow 或把路径交给已有单实例；专注模式、主题等状态通过 `settings` 或 `session` 管理。

## 安装与发现

NSIS 安装包和便携 ZIP 都同时包含：

- `InkFlow.exe`：桌面应用和隐藏 WebView2 渲染进程。
- `inkflow-cli.exe`：普通无头命令入口。

安装程序结束时会询问是否把安装目录加入当前用户 `PATH`，默认选择“否”；静默安装不会修改 `PATH`。由 InkFlow 添加且唯一可辨认的路径项会在卸载时单独移除，同时保留原 PATH 是否存在、原值类型及尾部分隔符状态。如果安装后又出现同值重复项，卸载程序无法可靠区分所有权，因此会保留全部重复项并在非静默卸载时提示手动处理。不加入时可使用完整路径调用。便携版中两个程序必须保持在同一目录，否则 HTML/PDF、KaTeX 和 Mermaid 渲染不可用。

首次接入应先自发现：

```powershell
inkflow-cli capabilities --format json
inkflow-cli schema --format json
inkflow-cli --help
inkflow-cli document --help
```

生成并提交的 Schema 位于 [inkflow-cli.schema.json](inkflow-cli.schema.json)。

## 全局参数与输出

```text
--format auto|text|json|jsonl
--data-dir ABSOLUTE_PATH
--root PATH
```

- `--format auto` 在真实终端输出易读文本，被管道或子进程捕获时输出 JSON。
- `--data-dir` 覆盖共享数据目录；也可设置 `INKFLOW_DATA_DIR`。命令行参数优先，路径必须为绝对路径。目录创建后会先规范化，因此后续 JSON 路径不会保留 `..`、`.` 或 Windows 扩展路径前缀。`capabilities` 与 `schema` 是无状态自发现命令，不会创建或校验数据目录，也不会访问 `--root`。
- 默认数据目录是 `%LOCALAPPDATA%\InkFlow\InkFlow\data`。测试应使用一次性 `--data-dir`，避免修改用户的设置、会话或恢复记录。
- `--root` 把文档、工作区、资源以及设置/会话更新内嵌的路径限制在同一根目录。它与 `workspace` 子命令必填的工作区位置参数相互独立；位置参数不能覆盖安全根。越界路径、带 `SYMLINK` / `MOUNT_POINT` 标签的符号链接和目录联接会被拒绝；由文档名派生的 `<文档名>.assets` 目录也执行同样检查，不能借助目录联接绕过边界。Windows 上的变更命令还会记录目标父目录身份，在最终检查后持有禁止重命名/替换的目录句柄直到文件或工作区操作完成；目录在验证与提交之间被同名目录或联接替换时返回 `path_changed`。OneDrive 等其他 Cloud Files 重解析点不会仅因 `REPARSE_POINT` 属性位相同而被拒绝。
- JSON 返回的文件路径统一为不带 Windows `\\?\` 前缀的规范化绝对路径。带 `--root` 读取设置或会话时，根目录外或已不可访问的配置路径会被省略并写入 `warnings`，不会泄露其他工作区路径；受限会话更新只替换根目录内状态，磁盘上被过滤的其他标签、工作区和活动路径会保留。

JSON 成功结果：

```json
{
  "apiVersion": "inkflow.cli/v1",
  "ok": true,
  "command": "document.analyze",
  "data": {},
  "warnings": []
}
```

JSON 失败结果：

```json
{
  "apiVersion": "inkflow.cli/v1",
  "ok": false,
  "command": "document.edit",
  "error": {
    "code": "revision_conflict",
    "message": "The document revision has changed."
  }
}
```

公共 CLI JSON 中的时间统一为 UTC RFC 3339。文档和会话修订使用 `modifiedAt`（例如 `2026-08-08T12:34:56.789Z`）、`size` 与 `hash`；桌面 IPC 内部使用的毫秒字段不属于 CLI v1 契约。

`jsonl` 搜索逐行输出 `type: "item"`，结束时输出一条 `type: "summary"`。工作区搜索会直接开始流式扫描，不先构造文件树快照；单个文件先确认编码，随后每次命中立即输出，不等待该文件剩余行或整个工作区。stdout 只包含命令结果；日志、文本模式警告和诊断进入 stderr。下游主动关闭管道时 CLI 会安静结束，不输出 Rust panic 或把正常的管道截断误报成操作失败。所有命令都响应 `Ctrl+C`：stdin、搜索和 renderer 等协作式任务会立即开始清理；只读任务在 500ms 清理窗口后仍未结束时返回退出码 `3`，JSON/JSONL 模式同时输出 `cancelled` 错误包络。可能写盘、启动进程或持有事务回滚守卫的命令不会强制杀死工作线程，而会等待当前安全操作结束并返回其真实成功或失败结果，确保原子写入和资源回滚析构器得到执行。调用方应丢弃被中断 JSONL 命令在错误包络前可能已经输出的项目。

| 退出码 | 含义 |
|---:|---|
| `0` | 成功 |
| `2` | 参数、JSON、Schema、无效文本位置或不支持的编码/BOM 组合错误 |
| `3` | 文件、路径、渲染等操作失败，或命令被 `Ctrl+C` 中断 |
| `4` | 修订、`expectedText` 或条件写入冲突 |
| `5` | 缺少 `--yes`、期望哈希或 `--force` |
| `6` | 部分成功，例如资源已写入但链接插入失败 |

## 文档

```powershell
inkflow-cli document read .\note.md --format json
inkflow-cli document analyze .\note.md --format json
inkflow-cli document search .\note.md "InkFlow" --format jsonl
inkflow-cli document replace .\note.md "old" "new" --all --dry-run
inkflow-cli document edit .\note.md --request .\edit.json --dry-run
inkflow-cli document write .\new.md --input .\content.txt --create
inkflow-cli document save-as .\note.md .\copy.md
```

`read` 返回 LF 规范化正文、编码、EOL、BOM、末尾换行、只读状态和 `DiskRevision`。`analyze` 返回词数、字符数、行数、大纲和远程图片检测结果；HTML `srcset` 中的每个候选地址也参与检测。文本位置从 1 开始，列按 Unicode 字符而非 UTF-8 字节计数。

单次 `document edit` 请求最多包含 256 个操作，超过限制返回 `too_many_operations`，Agent 可按顺序拆分请求并在每批之间传递最新修订。`format` 属于行内 Markdown 操作，范围必须位于同一行且选中文字应处于段落或标题内；跨行、跨块或代码块内格式化会返回 `invalid_range`。加粗、斜体和删除线会保留选区中的既有 Markdown，必要时改用等价分隔符或转义冲突分隔符；选区首尾空白保留在格式标记外，空白选区或无法安全表示的选区返回 `invalid_range`。格式校验只解析选区所属的 Markdown 块，单块上限为 512 KiB；超过限制返回 `format_context_too_large`，Agent 应缩小段落或改用精确范围替换。这样既不会生成无法按预期解析的行内格式标记，也不会因大文档或超大批次反复解析全文。

`document search` 和 `document replace` 的普通字面量查询不能为空；只有显式 `--regex` 才允许零宽匹配。`document replace --all` 默认最多替换 500 处，可用 `--max-replacements` 在 1–100000 之间调整。仍有匹配项但达到上限时，已经计算或提交的前 N 处修改会返回 `truncated: true`、警告和退出码 `6`，Agent 不应把它当作完整替换。

编辑请求按顺序在内存中执行；任一操作失败时不会写盘，全部成功后仅提交一次：

```json
{
  "schemaVersion": 1,
  "expectedRevision": {
    "modifiedAt": "2026-08-08T12:34:56.789Z",
    "size": 0,
    "hash": "从 document read 取得"
  },
  "operations": [
    {
      "type": "replace",
      "range": {
        "start": { "line": 2, "column": 1 },
        "end": { "line": 2, "column": 8 }
      },
      "expectedText": "旧文字",
      "text": "新文字"
    },
    {
      "type": "format",
      "range": {
        "start": { "line": 3, "column": 1 },
        "end": { "line": 3, "column": 8 }
      },
      "expectedText": "InkFlow",
      "format": "bold",
      "url": null
    },
    { "type": "toggleTask", "line": 5, "checked": true },
    { "type": "table", "line": 8, "action": "addColumn" }
  ]
}
```

可用格式为 `bold`、`italic`、`strike`、`code`、`link`；块操作为 `heading1`、`heading2`、`heading3`、`bulletList`、`task`、`quote`、`codeBlock`、`mathBlock`；表格操作为 `addRow`、`removeRow`、`addColumn`、`removeColumn`。`code` 和 `codeBlock` 会自动选择长于正文中连续反引号的围栏，`link` 会转义标签边界、保护字面 `&name;` 不被当成字符引用，并使用尖括号目标，因此正文或 URL 内的 Markdown 分隔符不会提前结束或改变新结构。`toggleTask` 只修改 Markdown 解析器确认的任务列表标记；围栏代码和四空格缩进代码中的 `- [ ]` 示例会返回 `not_a_task`，不会被改写。
编辑请求顶层、`expectedRevision` 和每个 `operations` 项均拒绝未知字段；例如把 `expectedRevision` 拼错不会被静默忽略。

安全写入规则：

- `replace` 与 `edit` 在读取后使用条件原子写入；外部修改返回退出码 `4`。真实的 `replace`、`edit`、`write`、`save-as`、片段写出和 HTML/PDF 最终提交与 InkFlow 工作区创建、重命名及回收站移动共享跨进程路径锁，锁会保持到最终修订读取或原子提交完成，因而不会在提交后被路径移动制造虚假失败，也不会由强制写入重新创建刚被移动的旧文件名。带 `--root` 的 Windows 提交同时复核并锁定实际父目录身份，外部目录替换不能把写入重定向到根目录外。耗时的 Markdown 渲染和 PDF 打印发生在锁外，不会长期阻塞工作区操作。
- `write` 新建文件必须传 `--create`。覆盖文件必须传 `--expected-hash`，只有明确的 `--force` 才跳过修订保护。输入先规范化为 LF 再计算内容哈希、变更范围和差异；随后按目标 EOL 编码。最终字节未变化时不会重写文件或产生历史检查点，但真实命令仍会在返回成功前复核完整磁盘修订；复核期间发生的外部修改会返回冲突，`--force` 则按请求内容完成提交。
- `save-as` 覆盖目标时必须传 `--expected-destination-hash` 或 `--force`；传入期望哈希但目标已不存在同样属于修订冲突，不会把它当作新文件创建。覆盖前会为旧目标创建历史检查点。Markdown 图片、引用式图片、HTML `img/src` 以及 `source/srcset` / `img/srcset` 的每个本地候选都会按 CommonMark/HTML 语义解码字符引用后复制并改写，密度与宽度描述符保持不变；改写后的 URL 会把文件名中的字面 `%`、`&` 分别编码为 `%25`、`%26`，写回原始 HTML 时还会按属性原有的引号上下文编码可能破坏边界的空白或定界字符。共享同一 `<文档名>.assets` 命名空间的桌面端、CLI 另存为和资源写入由工作区外的跨进程锁串行化，资源复制只有在文档原子提交成功后才确认；保存失败会回滚本次创建且尚未被改动的资源。
- 所有文档变更支持 `--dry-run`；文本模式输出局部差异，JSON 返回旧修订、变更范围和预期新内容哈希。资源链接改写也参与另存为预演，但预演不会复制图片；同名但内容不同的图片会在内存中预留各自目标名，因此预演的链接和 `contentHash` 与随后的真实提交一致。若另存为后的目标字节相同且没有资源需要复制，`changed` 为 `false`，真实执行不会重写目标或创建历史；若 Markdown 不变但需要补齐引用资源，`changed` 仍为 `true`，且只提交资源复制。
- 保存恢复原编码、CRLF/LF、BOM 和末尾换行状态。新建文件或显式切换编码时，UTF-16LE/BE 默认写入 BOM，UTF-8 和旧编码默认不写；显式指定相同编码会保留现有 BOM。`--bom` 只允许用于 UTF-8、UTF-16LE 和 UTF-16BE；UTF-16LE/BE 拒绝 `--no-bom`，因为 InkFlow 依靠 BOM 无歧义识别字节序。未知 Markdown 不会被自动删除或格式化。

## 工作区

```powershell
inkflow-cli workspace tree C:\Notes --format jsonl
inkflow-cli workspace search C:\Notes "告警" --format jsonl
inkflow-cli workspace create C:\Notes . draft.md
inkflow-cli workspace create C:\Notes . images --directory
inkflow-cli workspace rename C:\Notes draft.md final.md
inkflow-cli workspace trash C:\Notes final.md --yes
```

工作区扫描遵循 `.gitignore`，跳过 `.git`、`node_modules`、`target`、`.idea`、`.vscode` 和超过 20MB 的文件，不跟随链接或联接。变更命令始终需要显式工作区根；`trash` 进入 Windows 回收站且必须传 `--yes`。三种变更均支持 `--dry-run`。

## 图片资源

```powershell
inkflow-cli asset add --document C:\Notes\note.md --source .\diagram.png
inkflow-cli asset add --document C:\Notes\note.md --source .\diagram.png --line 4 --column 1 --alt 架构图
cmd /d /c "inkflow-cli asset add --document-id draft-123 --stdin --mime-type image/png < clipboard.png"
inkflow-cli asset read --document C:\Notes\note.md "note.assets/diagram.png"
```

源文件保留受支持的原图片格式；stdin 使用 `--mime-type`。输入在读取/解码前后受 50MB 硬限制，新资源使用完整内容哈希稳定命名，内容相同的资源复用现有文件，因此 `--dry-run` 报告的资源路径和随后真实提交一致。已保存文档写入 `<文档名>.assets`，无 `--root` 时可用 `--document-id` 写入恢复区等待首次保存迁移；启用 `--root` 后资源必须通过根目录内的 `--document` 归属，未绑定文档的恢复区资源会以 `path_outside_workspace` 拒绝。插入文档的图片目标使用 CommonMark 尖括号形式，因此文档名包含空格或括号时仍会解析为图片。`asset read --document` 必须指向现有普通文件，目录不能充当文档作用域或扩大 `--root` 边界。`asset add --dry-run` 仍会完整读取并验证文档、源文件/stdin、MIME、大小、根目录和可选插入位置，但不会创建资源目录或修改文档；JSON 的 `asset` 和 `documentMutation` 给出计划结果。图片 Alt 中的换行与控制字符会转为空格，反斜杠和方括号会转义，不能借由 Alt 注入额外 Markdown 结构。指定 `--line` 时必须同时提供 `--document`，行列会在图片落盘前验证；若验证后发生并发编辑，图片已写入但 Markdown 链接无法安全提交时，结果返回退出码 `6` 和警告，不会覆盖新文本。

## 恢复、设置与会话

```powershell
inkflow-cli recovery list --format jsonl
inkflow-cli recovery checkpoint .\note.md --kind history
inkflow-cli recovery restore SNAPSHOT_ID --output .\restored.md --create
inkflow-cli recovery delete SNAPSHOT_ID --yes

inkflow-cli settings get --format json
inkflow-cli settings patch --input .\settings-patch.json
inkflow-cli settings reset

inkflow-cli session get --format json
inkflow-cli session update --input .\session.json --expected-hash HASH
inkflow-cli session clear --yes
```

设置 patch 只改变 JSON 中出现的字段，并与桌面端并发写入的其他字段合并；磁盘设置文件损坏或不可读时返回错误并保留原文件。`editorFont` 与 `codeFont` 接受普通的 CSS `font-family` 名称列表（含引号和逗号），每项最多 256 个 Unicode 字符，并拒绝控制字符、声明/规则定界符、注释、`url()` 与 `@import`，防止设置值越出字体声明或触发资源请求；已有文件包含此类值时使用安全默认值且不改写原文件。删除不存在的恢复 ID 返回 `recovery_not_found`，不会报告虚假的成功。`session update` 仅接受 `schemaVersion: 1`，会话顶层和标签项均拒绝未知字段，避免 Agent 拼写错误被静默保存或把未来协议降级。无 `--root` 时会话更新覆盖完整 `SessionV1`；带 `--root` 时只替换根目录内可见标签及对应活动/工作区字段，不会删除磁盘上被过滤的其他工作区状态。若保留的根目录外标签占用 50 个标签的会话容量，导致请求的根内标签无法全部保存，命令会明确警告丢弃数量并返回部分成功退出码 `6`。`session clear --yes` 遵循同一规则：带 `--root` 时只清除根目录内状态，并在结果中省略、在磁盘上保留其他工作区状态。已有 `session.json` 时必须提供同一次锁定读取的 `session get` 返回的期望哈希或明确使用 `--force`。桌面端运行期间，关闭窗口时保存的较新会话仍可能覆盖 CLI 更新，命令会返回相应警告。

设置、会话和恢复目录使用跨进程文件锁及原子写入。CLI `recovery checkpoint` 与桌面自动检查点共用 32 MiB 正文上限；压缩记录和受限解压 JSON 各自最大 33 MiB，zstd 解码窗口最大 32 MiB，超限返回 `recovery_too_large`；流式编码器在转义后的 JSON 达到 33 MiB 时立即中止，不会先序列化和压缩完整膨胀内容。恢复目录维护 v2 文件指纹和条目元数据索引；稳定状态下限频、列表与配额清理不会逐个解压历史快照，索引缺失、损坏、不完整或不一致时才用有界 reader 从压缩记录重建。有效历史的压缩总量和实际解压总量分别最多 500 MiB；无法读取、超限或校验失败的记录会移入最长保留 30 天的 `Quarantine`，并在磁盘超限时先于正常历史清理。`Quarantine` 必须是 Recovery 规范目录下的真实直属子目录；清理在目录身份保护下只处理 InkFlow 隔离命名的普通文件，不跟随符号链接或目录联接。多个短生命周期 CLI 进程共同遵守草稿 2 秒、历史 1 分钟的限频。未命名文档的待迁移图片与恢复记录共用恢复锁；保存只迁移当前 Markdown 已引用的完成资源，并在同一锁周期内完成迁移、文档提交和对应临时文件清理。仍在上传或未被当前版本引用的文件会留给后续保存，没有待迁移引用的普通保存不会争用恢复锁。显式传入空的 `recentFiles` 或 `recentWorkspaces` 会真正清空对应列表，`settings reset` 会恢复完整默认值。带 `--root` 时，设置/会话结果会过滤越界配置路径；恢复列表会省略根目录外和无法归属的未保存快照，恢复与删除也拒绝访问这些记录。若快照原路径仍按规范化路径归属于根目录，即使文档的直接父目录已经被删除，快照仍可列出、恢复和删除；范围检查会从最近存在的祖先继续验证且不会因此允许越界或跟随目录联接。

## 渲染与导出

```powershell
inkflow-cli render fragment .\note.md --output .\fragment.html
inkflow-cli export html .\note.md --output .\note.html
inkflow-cli export pdf .\note.md --output .\note.pdf --page-size A4
```

片段、HTML 和 PDF 按需启动隐藏、无焦点、无任务栏的 WebView2 进程；普通命令不加载 Tauri 或 WebView2。桌面预览与 CLI 共用 Unified/Remark/Rehype、KaTeX、Mermaid strict、安全净化和本地图片内嵌管线；响应式图片的 `source/srcset` 与 `img/srcset` 会逐候选内嵌并保留密度/宽度描述符，Mermaid 元数据中的本地 `img` 和序列图所有非 `@` 的 `icon` 必须先由受限资源加载器转换为 `data:` URL，加载失败时保留源码块且不会把原始 URI 交给 Mermaid；流程图等使用的 Iconify `prefix:name` 与 `@` 开头的内嵌引用保持原语义。Mermaid 生成 SVG 中的本地 `href` / `xlink:href` 图片也会经过同一加载器内嵌，无法读取的 SVG 图片引用会移除。Mermaid 图标不会触发在线图标包下载；当前离线支持 `inkflow:document` 与 `logos:github-icon`。PDF 打印前会在有界等待内确认动态插入图片已经解码；损坏或长期无响应的远程图片不会无限阻塞导出。 Mermaid 的共享状态位于可销毁的同源 iframe 独立 realm；单次渲染超过 30 秒会销毁挂起 realm 并让下一张图使用新实例，不会永久阻断当前隐藏 renderer 中的后续图表。

- 远程图片默认阻止；只有显式 `--allow-remote-images` 才允许加载。
- `render fragment --document-path` 必须指向现有普通文件；目录不能用于扩大本地资源作用域。
- 片段/HTML 超时为 30 秒，PDF 超时为 75 秒；内部 PDF 打印等待上限为 60 秒。
- 覆盖片段输出需要 `--force`。HTML/PDF 覆盖目标需要 `--expected-output-hash` 或 `--force`，并在最终提交时再次检查修订。`--force` 仍可覆盖原路径上已变化的内容，但若最初存在的输出已被移动或删除则返回退出码 `4`，不会在旧路径重新创建文件。
- 隐藏子进程永远不会获得最终导出路径，只能在随机私有请求目录内生成完整 HTML/PDF 临时产物并原子写入响应。`Ctrl+C` 或 30/75 秒超时因此可在任意渲染阶段强制结束子进程，不会卡在提交屏障或留下半写入的用户文件。只有父 CLI 读到完整响应后，才会重新检查目标修订并在短时路径锁内原子提交；取消发生在此之前时目标保持不变。正常完成、冲突、取消、超时和子进程失败都会清理私有目录，中间 PDF 不会出现在目标文档旁。CLI 或系统异常终止后，下一次渲染会清理超过 24 小时、名称严格匹配 InkFlow UUID 规则且不是符号链接或目录联接的遗留私有目录；近期目录和相似命名目录不会被触碰。

## 启动桌面端

```powershell
inkflow-cli app open
inkflow-cli app open .\note.md
inkflow-cli app open --workspace C:\Notes
```

若 InkFlow 已运行，现有单实例机制接收路径；`--workspace` 会走独立的工作区打开通道，其他位置参数只接受普通文件。CLI 启动桌面进程时分离 stdin、stdout 和 stderr，因此即使 Agent 捕获 CLI 输出，也会在返回启动结果后立即收到 EOF，不等待窗口关闭。CLI 不远程点击或修改当前编辑器状态。

## Agent 使用建议

1. 固定传 `--format json`；大结果搜索使用 `jsonl`。
2. 先 `document read`，把返回修订或哈希带入后续变更。
3. 先运行 `--dry-run`，核对 `changedRanges`、`contentHash` 和差异，再执行真实写入。
4. 给受限任务传 `--root`，给测试传隔离的 `--data-dir`。
5. 只有用户明确授权时使用 `--force`、`--yes` 或 `--allow-remote-images`。
6. 以退出码和 `error.code` 做控制流，不解析本地化的人类文本。

InkFlow CLI v1 不提供跨文件批量替换、窗口远程控制、联网 Agent 服务或独立于 `InkFlow.exe` 的渲染发行包。
