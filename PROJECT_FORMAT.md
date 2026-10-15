# Compositor 项目格式与 Agent 编辑参考

本文将原项目的格式和外部编辑说明集中保存。Windows 默认保存为单文件容器 v1，内嵌清单语义保持版本 11，并兼容读取旧目录包版本 1–11。下面两份原文保留为 **macOS 原版参考**，其中 Swift 文件名、QuickLook、Mac 内存预算和平台保存机制描述属于原程序；Windows 行为以本节及 `src/project.rs`、`src/model.rs`、`src/app.rs` 为准。

## Windows 行为

- `.comp` 默认是单个文件：容器 v1 内嵌 `manifest.json` 与 `images/` PNG 资产，读取时直接访问内嵌资产，不解包到文件夹。内嵌清单仍为版本 11。旧目录包版本 1–11 继续可读；旧目录不会被原地覆盖，GUI 保存时另存为单文件，CLI／live 需指定新的文件路径。原 macOS 程序不直接读取新的单文件容器。图层按自底向顶保存；图像和蒙版文件用规范 UUID 名称，蒙版为 `.mask.png`。文字样式 run 使用 UTF-16 偏移。
- 单边上限 30,000 像素，单像素表面上限 2 亿像素；文档、工作区和撤销历史另有随内存变化的预算，图像及蒙版资产分别校验。原文的 1 亿像素预算是 macOS 参考。
- 清单上限 4 MiB、单个编码资产 512 MiB、图层最多 10,000、分组深度最多 64、剪贴链最多 256、参考线最多 1,000。拒绝路径逃逸、符号链接、非法字段、循环关联和超出预算的资产。
- 程序保存使用同目录暂存文件及恢复日志，成功后仅保留目标 `.comp` 文件；替换中断时可恢复完整的旧文件或已完成的暂存文件。旧目录保存日志也仍支持恢复。
- GUI 每约 500ms 检查单文件的大小和修改时间；旧目录检查 manifest 与 images 的路径、大小和修改时间。刻意维持同大小、同修改时间时不保证发现。Agent 应使用 CLI 或 live 更新工程，由存储层完成替换。
- 外部更新失败会显示消息；未保存编辑会提示选择。成功重载会保留仍存在的活动层和裁剪至新画布的选区，并清空撤销历史。选区是会话状态，不随项目保存。Windows 不生成 QuickLook。
- 单文件外部编辑使用 CLI／live，保留已有图层 ID，不直接改二进制容器。以下原版说明的 manifest／PNG 文件写入步骤只适用于旧目录工程；新容器的结构与读取限制以 `src/project.rs` 为准。

### Windows 单文件容器 v1

单文件使用 64 字节头、连续资产区和末尾 JSON 索引。整数为小端；图像仍使用无损 PNG，不额外重复压缩，也不依赖原始素材位置。

| 头部偏移 | 长度 | 内容 |
|---|---|---|
| 0 | 8 | 魔数 `COMP\r\n\x1a\n` |
| 8 | 4 | 容器版本，当前为 1 |
| 12 | 4 | 索引条目数 |
| 16 | 8 | 索引起始偏移 |
| 24 | 8 | 索引字节长度 |
| 32 | 32 | 索引的 SHA256 |

索引是条目数组，每项含 `path`、`offset`、`length` 和 `sha256`（32 个字节值）。只允许 `manifest.json` 和规范 UUID 命名的 `images/*.png`／`images/*.mask.png`；索引最多 8 MiB、20,001 项。每项须连续、无重复、无重叠或空洞，全部条目必须与清单资产引用相符；索引后的额外数据、截断、未知容器版本和校验失败会报错。资产逐项读取并校验，解码继续遵守像素预算。CLI 指纹是整个单文件的 SHA256；旧目录仍按清单及引用资产计算。

## 原版格式参考（macOS）

# Compositor project format, versions 1–11

A `.comp` file is a macOS document package containing `manifest.json` and an `images/` directory of `<layer UUID>.png` assets.

The manifest identifies `com.compositor.project`, version `11` for new saves (versions `1`–`10` remain readable), and the sRGB working space. It stores document UUID, pixel dimensions, active layer UUID, and layers in bottom-to-top order. Each layer stores its UUID, name, visibility, transform (origin, size, clockwise rotation, flips, sampling), and optional image filename. Blank layers have no image asset.

Embedded PNGs preserve source pixels and transparency; transforms remain separate. Projects survive moving or deleting imported source photos. Saving uses a coordinated atomic package replacement. Unsupported versions, invalid metadata, missing assets, unsafe paths, and oversized data are rejected before replacing the live document.

Limits: 30,000 pixels per canvas/image side, 100 million total source pixels, 10,000 layers, 4 MiB manifest, 512 MiB per encoded asset. See `ProjectStore.swift` for validation.

Undo history and viewport are session-only. Opening fits the canvas, restores selection, and starts with clean history. Future editable features must extend the schema and round-trip tests. PNG export is a flattened derivative and does not mark project edits saved.

Image Size adds optional `resolution` (pixels/inch, 1–9600). Older manifests without it default to 72. This additive field retains version 1 compatibility. Both PNG and JPEG exports include document resolution metadata. Resampling stores the new layer pixels and bounds; undo retains the prior sources only during the current session.

Version 2 adds optional `parentID` and `isGroup` on layer records. A group has no image file. Root nodes have no parent; children refer to an existing group. Array order defines bottom-to-top sibling order; renderers traverse each group as a contiguous subtree. Visibility is inherited without changing child flags. Cycles, missing/non-group parents, image-bearing groups, and nesting beyond 64 ancestor levels are rejected. Group ancestors permit room for leaf nodes at the deepest level. Group metadata survives image/canvas resizing and cropping. Older app builds reject version 2 rather than misrender grouped documents. Collapse state is not serialized.

Version 3 adds optional per-layer `opacity` (finite 0–1) and `blendMode` (Normal, Multiply, Screen, Overlay, Darken, Lighten, Difference, Color Dodge, Color Burn). Missing fields default to full opacity and Normal. Group records required those defaults until version 8, which lets a folder carry its own opacity; a folder's opacity multiplies into every layer inside it, while its blend mode stays Normal because folders are pass-through. Effects are applied during compositing and retained as metadata when resizing sources. Files declaring older versions cannot contain non-default appearance values.

Version 4 adds optional `maskFile` and `maskEnabled` fields to individual layers. Mask filenames must be `<layer UUID>.mask.png` under `images/`; enabled defaults to true when a mask exists. Records without masks omit both fields. Groups cannot carry masks in this version. Files declaring versions 1–3 cannot contain mask metadata.

Masks store 8-bit grayscale coverage without alpha (white reveals, black hides). Their normalized extent matches the image’s local rectangle, so the same layer transform applies to both. A uniform 1×1 mask is valid and avoids allocating full-resolution pixels before painting. Nonuniform mask pixels and a thumbnail are immutable assets shared by history. Image Size resamples them with the image transform; Canvas Size and Crop preserve their pixels. Up to 100 million mask pixels may be stored in addition to the existing 100 million image pixels; per-side and per-file limits also apply to masks. Disabled masks remain embedded and editable but do not affect compositing. Image-versus-mask target selection is session-only and reopens on image pixels.

Version 5 adds optional `maskSourceID`: the UUID of a non-group layer supplying live alpha in document coordinates. It multiplies the target’s alpha alongside its enabled raster mask. Source pixels, transform, opacity, raster mask and upstream live masks contribute coverage; visibility and RGB color do not. Sources remain independent layers. Missing references, self-links, cycles, group endpoints and chains over 256 nodes are rejected. Deletion can bake the live coverage into dependent image pixels (retaining their raster masks) or remove the links, as one undoable operation. Links survive image/canvas resize and crop. Older versions default to no live mask; older app builds reject v5.

UI terminology: these alpha links are clipping masks. Option-click assigns the lower sibling’s base or releases the connection. Multiple clipped layers share one base, show indented above it, and release when moved outside the contiguous stack. The underlying `maskSourceID` representation is unchanged.

Version 6 allows `maskFile` and `maskEnabled` on group records. A folder has no image, so its mask covers the folder's own transform rectangle (the canvas size when the folder was created); Image Size resamples it through that transform, and Canvas Size and Crop preserve its pixels, exactly as for layer masks. Groups are pass-through, so an enabled folder mask multiplies the coverage of every descendant layer, together with that layer's own mask and any enclosing folders' masks; clipping-mask coverage is unaffected. Files declaring versions 1–5 cannot give a group a mask, and older app builds reject v6.

Version 7 adds adjustment layers: a layer record with an optional `adjustment` object and no `imageFile`. Adjustment layers cannot be groups and cannot carry `text`; like any layer they take a transform, opacity, blend mode, raster mask and clipping link, and they affect everything composited below them within their folder. `kind` is one of `Hue/Saturation`, `Levels`, `Curves`, `Exposure`, `Gradient Map`, `Grain`, `Invert`, `Black & White` and `Color Balance` (version 9 adds three more). The record carries the settings of every kind, each optional and defaulting to an identity adjustment: `hue` (±360), `saturation` and `lightness` (±100), `colorize` and `hsvSettings` for Hue/Saturation; `levels` (four channel ranges, RGB then red, green, blue); `curves` (four channel point lists); `exposureSettings`; `gradientMapSettings` (`shadows`, `highlights`, `reversed`); `grainSettings`; `blackWhiteSettings`; `colorBalanceSettings`. Out-of-range or non-finite values are rejected. Files declaring versions 1–6 cannot contain adjustment records, and older app builds reject v7. See `LayerAdjustment.swift` for the exact ranges.

Version 8 lets a folder carry its own `opacity`, which multiplies into every layer inside it; a folder's blend mode stays Normal because folders are pass-through (files declaring 1–7 require folders at full opacity). It also adds an optional top-level `guides` array of alignment guides, each with `id`, `axis` (`horizontal` or `vertical`) and `position` in document pixels (finite, at most 1,000,000 in magnitude). At most 1,000 guides are stored; files declaring 1–7 cannot contain guides. Guides survive Canvas Size and Crop by offsetting with the canvas.

Version 9 adds three adjustment kinds that sample neighboring pixels: `Gaussian Blur` (`blurRadius`, 0.1–250 document pixels), `Motion Blur` (`motionAngle`, −90 to 90 degrees, and `motionDistance`, 1–2000) and `Add Noise` (`noiseAmount`, 0.1–400, `noiseGaussian`, `noiseMonochromatic` and `noiseSeed`, so the pattern is stable between sessions). Files declaring 1–8 cannot contain these kinds; the earlier adjustment kinds remain valid at version 7 and up.

Version 10 lets a text layer color some of its letters differently: optional `colorRuns` in its `text` metadata (see Editable text). Files declaring 1–9 cannot contain it.

Version 11 lets those letters use different faces too: optional `fontRuns` in the same metadata. Files declaring 1–10 cannot contain it. `colorRuns` stays valid from version 10.

### Additive layer fields

Later fields are optional and not gated on the version, so older readers ignore them and keep the pixels or the linked mask as they were:

- `maskPlacement` and `maskLinked`: an unlinked mask (`maskLinked` false; missing means linked) keeps its own transform in `maskPlacement`, a document-space rectangle like the layer transform, and no longer follows the layer when it moves. Both require a `maskFile`.
- `shape`: a layer made with the Shape tool keeps its style (`kind`, `red`/`green`/`blue`, `cornerRadius` in document pixels, and for lines `lineWidth` plus `start` and `end` as fractions of the layer box) so it redraws cleanly when scaled. Its PNG is still an ordinary raster; once anything else changes those pixels the metadata is dropped.

### Editable text

Pixel layer records may include optional `text` metadata: content, PostScript font name, font size in pixels, RGB color, alignment, tracking, line spacing and optional `boxSize` paragraph bounds. Text wraps inside these bounds; changing them reflows the text without scaling the font. The PNG remains the display and export fallback. Older readers ignore this metadata. Transforms, duplication, masks and canvas-size changes preserve it; destructive pixel operations rasterize text and omit the metadata on the next save. Missing fonts use the system font when edited, while the saved PNG preserves the original appearance until then. From version 10, optional `colorRuns` lists letters painted in another color than the text's own `red`/`green`/`blue`: each run has `location` and `length` in UTF-16 units of the content, plus `red`, `green` and `blue` (0–1). From version 11, optional `fontRuns` lists letters set in another face than `fontName`: the same `location` and `length`, plus `fontName`. Runs of either kind are sorted, do not overlap, have a positive length and end within the content; letters outside every run use the text's color or face.

### Layer effects

An optional `effects` record contains independent `stroke`, `shadow`, `colorOverlay`, `innerShadow`, `outerGlow` and `innerGlow` records. Stroke carries a size (0–500 layer pixels), a color, an opacity and an `inside` flag choosing which side of the edge it sits on; drop shadow and inner shadow each carry an angle, a distance, a blur, a color and an opacity; color overlay carries a color and an opacity; outer glow and inner glow each carry a size (0–500 layer pixels), a color and an opacity. Each supports optional `enabled` visibility (missing means visible); hidden effects keep all parameters and remain listed under their layer. Effects, including their visibility, are saved and participate in document undo. Canvas previews run on a serial background worker with a shared pixel budget; exports render the full-resolution effects. A record omitting an effect means that layer does not have it, so older readers see the effects they understand and ignore the rest.


## 原版 Agent 编辑参考（macOS）

# Writing Compositor projects (for AI agents and scripts)

A Compositor project (`.comp`) is a folder of PNG layer images plus a `manifest.json`. Anything that can write files can build or edit one, and Compositor updates the open canvas as the files change. No plugin or API is involved.

## Try it

1. Open a project in Compositor 1.3 or later (save a new canvas somewhere, e.g. `~/Desktop/demo.comp`), and keep it open.
2. Ask an AI agent that can edit files on your Mac (Claude Code, Codex and the like):

   > Read PROJECT_FORMAT.md in this Windows port, then design a moody night scene in ~/Desktop/demo.comp. Work in steps, one or two layers at a time.

3. Watch the canvas. Each time the agent writes the project, Compositor reloads it, usually within half a second.

What you get are ordinary layers: select them, change their opacity or blend mode, paint on their masks, save.

## The package

```
Example.comp/
├── manifest.json
└── images/
    ├── 6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F.png        a layer's pixels
    └── 6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F.mask.png   its mask (optional)
```

A minimal manifest with one full-canvas image layer:

```json
{
  "format": "com.compositor.project",
  "version": 11,
  "colorSpace": "sRGB",
  "documentID": "0C5E7A91-3B2D-4F6A-8E1C-9D0B7A6F5E4D",
  "width": 1920,
  "height": 1080,
  "resolution": 72,
  "activeLayerID": "6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F",
  "layers": [
    {
      "id": "6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F",
      "name": "Background",
      "imageFile": "6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F.png",
      "isVisible": true,
      "isGroup": false,
      "opacity": 1,
      "blendMode": "Normal",
      "transform": {
        "origin": [0, 0],
        "size": [1920, 1080],
        "rotation": 0,
        "flipX": false,
        "flipY": false,
        "sampling": "High quality"
      }
    }
  ]
}
```

- `layers` runs **bottom to top**: the last layer draws on top.
- Keep `documentID` as it is when editing an existing project.
- `transform` places the layer in document pixels: `origin` is its top-left corner, `size` its width and height, `rotation` is in degrees, clockwise. The image is stretched to `size`, so a layer can be smaller than the canvas (a cut-out placed with `origin`) or scaled.
- `sampling` is `"High quality"`, `"Smooth"` or `"Nearest"`.
- `opacity` runs from 0 to 1.

## Rules that matter

Break one of these and Compositor refuses the whole file **without any message**: the open canvas just stays as it was. If nothing updates, check these first.

- **Image files are named after their layer.** A layer with `"id": "6F1D…"` must use `"imageFile": "6F1D….png"`, and a mask `"maskFile": "6F1D….mask.png"`, with the ID in uppercase as written in the manifest. One ID per layer, unique in the project.
- **Images are 8-bit PNGs** in `images/`. Layer images are RGBA; masks are 8-bit grayscale (white shows the layer, black hides it).
- **Blend modes are spelled exactly** as Compositor names them: `Normal`, `Darken`, `Multiply`, `Color Burn`, `Linear Burn`, `Lighten`, `Screen`, `Color Dodge`, `Linear Dodge (Add)`, `Overlay`, `Soft Light`, `Hard Light`, `Vivid Light`, `Linear Light`, `Pin Light`, `Hard Mix`, `Difference`, `Exclusion`, `Subtract`, `Divide`, `Hue`, `Saturation`, `Color`, `Luminosity`.
- **Every layer the manifest names has its image in place**, and the manifest is valid JSON.

## Writing safely while the project is open

Compositor reads the project as soon as it changes, so never leave it half written:

1. Write any new or changed PNGs into `images/` first.
2. Then write the manifest to a temporary file inside the package (for example `.manifest.json.tmp`) and rename it over `manifest.json`. A rename is atomic: Compositor sees either the old manifest or the new one, never part of one.

To change an existing layer, keep its `id` and overwrite its PNG, then rewrite the manifest. The layer updates in place, in the same spot in the stack.

Remove images you no longer reference once the manifest no longer lists them.

A project Compositor saved also has a `QuickLook` folder (`Preview.jpg`), which Finder shows as its Space-bar preview. When you change a project, delete that folder, so Finder doesn't preview an out-of-date picture; Compositor writes it again the next time it saves.

## What the open app does

- It reloads about a third of a second after writes stop. Several writes in quick succession arrive as one update, so pause briefly between steps if a viewer should see each one.
- A reload keeps the zoom, scroll and selection, but clears undo, as reopening a file does.
- If the person has unsaved changes of their own, Compositor asks them to revert to your version or keep theirs, and never replaces their work silently.
- A write that fails to load is ignored until the next change, so a mistake you then fix will still show up.
- Changes are noticed from the manifest's contents and from each image's name and size, not from when files were written. Rewriting a PNG with different pixels changes its size in practice. If you replace an image with one of exactly the same byte size, also make a change to the manifest, such as renaming the layer; writing identical manifest bytes back isn't enough.

## Masks

Add a mask to any layer with `"maskFile": "<id>.mask.png"` and `"maskEnabled": true`. The mask covers the layer's own pixels, so it has the same pixel size as the layer's image. Soft grays give soft edges.

## Adjustment layers

An adjustment layer has an `adjustment` object and no `imageFile`, and it affects everything below it. Every kind carries identity `levels` and `curves` blocks, plus its own settings. A warming Curves layer:

```json
{
  "id": "A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D",
  "name": "Warm Grade",
  "isVisible": true,
  "isGroup": false,
  "opacity": 1,
  "blendMode": "Normal",
  "transform": { "origin": [0, 0], "size": [1920, 1080], "rotation": 0, "flipX": false, "flipY": false, "sampling": "High quality" },
  "adjustment": {
    "kind": "Curves",
    "hue": 0, "saturation": 0, "lightness": 0, "colorize": false,
    "levels": { "channel": "RGB", "ranges": [
      { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 },
      { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 },
      { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 },
      { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 } ] },
    "curves": { "channel": "RGB", "channels": [
      [ { "x": 0, "y": 0 }, { "x": 255, "y": 255 } ],
      [ { "x": 0, "y": 0 }, { "x": 120, "y": 147 }, { "x": 255, "y": 255 } ],
      [ { "x": 0, "y": 0 }, { "x": 100, "y": 114 }, { "x": 255, "y": 255 } ],
      [ { "x": 0, "y": 0 }, { "x": 115, "y": 97 }, { "x": 255, "y": 238 } ] ] }
  }
}
```

- `ranges` and `channels` run RGB, then red, green, blue. Curve points run from x 0 to x 255, in increasing x.
- `kind` is one of `Hue/Saturation`, `Levels`, `Curves`, `Exposure`, `Gradient Map`, `Grain`, `Invert`, `Black & White`, `Color Balance`, `Gaussian Blur`, `Motion Blur`, `Add Noise`.
- For Hue/Saturation, set `hue`, `saturation` and `lightness` on the adjustment itself. Color Balance takes a `colorBalanceSettings` object (`shadowCyanRed`, `shadowMagentaGreen`, `shadowYellowBlue`, and the same for `mid` and `highlight`, each −100 to 100, plus `preserveLuminosity`).
- For the other kinds, the easiest way to get the exact shape is to add one in Compositor, save, and copy it from that project's manifest.

## More

- Folders, text layers, layer effects and everything else the format holds: [Windows 格式说明](#windows-行为).
- Limits: canvases up to 30,000 pixels on a side; layers and masks count toward a memory budget that scales with the Mac.
