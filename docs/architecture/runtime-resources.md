# 运行时资源所有权与子步提交

本轮先落实架构审查末尾建议的一致写入和人群热路径，并把运行时 CPU/GPU 资源以及实验自定义资产纳入同一套生命周期设计。现有 Vulkan 三线程流水线、世界时钟及旧场景格式是兼容边界。

## 调研依据

Epic 公布的 UE6 路线描述了 UE 与 UEFN 的整合、模块化和新数据模型。公开路线没有给出一个足以复刻的 UE6 内存分配器。以下具体机制取自 Epic 已公开的引擎文档。[UE6 路线](https://www.unrealengine.com/news/the-road-to-ue-6) · [数据模型与模块化方向](https://www.unrealengine.com/news/an-update-on-blueprints-and-the-path-to-ue6)

UE 的派生数据缓存通过可重建产品降低重复构建开销。Virtual Assets 将大载荷与描述分开，主要解决制作与分发环节。运行时还需要决定谁持有解码结果，以及 GPU 的最后一次访问何时完成。Render Lab 保留可独立读取的场景包，在需要跨场景共享的自定义产品上使用有版本的键。[派生数据缓存](https://dev.epicgames.com/documentation/unreal-engine/using-derived-data-cache-in-unreal-engine) · [Virtual Assets](https://dev.epicgames.com/documentation/unreal-engine/overview-of-virtual-assets-in-unreal-engine)

UE RDG 根据图内资源生命周期计划瞬态复用，外部资源由调用者持有。Render Resource Viewer 将资源占用与所有者关联。这里沿用已有的 RHI 物理分配计数和队列时间线，将 CPU 资产缓存与图内别名分别计量。[UE RDG](https://dev.epicgames.com/documentation/unreal-engine/render-dependency-graph-in-unreal-engine) · [Render Resource Viewer](https://dev.epicgames.com/documentation/unreal-engine/render-resource-viewer-in-unreal-engine)

## 所有者、身份与释放点

| 类别 | 身份和所有者 | 复用与释放 |
| --- | --- | --- |
| 世界组件和可写模拟状态 | 世界实体与组件键，子步内按依赖顺序提交 | 原始 JSON 或已解码的类型值只允许一个权威写入值。存档逐行输出。失败子步撤销写入。 |
| 姿态和只读渲染快照 | 骨架布局、实体生命期、姿态版本 | 调色板和快照通过 Arc 共享，只有不同结果才能导致写时复制。分别统计变化和复制。 |
| 自定义 CPU 资产 | 种类、构建版本、输入身份及模式 | 只读产品在使用者之间共享。对适合持久化的二进制产品，内容与构建版本决定磁盘身份。活动使用者决定内存生命周期。载入失败允许重试。 |
| 持久 GPU 数据 | 已有 RHI 驻留输入、版本化表、历史或静态 BLAS 各自持有引用 | 相同版本可以跨帧共享。历史依赖视图或几何的语义。模型卸载后仍由在途槽持有 GPU 所需引用直到退役。 |
| 瞬态 GPU 缓冲 | RDG 资源、首末访问和物理队列 | 在已有先行关系覆盖前一资源全部访问时，允许容量足够的兼容缓冲复用。排除地址外泄、持久历史、AS scratch、独立输出和其他特殊用途。物理容量和逻辑长度分别处理。 |

与资源规模无关的小控制数据保留在原模块。CPU 查询、图编译和 GPU 生命周期各自使用合适的数据结构，避免在每个资源访问时争用中央锁。

## 热路径的实施顺序

1. 统一类型化与 JSON 组件的修改语义，变换在进入世界时检查有限性。覆盖替换、旧档、未知组件、实体 ID 重用和失败回滚。
2. 对热组件提交和撤销降低行查找与小分配，按需复制姿态数据。用 500 名角色、六个固定子步、96 个样本统计工作量和 P95，并和独立冷提取核对。
3. 为自定义产品接入共享和版本识别，优先验证动作库及纹理压缩产物。验证同键共享、最后一份引用释放、重建与缓存损坏恢复。磁盘数据可删除后重建。
4. 在 Vulkan 现有瞬态缓冲分配路径实行容量兼容复用。检查跨队列无依赖、同一 pass 同时访问以及实际绑定容量。用真实 GPU 图验证像素或缓冲内容，同时报告设备分配和逻辑复用字节。
5. 森林沿用相机、单树、共享树皮三种输入。测试结果区分 CPU 提取、求解、GPU 跨度、缓存文件和物理显存。

## 流式页设计

Epic 公开的 Nanite 资料中，层级元数据常驻。遍历可以看到当前驻留边界之外，提出理想细节请求；实际绘制使用足以覆盖表面的已驻留祖先。虚拟纹理由 GPU 反馈可见 tile 的需求，CPU 获知需求存在帧延迟，物理页池与页表分开。[Nanite 团队 SIGGRAPH 讲义](https://advances.realtimerendering.com/s2021/Karis_Nanite_SIGGRAPH_Advances_2021_final.pdf) · [Streaming Virtual Texturing](https://dev.epicgames.com/documentation/en-us/unreal-engine/streaming-virtual-texturing-in-unreal-engine) · [虚拟纹理物理池](https://dev.epicgames.com/documentation/unreal-engine/virtual-texture-memory-pools-in-unreal-engine)

每个流式资产有不可变清单、常驻粗级和独立细节页。以 64 KiB 为物理页的单位；逻辑页约定有效字节和内容版本。贴图、几何和骨骼各自有独立的细节选择。第一版以完整级别作为提交单位，单个级别可以跨页。后台读取按页进行，前台限制每帧接纳的页数。页未齐、请求失败或预算不够时继续画粗级。发布新映射时冻结当帧的地址表；RHI 只能在最后一次 GPU 使用完成后回收页。细化请求从已完成帧得到，GPU 无需等待 CPU 返回请求。实验在同一相机下分别强制粗级、延迟提供细级、以及全部驻留，并检查实际像素。

### 文件路径、传输队列和帧预算

GPU 根据屏幕上的实际使用反馈期望细节。CPU 消费已退役帧的请求，处理文件范围、驻留和预算。每帧有请求数、在途页数、上传字节限制，后台读取遇到慢盘时继续使用父级。流式 copy 是 RDG transfer pass，通过时间线信号量与消费队列衔接。物理队列不足时沿用 RHI 的同队列映射。

Windows 的 DirectStorage 可以向 D3D12 缓冲写入。Vulkan 的 NVIDIA 解压与间接复制扩展本身没有打开和读取文件的接口。原生路径采用同适配器的 DirectStorage 文件队列与可导入的 GPU 缓冲；可移植路径只在后台读取本次请求的文件页，通过暂存拷贝。两种后端应分别标记。系统内部是否使用中转内存取决于磁盘、驱动和运行时。[NVIDIA RTX IO](https://developer.nvidia.com/rtx-io) · [Microsoft DirectStorage](https://github.com/microsoft/DirectStorage)

### 从请求到销毁

`Streamer::from_packages` 把多个包交给同一个页池和后台文件线程。实验的 32 位反馈字最多描述八个独立部分，每部分有常驻根和最多四个细级。解码时按粗到细分配容量，整个级别的页就绪后才提升。两张图同时读取一个物理页时，两张图各有一份引用。后台文件读失败保留根级；DirectStorage 导入失败会改用分段文件读取；设备页分配超预算后隔 60 个主视图构建再试。[页管理](../../src/streaming.rs) · [包格式](../../src/streaming/package.rs) · [Vulkan 资源准备](../../src/rhi/mod.rs)

| 时刻 | 新帧能看到什么 | 物理内存发生什么 |
| --- | --- | --- |
| GPU 首次看到表面 | mesh shader 用当前级几何和骨骼生成可见性，resolve 用命中 UV 采样当前级纹理 | resolve 用相邻可见像素的 UV 足迹计算各部分的需求，各工作组归并后写反馈 |
| RHI 读到已退役的反馈 | 保持当前精度 | 主视图默认每帧最多接纳 4 个文件请求、提交 2 页复制（128 KiB）；等待文件或等待复制的页合计上限为 8 |
| 细级传输完成 | 下次构图选择完整细级，把对应设备地址写入新帧的地址表 | 传输队列发出 timeline 值，读取簇或纹理的队列建立可见性。RHI 的三队列配置在同一通用队列族内为 transfer 分配第 3 条队列。双队列配置和单队列配置把 transfer 映射到图形队列 |
| 拉远、失去可见性或显式卸载 | 收到较低精度反馈后重新选级；显式 `unload_part` 立即使后续地址表省略该部分，迟到的反馈无法将它复活 | 已构建的图、上传命令和在途 GPU 提交继续持有旧页。常规帧释放暂存、导入的共享缓冲和栅栏 |
| 最后一个读者退役 | 下次申请可以使用相同页槽 | 最后一份引用向 RHI 投递回收事件。每次清理最多处理 8 个事件；队列空闲时也处理。RHI 先核对该槽目前的所有者，再销毁缓冲、归还子分配。专用块内最后一页归还后释放 Vulkan device memory |

默认细页池最多 32 页，即 2 MiB **有效页容量**。常驻根、地址表、在途传输源和驱动内部资源另计。块内可复用空间仍占承诺显存；观察 `managed.device_local.stream_page` 看已绑定页，观察 `managed.stream_pool_bytes` 看页池专用块。设备要求专用分配或预算只容得下单页时，分配器可以退回单独分配。DirectStorage 的共享传输源额外计入帧资源字节；系统的私有 staging 和驱动总占用不在该 Vulkan 管理器的快照内。[分配器](../../src/rhi/allocator.rs) · [内存计数](../../src/rhi/memory.rs) · [DirectStorage 数据流与内部 staging](https://github.com/microsoft/DirectStorage/blob/main/Docs/DeveloperGuidance.md)

### 世界逻辑分区

`world-stream` 有持久运动场景和三个独立的逻辑单元，每个单元六个带重力的实体。空间格索引查询相机以及 `Engine::streaming_sources` 提供的流式源。启动半径是 9，预取半径是 14，卸载半径是 18。进入预取范围时后台读文件，进入启动范围时把整单元插入 ECS，当次世界 tick 即可推进模拟。离开 14 后停止推进，离开 18 后释放预取副本。一帧增删实体合计最多 32 个，读和写合计最多 2 个在途任务。单元定义中的实体数量必须适合这一帧预算。[分区调度](../../src/scene/partition.rs) · [单元格式](../../src/scene/partition/cell.rs) · [Epic World Partition](https://dev.epicgames.com/documentation/en-us/unreal-engine/world-partition-in-unreal-engine)

有状态单元退场时冻结一份行数据交给后台写临时检查点。写成后释放行数据，重入时再加载。保存场景会在存档旁发布按内容命名的 `cells/` 文件，然后原子替换根文档。复制存档时需要同时复制这个目录。叶实体可以持有持久的网格、材质和几何代理引用，行内保存变换和插件组件。父子关系跨单元迁移、活动角色自动改属单元和全世界 HLOD 留给后续工作。[存档](../../src/scene.rs) · [带运动与物理的预设](../../src/experiments/movement_lab.rs)

### 接入与复现

普通构建使用后台文件读取和 Vulkan transfer。Windows x64 MSVC 构建可设置 `RENDER_LAB_DSTORAGE_SDK` 指向官方 NuGet 包中包含 `include/`、`bin/x64/` 的 `native` 目录，同时配置 `VULKAN_SDK`。构建脚本将两个官方运行库放在可执行文件旁。启动时检查同一适配器的 LUID、可导入 D3D12 资源及时间线栅栏。严格测试要求所有成功上传均走 DirectStorage。[SDK](https://www.nuget.org/packages/Microsoft.Direct3D.DirectStorage/1.3.0) · [桥接实现](../../src/rhi/storage_bridge.cpp)

```powershell
cargo run --release --locked -- --experiment streaming
cargo run --release --locked -- --experiment world-stream
cargo test --release --locked --lib preload_activate_simulate_unload_save_and_resume
cargo test --release --locked --lib streamed_pixels_refine_then_release -- --ignored --nocapture
```

在准备好 SDK 的终端里设置 `$env:RENDER_LAB_REQUIRE_DIRECTSTORAGE='1'` 可将最后一项变成严格传输后端测试。研究场景已经按三种细节实际绘制，旧森林 DAG、普通材质图集及银狼完整动画库各自沿用原有驻留路径。此版以完整级别为发布单位，距离可按任意可见纹理 tile、单个 DAG 页和动画片段寻址的生产系统还有粒度差距。世界层和资产层有各自的实验入口。[渲染场景与 GPU 回归](../../src/experiments/streaming_lab.rs) · [测量数据](performance.md)
