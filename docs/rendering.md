# 一帧如何从几何得到画面

渲染线程为每个视图调用 `Technique::build`。默认可见性管线先确定每个像素看见的表面和三角形，再由该表面的材质选择着色程序。普通网格、簇网格和过程水面可以出现在同一机位。需要研究主射线或纹理解码的实验也可注册专用技术。每种技术用同一个 `FrameGraph` 交付输出。[渲染入口](../src/render.rs) · [实验目录](experiment-catalog.md) · [世界快照的来源](extraction.md)

## 几何、剔除与可见性

| 输入 | 从快照到可见性缓冲 |
| --- | --- |
| 普通网格 | 为每个表面登记实例、材质与索引。少于 16 个兼容表面时直接绘制。大批次先在 GPU 测试每个表面的视锥，再按原顺序生成间接 mesh draw；单包上限为 256 个表面、32,768 个工作组。共面覆盖顺序和表面编号都得以保留。[绘制列表](../src/render/draw_list.rs) · [视锥测试](../shaders/ordinary_cull.comp) |
| 蒙皮网格 | Compute 按姿态页和源网格分组，以每个实例的权重和骨矩阵生成顶点流。可见性、运动和加速结构共享相应的变形结果。普通刚体用包围盒剔除，蒙皮批次用当帧已变形的位置判断视锥。[GPU 蒙皮](../src/render/geometry/skin.rs) |
| 森林簇 | 构建时以一组子簇为单位准备三角或体素替代，比较误差。运行时先筛实例视锥，GPU 通过全局列表分发 BVH 节点和候选簇，按投影误差选择层级。上一帧 Hi-Z 先筛遮挡，画出可见三角簇后用新深度复查被挡候选。末尾处理压紧的体素砖；compute 路径对候选像素遍历砖内占用并按深度竞争，另有 mesh 路径可作对照。[DAG 构建](../src/experiments/clusters/bvh/voxels.rs) · [剔除](../src/experiments/clusters/gpu.rs) · [体素光栅](../src/experiments/clusters/gpu/bricks.rs) |
| 特殊几何 | 水面先发布本帧及需要的历史位置，随后进入可见性和反射；法线外扩、退化四边形描边可提交附加几何。组合场景装配时检查各路由认领的实例，避免一个表面重复绘制。[场景组合](../src/experiments/composition.rs) · [水面](../src/experiments/effects.rs) · [描边](../src/experiments/cartoon/renderer.rs) |

## 着色、光照与时域重建

表面和三角 ID 指向这一帧的表面表、几何流及材质槽。GPU 先按材质程序统计像素、分配区间、散射像素索引，再间接运行各材质的着色器。PBR 和卡通材质因此可以共享同一次可见性绘制。组合场景在线性 HDR 中做环境背景、河流反射与大气。需要显示空间输入的图像描边、调试轨迹在重建与色调映射之后绘制。[材质程序](../src/render/materials.rs) · [可见性解析与分箱](../src/render/visibility.rs) · [阶段装配](../src/render.rs)

可见性解析向照明阶段提供 ID、深度、表面表、地址表、相机和按需生成的运动向量。仓库内的实验通过 `experiments::light_stack` 注册照明阶段，每个阶段声明自己读取的信号和唯一输出。外部 crate 可实现公开的 `Technique`，目前阶段注册接口的可见范围限于内置实验。装配器先检查重复写入、必需输入缺失与依赖环，再按依赖顺序建立 RDG pass。RDG 仍根据实际资源访问安排执行与同步。材质按自己声明的绑定顺序接收最终信号。[阶段接口与验证](../src/experiments/light_stack/stages.rs) · [材质绑定](../src/experiments/light_stack.rs)

| 阶段 | 读入 | 产出与可替换实现 |
| --- | --- | --- |
| 表面重建 | 可见性、光源、射线几何与运动 | 每像素世界位置、法线、深度、实例标识、有效历史像素与求交资源。存在独立阴影代理时，间接光按视觉几何求最近命中。 |
| 直接光采样 | 重建表面与光源表 | 原始直接辐亮度和候选储层。可用均匀、RIS、时空 ReSTIR 或分层参考采样。[直接光](../src/experiments/restir/graph.rs) |
| 直接光解析 | 表面与原始直接光 | 最终直接光。可选无滤波、两轮空间 à-trous、或带时间矩的 SVGF 加三轮 à-trous。无滤波时仍提交下一帧采样所需的时钟和储层。[降噪与历史提交](../src/experiments/light_stack/denoise.rs) |
| 漫反射间接光 | 重建表面、光源与命中材质 | 可独立选择关闭、无缓存单跳、带缓存单跳、带缓存双跳。无缓存模式每个 8×8 像素块发出八条半球探针射线，在命中 UV 计算辐亮度，再按深度与法线插值。缓存模式使用 131,072 个世界空间单元与上一帧探针；双跳还把第二次命中的贡献延迟发布。[间接光](../src/experiments/light_stack/diffuse_gi.rs) · [探针命中](../shaders/gi12_trace.glsl) |
| 环境光 | HDR 环境图和相机 | 漫射辐照度、预滤镜面与 BRDF 表，可同以上直接光和间接光组合。[IBL](../src/experiments/ibl.rs) |

在 `mixed` 场景使用 `restir gi direct|uncached|single|multi` 选择间接光，使用 `restir denoise off|spatial|svgf` 选择直接光滤波。旧存档沿用原有模式，缺少 `gi` 字段时只计算直接光。切换照明输出会重置最终颜色历史，各视图仍有独立的运动历史。主视图支持 TAA、DLSS 或 DLAA，次离屏视图可使用 TAA。[配置与命令](../src/experiments/restir.rs) · [运动输入](../src/render/motion.rs) · [后处理](../src/render/post.rs)

**照明阶段的 GPU 对照。** RTX 5070，`mixed` 固定机位 320×180，IBL 同时开启，32 个点光源、分层参考直接光、软件 BVH，每档预热 4 帧后测 14 帧。下表为整帧 GPU 时间，包含可见性、材质与输出。样本较少，P95 落在本组最慢一帧。

| 间接光与直接光滤波 | GPU P50 | GPU P95 |
| --- | ---: | ---: |
| 关闭间接光，关闭滤波 | 2.508 ms | 2.679 ms |
| 单跳缓存，关闭滤波 | 3.039 ms | 3.268 ms |
| 无缓存单跳，关闭滤波 | 2.832 ms | 3.728 ms |
| 无缓存单跳，两轮空间滤波 | 2.829 ms | 3.064 ms |

切回只算直接光时得到相同的像素。另在 `restir` 固定机位用 64 灯、32 个参考样本，预热两帧后与拆分前的可执行程序逐像素比较。直接光、缓存单跳和缓存双跳的三张 320×180 截图分别有 0 个变化像素。将直接光切为无缓存单跳后有 42,109 个像素变化。可用 `cargo test --release --locked --lib experiments::light_stack -- --include-ignored --nocapture --test-threads=1` 重跑混合场景测试及三种求交后端的命中校验。[GPU 测试](../src/experiments/light_stack/diffuse_gi/tests.rs)

## 绑定预计算包围盒与阴影代理

一个普通刚体实例可引用共享的 `GeometryAssets`。其中的局部 AABB 必须包围源网格全部顶点；实例的模型矩阵负责变换包围盒。投影代理与视觉网格使用同一局部坐标。可见性与 GI 采用视觉网格，直接光任意命中可查询代理。没有单独代理时，两种用途可以共享加速数据。蒙皮、风动和簇几何按自身变形或层级处理边界和阴影。[绑定校验](../src/scene/geometry_assets.rs) · [表面准备](../src/render/geometry.rs)

前端区分 KHR 光追管线、内联 ray query、NV 簇加速结构三项设备能力。软件遍历读取 BVH 与三角流，硬件求交读取 BLAS 和 TLAS；同一帧内视觉与遮挡可以复用相同几何的 BLAS。静态几何可按版本跨帧槽共享，动态几何随姿态更新。森林启用 Mega Geometry 时还要把屏幕外可能投影进来的簇加入候选集。选择何种后端要结合设备能力、场景设置和实际 pass。[求交前端](../src/render/rays/scene.rs) · [KHR 后端](../src/rhi/ray.rs) · [森林簇光追](../src/experiments/clusters/gpu/mega.rs)

## RDG、Vulkan 与呈现

1. **声明资源。** 每个 pass 指定操作、分类、队列及资源访问。缓冲可以声明字节区间，例如一个 Hi-Z mip；图像以整个资源追踪。普通 GPU 临时输出用 `Graph::local_buffer` 声明逻辑字节数和设备本地内存偏好。带初始内容的输入、历史和可复用 scratch 各有对应声明。[RDG](../src/rdg.rs) · [区间访问](../src/rdg/ranges.rs)
2. **编译依赖。** 编译器从资源读写、字节区间、图像布局和显式前驱生成顺序与屏障。主视图和次视图各保存最近一份拓扑。命中需要操作类型、队列、资源类型与初值状态、直接绑定、间接地址表和区间边界等调度输入一致。着色器、间接参数和加速结构输入仍用当帧命令校验。随后从当帧呈现、读回、历史写入和外部副作用追溯活跃 pass，再决定实际分配与提交。[调度模板](../src/rdg/schedule_cache.rs) · [活跃性裁剪](../src/rdg/liveness.rs)
3. **录制与提交。** RHI 使用退役帧槽准备资源和描述符堆，安排 worker 用独立的录制器录制命令。逻辑队列按设备配置映射到同一通用队列族的物理队列。同一队列内用屏障约束有依赖的阶段和内存访问。跨物理队列的依赖合并到时间线信号量，已被等待覆盖的普通缓冲读可以省去相应屏障。图像布局转换、主机可见性和别名边界仍有各自的同步要求。[帧槽](../src/rhi/slot.rs) · [录制](../src/rhi/record.rs) · [队列与等待](../src/rhi/queues.rs)
4. **显示与读回。** 窗口帧在 GPU 端写入交换链呈现，优先 MAILBOX，设备不支持该模式时使用 FIFO。离屏输出、截图和录像按需读回。比较异步调度应使用对齐后的队列时间线、跨队列等待和整帧跨度；相互重叠的 pass 时长直接相加会重复计算。[交换链](../src/rhi/present.rs) · [GPU 时间线](../src/rhi/timeline.rs) · [性能与复现](performance.md)

### 调度策略与复测

默认优先延续上一逻辑队列。设置 `RENDER_LAB_RDG_ORDER=critical` 会优先调度下游依赖链较长的就绪 pass，链长按 pass 个数计算。物理队列映射由 RHI 在裁剪之后决定。完全串行的图可以使用一条队列，另外保留 RTX 5070 上经过 1080p 森林回放校准的组合。设置 `RENDER_LAB_GPU_QUEUES=1|2|3` 可以固定同机 A/B 所用队列数。设置 `RENDER_LAB_RDG_PLAN_CACHE=0` 可以单独关闭调度模板。基准逐帧记录构图、依赖编译、缓存命中、映射可用队列数、实际收到提交的队列数、时间线等待和 GPU 跨度。[物理队列映射](../src/rhi/queues.rs) · [森林镜头回放](../examples/forest_shadows/zoom.rs)

## 验证入口

以下测试需要 Vulkan 设备。第一项在同一机位比较视觉 ID、代理阴影与 GI 几何，覆盖软件 BVH、KHR 管线和内联查询；第二项用相同模拟输入核对两个相机各自的 TAA 历史。普通网格合批的逐像素 ID、深度与运动对照，以及森林体素与 GPU 性能的运行条件，分别见[普通绘制回归](../src/experiments/motion/silver/contact/crowd_draw_test.rs)和[性能记录](performance.md)。

```powershell
cargo test --release --locked --lib rigid_derived_assets_change_shadows_but_preserve_visual_and_gi_geometry -- --ignored --nocapture
cargo test --release --locked --test offscreen_views -- --ignored
```
