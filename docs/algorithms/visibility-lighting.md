# 从可见性缓冲到直接光、间接光与最终颜色

渲染线程为每个视图调用 `Technique::build`。默认可见性管线先确定每个像素看见的表面和三角形，再由该表面的材质选择着色程序。普通网格、簇网格和过程水面可以出现在同一机位。需要研究主射线或纹理解码的实验也可注册专用技术。每种技术用同一个 `FrameGraph` 交付输出。[渲染入口](../../src/render.rs) · [实验目录](../experiment-catalog.md) · [世界快照的来源](../architecture/extraction.md)

## 几何、剔除与可见性

| 输入 | 从快照到可见性缓冲 |
| --- | --- |
| 普通网格 | 为每个表面登记实例、材质与索引。少于 16 个兼容表面时直接绘制。大批次先在 GPU 测试每个表面的视锥，再按原顺序生成间接 mesh draw；单包上限为 256 个表面、32,768 个工作组。共面覆盖顺序和表面编号都得以保留。[绘制列表](../../src/render/draw_list.rs) · [视锥测试](../../shaders/ordinary_cull.comp) |
| 蒙皮网格 | Compute 按姿态页和源网格分组，以每个实例的权重和骨矩阵生成顶点流。可见性、运动和加速结构共享相应的变形结果。普通刚体用包围盒剔除，蒙皮批次用当帧已变形的位置判断视锥。[GPU 蒙皮](../../src/render/geometry/skin.rs) |
| 森林簇 | 构建时以一组子簇为单位准备三角或体素替代，比较误差。运行时先筛实例视锥，GPU 通过全局列表分发 BVH 节点和候选簇，按投影误差选择层级。上一帧 Hi-Z 先筛遮挡，画出可见三角簇后用新深度复查被挡候选。末尾处理压紧的体素砖；compute 路径对候选像素遍历砖内占用并按深度竞争，另有 mesh 路径可作对照。[DAG 构建](../../src/experiments/clusters/bvh/voxels.rs) · [剔除](../../src/experiments/clusters/gpu.rs) · [体素光栅](../../src/experiments/clusters/gpu/bricks.rs) |
| 特殊几何 | 水面先发布本帧及需要的历史位置，随后进入可见性和反射；法线外扩、退化四边形描边可提交附加几何。组合场景装配时检查各路由认领的实例，避免一个表面重复绘制。[场景组合](../../src/experiments/composition.rs) · [水面](../../src/experiments/effects.rs) · [描边](../../src/experiments/cartoon/renderer.rs) |

## 着色、光照与时域重建

表面和三角 ID 指向这一帧的表面表、几何流及材质槽。GPU 先按材质程序统计像素、分配区间、散射像素索引，再间接运行各材质的着色器。PBR 和卡通材质因此可以共享同一次可见性绘制。组合场景在线性 HDR 中做环境背景、河流反射与大气。需要显示空间输入的图像描边、调试轨迹在重建与色调映射之后绘制。[材质程序](../../src/render/materials.rs) · [可见性解析与分箱](../../src/render/visibility.rs) · [阶段装配](../../src/render.rs)

可见性解析向照明阶段提供 ID、深度、表面表、地址表、相机和按需生成的运动向量。仓库内的实验通过 `experiments::light_stack` 注册照明阶段，每个阶段声明自己读取的信号和唯一输出。外部 crate 可实现公开的 `Technique`，目前阶段注册接口的可见范围限于内置实验。装配器先检查重复写入、必需输入缺失与依赖环，再按依赖顺序建立 RDG pass。RDG 仍根据实际资源访问安排执行与同步。材质按自己声明的绑定顺序接收最终信号。[阶段接口与验证](../../src/experiments/light_stack/stages.rs) · [材质绑定](../../src/experiments/light_stack.rs)

| 阶段 | 读入 | 产出与可替换实现 |
| --- | --- | --- |
| 表面重建 | 可见性、光源、射线几何与运动 | 每像素世界位置、法线、深度、实例标识、有效历史像素与求交资源。存在独立阴影代理时，间接光按视觉几何求最近命中。 |
| 直接光采样 | 重建表面与光源表 | 原始直接辐亮度和候选储层。可用均匀、RIS、时空 ReSTIR 或分层参考采样。[直接光](../../src/experiments/restir/graph.rs) |
| 直接光解析 | 表面与原始直接光 | 最终直接光。可选无滤波、两轮空间 à-trous、或带时间矩的 SVGF 加三轮 à-trous。无滤波时仍提交下一帧采样所需的时钟和储层。[降噪与历史提交](../../src/experiments/light_stack/denoise.rs) |
| 漫反射间接光 | 重建表面、光源与命中材质 | 可独立选择关闭、无缓存单跳、带缓存单跳、带缓存双跳。无缓存模式每个 8×8 像素块发出八条半球探针射线，在命中 UV 计算辐亮度，再按深度与法线插值。缓存模式使用 131,072 个世界空间单元与上一帧探针；双跳还把第二次命中的贡献延迟发布。[间接光](../../src/experiments/light_stack/diffuse_gi.rs) · [探针命中](../../shaders/gi12_trace.glsl) |
| 环境光 | HDR 环境图和相机 | 漫射辐照度、预滤镜面与 BRDF 表，可同以上直接光和间接光组合。[IBL](../../src/experiments/ibl.rs) |

在 `mixed` 场景使用 `restir gi direct|uncached|single|multi` 选择间接光，使用 `restir denoise off|spatial|svgf` 选择直接光滤波。旧存档沿用原有模式，缺少 `gi` 字段时只计算直接光。切换照明输出会重置最终颜色历史，各视图仍有独立的运动历史。主视图支持 TAA、DLSS 或 DLAA，次离屏视图可使用 TAA。[配置与命令](../../src/experiments/restir.rs) · [运动输入](../../src/render/motion.rs) · [后处理](../../src/render/post.rs)

**照明阶段的 GPU 对照。** RTX 5070，`mixed` 固定机位 320×180，IBL 同时开启，32 个点光源、分层参考直接光、软件 BVH，每档预热 4 帧后测 14 帧。下表为整帧 GPU 时间，包含可见性、材质与输出。样本较少，P95 落在本组最慢一帧。

| 间接光与直接光滤波 | GPU P50 | GPU P95 |
| --- | ---: | ---: |
| 关闭间接光，关闭滤波 | 2.508 ms | 2.679 ms |
| 单跳缓存，关闭滤波 | 3.039 ms | 3.268 ms |
| 无缓存单跳，关闭滤波 | 2.832 ms | 3.728 ms |
| 无缓存单跳，两轮空间滤波 | 2.829 ms | 3.064 ms |

切回只算直接光时得到相同的像素。另在 `restir` 固定机位用 64 灯、32 个参考样本，预热两帧后与拆分前的可执行程序逐像素比较。直接光、缓存单跳和缓存双跳的三张 320×180 截图分别有 0 个变化像素。将直接光切为无缓存单跳后有 42,109 个像素变化。可用 `cargo test --release --locked --lib experiments::light_stack -- --include-ignored --nocapture --test-threads=1` 重跑混合场景测试及三种求交后端的命中校验。[GPU 测试](../../src/experiments/light_stack/diffuse_gi/tests.rs)

## ReSTIR 如何在一个像素上选灯

均匀提出多个候选光源，用接收点入射贡献的亮度计算目标权重。储层只保留被选光源、候选计数、累计权重与被选样本的目标值；每加入一个候选，以它在累计权重中的比例替换当前选择。时间复用先用共享运动向量找回上一帧像素，再用表面标识和深度校验。空间复用读邻居储层，在当前接收点重新估计目标并合并，最后对选中的光源求可见性并以储层归一化因子加权。这样能将候选灯数与实际发出的阴影射线数分开控制。[初始与时间采样](../../shaders/restir_initial.comp) · [储层权重](../../shaders/restir_lights.glsl) · [空间复用](../../shaders/restir_spatial.comp) · [重投影](../../shaders/restir_reproject.comp)

SVGF 的时间积累、亮度矩与空间滤波处理光照噪声，TAA 在后处理阶段重建最终颜色。两者共享表面运动信息，各自维护历史。分析拖影时先检查当前像素是否找回同一实例和相容的深度，再分别检查光照历史与最终颜色历史。可用运动中的银狼和树冠在固定机位下作对照。[光照滤波](../../shaders/restir_temporal_filter.comp) · [最终颜色重建](../../shaders/taa.comp) · [UE Lumen 对照](../unreal/rendering.md)
