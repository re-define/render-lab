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

运行时使用 64 KiB 的物理页。烘焙者为每个可独立使用的单元提供一个常驻根和若干细级。森林把一个可替换的簇组或一块树皮纹理登记成一个单元，织纹实验则将贴图、网格和关节分别登记。一个细级可跨多个页，全部到齐后一次发布。最多 256 字节的细级在根容量足够时与根共享缓冲和字偏移，相同根数据在同一管理器内共享。旧格式的索引仍可读取。[包格式](../../src/streaming/package.rs) · [调度](../../src/streaming/policy.rs)

### 反馈、预算与页表

GPU 向有界缓冲报告单元编号、期望等级和相对优先级。同一单元用原子最大值合并，首次请求写入最多 512 个去重编号的短表。短表溢出时从完整分数表恢复需求，同时累计溢出次数。每个管理器最多登记 16,384 个单元。主、次视图独立上报，主视图负责每帧接纳预算；次视图过期后退出合并。显式最低等级可用于预取。无可用细级时仍绘制常驻根。[着色器协议](../../shaders/stream_requests.glsl) · [读回与多视图](../../src/streaming/feedback.rs)

缺页调度先按收益排序，同收益优先等待较久的单元，预留能完整发布的表示并在剩余容量内准备中间级。默认每主视图帧最多接纳 8 个页读请求、上传 4 页（256 KiB），16 页可同时在途。最近 GPU 跨度的平滑值超出目标帧时间后，单帧上传降至 1 页。延迟从 CPU 首次收到对应需求计到首次构建可使用它的地址表，统计最近 512 次完成的 P95。未满足需求另计。慢盘、暂时的读取失败或设备容量不足时保留可绘制的较粗数据，失败读取在继续需要时退避重试。[管理器与计数](../../src/streaming.rs)

### 从硬盘到最后一次 GPU 使用

1. 后台线程按文件和传输后端归并一帧已接纳的请求，并按文件偏移排序。连续文件范围可合并读取。Windows DirectStorage 后端允许多批在途，复用同适配器共享缓冲，一批请求共用时间线点。普通后台文件读取通过 Vulkan 暂存复制，两条路径都受预算约束。[文件读取](../../src/streaming/disk.rs) · [DirectStorage 桥接](../../src/rhi/storage_bridge.cpp)
2. RDG 把复制登记成 transfer pass。RHI 查询精确的复制时间线点，允许页比整帧更早就绪；新帧冻结自己的地址表，消费队列与该上传点同步。原生后端检查同适配器和 Win32 外部内存、栅栏导入能力。同一通用队列族有三条队列时 transfer 用第三条，队列不足时使用图形队列。[时间线](../../src/rhi/queues.rs)
3. 失去需求后，映射降级；有空位时短暂保留已完成的冷页，通用默认 8 帧，森林演示 75 帧。显式卸载会撤销管理器持有的引用，旧图和在途命令继续持有页租约。RHI 收到最后引用的退役通知后回收槽位，专用块内全部页退役后归还 Vulkan device memory。[帧租约](../../src/streaming.rs) · [显存分配](../../src/rhi/allocator.rs)
4. 设备级流式预算默认 64 MiB，可用 `RENDER_LAB_STREAM_BUDGET_MIB` 配置。各管理器保留各自的逻辑容量。设备高水位时优先向含冷页的块发回收请求。监控分别报告常驻根、绑定细页、页池已分配块、待退役页和上传暂存；这些数值部分互相包含。驱动私有内存与系统内部中转另计。[设备级预算](../../src/rhi/streaming.rs) · [内存计数](../../src/rhi/memory.rs) · [Microsoft DirectStorage](https://github.com/microsoft/DirectStorage/blob/main/Docs/DeveloperGuidance.md)

### 世界分区与共享资产

单元声明带版本的资产名和启动所需的最低等级。空间索引汇总各加载源范围内的声明，重叠需求取最高等级并发布到提取快照。渲染端可报告实际达到的等级，世界在随后的 tick 检查激活条件。撤销一个单元时只撤销该单元的需求。完全卸载后重入会创建新的就绪状态，先前快照迟到的反馈无法放行新一轮激活。[单元与依赖](../../src/scene/partition.rs) · [森林消费者](../../src/experiments/vegetation/streaming.rs)

`world-stream` 的激活、预取和卸载半径分别为 9、14 和 18。实体增删预算为每 tick 合计 32 行，装配批之间检查 1 ms 的主线程时间预算。由独立叶实体组成的大单元可以烘焙成共享位置和依赖的多个原子批，各批独立激活和存档。单个批内部的执行耗时可能超过时间预算。有状态实体退出时写入后台检查点；可移动叶实体改变所属单元时保留稳定 ID、姿态和动量。存档发布内容寻址的单元文件并原子替换根清单，重复保存复用未变单元。复制存档需要同时复制旁边的 `cells/` 目录。跨单元骨架和有父子依赖的实体组还需要专门的依赖描述。[移动实体](../../src/scene/partition/mobile.rs) · [增量保存](../../src/scene/partition/archive.rs) · [Epic World Partition](https://dev.epicgames.com/documentation/en-us/unreal-engine/world-partition-in-unreal-engine)

### 接入与复现

普通构建使用后台文件读取和 Vulkan transfer。Windows x64 MSVC 的可选原生后端需要 `VULKAN_SDK`，以及指向官方 NuGet 包（含 `include/` 和 `bin/x64/` 的 `native` 目录）的 `RENDER_LAB_DSTORAGE_SDK`。构建脚本将两个运行库放在可执行文件旁。设置 `RENDER_LAB_REQUIRE_DIRECTSTORAGE=1` 运行上传回归可以核对实际成功的上传均使用该路径。[SDK](https://www.nuget.org/packages/Microsoft.Direct3D.DirectStorage/1.3.0)

```powershell
cargo run --release --locked -- --experiment forest-stream
cargo run --release --locked --example forest_stream
cargo test --release --locked --lib scene::partition::tests
cargo test --release --locked --lib streamed_pixels_refine_then_release -- --ignored --nocapture
```

`forest-stream` 使用两个相邻分区共享的一种森林橡树，验证树冠三角 DAG 和树皮 tile 的可见反馈。完整岩柱森林的体素、阴影/GI 和银狼完整动画库仍有各自的驻留路径。织纹和世界分区仍可独立通过 `streaming`、`world-stream` 运行。[实验目录](../experiment-catalog.md) · [实测与边界](performance.md)
