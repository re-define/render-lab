# Render Lab 全引擎模块图

从第一张图沿 `Snapshot → FramePlan → Frame` 可以追踪一帧的生命周期。后面的分图展开前端、实验和后端。实线表示数据或调用方向，虚线表示配置、可选消费或遥测。表中的源码入口相对于本仓库。线程状态与场景生命周期见[运行时与场景](runtime-and-scene.md)，GPU 数据契约见[渲染管线](render-pipeline.md)，各预设的算法与入口见[实验目录](experiment-catalog.md)。

## 一帧经过哪些模块

~~~mermaid
flowchart TB
    cli["main · CLI / GPUI / 控制台"] -->|输入与命令| main["主线程 · Engine / Schedule"]
    imp["glTF 导入 / JSON 读档 / 实验建场"] -->|构造或恢复实体| world["World · 实体与共享资产"]
    main -->|有序提交系统修改| world
    world -->|变更日志、受影响的实体| extract["SceneExtractor · 提取与增量缓存"]
    extract -->|只读 Snapshot| render["渲染线程 · Technique 构建 RDG"]
    exp["experiments · 相机、系统、材质与技术"] -.->|安装相机与系统| main
    exp -.->|安装材质与 Technique| render
    render -->|Graph| rdg["RDG 编译 · 依赖、屏障和执行顺序"]
    rdg -->|FramePlan 含执行计划与输出标识| rhi["RHI 线程 · 分配、并行录制、提交"]
    rhi -->|命令缓冲与时间线| gpu["Vulkan 图形 / 计算 / 传输队列"]
    gpu -->|交换链图像| present["窗口呈现"]
    gpu -->|完成的提交| frame["Frame · 时间戳、计数、可选像素"]
    frame -.->|最新帧报告| ui["GPUI · HUD / RDG 检查器"]
    frame -.->|请求采集时| capture["PNG / FFmpeg 视频与 GIF"]
    ui -.->|桌面合成覆盖| present
~~~

主线程和渲染线程之间传帧输入，渲染线程和 RHI 线程之间传已编译计划，各有一个有界通道。三个线程共享 Rayon 任务池，同阶段模拟、普通几何准备和命令录制分别使用它。帧报告经完成邮箱交回 UI。窗口正常显示时，GPU 直接把结果拷入交换链。

| 模块 | 输入、职责与去向 |
| --- | --- |
| [main](../src/main.rs) · [lib](../src/lib.rs) | 二进制解析参数，从[预设清单](../src/presets.rs)选择场景，启动窗口、benchmark、截图或固定步长录制。库入口导出运行时、渲染和实验模块。 |
| [engine](../src/engine.rs) · [schedule](../src/engine/schedule.rs) | Engine 持有活动世界和相机，拥有两条工作线程与帧邮箱。Schedule 让同阶段系统并行读世界并按注册顺序提交修改，支持固定或可变步长。 |
| [scene](../src/scene.rs) · [components](../src/scene/components.rs) · [light](../src/scene/light.rs) | 实体通过 ID 引用网格、材质、蒙皮和父节点。World 校验、迁移和保存场景。components 管理扩展键和类型化运行时值，light 定义光源及其校验。 |
| [changes](../src/scene/changes.rs) · [incremental](../src/render/scene/incremental.rs) | 实体表在写入时记录 ID 和修改类别。提取器读取自己的游标，通过子节点、关节与 skin 的反向引用，更新快照中受影响的记录。[pages](../src/render/scene/pages.rs) 以 64 个实例为一页做写时复制。[routes](../src/render/scene/routes.rs) 分配稳定模型与材质槽位，并维护普通实例和按原型分组的簇实例。 |
| [import](../src/import.rs) · [experiments](../src/experiments/mod.rs) | glTF 导入器把资源转换成实体，校验后发布到世界。实验配置根据组件选择 technique、相机、材质程序和模拟系统，也用于恢复存档。 |
| [ui](../src/ui.rs) · [input](../src/ui/input.rs) · [viewport](../src/ui/viewport.rs) | GPUI 工作台处理更新、指令与 RDG 面板。input 汇总 WASD、鼠标和角色意图，viewport 管理原生窗口、尺寸与覆盖层。 |
| [hud](../src/ui/hud.rs) · [benchmark](../src/ui/benchmark.rs) | HUD 显示设备与帧状态，位于交换链画面上方。benchmark 从完成帧汇总时序、计数和分位数。 |
| [console](../src/console.rs) · [console/lights](../src/console/lights.rs) · [recording](../src/recording.rs) | CLI 和 UI 共用命令分发。灯光命令编辑场景实例。录制模块从完成帧取 BGRA，送入 FFmpeg，并在编码成功后发布文件。 |

组合场景由 [composition](../src/experiments/composition.rs) 分配普通几何、簇和效果几何的主绘制，并接入光照。稳定槽位由 [routes](../src/render/scene/routes.rs) 分配，两条几何路径共享 [GPU 场景表](../src/render/tables.rs)。[组合验收与森林实测](scene-composition.md)

## 可见性渲染与后处理

~~~mermaid
flowchart TB
    snap["Snapshot · 场景、相机、材质注册表"] -->|实例、网格、姿态| geom["geometry · 普通网格与 GPU 蒙皮"]
    snap -->|效果设置与河道网格| effects["effects · 水面重建"]
    snap -->|原型、风姿、屏幕误差| lod["clusters · BVH、DAG 与两阶段 Hi-Z"]
    geom -->|mesh shader 写入| vis["可见性 ID 与深度"]
    effects -->|mesh shader 写入| vis
    lod -->|mesh shader 写入| vis
    vis -->|像素归属| bins["visibility · 分类、分配、散射"]
    mat["材质程序、实例表与纹理 atlas"] -->|材质槽与几何地址| bins
    vis -->|重建当前表面| mv["motion · 表面上一帧的位置"]
    prev["上一帧相机、实例、骨骼和程序化几何"] -.->|历史输入| mv
    mv -->|时域表面对应| lighting["可选光照 · IBL、ReSTIR 与 GI"]
    vis -->|可见表面| lighting
    bins -->|每桶的像素列表| shade["材质 compute 与实验合成"]
    lighting -->|直接、间接或环境光输入| shade
    shade -->|场景颜色| post["post · 按 technique 和设置选择后端"]
    mv -->|无抖动运动、预期深度和表面标签| post
    post -->|TAA 或 NGX 重建结果| out["最终颜色 · HDR 路径执行色调映射"]
    shade -->|关闭后处理时输出| out
~~~

| 模块 | 职责与上下游 |
| --- | --- |
| [render](../src/render.rs) · [render/scene](../src/render/scene.rs) | 前者提供 Camera、Snapshot、Technique 和可见性帧构建入口。后者从 ECS 更新层级、变换、调色板、实例和实验设置，复用不可变资产。实验通过[快照注册项](../src/render/scene/components.rs)声明组件键、依赖和派生值，银狼模块提供轨迹读取与更新函数。 |
| [frame_graph](../src/render/frame_graph.rs) | `FrameProducts` 在渲染器发布最终颜色、可见性、深度、运动与射线几何。`FramePlan` 把输出标识随 RDG 指令一起交付 RHI。 |
| [geometry](../src/render/geometry.rs) · [visibility](../src/render/visibility.rs) | geometry 准备可见性绘制使用的顶点、索引和 GPU 蒙皮流。visibility 将实例、图元和深度变成着色分桶、间接派发及场景颜色。 |
| [materials](../src/render/materials.rs) · [atlas](../src/render/atlas.rs) · [render/lights](../src/render/lights.rs) | 材质名映射到帧内程序分桶。atlas 为普通纹理和压缩载荷提供寻址、对齐和缓存。灯光提取将实体变换和参数打包给 GPU。[scene/gi](../src/scene/gi.rs) 定义 GI 命中材质，[atlas/gi](../src/render/atlas/gi.rs) 将纹理采样与材质节点编成 GPU 数据。 |
| [tables](../src/render/tables.rs) · [rays](../src/render/rays.rs) · [ray scene](../src/render/rays/scene.rs) | tables 将稳定槽绑定到实例、几何地址和材质记录。rays 发布变形后顶点与拓扑。RayScene 为同帧的遮挡和最近命中查询准备软件静态/动态 BVH 或硬件 BLAS/TLAS，硬件命中属性按实例引用同一份几何。 |
| [motion](../src/render/motion.rs) · [运动着色器](../shaders/surface_motion.comp) | 结合当前最终可见表面及上一帧几何状态，计算当前到上一帧的屏幕偏移、预期深度和实例／表示标签。 |
| [post](../src/render/post.rs) · [taa](../src/render/post/taa.rs) · [post/ngx](../src/render/post/ngx.rs) | 前端管理上帧快照、抖动、分辨率和历史重置。TAA 验证并累积历史，还提供诊断视图。NGX 适配层将输入打包为图像，把 SDK 求值声明给 RDG。SDK 的实际验证状态见[后处理文档](viewport-post.md)。 |

从场景组件到相机、调度器、材质和主技术的装配由[实验注册与装配](../src/experiments/assembly.rs)负责。每个模块拥有自己的注册项，多个独占主技术或独占技术与可见性模块同时被要求时，错误列出发生冲突的键。GPU 设备能力与帧槽版本的交界见[光追能力矩阵](ray-tracing-rhi.md)。

## 实验怎样接入引擎

~~~mermaid
flowchart TB
    p["植被、风骨骼及混合三角和体素 DAG"] -->|实例与误差数据| cut["GPU 选簇列表"]
    cut -->|两阶段 Hi-Z 后的三角簇| v["可见性、深度和材质桶"]
    cut -->|两阶段 Hi-Z 后的体素簇| bricks["稀疏砖块按深度分桶"]
    bricks -->|逐像素命中面及深度| v
    anim["Motion Matching"] -->|ECS 骨骼姿态| c["蒙皮角色与卡通材质"]
    c -->|当帧几何| v
    e["台阶水流、河面与破碎消融"] -->|形变几何与覆盖| v
    comp["神经特征网格或二维高斯载荷"] -->|材质解码输入| v
    cut -->|选中簇与屏外投影簇的三角及体素表面页| clas["原生 CLAS 与 Cluster BLAS"]
    surf["普通网格、蒙皮、水面和植被代理"] -->|变形后表面、空间与拓扑| sw["软件 BVH"]
    surf -->|索引和实例| hw["常规 BLAS"]
    clas -->|森林簇实例| top["TLAS"]
    hw -->|普通几何实例| top
    top -->|硬件遮挡或漫反射最近命中| d["ReSTIR DI 与漫反射探针"]
    sw -->|遮挡或漫反射最近命中| d
    d -->|直接与间接光结果| m["材质着色"]
    v -->|可见表面与材料归属| m
    ibl["IBL"] -->|环境光输入| m
    hw -.->|常规实例建立 TLAS 并追踪主射线| rt["独立 RT 或硬件命中诊断"]
    m -->|着色 pass 与输出| g["RDG"]
    rt -->|射线 pass 与输出| g
~~~

| 实验及入口 | 内部模块如何协作 |
| --- | --- |
| [Cluster LOD](../src/experiments/clusters.rs) | [dragons](../src/experiments/clusters/dragons.rs) 创建多龙与普通网格共存的场景。[foliage](../src/experiments/clusters/foliage.rs) 在简化时校正叶片面积。[bvh](../src/experiments/clusters/bvh.rs) 构建 group 层次并打包资源，[voxels](../src/experiments/clusters/bvh/voxels.rs) 比较三角与体素候选的投影覆盖误差。[gpu](../src/experiments/clusters/gpu.rs) 将实例剔除、全局节点列表、cluster 列表、Hi-Z 和 mesh shader 绘制接入 RDG。 |
| [程序化植被](../src/experiments/vegetation.rs) | [generate](../src/experiments/vegetation/generate.rs) 生成树草网格、天空可见度、骨骼及权重，[landscape](../src/experiments/vegetation/landscape.rs) 生成地面和树冠遮蔽纹理。[river_forest](../src/experiments/vegetation/river_forest.rs) 布置河岸植被，[karst](../src/experiments/vegetation/karst.rs) 构建岩柱、碎石坡及其林地。场景复用 cluster 路径，主线程推进风场时间，GPU 生成骨骼调色板。 |
| [Motion Matching 参考实现](../src/experiments/motion.rs) · [银狼](../src/experiments/motion/silver.rs) | 参考骨架支持外部剪辑库。银狼用 [locomotion](../src/experiments/motion/silver/locomotion.rs) 推进角色和预测轨迹，用 [search](../src/experiments/motion/silver/search.rs) 按相同归一化特征执行 PCA/KD-tree 或扫描查询。[trajectory_view](../src/experiments/motion/silver/trajectory_view.rs) 将预测轨迹提取并合成到角色画面。 |
| [流水与破碎消融](../src/experiments/effects.rs) | [water](../src/experiments/effects/water.rs) 把水体采样点的重力和阶面碰撞、法线重建注册为 GPU pass。[fracture](../src/experiments/effects/fracture.rs) 用有封口的 Voronoi 单元分割晶体，ECS 更新碎块变换。消融物体在可见性光栅时裁掉表面，材质按同一个侵蚀场绘制亮边。[river](../src/experiments/effects/river.rs) 维护河道几何和水面反射，与台阶水流分用两套计算着色器。 |
| [卡通渲染](../src/experiments/cartoon.rs) | 场景配置、拓扑和面部参数交给 [renderer](../src/experiments/cartoon/renderer.rs)，复用普通几何、蒙皮和可见性路径，叠加图像、外壳或边四边形描边。 |
| [神经纹理](../src/experiments/texture.rs) · [高斯纹理](../src/experiments/gaussian.rs) | 两种实验通过 atlas 向着色器提供各自编码。高斯的 [bvh](../src/experiments/gaussian/bvh.rs) 为纹素查询组织空间索引，[compact](../src/experiments/gaussian/compact.rs) 生成紧凑记录与索引，并保留原图和扫描对照。 |
| [GI、阴影、硬件 RT](../src/experiments/lighting.rs) | [scenes](../src/experiments/lighting/scenes.rs) 提供场景几何，[geometry](../src/experiments/lighting/geometry.rs) 生成并缓存世界空间三角形和软件 BVH。[pipeline](../src/experiments/lighting/pipeline.rs) 声明 GI、阴影或硬件光追操作。硬件 RT 预览直接产出颜色。 |
| [ReSTIR 与降噪](../src/experiments/restir.rs) | [scene](../src/experiments/restir/scene.rs) 创建多光源场景。[graph](../src/experiments/restir/graph.rs) 组织选灯、时空复用和过滤。[ray_scene](../src/experiments/restir/ray_scene.rs) 将阴影和探针绑定到渲染器的共享射线场景。[diffuse_gi](../src/experiments/restir/diffuse_gi.rs) 声明探针、世界缓存和间接光，[studies](../src/experiments/restir/studies.rs) 配置森林与银狼的 GI 命中材质。 |
| [IBL](../src/experiments/ibl.rs) · [混合光照](../src/experiments/mixed.rs) | [bake](../src/experiments/ibl/bake.rs) 烘焙漫反射、预滤环境与 BRDF LUT。[light_stack](../src/experiments/light_stack.rs) 按场景组件注册环境光、直接光及材质输入。混合实验仅负责建场，独立启用任一光照输入也能驱动同场的 PBR 与 Toon。 |

## RDG、Vulkan 与跨帧生命周期

~~~mermaid
flowchart TB
    pass["Graph · pass、资源、访问声明"] -->|着色器与资源契约| checks["validate"]
    checks -->|通过校验的使用记录| deps["compile · 展开访问、构建依赖和屏障"]
    deps -->|拓扑顺序与资源访问表| plan["Plan"]
    plan -->|资源形状与用途| backend["Backend · 帧槽与资源准备"]
    device["device · 设备和能力"] -->|队列、内存、描述符属性| backend
    backend -->|独立录制对象| record["record · Rayon 并行录制"]
    pipes["pipeline、ray、clusters 与 NGX"] -->|专用原生对象| record
    record -->|命令缓冲| queues["queues · 合批并提交"]
    plan -->|逻辑队列和已展开的依赖| queues
    queues -->|图形提交与二值信号量| wsi["present · 交换链"]
    queues -->|各队列完成值| retire["提交退役"]
    retire -->|时间戳、计数与可选像素| report["Frame"]
    queues -.->|下一帧接续历史读写的时间线值| queues
~~~

| 模块 | 职责与上下游 |
| --- | --- |
| [rdg](../src/rdg.rs) · [validate](../src/rdg/validate.rs) | 图持有逻辑资源、操作和显式依赖。编译器验证使用约束与数据初始化，推导 RAW/WAR/WAW 和布局依赖，拓扑排序并生成执行计划。每个 pass 的展开访问表随 Plan 传给 RHI。[rows](../src/rdg/rows.rs) 为 GPU 表保留不可变的行快照和版本，帧槽据此比较并写入变化行。 |
| [rhi](../src/rhi/mod.rs) · [slot](../src/rhi/slot.rs) · [device](../src/rhi/device.rs) | Backend 管理提交、退役、常驻资产与历史资源。slot 为每个退役帧槽找回物理绑定，维护 GPU 工作缓冲容量。device 选择设备和队列，分配 Vulkan 内存、buffer 和 image。 |
| [pipeline](../src/rhi/pipeline.rs) · [record](../src/rhi/record.rs) | 创建并缓存兼容的着色管线和布局。录制器将计划中的操作和屏障翻译为 Vulkan 命令，各 worker 拥有独立命令池。 |
| [queues](../src/rhi/queues.rs) · [present](../src/rhi/present.rs) | queues 把逻辑队列映射到可用物理队列，合并等待并追踪跨帧时间线。present 管理 Win32 surface、交换链、尺寸变化和 WSI 二值信号量，优先 MAILBOX，回退 FIFO。 |
| [ray](../src/rhi/ray.rs) · [rhi/ngx](../src/rhi/ngx.rs) · [NGX C++ 桥](../src/rhi/ngx_bridge.cpp) | ray 实现常规 Vulkan BLAS/TLAS 所需对象。[rhi/clusters](../src/rhi/clusters.rs) 查询 NV 构建尺寸、分配对齐存储与 scratch，并录制 CLAS 和 Cluster BLAS 构建。NGX 的 Rust 层管理特性、传统描述符绑定和 SDK 异步关闭期间的 Vulkan 对象所有权。C++ 桥执行 SDK 调用。[描述符](../src/rhi/ngx_descriptors.rs) · [关闭流程](../src/rhi/ngx_lifecycle.rs)。 |
| [allocator](../src/rhi/allocator.rs) · [memory](../src/rhi/memory.rs) · [purposes](../src/rhi/purposes.rs) | 分配器选择独立内存或块内对齐区间，回收和合并空闲区间。总账本保存物理堆占用、高水位和预算。用途账本为控制台和帧报告区分绑定、驱动调用、有效长度及闲置容量。 |
| [transient](../src/rhi/transient.rs) | 帧内临时缓冲规划按物理队列的最后访问决定哪些逻辑资源可以共用同一个缓冲，交接进入原有的屏障和时间线提交流程。 |
| [frame](../src/rhi/frame.rs) | Frame、PassReport、FrameProfile 和 FrameMemory 向 UI、benchmark、采集和测试提供统一的完成帧数据。 |

对森林直接光或 GI，`render::rays` 和实验模块选取当帧几何，`rdg` 建立 BLAS、TLAS 与追踪 pass 之间的依赖，`rhi::ray` 创建原生结构、SBT 和派发命令，`rhi::pipeline` 选择并缓存管线，`rhi::clusters` 承担 NV 簇构建。使用普通代理的 KHR 路径和使用选中簇的 NV 路径最终都在 TLAS 上追踪。[森林射线数据流与能力矩阵](ray-tracing-rhi.md)

## 构建、资产与验证入口

| 入口 | 与运行时的关系 |
| --- | --- |
| [build.rs](../build.rs) · [shaders](../shaders) · [材质 shader](../shaders/materials) | 在构建时生成 SPIR-V，Rust 各 pass 引用对应产物。构建配置也控制可选 NGX 桥及 Windows 清单。[Windows 清单](../windows.manifest) |
| [资产清单](../assets/README.md) · [Kimodo 生成](../tools/generate_kimodo.py) · [银狼重定向与烘焙](../tools/bake_silver_wolf.py) · [动作索引](../tools/motion_index.py) | 离线动作、姿态和搜索索引供银狼实验加载。场景、角色和环境资产由相应实验导入或装载。 |
| [神经纹理训练](../tools/train_texture_grid.py) · [高斯拟合与打包](../tools/gaussian_texture.py) · [剪枝与消融](../tools/gaussian_ablation.py) | 离线工具产生材质解码器消费的载荷。[CUDA 梯度检查](../tools/check_2dgaussian_gradients.cu)和[反向传播补丁](../tools/2dgaussian-backward.patch)对应原高斯训练实现。 |
| [光照对照](../examples/lighting_comparison.rs) · [森林基准](../examples/vegetation_sweep.rs) · [框架](../tests/framework.rs) · [实验](../tests/experiments.rs) · [导入](../tests/import.rs) · [录制](../tests/recording.rs) | 示例走真实引擎入口采样。集成测试覆盖 API、存档、GPU 结果及录制。导入与录制测试各自创建临时目录。 |
| [光照 GPU 测试](../src/experiments/lighting/tests.rs) · [WSI 测试](../src/rhi/present/tests.rs) · [高斯解码对照](../tools/test_gaussian_texture.py) | 分别验证照明依赖与像素、交换链呈现与缩放、打包纹素与独立参考结果。其他就地单元测试随所属模块维护。 |

森林选簇的 [mega](../src/experiments/clusters/gpu/mega.rs) 将光栅选簇交给原生 AS 构建，[proxy](../src/experiments/clusters/gpu/proxy.rs) 和 [detail](../src/experiments/clusters/gpu/proxy/detail.rs) 生成软件投影几何。两路共享姿态，几何精度与建造成本保留独立对照。[同机位阴影测量](../examples/forest_shadows.rs) · [森林流水录像](../examples/forest_river_film.rs)

主视图的体素分支消费两轮 Hi-Z 输出的砖列表。计算路径负责像素任务分配、深度拒绝和命中竞争，mesh 路径供硬件回退与同列表对照。两条路径共享像素中心射线、DDA、骨骼姿态和物理命中页。最终表面 ID 同时供材质、运动向量与硬件光追使用。[RDG 注册](../src/experiments/clusters/gpu/bricks.rs) · [计算光栅](../shaders/brick_raster.comp) · [数据与测量](procedural-vegetation.md#体素计算光栅)

具体算法与测量结果见[架构与帧流程](architecture.md)，修改模块时的约定见[开发规范](../CONTRIBUTING.md)。
