# 视口输入、HUD 与后处理

## 输入与 HUD

键鼠事件由 GPUI 提供。WASD 平移相机，在视口内按住右键拖动改变方向，F 重新取景。Motion Matching 场景保留 IJKL。控制台打开或窗口失焦时清除按键与拖动状态。相机姿态和取景沿用引擎里基于 glam 的实现，可由命令、场景和录制脚本共同使用。

HUD 由透明 GPUI 窗口绘制，随 Vulkan 视口调整位置和尺寸。Windows 桌面合成器将 HUD 放在交换链画面上方，窗口保持点击穿透和无焦点。HUD 显示实际呈现模式、GPU 时间和当前生效的抗锯齿，使用 `hud on` 或 `hud off` 控制。截图和引擎录制从后处理后的 GPU 场景帧取图，交互 HUD 随桌面窗口显示。

## TAA

```powershell
cargo run --release --locked -- --experiment vegetation --command "post taa"
```

使用时域后处理时，帧构建器向后端提供场景颜色及其编码、深度和逐像素运动。选中的后端追加 RDG pass，并返回最终颜色资源。RGBA32F 线性场景先累积、再做色调映射。BGRA8 场景保持原有颜色编码。呈现、截图和录制都读取处理后的资源。原生光追预览目前只输出颜色，沿用自己的帧路径。

ReSTIR 时域采样或 SVGF 启用时也会请求共享运动，即使 `post off`。它们从实际渲染的上一帧采样，额外补偿前后两帧的抖动差，并共用一次身份、深度及法线校验。单独启用它们时，渲染保持零抖动。[验证结果](lighting-comparison.md)

TAA 用 Halton 子像素抖动采样。当前帧先重采样到输出像素中心，历史颜色和重投影也统一使用无抖动的像素网格。深度差拒绝遮挡变化的历史，当前帧邻域限制历史颜色范围。

`Resolve visibility motion` 根据最终可见性 ID 和深度找到当前像素的表面点。运动向量等于该点上一帧的无抖动像素位置减去这一帧的无抖动像素位置，单位是输入渲染像素。刚体通过前后模型矩阵变换同一个点。GPU 蒙皮网格使用当前三角形的透视校正重心坐标，在上一帧姿态的三个顶点间插值。风动植被用两帧的风场、根部变换和骨骼调色板，重建相同三角形或体素表面的对应点。静止表面只需相机变换，天空使用观察方向。

运动 pass 同时写出预期的上帧深度与表面标签。TAA 对双线性历史采样的四个采样点分别核对深度、实例身份和三角簇或体素表示。运动边缘的速度扩张限制在同一实例、几何表示与深度层。静止机位的亚像素轮廓和缓慢风动另有降权的覆盖累积。三角簇和体素簇互换时，TAA 拒绝该表示上的旧样本。同类三角簇不同细节级别之间仍由深度做近似判定。

| 视口控制台命令 | 如何读图 |
| --- | --- |
| `post view motion` | 红、绿分量分别编码向右、向下的历史查找偏移。中值表示零，每轴满量程为 ±16 像素。蓝色表示速度大小，洋红色表示无法计算上一帧位置。 |
| `post view reprojection` | 把上帧原始场景颜色重投影到本帧。黑色表示吻合，橙色越亮表示颜色差越大，洋红色表示历史几何校验失败、历史已重置或采样出界。光照或描边本身变化也会产生颜色差。 |
| `post view off` | 退出诊断，恢复所选后处理模式。 |

两个诊断视图均使用原生尺寸，并关闭投影抖动。运动视图只读取这一帧的运动缓冲。重投影视图保存前一帧原始颜色，方便在固定机位分别观察角色肢体或树叶运动。

森林近景固定机位在 RTX 5070 上以 800×500 运行。每组预热 24 帧，随后比较 48 对相邻帧。前三列数据是全画面每个 8 位 RGB 通道的平均绝对差，其中有风的数据包含真实风动产生的变化。末列取当前版本整帧 GPU 时间的中位数。可用 `cargo run --release --locked --example vegetation_sweep -- wind` 复测。

| 模式 | 早期相机重投影 | 植被专用运动向量 | 统一运动向量与当前 TAA | GPU 中位数 |
| --- | ---: | ---: | ---: | ---: |
| 风关，TAA 关 | 0.0008 | 0.0011 | 0.0011 | 2.67 ms |
| 风关，TAA 开 | 1.673 | 0.288 | 0.221 | 2.92 ms |
| 风开，TAA 关 | 0.341 | 0.342 | 0.342 | 3.04 ms |
| 风开，TAA 开 | 1.703 | 0.644 | 0.599 | 3.40 ms |

窗口尺寸和渲染技术变化、相机跳切或 `post reset` 都会重建历史。RDG 通过 GPU 历史缓冲和队列时间线管理跨帧访问。`post off` 用于需要原始像素的实验对照，默认关闭后处理。

## NVIDIA NGX 接入实验

实验后端把颜色、深度和共享的屏幕运动向量交给 Vulkan NGX。适配 pass 将向量打包到 RG16F 输入图，无对应点填零。抖动通过单独的 NGX 参数传递。DLSS Quality 将渲染尺寸缩到输出的约 2/3，DLAA 保持同尺寸。图像复制、布局切换、跨队列依赖和最终颜色都在 RDG 中声明。NGX 调用在 RHI 线程串行执行，其他 pass 仍走并行录制。

在 RTX 5070、驱动 621.14、NGX 310.9.16 上，森林的 DLSS Quality 已能输出完整场景。原来的黑屏可用同一段录制逐帧复现。NGX 报告成功，但读取不到有效图像。在 NGX 使用的计算命令缓冲中先绑定一个传统 Vulkan 描述符集，颜色便恢复。引擎自己的 pass 继续使用 descriptor heap。NVIDIA 的问题追踪中记录了相同的现象与复现条件。[NVIDIA nvbug 6436675](https://github.com/NVIDIA-RTX/Streamline/issues/109)

关闭时的调用栈显示，`NVSDK_NGX_VULKAN_Shutdown1` 在 `UninitializeTelemetry` 等待，遥测线程在尝试打开 NVIDIA Container 的命名管道。引擎现在先等 GPU 工作完成，由专用线程释放 feature 和参数、执行 SDK shutdown。该线程持有 Vulkan 设备、实例、加载器和 NGX 所需路径，直到 SDK 返回。应用最多为此等待两秒。如果长寿命宿主在旧设备的 shutdown 尚未完成时重新请求 NGX，会收到关闭仍在进行的状态。进程退出时由操作系统回收仍在等待的驱动线程。

### 森林录制与复测

场景使用 2,665 棵树、相同的风和 `near` 机位。每组以 30 Hz 更新，预热 24 帧再录制 96 帧。片头 0.8 秒固定机位，随后相机目标横移 1.8 个场景单位，同时偏航 0.06 弧度。TAA 使用 800 × 500，DLSS Quality 从 534 × 334 重建到相同输出尺寸。引擎用内部渲染高度计算 Cluster LOD 的屏幕误差，因此两边也可能选择不同的远景植被细节。录制器逐帧检查后处理 pass、尺寸及画面内容。另有原生 GPU 回归在 DLSS Quality、DLAA 和奇数尺寸之间切换，将输出颜色与同场景原生渲染作比较。[对照视频](../captures/forest-dlss-quality-vs-taa.mp4) · [同帧局部放大](../captures/forest-dlss-quality-vs-taa.png)

从引擎根目录执行下列命令可复录。构建时 `RENDER_LAB_NGX_SDK` 指向 NVIDIA SDK 根目录，运行时 `RENDER_LAB_NGX_RUNTIME` 指向 `nvngx_dlss.dll` 所在目录。下例使用本机测试用的 SDK 缓存。其他机器可替换为实际安装位置。Vulkan 头文件使用 `VULKAN_SDK` 或 `RENDER_LAB_VULKAN_INCLUDE`。SDK 和 DLL 由本机提供。

~~~powershell
$env:RENDER_LAB_NGX_SDK = (Resolve-Path target/post/ngx).Path
$env:RENDER_LAB_NGX_RUNTIME = (Resolve-Path target/post/ngx/lib/Windows_x86_64/rel).Path
cargo run --locked --release --example vegetation_sweep -- aa-taa target/forest-aa
cargo run --locked --release --example vegetation_sweep -- aa-dlss target/forest-aa
~~~

后处理中输入图和输出图的格式、布局及用途声明遵循 SDK 的 Vulkan 资源约定。[NVIDIA DLSS 编程指南](https://raw.githubusercontent.com/NVIDIA/DLSS/main/doc/DLSS_Programming_Guide_Release.pdf)
