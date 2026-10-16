---
name: compositor-editor
description: 用 Compositor Windows 的本地 CLI 新建或编辑分层图片、按页图片化编辑 PDF、导入素材、离线抠图、保存 .comp 工程并导出图像或图片型 PDF。用户要求由 Agent 操作 Compositor 或编辑 .comp 项目时使用。
---

# Compositor 图像编辑

本技能对应 CompositorWin 0.0.2。PDF 功能自 0.0.2 起提供，旧版 0.0.1 不包含；操作前通过 `cli capabilities` 查询实际程序的能力，不仅凭版本号推断功能。

使用 `compositor.exe cli` 操作本地图片与 `.comp` 工程。离线命令与 `serve` 无需启动窗口或 GPU；`live` 操作用户已经打开的界面文档，包括未保存内容。根据用户目标选择方式，完成编辑并交付工程与所需导出图。

## 连接与发现

先读取本技能末尾安装器记录的程序位置；也可使用用户提供的路径、PATH 中的 `compositor.exe`，或当前工程的发行包。技能安装目录只有说明，不包含程序。找不到时询问软件位置。下方代码的 `executable_path` 使用已定位的绝对路径。

`cli skill install` 安装内嵌技能并记录当前 EXE 位置。重复安装相同内容不会新增备份；内容不同先保存为同目录 `SKILL.md.<uuid>.bak`，响应的 `backup` 字段给出备份位置，保留本地定制。软件移动后重新安装以更新位置。

Windows 发行程序采用 GUI 子系统。用 Python `subprocess` 等待退出并捕获 UTF-8 输出，直接传 argv 列表；路径含空格、中文和 JSON 参数时仍按独立参数传递。

```python
import json
import subprocess
from pathlib import Path

exe = str(Path(executable_path))

def cli(*args):
    run = subprocess.run(
        [exe, "cli", *map(str, args)],
        capture_output=True, encoding="utf-8", errors="strict",
    )
    reply = json.loads(run.stdout)
    if run.returncode != 0 or not reply.get("ok"):
        raise RuntimeError(reply.get("error", run.stderr))
    return reply["result"]

capabilities = cli("capabilities")
```

以 `capabilities`（别名 `schema`）返回的参数清单和示例为准，读取当前操作名、字段、类型、限制和支持格式，再构造操作。先对输入执行 `cli("inspect", input_path)`，读取画布、图层 ID、变换、蒙版和项目指纹。操作使用检查返回的 ID；不要依赖图层显示名称或臆造字段。坐标与界面一致：画布左上为原点，x 向右、y 向下。

创建文字前用 `cli("fonts")["fonts"]` 查询当前字体名称，再设置 `style.fontName`；内容、字号、颜色等字段从 `operations.text_defaults` 发现。导入与打开响应中的 `warnings` 记录格式转换，按实际内容向用户说明。

## 编辑与导出

- 新建：`cli("new", "--width", 1920, "--height", 1080, "--output", project_path)`。
- 编辑：按参数清单构造 JSON 操作数组，写入 UTF-8 文件，再执行 `cli("apply", input_path, "--output", project_path, "--operations", "@" + str(operations_file))`。需要保护读取后版本时附加 `--expected-fingerprint` 与检查得到的指纹。一批操作整体成功才保存；失败后读取错误，修正再执行。
- 导出：`cli("export", project_path, "--output", output_path)`，根据扩展名选择 PNG、JPEG、TIFF、BMP、GIF、WebP 或 PDF。PNG、TIFF、BMP、WebP 可保留透明，JPEG 不保留透明，GIF 为单帧并受调色板限制；PDF 是当前文档的单页合成图片。用 `--max-side` 生成检查用缩略图、用 `--quality` 设置 JPEG 质量。PDF 缩略导出降低像素数，保持文档的物理页面尺寸。

## PDF 图片化编辑

先检查 `capabilities["methods"]` 是否含 `pdf-info`、`pdf-open`，以及 `capabilities["exportFormats"]` 是否含 `pdf`；缺少时说明需要包含 PDF 功能的程序，不对旧版发送这些命令。

先执行 `cli("pdf-info", pdf_path)`，读取 `pageCount` 与各页从 1 起的 `page`、`widthPoints`、`heightPoints`。PDF 的 points 单位为 1/72 英寸。打开时必须明确选择一页和 DPI（整数 72–600），例如：

```python
cli("pdf-open", pdf_path, "--page", 2, "--dpi", 150,
    "--output", page_project_path)
cli("apply", page_project_path, "--operations", "@" + str(operations_file),
    "--output", edited_project_path)
cli("export", edited_project_path, "--output", output_pdf)
```

`pdf-open` 另存所选页为 `.comp`。`inspect`、`apply`、`export` 直接读取 PDF 时同样要求 `--page` 和 `--dpi`。`serve` 的 `open` 或 `pdf-open` 接受 `{"path":"input.pdf","page":2,"dpi":150}`；可先调用 `pdf-info` 获取页数。未指定页码会报错并保留原会话，绝不默认丢弃其余页面。逐页执行可处理多页 PDF；这些 CLI 导出命令每次生成一个文档页面。PDF 页面转图片后的像素尺寸取决于 DPI，较高 DPI 使用更多内存。

页面的可见内容会转为图片，原文字、矢量、链接、表单和数字签名结构不保留；图片型 PDF 导出不会恢复文字搜索、链接、交互表单或原签名。使用现有画笔、图层、蒙版、文字等工具修改像素或叠加内容，并另存 `.comp` 保留新增图层。PDF 导出的透明区域使用白色背景。

需要直接在界面处理多页时，使用 `文件 → 打开…` 的 PDF 导入对话框选择页码与 DPI；界面默认 300 DPI，可选 72、150、300、600，每页打开为独立文档。`Ctrl+S` 将当前页保存为 `.comp` 工程，不写回原 PDF。通过 `文件 → 导出 PDF（可合并多页）…` 勾选已打开画布，用“上移／下移”调整顺序，生成图片型多页 PDF；原 PDF 中未打开的页面不会自动附加到输出。`live` 只控制已经打开的文档，不负责打开 PDF；可在完成界面导入后按下述 guard 规则编辑、导出当前页。普通图片作为图层导入的批处理操作不接受 PDF 页码；PDF 请选择专用导入流程。

新建后按 capabilities 的导入操作添加素材；编辑已有工程先检查再修改。移除背景使用本机离线主体分割操作，保留原始图层像素和可编辑蒙版。无需远程模型、API key 或上传图片。需要接入用户自选视觉模型时，先确认该接口已实现，勿把本地分割说成远程 AI 调用。

常见的导入后抠图操作数组如下；文件路径使用实际素材，省略 `layer` 表示当前活动图层。`subject_mask` 尊重现有选区；处理整张图层前检查选区，按需清除。操作与可选字段仍以该版本 capabilities 为准。

```json
[
  {"op": "import_layers", "paths": ["C:/Pictures/input.png"]},
  {"op": "subject_mask", "replace_existing": true}
]
```

`.comp` 默认保存为单个可编辑工程文件，交付该文件即可。旧目录包仍可读取；将旧工程转换时另存到新的 `.comp` 文件路径，不覆盖原目录。保存到用户授权的目标；编辑已有工程优先另存为，新文件冲突时使用新路径。不要直接改 manifest 或将导出的 PNG 当作分层工程。多个进程编辑同一个项目时使用指纹检查；`project_busy` 表示其他进程正在读写目标，等待后先重新 `inspect`，再按最新状态决定操作。收到指纹冲突后同样重新检查，勿盲目覆盖。

## 连续操作会话

连续编辑、撤销或重做用 `cli serve`，通过 stdin/stdout 每行传一个 JSON 对象。响应回显请求 `id`，`protocolVersion` 为 1，`ok` 为真时读取 `result`，否则读取 `error.code` 与 `error.message`。日志走 stderr。

```python
session = subprocess.Popen(
    [exe, "cli", "serve"], stdin=subprocess.PIPE,
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    text=True, encoding="utf-8", bufsize=1,
)
sequence = 0

def request(method, **params):
    global sequence
    sequence += 1
    session.stdin.write(json.dumps(
        {"id": sequence, "method": method, "params": params},
        ensure_ascii=False,
    ) + "\n")
    session.stdin.flush()
    reply = json.loads(session.stdout.readline())
    if reply.get("id") != sequence or not reply.get("ok"):
        raise RuntimeError(reply)
    return reply["result"]

try:
    info = request("open", path=str(input_path))
    info = request("inspect")
    # operations 是按本机 capabilities 构造的数组。
    info = request("apply", operations=operations, expectedRevision=info["revision"])
    request("save", path=str(project_path), overwrite=False)
    request("export", path=str(preview_png), maxSide=1024)
    request("close")
finally:
    session.stdin.close()
    session.wait()
```

会话还支持 `capabilities`、`fonts`、`new`、`undo`、`redo`。保存与导出读取各自响应，编辑成功不等于文件已写入。先保存需要保留的修改再 `close`、`open` 或 `new`；只有用户要求丢弃修改时传 `discard:true`。同一已打开项目保存时会检查磁盘变化；保存到其他已存在路径需明确 `overwrite`。选区和撤销历史只在当前内存会话内，`.comp` 不持久化它们。

## 操作正在打开的界面

用户要直接修改当前窗口时用 `cli live`，不要以磁盘旧版本替代界面中的未保存内容。先调用 `cli("live", "capabilities")` 获取文档会话，再 `inspect` 选定会话。多个窗口时按返回提示用 `--endpoint` 选择本机 endpoint 文件；客户端负责认证，不需要手工读取或发送 token。`live` 不负责启动 GUI 或打开新文档。

每次修改、撤销、重做、保存或导出都携带最新检查返回的 `sessionId`、`expectedRevision` 和 `expectedSerial`。`serial` 也跟踪选区等不产生新历史版本的变化。收到冲突或 `busy` 时重新检查或等待用户完成交互，不去掉版本条件强行重试。

```python
def live(method, **params):
    return cli("live", method, "--params", json.dumps(params, ensure_ascii=False))

def guard(state):
    return {"sessionId": state["sessionId"],
            "expectedRevision": state["revision"],
            "expectedSerial": state["serial"]}

state = live("inspect")
state = live("apply", operations=operations, **guard(state))
live("export", path=str(preview_png), maxSide=1024, **guard(state))
state = live("save", path=str(project_path), overwrite=False, **guard(state))
```

小请求可如上把 JSON 作为一个 argv 参数；较大批次将整个 params 对象写入 UTF-8 文件，用 `--params @文件`。`undo`、`redo` 同样接收 guard 并返回更新后的状态。一次 `apply` 是一个撤销步骤，批内失败不提交任何编辑。界面即时显示成功提交的修改。

`live save` 默认拒绝任何已存在目标，包括当前项目路径；覆盖已有目标需传 `overwrite:true`，并建议附加 `inspect` 返回的非空 `fingerprint` 作为 `expectedFingerprint`。`fingerprintError` 表示磁盘目标读取异常，先查明原因。导出预览不会将工程标记为已保存。请求超时后先 `inspect` 或检查目标文件再决定下一步；`error.commitAlreadyStarted:true` 表示落盘已经开始，结果可能仍会完成，避免重复应用。

## 结果验证

重新检查保存的 `.comp`，确认画布、图层、蒙版及目标修改；打开导出的 PNG 或缩略图做视觉检查，尤其检查主体边缘、透明背景、裁切和文字。需要时继续调整可编辑蒙版。向用户给出工程与最终图片的路径，说明实际完成的操作和可见限制。

离线模式保存的 `.comp` 若已在界面打开，会触发外部变更重载；界面存在未保存修改时会请求确认。`serve` 的内存编辑需保存后才会反映到界面；`live` 直接更新对应界面文档。
