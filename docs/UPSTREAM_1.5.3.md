# 原版 1.5.3 迁移对照

核对日期：2026-10-17。原版为 [Compositor v1.5.3](https://github.com/robbietilton/Compositor/tree/v1.5.3)，本地源码位于 `D:\Desktop\Compositor To Win`；Windows 工程位于 `D:\Desktop\CompositorWin`。生产实现使用 Rust／WGSL，不编译或加载原版 Swift／C；原版 C 只用于开发时生成参照数据。

Windows 当前版本为 **0.0.4**，下载见 [v0.0.4 发布页](https://github.com/shuguang-liming/CompositorWin/releases/tag/v0.0.4)。0.0.4 补充明确的 PDF 导入入口；下文保留 0.0.3 迁移验收记录及其产物身份。0.0.3 加入 16 位与 HDR 图层、CMYK 印刷输出、Camera Raw 大图改进、扫描线与渐进模糊、平滑圆角、命令搜索及导航小地图；下表逐项说明范围。原版 1.5.3 与 Windows 版使用各自的版本号。

本文区分“已写入生产路径”“已运行模块验证”和“最终应用验收”。“已接入”表示功能代码与调用已经落盘；本轮整仓检查、715 项测试、另两项显卡测试、Release、CLI 和便携包验证已经完成，实际范围及 GUI／安装验收状态见文末。模块通过不等同于全部交互均已验收。

## 对照依据

- 原版 `appcast.xml` 标明 1.5.3、build 49；当前工程清单版本为 14。
- 旧迁移记录收录 250 个原始文件，本轮差异记录为 101 个修改、38 个新增。这是文件数，不是功能数量。
- 三个关键基线 `CameraRaw.swift`、`ShapeTool.swift`、`docs/project-format.md` 的 SHA256 与官方 v1.4.5 完全吻合；未据此声称全部旧文件都来自同一提交。
- 原文件身份、官方基线和差分：`build/validation/upstream-1.5.3-20261016/source-review/`。其他模块证据也位于 `build/validation/`，不随软件分发。

## 1.5.3 本次发布的七项变化

| 功能 | 原版依据 | Windows 实现及状态 |
|---|---|---|
| 大图 Camera Raw GPU 加速 | `Document/CameraRaw.swift`、`CameraRawTables.swift`、`Rendering/MetalCameraRawLookup.swift` | **已接入**新版实测 LUT、自适应阴影、合并颜色阶段及 WGSL 查表。`src/camera_raw.rs`、`src/camera_raw_lookup.rs`、`assets/camera_raw_lookup.wgsl`；复用现有 Device／Queue，大于 800 万像素自动选择，GPU 失败回退 CPU，舍入边界用 CPU 修正。CPU 小图与原版 C 逐字节对照、GPU 实机对照已通过 |
| Camera Raw 滑块保持原始分辨率 | `Document/Filters.swift` | **保留并接入加速**。`src/filter_panel.rs`、`src/app.rs`；拖动及应用保留原生像素网格。已有 native-preview 测试保留，最终 GUI 验证需检查拖动、松手和确认结果 |
| 更快的直方图与示波器 | `Document/CameraRaw.swift`、`Rendering/AdjustPixels.c` | **已接入**每方向 4／8 步采样、16 行带对齐、共用缓存。`src/filter_panel.rs`；只减少显示统计成本，自动色阶继续全像素统计。10 项隔离回归和统计耗时实测通过 |
| 矩形 Smooth Corners | `Document/ShapeTool.swift`、`UI/ShapeControls.swift` | **已接入**平滑圆角几何、选项、预览／提交／缩放重绘。`src/smooth_corners.rs`、`src/edit.rs`、`src/app_tools.rs`；启用该语义的工程写清单 14 |
| 形状填充色移到参数前 | `UI/ShapeControls.swift` | **已接入**三种形状控件顺序调整，`src/app_tools.rs`；复用原选色流程 |
| 中键拖动图层面板 | `UI/NativeLayerList.swift`、`MiddleButtonTests.swift` | **已接入**专属拖动状态和边界处理，`src/layer_panel.rs`；不改变选择／历史，最终 GUI 验收覆盖释放、短列表与子控件 |
| 缩放反向立即更新光标 | `Rendering/EditorCanvas.swift`、`MiddleButtonTests.swift` | **已接入**中键平移、Ctrl＋中键缩放及方向光标，`src/canvas_navigation.rs`、`src/app.rs`；静止保留方向，反向即时更新 |

## 从旧移植基线累积的其他变化

这些能力存在于当前原版，不全是在 1.5.3 单次发布时新增。仅凭覆盖文件不补造首次发布版本。

| 能力 | Windows 实现／状态 | 验收重点或真实差异 |
|---|---|---|
| 命令搜索面板 | **已接入** `src/command_palette.rs`、`src/app.rs` | 使用现有命令分发；搜索、键盘选择、无文档禁用和中文输入由整仓／GUI 验收确认 |
| Canvas Only | **已接入** `src/app.rs`、`src/app_chrome.rs` | 隐藏工作面板；文字输入和对话框不抢单字母快捷键；不等同于窗口最大化 |
| 重复上次滤镜 | **已接入**最后一次成功确认的种类和参数，`src/app.rs` | 取消／失败不替换记录，作用于当前图层／选区，保留单步撤销 |
| 新建画布单位与背景 | **已接入** `src/canvas_units.rs`、`src/welcome_ui.rs` | px／in／cm／mm、DPI 换算及透明／白／黑背景；DPI 范围仍为 1–9600 |
| 画布顺／逆时针 90° 旋转 | **已接入** `src/edit.rs::rotate_canvas`、GUI／自动化入口 | 图层、独立蒙版、选区和参考线一同旋转，保持源像素；不是图层旋转命令 |
| Navigator 小地图 | **已接入** `src/navigator.rs`、`src/app.rs` | 高倍率点击／拖动定位、坐标逻辑与缩略图缓存；不因选区变化重算图像 |
| 独立 Scanlines 滤镜 | **已接入** `src/scanlines.rs`、`src/filters.rs`、`src/filter_panel.rs` | 保留旧 Dither 内 CRT。46 组关闭辉光的 C 对照零差异；辉光单独验证，不宣称等同 Apple 高斯采样 |
| 渐进模糊与画布控制线 | **已接入** `src/progressive_blur.rs`、`src/progressive_canvas.rs`、`src/filter_edit.rs` | 线性／径向、两个非零端点、反转、旋转、选区及扩边；使用 Rust 多尺度高斯，覆盖原版行为测试，不宣称等同私有 `CIMaskedVariableBlur` 的每个像素 |
| RAW 传感器降噪及高光恢复 | **已接入** `src/raw_develop.rs`、`src/image_io.rs`、显影面板 | 在白平衡、相机矩阵及量化前处理；完整导入从浮点显影直接生成 U16，EXIF 方向保留低位，限尺寸预览仍为稳定抖动的 RGBA8。覆盖 Bayer／RGB RAW 合成 DNG，尚无真实相机样片画质对照。解码器不提供镜头 profile／校正 opcode，自动配置项禁用，强行请求明确报错 |
| Camera Raw 十分区独立重置 | **已接入** `src/filter_panel.rs::reset_camera_raw_section` 和编辑器事件清理 | 仅重置指定分区并重新启用；混色点选择与几何引导草稿同时清理。隔离回归通过 |
| 文件夹导入 | **已接入** `src/image_folder.rs` 和打开／拖放队列 | 只展开直接子图片、自然排序；保留 `.comp` 目录识别，不递归任意文件夹 |
| CMYK 印刷／软打样／色域警告 | **已接入** `src/color_management.rs`、`src/print_export.rs`、GUI | 真实 CMYK ICC，RGB 编辑；输出四通道 JPEG／TIFF／PDF 并嵌入 ICC。11 项模块测试和独立文件解析通过；使用用户／系统 profile，不宣称 PDF/X 或跨 CMS 像素等效 |
| CMYK PSD／PSB 导入 | **已接入** `src/psd.rs` | 接受 8-bit RGB／CMYK，处理 Photoshop 反相墨色；读取资源 1039 内真实 CMYK ICC 的 A2B 转换。无配置或损坏配置时明确提示，使用设备 CMYK 数学近似，不把 RGB ICC 当作 CMYK。23 项独立回归通过，其中 4 项新增；原版本身也拒绝 16-bit PSD |
| 16-bit 图层／编辑／保存／导出 | **已接入** `src/deep_pixels.rs`、模型／历史／滤镜／编辑／画笔路径、`src/deep_io.rs`、`src/project.rs` | 权威 U16、COW、独立显示缓存；native 色阶／曲线／曝光／模糊、颜色 cube、原版 4／10 次精度重构；变换／复制／合并和撤销保低位；保存 16-bit PNG，导出 PNG／TIFF。Dither 明确转 8-bit。最终整链验收为 `tests/deep_pipeline.rs` |
| HDR 图层／EXR／Radiance HDR | **已接入** half-float extended-sRGB 图层、浮点合成、曝光及 EXR 工程资源／导出 | 保留超白亮部，降低线性曝光可找回细节。EXR 关联 Alpha／线性 RGB，目前假定 Rec.709／sRGB 色度；自定义色度未转换。浮点＋ICC 超范围明确拒绝，不静默截白 |
| 统一导出与位深／尺寸／印刷选项 | **已接入** `src/export_ui.rs` 和 `image_io`／`print_export` | 保留 PNG／JPEG／TIFF／BMP／GIF／WebP／PDF，新增 EXR 与印刷选项；实际通道／铺底／尺寸／取消／覆盖由最终验证确认 |
| 图像大小与图层效果 | **已修正** `src/edit.rs::resize_image` | 分辨率修改且像素尺寸不变时保留文字／形状；真正重采样按原版光栅化，效果尺寸／距离／模糊按几何平均比例缩放并限幅，角度不变；深层原精度采样 |
| Color Overlay | **已修正** `src/filters.rs` | 只改图层源 RGB，保持 Alpha，不把阴影一起染色；新增原版三个透明／混色／阴影验收；深层浮点效果保留源低位和 HDR |
| Content-Aware 修复与填充 | **已接入**新版 PatchMatch，`src/patch_match.rs` 和画笔入口 | 新 `heal0`／`contentFill` 使用实际 1.5.3 C 新参照；未改的 proximity／texture 继续旧参照，保持零容差 |
| 应用更新 | **保留 Windows 安装器方案** | 按用户约定双击新版安装包覆盖升级，不迁入 macOS 更新框架；卸载仅处理所属文件 |

Camera Raw **滤镜**原有的 detail／optics 继续保留，包括亮度与颜色降噪、通用畸变、去色差及紫／绿去边；与 RAW 文件的传感器显影是两个阶段。

## 工程和高精度边界

Windows 默认仍保存为一个 `.comp` 文件，容器版本仍为 1；读取旧目录包但不原地覆盖。清单 1–14 可读，普通 8-bit 写 11，U16 至少 12，HDR 至少 13，平滑圆角用 14。图层可以是 8／16-bit PNG 或 HDR EXR，蒙版仍为 8-bit 灰度 PNG。详见 [PROJECT_FORMAT.md](PROJECT_FORMAT.md)。

权威深层与显示缓存分别存储，撤销／内存预算包含两者；未改源低位不会被预览覆盖。深精合成目前用后台 CPU 浮点路径，通用 GPU 画布遇到深层回退；这与 Camera Raw 专用 GPU LUT 是不同路径，不能宣传所有 16-bit／HDR 绘制都已由 GPU 加速。

## 已运行的模块验证

| 模块 | 实际运行的证据 | 限制 |
|---|---|---|
| Camera Raw CPU／GPU | `build/validation/camera-raw-1.5.3-20261017/`：14 组小图 C 零差异；GPU 恒等／实测表、舍入、透明、重绑和设备销毁回退通过 | 包含新增阴影用例的最终整仓测试已通过，另两项显卡测试也已运行通过；见文末，不代替 GUI |
| Camera Raw 查表耗时 | RTX 3050 Laptop／Vulkan，开发 opt-level 1，预热后：1920×1080 CPU 17.33 ms／GPU 5.82 ms；3840×2160 89.12／18.26 ms；6000×4000 226.30／54.94 ms，输出零差异 | 仅查表步骤，不是整套滤镜或 Release 性能承诺 |
| 示波统计／重置 | `upstream-153-scopes-20261017/`：10 项隔离回归；四种尺寸计数／统计中位耗时在 `validation.json`／`benchmark.csv` | 只减少显示统计成本，原图和自动色阶保精度 |
| Scanlines | `upstream-153-scanlines-20261017/`：3 项模块测试，46 组关闭辉光的 C golden 零差异 | 辉光另验透明与有效性 |
| 渐进模糊 | `upstream-153-progressive-20261017/`：5 项核心；隔离集成中 filter_edit 15 项、filter_panel 14 项等通过 | 最终整仓及 Release CLI 回归已通过，GUI 观察见文末 |
| CMYK | `upstream-153-cmyk-20261017/`：11 项模块测试；Pillow 验四通道／ICC，pypdf 严格解析，Windows PDF 服务重新导入 | 真实系统 RSWOP.icm，不宣称跨 CMS 零差异 |
| RAW 完整导入 U16 | `upstream-153-raw-u16-20261017/`：当前生产模块隔离集成 4 项通过，合成 Bayer／线性 RGB DNG、4 组参数、非 257 倍数低位、7×6 图上 8 种 EXIF 方向逐 u16 一致，预览保持 8-bit | 无真实相机样片；最终整仓检查已通过，见文末统一记录 |
| DeepPixels | `deep-pixels-1.5.3-20261017/`：10 项独立生产模块通过；原版 split／merge 四组逐 u16 一致，覆盖 Alpha／混通道／全部有限 half／细线面积缩小／HDR | 最终整仓包含 deep_pipeline 5 项通过，Release CLI 另验 PNG／TIFF 低位与 HDR 往返，见文末 |

## 发布前功能集成验证（2026-10-17）

以下证据位于 `D:\Desktop\CompositorWin\build\validation\`，验证记录与缓存不随源码或软件分发。下表记录版本号调整前、标识为 0.0.2 的功能集成基线，保留其实际测试结果；其中 EXE、ZIP、安装包的体积和 SHA256 仅对应该次基线产物，不用于核验 0.0.3 正式包。当前应用版本为 0.0.3，正式下载及校验信息以 [v0.0.3 发布页](https://github.com/shuguang-liming/CompositorWin/releases/tag/v0.0.3) 为准；这里的 1.5.3 指原版功能对照版本。

| 范围 | 实际结果与证据 |
| --- | --- |
| 整仓检查和 Release | `pr-split-20261017/program/all-final-05/build-all.json`：`cargo fmt --check`、离线锁定依赖的 all-targets check、严格 clippy（`-D warnings`）、完整 test、Release 均退出 0。测试为 lib 654、bin 15、C 对照 5、CLI 8、主题 11、深层整链 5、GPU 合成 13、IO 4，合计 **715 通过、0 失败**；常规测试中另有 2 项显卡测试 ignored，随后单独实测通过。编译输入的前后摘要一致。 |
| 显卡实测 | `pr-split-20261017/program/hardware-final-05/hardware-tests.json`：RTX 3050 Laptop／DX12，Camera Raw 舍入修正、重绑、设备销毁回退，以及预备图形资源被运行时渲染器直接复用的 **2 项实测通过**。查表 CPU／GPU：1920×1080 为 17.08／7.19 ms，3840×2160 为 77.76／19.54 ms，6000×4000 为 243.63／62.54 ms，三个尺寸输出零差异。这是开发 opt-level 1 的查表数据，不是 Release 整套滤镜或 GUI 性能。 |
| Release 身份及运行库 | `pr-split-20261017/program/runtime-final-05/runtime-imports.json`：`build/rust/release/compositor.exe` 为 **53,939,200 字节**，SHA256 **`20a08ec3dd45e5fe6ce8964a43fa2fd0f040355b14c43b6629237d074a070556`**。静态 CRT 审核通过，无 VCRUNTIME／MSVCP／UCRT 动态运行库导入；已检查的 Win11 专用导入黑名单未命中。PE 为 x64 Windows GUI，含 Win10 兼容声明。驱动、动态加载组件和 WIC 编解码器仍有系统条件，导入表检查不代替 Win10 实机。 |
| Release CLI 功能 | `upstream-153-20261017/release-20261017-140634_中文路径/result.json` 首轮整体标记失败：functional 的圆角样例在 104×72、半径 32 下，平滑度自动降为 0.125，两个轮廓恰好命中相同 4×4 抗锯齿样本；不是产品修复。`smooth-fixture-diagnosis-20261017T061057Z/` 记录了原因和 QA 差分，半径改为 20 后保留原有差异断言，并增加重开像素一致断言。只重跑 functional 的 `release-20261017-141122_中文路径/result.json` **41 次操作通过**：中文路径、清单 12／13／14、选择及撤销重做、PNG16／TIFF16（最大低位误差 0）、HDR（EXR 往返误差上限 1e-5）、扫描线、渐进模糊、平滑圆角、90° 旋转、RGB／CMYK PSD／PSB 和不支持位深拒绝。两轮 EXE 的上述 SHA 均未改变；首轮失败记录保留，不把 focused 复跑称为首轮全通过。 |
| 大图与多图层 | 首轮 CLI 的 **6000×4000 单图层**及 **1600×1250 八图层**两项均通过编辑、导出、保存、关闭、重开和独立解码。单层曝光 855.46 ms、进程峰值工作集 568,893,440 B；八层活动图层曝光 61.65 ms、峰值工作集 116,887,552 B。均为本机 SHA 锁定 Release 的无界面 CPU 单次流程，含相应 IO／编解码，不是优化前后对比或 GUI／GPU 测速。完整阶段记录仍在首轮 `result.json`。 |
| 默认 GUI 启动和内置冒烟 | `upstream-153-20261017/gui-final-default-05/startup-result.json`、`smoke/gui-smoke.json`：同一 SHA 的 Release 在本机默认选择 RTX 3050 Laptop／Vulkan，首次 update 完成约 264 ms；绘画、形状、撤销、重做、保存、重开和合成 PNG 冒烟通过。`startup-window-summary.json` 的 59 次采样未观察到主窗口“出现→消失→重现”或目标 PID 的控制台窗口；约 100 ms 采样可能漏掉更短变化，未测 GPU 首次呈现，也没有 renderer generation 数据。 |
| 三种渲染模式 GUI | `upstream-153-20261017/gui-final-dx12-05/`、`gui-final-vulkan-05/`、`gui-final-glow-05/` 各自的 startup／smoke／close 报告均确认：上述 SHA 的同一 EXE 出现可见主窗口、七项冒烟通过、WM_CLOSE 正常退出 0，未强制终止。DX12／Vulkan 的 adapter 为 RTX 3050 Laptop，glow 报告的 adapter 为 null，未据此推定显卡。ready 仅指首次 update 完成，不作呈现时间或性能比较。 |
| GUI 交互观察和正常关闭 | 主代理在锁定的默认 Vulkan QA 窗口内完成中文命令搜索及旋转、中文路径图片导入、Camera Raw、扫描线及重复／撤销、渐进模糊／反向／取消、16-bit PNG 导出重开、导航器及真实拖动平滑圆角，详细操作和未覆盖项见 `upstream-153-20261017/gui-final-default-05/manual-observations.json`。`close-result.json` 确认 WM_CLOSE 后 0.5 秒正常退出，exitCode 0，未强制终止。部分窗口模式截图全黑，保留但排除视觉证明；`screenshots/smooth-corners-final-04.png` 则已经打开查看并确认有效。 |
| 便携包 | `pr-split-20261017/program/portable-20261017T061508-3636bfed/report.json` 和 `package-verification.json` 均通过，包内 EXE 与上述 SHA 一致；ZIP 为 **23,661,718 字节**，SHA256 `c86e896375e0ef555ccd2a1339a7874222140cd45542af8a53540a8af0ac7008`。嵌入许可校验覆盖 344 项依赖声明、648 份正文；包外 `LICENSES.md` 是 944 字节在线索引，所需正文已压缩嵌入 EXE。 |
| 安装、覆盖升级和卸载 | `pr-split-20261017/program/installer-qa-final-05/verification.json` 记录 **71／71 通过**，使用生产安装器同一源码、随机 QA 身份执行真实安装、实际 0.0.1 发布负载升级、同版覆盖、占用与回滚、用户工程及设置保留、精确卸载和降级拒绝。1.0.5 仅为 PE 版本策略合成夹具，不冒充历史应用版本。生产 setup 已构建并绑定源码／编译器／负载身份，为 18,000,378 字节，SHA256 `e65e557ee7758fe755d320895b99368f27e7fc24bfe0cfe88a69ad3527d16323`；此次实际执行的是随机 QA 身份的安装器，未把生产 setup 安装到用户正式 Compositor 身份。 |

GUI 手动记录还包含这些具体边界：Camera Raw 中切换直方图／矢量示波、自动白平衡显示 -81／-3、曝光 2.56、100% 原图以及取消；统一导出模态内 Ctrl+N／Ctrl+O 未穿透创建或打开文档，标签数保持不变。实际保存的 `gui-export-16.png` 为 22,679 字节、128×64、PNG IHDR 16-bit RGBA，SHA256 `f683ae599451b986d41a6447ed78a73a0749ddf50d156e7890262ed738ac8d9d`，随后 Ctrl+O 重开显示棋盘。导航器在默认关闭、800% 启用、1953.1% 拖动小地图及回到 100% 后隐藏的流程中观察正常。形状控件的填充位于半径前，半径 20／平滑启用时实际拖出 289×137 蓝色矩形，并另存 `gui-final-edit.comp`。

上述基线候选便携包和生产 setup 已生成且绑定同一 Release 身份；实际安装验收的 71 项结果属于随机 QA 身份，未覆盖用户已有正式安装。该次 `pr-split-20261017/program/final-compiled-input-check-05.json` 的编译输入 SHA256 为 `ddb6e8a7dfd28484e206d1a338dd65d97e426b5533d3115c4916fd1cf5268842`，与 All05 一致。0.0.3 的版本调整与正式打包使用独立发布记录，不将基线产物哈希沿用为新版哈希。多图层长列表中键、缩放反向光标、CMYK GUI、真实相机 RAW 和每个模态的拖放未逐项手动复测，只按对应自动测试、CLI 或模块记录陈述覆盖；本记录不声称全部手工验收。

实测主机为 Windows 11 x64（build 26200），不是 Win10。真实 Win10 专业版 22H2 机器目前不可用，因此 Win10 实机、真实相机 RAW 画质和所有交互场景尚未得到完整验收；不把导入表检查、模块测试或本机观察外推为这些环境的通过记录。
