# 原版 1.5.3 迁移对照

核对日期：2026-10-17。本文是迁移清单，**不是已经完成移植的发布说明**。状态来自当前 Windows 源码与本地覆盖的原版源码静态对照；列出的测试名是验收依据，本轮未重新运行这些测试，也未运行 macOS 测试。

原版来源为 [Compositor v1.5.3](https://github.com/robbietilton/Compositor/tree/v1.5.3)，本地位于 `D:\Desktop\Compositor To Win`。Windows 工程位于本仓库。下表原版业务文件省略共同的 `Compositor/` 前缀，原版测试文件省略 `CompositorTests/` 前缀；Windows 文件均相对本仓库根目录。

## 对照范围

- 本地 `appcast.xml` 标明版本 1.5.3、build 49，并列出本次发布的七项变化；`Compositor/IO/ProjectStore.swift` 的 `ProjectManifest.current` 为 14。
- 旧迁移记录 `migration/original-files.json` 收录 250 个原始文件。已生成的对照记录发现 101 个修改、38 个新增；这是文件差异数量，不是新增功能数量。
- 旧记录中 `CameraRaw.swift`、`ShapeTool.swift`、`docs/project-format.md` 三个文件的 SHA256 与官方 v1.4.5 原文完全相同。它们确认了这三个关键基线；并未据此声称所有旧文件都来自同一 Git 提交。
- 基线与差异证据保存在开发目录 `build/validation/upstream-1.5.3-20261016/source-review/`：`baseline-tag-probe.json`、`local-vs-migration-baseline.json` 及三份官方 v1.4.5 源码副本。生成证据不随发布包分发。

## 1.5.3 本次发布的七项变化

| 功能 | 原版文件与行为 | Windows 现状 | 待迁移与验收依据 |
| --- | --- | --- | --- |
| 大图 Camera Raw GPU 加速 | `Document/CameraRaw.swift:208`、`Document/CameraRawTables.swift`、`Rendering/MetalCameraRawLookup.swift`：合成颜色查找表，用 GPU 执行；接近 8 位舍入边界的结果由 CPU 修正，失败可回退 | **未迁移此路径**。`src/camera_raw.rs` 仍在 CPU 上执行各阶段，部分颜色阶段已用 Rayon 并行；`src/gpu.rs` 的画布合成加速不等于 Camera Raw 查找表加速 | 将查找表、阶段顺序与舍入修正移为 Rust/WGSL，并保留 CPU 回退。参考 `CameraRawGPUTests.swift`、`CameraRawGPUPerformanceTests.swift`；先验证像素等效，再量大图拖动耗时 |
| 拖动 Camera Raw 滑块保持原始分辨率 | `Document/Filters.swift:622` 将 Camera Raw 排除在交互降采样之外 | **已具备原始网格预览**。`src/app_state_tests.rs` 的 `camera_raw_preview_retains_original_grid_and_shares_clean_output`，`src/filter_panel.rs` 的 `camera_raw_native_preview`；另有适合窗口/100% 查看模式 | 保留现有细节，不通过缩小原图达到加速。新增 GPU 路径时用同一原图核对拖动中、松手后、最终应用三者；现有测试源码不代替新实现的运行结果 |
| 更快的直方图与示波器 | `Document/CameraRaw.swift:347`：≤400 万像素时每方向隔 4 像素采样，更大图隔 8；直接读取适合的图像缓冲 | **已有缓存与后台预计算，尚未采用该采样优化**。`src/filter_panel.rs` 的 `histogram_bins`、`vectorscope_bins` 仍扫描每个像素，`prepare_camera_raw_scopes` 会预计算并缓存 | 只减少统计采样成本，保留图像预览与导出像素。参考 `CameraRawScopePerformanceTests.swift`；验证透明权重、主色方向、缓存失效和大图耗时 |
| 矩形 Smooth Corners | `Document/ShapeTool.swift:10`、`UI/ShapeControls.swift:56`：60% smoothing 的三次曲线—圆弧—三次曲线圆角，空间不足时降低平滑度 | **未迁移**。`src/edit.rs` 的 `shape_raster` 只有普通圆角，`src/app_tools.rs` 没有此选项；画笔“平滑”是另一功能 | 预览、提交、缩放重绘使用同一几何；元数据 `shape.smoothing` 与格式 14 一并处理。参考 `ShapeToolTests.swift` 与原版格式文档；半径 0、最大半径、椭圆/直线、撤销及重开均需覆盖 |
| 形状填充色移到参数前 | `UI/ShapeControls.swift:19`：三种形状都先显示填充色，再显示宽度/半径 | **待调整位置**。`src/app_tools.rs` 的 `shape_header_contents` 已有可点击填充色，但位于宽度/半径之后 | 调整现有控件顺序；实际查看矩形、椭圆、直线三种模式，确认选色交互不变 |
| 中键拖动图层面板 | `UI/NativeLayerList.swift:519` 显式处理按下、拖动、释放，滚动位置限于列表；不改变选择、历史与画布 | **尚无专属实现与等效验收**。`src/layer_panel.rs:392` 使用通用 `ScrollArea`，当前工程无图层列表中键状态处理 | 核实真实中键行为后补齐；不以使用通用滚动控件推定等效。参考 `MiddleButtonTests.swift` 的 `middleDraggingScrollsOnlyTheLayerList`，包含短列表、边界、释放、子控件上拖动 |
| 缩放反向时立即更新光标 | `Rendering/EditorCanvas.swift:2042` 的中键缩放按最近垂直移动方向切换放大/缩小光标，静止保留方向 | **未迁移这套拖动与方向光标**。`src/app.rs` 的缩放工具目前点击倍增/减半，滚轮连续缩放；没有对应中键缩放状态 | 中键平移、Ctrl+中键缩放、普通缩放拖动按 Windows 键位移植，保护已有左键编辑。参考 `MiddleButtonTests.swift` 的 `zoomCursorChangesAsSoonAsTheDragReverses` |

## 从旧移植基线累积到 1.5.3 的其他变化

这些项目存在于当前原版，并非都在 1.5.3 这一个版本首次加入。仅凭本地覆盖文件不为每项补造首次发布版本。

| 功能 | 原版文件与行为 | Windows 现状 | 待迁移与验收依据 |
| --- | --- | --- | --- |
| 命令搜索面板 | `UI/CommandPalette.swift`、`UI/CommandPaletteView.swift`，在 `CompositorApp.swift:106` 打开；搜索菜单与工具，显示快捷键、可用和选中状态 | **缺少**。`src/app.rs` 已有命令分发与可自定义快捷键，没有可搜索面板 | 从统一命令表生成中文名称、菜单路径、快捷键与禁用状态，避免复制一套行为。参考 `CommandPaletteTests.swift`；中文输入、模糊排序、回车执行、无文档禁用需验证 |
| Canvas Only / 全屏画布 | `CompositorApp.swift:112`、`ContentView.swift`、`Document/EditorSession.swift:313`；F 切换，隐藏面板并铺开画布，文字输入/对话框时不抢键 | **缺少对应模式**。Windows 的窗口最大化与开关浮动面板不是该功能 | 在现有 `src/app.rs` / `src/app_chrome.rs` 接入显示状态；验证文字输入 F、编辑对话框、退出模式与窗口恢复 |
| 重复上次滤镜 | `Document/Filters.swift:748`、`CompositorApp.swift:330`；直接使用上次确认的种类/设置，不再打开面板 | **缺少**。Windows 有滤镜对话框与提交路径，未记录 Last Filter | 在滤镜成功提交后记忆可重复项，取消或失败不替换记录；验证当前目标/选区、撤销和禁用状态，参考 `FilterTests.swift` |
| 新建画布的英寸、厘米、毫米及背景 | `UI/NewCanvasSheet.swift`：px/in/cm/mm 与 DPI 换算，透明/白/黑背景 | **部分已有**。`src/welcome_ui.rs` 有像素宽高、尺寸预设、PPI 和剪贴板尺寸建议，只有透明背景 | 补单位换算与背景选项；PPI 合法范围须与 `src/project.rs` 的 1–9600 一致。参考 `NewCanvasUnitTests.swift`；单位往返、DPI 改变、边界、背景图层与重开需覆盖 |
| 画布顺/逆时针 90° 旋转 | `Document/CanvasRotation.swift`：宽高互换，图层、独立蒙版、选区、参考线一同转，不重采样源图，单步撤销 | **缺少整个画布旋转命令**。`src/edit.rs` 已有图层旋转变换和 `flip_canvas`，两者都不等于此操作 | 在 `src/edit.rs` 实现文档级几何变换并由 GUI/CLI 复用。参考 `CanvasRotationTests.swift`；矩形画布、往返、蒙版、参考线、软选区与撤销需逐像素验证 |
| Navigator 导航小地图 | `UI/NavigatorMinimap.swift`、`Rendering/NavigatorGeometry.swift`、`Document/EditorSession+Navigator.swift`；300% 及以上可显示，点击/拖动定位，编辑稳定后重绘 | **缺少**。Windows 有 Fit/100%、滚轮缩放和平移，没有文档导航小地图 | 独立坐标计算与缩略图缓存；只在可见且内容变化时重算，不因选区改变重绘。参考 `NavigatorTests.swift`；大图调整层、蒙版、比例与平移需验证 |
| 独立 Scanlines 滤镜 | `Document/Scanlines.swift`、`Rendering/DitherPixels.c`、`UI/FilterSheet.swift:275`；含线厚、位移、平滑、阈值、色彩分离、黑位、辉光等 | **仅有较旧 CRT 抖动样式**。`src/filter_panel.rs:1687` 与 `src/dither.rs` 已支持 Dither 内的 `Scanlines (CRT)`，`src/filters.rs` 没有独立 `Scanlines`；不能算参数等效 | 增加独立滤镜与参数，保持旧抖动样式兼容。参考 `ScanlinesTests.swift`，包括实色明暗、位移遮挡、色彩分离、辉光、透明度和确定性 |
| 渐进模糊 | `Document/Filters.swift:73,386`、`UI/FilterSheet.swift:92`；沿画布控制线或径向，从一种模糊半径渐变到另一种，支持反转 | **缺少**。当前 `src/filters.rs` 只有高斯、动感等，`src/filter_interactions.rs` 没有对应渐进控制线 | 先补像素算法与两个端点，再接现有画布交互和应用/取消。参考 `ProgressiveBlurTests.swift`；线性/径向/反转、非零起始半径、旋转图层、选区、透明扩边需验证 |
| RAW 原始显影增加降噪、镜头配置、高光恢复 | `IO/RawImporter.swift:20`、`UI/RawDevelopSheet.swift`；按解码器能力启用 noiseReduction / lensCorrection / highlightRecovery，记忆拍摄默认值 | **缺少显影器这三项**。`src/image_io.rs:18` 的 `RawDevelopSettings` 只有 exposure/temperature/tint/boost；Rust rawloader 与 Apple CIRAWFilter 也不是同一解码器 | 在传感器到输出像素的正确阶段实现或接入有实际能力的解码器；能力不足时明确禁用对应选项。以真实 Bayer/RGB RAW、通道部分过曝和带镜头信息的样本验证，不能用已有“高光”色调滑块替代恢复 |
| Camera Raw 十分区独立重置 | `Document/CameraRawReset.swift`：只重置指定分区，重新启用该分区，清理混色点/几何临时状态 | **缺少分区重置入口**。`src/filter_panel.rs:1856` 已有十分区启用复选框，曲线/颜色等也有局部控件，但没有该完整行为 | 以现有默认值生成指定分区的重置操作，保留其他分区和历史。参考 `CameraRawResetTests.swift`；选项、中点、分区显示与临时交互一并验证 |
| 文件夹批量导入 | `IO/ImageImporter.swift:23` 的 `ImageFolder`：仅直接子文件，按自然文件名排序，不递归、不把包当普通目录；打开和拖放接入 | **缺少普通目录展开**。`src/image_io.rs:104` 把目录交给 `.comp` 加载器；`src/app.rs` 接收拖放路径，未展开普通图片文件夹 | 在现有导入队列前增加有界、稳定排序的目录解析，保留 `.comp` 目录工程识别。验证中文路径、2/10 排序、子文件夹、损坏文件和累计像素预算 |
| 印刷 CMYK 配置/软打样/色域警告/导出 | `IO/CMYK.swift`、`CompositorApp.swift:143`、`UI/ExportAsSheet.swift`；内部文档仍是 RGB，选择/加载 ICC，CMYK JPEG/TIFF/PDF 输出 | **缺少这些印刷工作流**。`src/image_io.rs` 可将受支持的嵌入 RGB/Gray ICC 转为 sRGB，但不等于 CMYK 软打样和输出；`src/psd.rs:604` 仍拒绝非 8-bit RGB PSD | 先建立 ICC profile 验证、转换与软打样接口，再接 GUI/导出/PSD。参考 `CMYKTests.swift`；检查真实输出通道/嵌入 profile、透明铺底和超色域标记，不能只比较扩展名 |
| 16 位图层、编辑、工程和导出 | `Document/DeepLayers.swift`、`Rendering/DeepPixels.c`、`IO/ImageImporter.swift`、`IO/ImageExporter.swift`；保留高精度原像素与 8 位抖动预览，支持图层位深转换；格式 12 | **未迁移完整链路**。`src/model.rs:62` 的 `Raster` 为 RGBA8，`src/image_io.rs:172` 导入转为 RGBA8，PNG/TIFF 导出也是 RGBA8；即使能打开 16 位图片也已量化 | 需要先扩展像素/历史/渲染模型，再接编辑与保存。参考 `DeepLayerTests.swift` 的导入、色阶、滤镜、画笔、变换、合并、保存重开与 16 位导出；不能只扩展版本上限 |
| HDR 图层、EXR/Radiance HDR 与亮部保留 | `Document/DeepLayers.swift:79`、`IO/ImageImporter.swift:112`、`IO/ImageExporter.swift:131`；半浮点 extended sRGB 保存大于白色的值，格式 13 用 `.exr` 图层资源 | **未迁移**。Windows 编辑模型仍 RGBA8，没有 HDR/EXR 图层资产和对应导出流程 | 在高位深模型上保留 >1 值，处理曝光、预览、画布合成、保存/重开与 EXR 导出。参考 `DeepLayerTests.swift` 的 HDR 用例；降低曝光后需能找回亮部，不能以截白图像通过测试 |
| 统一导出面板及更多格式 | `UI/ExportAsSheet.swift`、`IO/ImageExporter.swift`，将预览、尺寸、格式/位深/CMYK 选项放在导出流程 | **普通图片格式已有，整体高级流程仍部分缺少**。Windows `src/image_io.rs` 已有 PNG/JPEG/TIFF/BMP/GIF/WebP/PDF 输出与 JPEG 品质预览；PDF 页图编辑也已独立实现 | 保留现有选项。若按上游合并面板，仍需覆盖格式选择与扩展名、尺寸/分辨率、JPEG 铺底、透明格式、取消/覆盖与实际输出解码；高位深/CMYK 不能提前显示为可用 |
| 应用内在线更新检查 | `IO/UpdateDriver.swift`、`CompositorApp.swift:102`，使用 macOS 更新框架 | **无需原样迁移**。Windows 已按用户约定使用双击新版安装包覆盖升级 | 保留 Windows 安装器更新方案，不把 macOS 框架加入 Windows；继续遵守精确卸载、占用文件拒绝和升级回滚要求 |

Camera Raw **滤镜**现有的降噪和镜头功能要保留：`src/filters.rs:89–90` 已有 detail/optics 参数；`src/camera_raw.rs` 的 `detail`、`optics` 已实现亮度/颜色降噪、通用镜头畸变、去色差及紫/绿去边。这与上表“RAW 原始显影”是两个阶段，不能相互替代，也不应重复作为缺失功能实现。

Camera Raw 新的查找表还包含调色语义变化：原版 `Document/CameraRawTables.swift` 从 `Resources/CameraRawTables.bin` 读取实测颜色表，`CameraRawShadows.swift` 从另一份资源读取自适应阴影系数。当前 Windows 的解析公式不能直接被视为这些新表的等效 CPU 实现。应先以原版新 CPU 路径为参照完成数学迁移，再证明 WGSL 与该参照的像素等效；不能仅把旧公式放到 GPU 就宣布原版加速已搬完。

## 工程格式是迁移门槛

当前 Windows `src/project.rs` 写入 manifest 11、单文件容器 1，读取老式目录包的 manifest 1–11。原版当前写入 manifest 14。**现有 Windows 会明确拒绝新版工程；不要仅把读取范围改成 1–14。**

| 原版 manifest | 新语义 | Windows 需要满足的保存/读取条件 |
| --- | --- | --- |
| 12 | 图层 PNG 可为 16 位/通道；蒙版仍是 8 位 | 资源位深检查、高精度资产与编辑历史、16 位原像素保存/重开；未实现时继续明确拒绝，避免静默降成 8 位 |
| 13 | 图层资源可为 `<UUID>.exr`，保留超白亮部 | EXR 路径/内容/内存校验、半浮点资产、亮部编辑和可逆保存；不能只允许文件后缀 |
| 14 | `shape.smoothing` 允许启用平滑矩形圆角 | 预览、编辑重绘、缩放重绘和重新打开使用同一几何；旧版清单不能宣称承载该新语义 |

升级时要同步修改 `src/project.rs`、`docs/PROJECT_FORMAT.md` 与 `AGENTS.md`，并保留 Windows 单文件保存方式及旧目录工程只读迁移规则。原版资产使用预乘 alpha，Windows `Raster` 使用 straight alpha，转换点必须写明并测试；直接照搬像素缓冲布局会产生透明边缘误差。

## 可以立即分工的实现模块

以下是待执行分工，不代表已提交业务代码。多个任务都会触及 `src/app.rs` 或面板文件，建议各模块先提供独立逻辑，由一个集成人员接 UI，避免并行覆盖。

建议执行顺序：

1. **先补独立交互和普通精度功能**：填充色顺序、中键与光标、新建单位/背景、画布 90° 旋转、命令搜索、Canvas Only、Last Filter、Navigator、文件夹导入。现有保存格式可继续用 11。
2. **并行补滤镜和 Camera Raw**：Scanlines、渐进模糊、分区重置、示波器采样；查找表按“新 CPU 参照 → GPU → 回退/数值/耗时验证”推进，保留原图预览。
3. **优先完成高精度基础再开放格式 12/13/14**：16 位、HDR、保存/重开/导出精度先成链，再开放 Smooth Corners 的 14 版工程；几何和 UI 可提前独立实现，但不提前承诺新版工程兼容。
4. **接入 RAW 显影能力与 CMYK 工作流**：需要真实相机与 ICC 样本及解码/转换能力验证。高级统一导出面板在这些能力可用后暴露对应选项，普通图片/PDF 现有流程持续保留。

| 模块 | 核心文件分工 | 集成点与验收 |
| --- | --- | --- |
| A. 工作区与命令 | 新建 `src/command_palette.rs`、`src/navigation.rs`；在现有 `src/app.rs` 保留唯一命令分发 | 搜索、Canvas Only、中键平移/缩放、Navigator 坐标先做纯逻辑；`src/app_chrome.rs` 接显示，`src/layer_panel.rs` 接中键列表。输入焦点、面板切换、无文档状态独立验收 |
| B. 新建和画布操作 | `src/welcome_ui.rs`、`src/edit.rs`、对应测试；可把换算放独立 `src/canvas_units.rs` | 单位/背景与 90° 旋转互不依赖高位深；GUI 由集成人员接 `src/app.rs`，CLI/Agent 复用 `edit` 操作。原尺寸/蒙版/选区/参考线与撤销核对 |
| C. 滤镜补齐 | `src/filters.rs`、新增 `src/scanlines.rs`、新增 `src/progressive_blur.rs`；算法与 UI 分开提交 | `src/filter_panel.rs` 与 `src/filter_interactions.rs` 最后接入；Last Filter 在成功提交处接入。旧 CRT 抖动保留，参数和像素测试参考上游用例 |
| D. Camera Raw 加速与重置 | `src/camera_raw.rs`、独立查找表/统计模块、WGSL 文件；先不改通用画布 GPU 管线 | 对照 `CameraRawTables.swift`、`CameraRawShadows.swift` 与两份 `.bin` 资源确认数学语义；CPU 为等效基准，GPU 回退和舍入修正必须验证。统计与分区重置经 `src/filter_panel.rs` 集成 |
| E. 高位深与格式 | `src/model.rs`、`src/history.rs`、`src/project.rs`、`src/image_io.rs`、`src/render.rs`、`src/gpu.rs` | **先完成资产/历史/持久化设计，再让其他模块依赖它**。16 位和 HDR 各自具备真实样本往返、导出和亮部/低位精度验证后再升级格式声明 |
| F. RAW / CMYK / 导入 | RAW：`src/image_io.rs` 显影链与设置；CMYK：独立 `src/color_management.rs` 后接现有导出；文件夹：`src/import_worker.rs` 前的目录解析 | RAW 与 CMYK 不是同一模块，适合先拆纯接口再集成。新解码/转换库需评估体积、Win10 可用性、许可证与错误恢复；不预置没有样本验证的开关 |

当前迁移状态应以本表和实际代码为准。完成一项后补上运行过的测试、可复现样本与交付包身份，再更新对应状态；原版测试文件存在、Windows 有同名菜单或单次编译成功，都不足以把整项改成“完成”。
