# 实验目录与组合边界

可执行程序提供 20 个命名预设。预设决定怎样构造初始世界，同一套实验设置也可从场景 JSON 恢复。每个预设进入同一个三线程引擎，使用快照、RDG 和 Vulkan 后端。下表中的命令可以在控制台输入，也可通过启动参数 `--command` 传入。[CLI 预设分派](../src/main.rs) · [场景配置与恢复](../src/experiments/mod.rs) · [资产来源与许可](../assets/README.md)

## 几何、植被和程序化效果

| 预设 | 一帧中实际运行的工作 | 可以观察或核验什么 |
| --- | --- | --- |
| `clusters` | 36 条龙共享一份网格和简化 DAG。实例视锥测试将根写入全局 BVH 节点表，叶子引用 cluster group。GPU 在全局候选簇表上测试投影误差，用上帧和本帧 Hi-Z 分两次处理遮挡。普通橙色方块与簇共写可见性。 | `lodview on` 给最终可见簇着色，`lod 0` 可对照最细层。测试覆盖不同资产共享列表以及移动相机后的补绘。[实验回归](../tests/experiments.rs) |
| `vegetation`、`vegetation-scale` | 树干、树冠和草分开生成。树冠和草的三角簇与局部体素簇进入同一 DAG，按投影误差选择。GPU 风骨骼驱动选中几何。前者是近景草甸，后者用于有数千棵树的森林密度与提取实验。 | `vegetation view paths` 区分三角和体素，`vegetation wind 0` 固定风姿。覆盖掩码、帧间轮廓变化和 GPU 耗时有独立的相机路径对照。[植被实验](procedural-vegetation.md) · [ECS 变更基准](ecs-changes.md) |
| `river-forest` | 固定种子生成 22,750 棵树、256,081 丛草和连续弯曲河床。GPU 按河道参数与时间生成波形、法线及流动细节，沿同一条水流线回查上一帧位置，再对可见水面做屏幕空间反射。 | `vegetation camera basin` 看流域，`vegetation camera grass` 看湿岸草。河面使用规定的水位与流速场。完整河谷和限域硬件阴影录像采用的树草密度范围见专题。[河谷实验](river-forest.md) · [一分钟录像条件](forest-mega.md) |
| `water` | 五级台阶上的水带由横向流线和水团年龄构成。GPU 从发射状态以小步长积分速度、重力和台阶碰撞，再从相邻水团重建表面、法线与泡沫。常规绘制和射线后端可读同一份变形几何。 | `effect time 1.32` 定位到同一模拟时刻。回归测试检查越过每级台阶、水团前一帧位置，以及水面平移后两种射线路径的结果。[积分 shader](../shaders/water_integrate.comp) · [效果测试](../src/experiments/effects/tests.rs) |
| `destruction` | 场景构造时按 Voronoi 单元裁切晶体，碎块成为实体。播放系统按时钟更新各碎块的变换，另一块完整晶体在可见性阶段按消融阈值丢弃片元，材质显示边缘。 | `effect restart` 或 `effect time 1.9` 可重播同一阶段。碎裂轨迹由该实验计算，当前复现范围是预切晶体和既定碰撞条件。[碎块生成与运动](../src/experiments/effects/fracture.rs) · [实录](../captures/fracture-and-dissolve.mp4) |

## 角色、动画与纹理

| 预设 | 数据生产与运行时路径 | 观察方式与适用范围 |
| --- | --- | --- |
| `toon` | 银狼 glTF 导入后预计算边和邻面关系，焊接 UV 缝时同时检查蒙皮影响。图像描边、法线外扩和退化四边形使用不同的几何或屏幕输入。脸部 UV 中预制七组明暗轮廓的距离场，材质依据头部与光向处理二分光照。 | `toon outline image`、`toon outline hull`、`toon outline quad` 比较轮廓，`toon sdf off` 检查面部贡献。默认面部距离场与角色的 UV、头节点和材质对应。[卡通实验](../src/experiments/cartoon.rs) · [描边渲染](../src/experiments/cartoon/renderer.rs) |
| `motion` | 离线生成 39 段 Kimodo 动作并重定向到银狼。运行时用局部速度、转向与阻尼预测弯曲轨迹，将未来位置、朝向和脚部姿态组成 24 维查询。库与查询共用归一化参数，六维 PCA 投影给 KD-tree 剪枝，叶子按完整代价精排。选中姿态写回骨骼供 GPU 蒙皮。 | I/K 前后，J/L 横移，I+J 或 I+L 转向。地面轨迹展示未来样点。`motionsearch kd` 与 `motionsearch scan` 使用同一代价和候选约束。库内另有七骨骼的小型全扫描测试角色。[银狼查询](../src/experiments/motion/silver/search.rs) · [轨迹积分](../src/experiments/motion/silver/locomotion.rs) · [动作实录](../captures/silver-wolf-motion.gif) |
| `texture` | Modern Hall 的三张不透明基础色贴图分别由 PyTorch 拟合。每个 4×4 纹素块保存八维量化特征，每张图有一套小 MLP。材质从可见性重建 UV，只解码需要的块内输出通道。 | `texture neural`、`texture original` 和 `texture stats` 比较着色和载荷。这个实验使用单级纹理、重复寻址和最近邻采样，原始纹理保留在 CPU 场景以支持切换。[训练器](../tools/train_texture_grid.py) · [材质解码](../shaders/materials/texture_grid.glsl) · [纹理局部对照](../captures/modern-hall-texture-detail.png) |
| `gaussian` | 二维高斯表示同一室内场景的地砖。材质按 UV 求纹素，有全扫描、tile 候选列表和二维 BVH 三种查询，另有剪枝及紧凑记录格式。tile 在离线打包时生成，BVH 可在选用该模式时从记录构建并复用。 | `gaussian original`、`gaussian tile`、`gaussian bvh` 对照，`gaussian compact-bvh` 使用紧凑路径。`--experiment gaussian --scene FILE.g2t` 可预览独立资产。显存比较计入候选索引或 BVH 节点。[算法和训练审查](gaussian-texture.md) · [剪枝与精度对照](gaussian-pruning.md) |

## 光照、阴影和射线

| 预设 | 算法与接入点 | 能力边界与验证入口 |
| --- | --- | --- |
| `gi` | Cornell 风格房间共享主视图表面。SSGI 沿屏幕步进，世界空间探针保存六方向辐照度，软件 BVH 模式追踪一次漫反射。间接光在半分辨率计算，再按几何信息上采样。 | `gi off`、`gi ssgi`、`gi probes`、`gi ray` 对照。`gi view indirect` 单独观察间接光。[RDG 实现](../src/experiments/lighting/pipeline.rs) |
| `shadow` | 在栅栏、接触物和悬空球上，对同一光源深度图比较硬阴影、PCF 和 PCSS。面积光模式用 GPU 软件遍历求遮挡。 | `shadow pcss`、`shadow ray`、`shadow view visibility` 显示同一机位的差异。该实验与森林的 ReSTIR 投影路径各有控制入口。[场景与参数](../src/experiments/lighting.rs) · [画面对照测试](../src/experiments/lighting/tests.rs) |
| `rt` | 独立主射线管线构建常规三角 BLAS 和 TLAS。raygen、miss 和 closest-hit 通过 Vulkan RT 实际运行，光线决定首个命中、硬阴影与一次镜面反射，由射线管线生成最终颜色。 | 需 KHR 加速结构与光追管线。`rt view normals`、`rt view distance`、`rt view reflections` 分离结果。其余采用可见性缓冲的场景仍可选择射线承担光照子任务。[射线实验入口](../src/experiments/lighting/pipeline.rs) · [硬件命中测试](../src/experiments/lighting/tests.rs) |
| `restir` | 在柱廊中从大量点光源抽样。初始 RIS、时空 reservoir 复用和可见性查询输出直接光。系统可选择空间 à-trous 或带重投影、方差估计的 SVGF 风格滤波。 | `restir uniform`、`restir ris`、`restir reuse` 与 `restir reference` 区分估计器。`restir denoise off` 隔离采样误差。运动中的银狼另有专门的重投影对照。[帧图](../src/experiments/restir/graph.rs) · [银狼逐帧误差实验](lighting-comparison.md) |
| `ibl` | 从 Studio Small 02 HDR 环境图烘焙漫反射辐照度、分粗糙度的 GGX 预过滤环境和 BRDF LUT。材质用 split-sum 合成环境反射，球阵改变金属度和粗糙度。 | `ibl yaw 90` 转动环境。烘焙在启动时进行并在进程内复用，显式表面反射射线另由 RT 实验研究。[环境预计算](../src/experiments/ibl/bake.rs) · [材质输入与控制](../src/experiments/ibl.rs) |
| `gi12-forest`、`gi12-silver` | 在风动森林和运动角色上组合 ReSTIR DI、固定分辨率屏幕探针与世界辐亮度缓存。漫反射命中点用 UV 采样材质定义的 GI 反射色和自发光。延迟的第二跳辐亮度反馈给缓存。 | `restir gi direct`、`restir gi single`、`restir gi multi` 比较直接光、单跳与两跳。实现范围是 GI-1.2 的漫反射缓存思路，当前反弹深度为两跳。[传播机制、画面与限制](gi12.md) |
| `forest-mega` | 复用 `gi12-forest` 世界，启动时选择直接光和 Mega Geometry。主视图按屏幕误差选中的簇与可能投影进来的屏外簇，在 GPU 上应用风姿态，生成 CLAS、聚合 Cluster BLAS，再与普通网格汇入 TLAS。 | 需 NVIDIA 的 `VK_NV_cluster_acceleration_structure` 与原生 RT。`vegetation shadows adaptive` 选择近远代理，`restir rays software` 切换软件遍历。硬件实验按实例、簇和几何版本复用 CLAS，每帧提交当前选簇的地址与 LOD 覆盖。专题列出缓存与强制重建对照。[实测、回归与复拍](forest-mega.md) |
| `forest-karst` | 用岩柱、碎石坡与混交林研究薄叶逆光和树冠内外亮度。默认开启 ReSTIR DI、GI-1.2 两跳漫反射、自适应代理的硬件光追和 TAA。 | `vegetation camera karst` 与 `vegetation camera karst-grove` 复拍俯视和谷内机位。`restir rays software`、`restir rays hardware` 切换场景射线后端。[机位与实验图](procedural-vegetation.md#岩柱森林的树冠受光) · [同路径光追开销](procedural-vegetation.md#岩柱森林的多跳光追) |

## 组合与默认场景

`mixed` 在 ReSTIR 柱廊中同时启用 IBL 和 ReSTIR，将两路贡献交给同一套 PBR、Toon 材质分桶。对照程序固定材质、相机和几何，分别保留环境光、直接光或两者。[场景构造](../src/experiments/mixed.rs) · [三组同机位图](lighting-comparison.md)

`confluence` 将树林、台阶流水和银狼合入一个世界。普通几何、蒙皮、水和簇写同一份可见性与深度。时域消费者按需共享表面运动，流水反射、植被大气和卡通覆盖在相应模块启用时追加。控制台 `feature water off` 会保留石阶并停用水带，`feature restir off` 会去掉直接光生产者。可开关的五层是 `forest`、`water`、`actor`、`ibl` 和 `restir`。组合设置、实例显隐与模拟状态存在场景，切换时保留当前相机。当前建场把森林和台阶等静态布景的已知资产引用重新映射到角色世界。该合并函数只接受扁平、无蒙皮与动画的布景，角色骨架和层级保留在原世界。[建场与 ID 重映射](../src/experiments/composition/scene.rs)[组合与测量](scene-composition.md)

省略 `--experiment` 会打开默认的 PBR 与 Toon 演示。`--scene` 可打开普通 glTF、GLB 或已保存的引擎 JSON。`--experiment clusters --scene FILE` 额外准备簇几何。`toon` 对自定义 glTF 还要求能找到约定的头节点与面部材质。[启动分派](../src/main.rs) · [卡通配置](../src/experiments/cartoon.rs)

组合器依据已有组件键组装几何、覆盖和光照。光照注册表目前提供直接光与环境光两个主通道，每个通道只接受一个生产者。GI 间接漫反射作为光照贡献的附加资源，部分自定义材质按程序名接收它。独立 GI、阴影、主射线、神经纹理和高斯实验在场景配置中另选专用 Technique。将这些实验接入 `confluence` 时，还需要明确贡献的阶段、资源和材质消费者。[光照贡献契约](../src/experiments/light_stack.rs) · [组合适配](../src/experiments/composition.rs) · [专用路径选择](../src/experiments/mod.rs)

## 运行、离线工具与证据

从包含 `Cargo.toml` 的引擎根目录运行。基础 Vulkan 能力和编译器要求见 [README](../README.md)。下列命令分别打开普通预设、回读固定机位的 PNG、在窗口中测量、调用固定相机和风时刻的阴影对照。最后一条需支持 CLAS 的设备。

~~~powershell
cargo run --release --locked -- --experiment mixed
cargo run --release --locked -- --experiment gi12-silver --command "restir gi multi" --warmup 48 --capture target/silver-gi.png
cargo run --release --locked -- --experiment river-forest --command "post taa" --benchmark 120
cargo run --release --locked --example forest_shadows -- target/forest-shadow-reference reference 64 2 mega
~~~

| 工作环节 | 工具与交付物 |
| --- | --- |
| 动作生成与烘焙 | Kimodo、PyTorch 生成 SOMA 动作，NumPy 与 SciPy 重定向和整理采样，构建归一化、PCA 和 KD-tree 参数。银狼运行时读取随程序嵌入的动作库与检索数据。变更库后需重新构建程序。[生成](../tools/generate_kimodo.py) · [重定向](../tools/bake_silver_wolf.py) · [索引](../tools/motion_index.py) |
| 纹理拟合与对照 | PyTorch 训练特征网格。高斯工具可拟合或打包已有 PLY，另有剪枝、量化、误差分析与旧 CUDA 梯度检查。两种运行时实验可用预训练资产直接启动。[特征网格](../tools/train_texture_grid.py) · [高斯打包](../tools/gaussian_texture.py) · [消融实验](../tools/gaussian_ablation.py) · [梯度核验](../tools/check_2dgaussian_gradients.cu) |
| 场景提取与时域观察 | 比较 ECS 提取访问量、驻留页变更和逐帧渲染计数。植被路径程序可输出轮廓掩码，光照与射线程序可固定姿态观察历史重投影或软硬件遮挡差异。[提取](../examples/forest_extraction.rs) · [实例与材质更新](../examples/forest_scene_updates.rs) · [植被路径](../examples/vegetation_sweep.rs) · [光照](../examples/lighting_comparison.rs) · [变形射线](../examples/posed_ray_comparison.rs) |
| 录制和性能口径 | CLI 的 `--capture` 与 `--record` 默认以 800×500 离屏读回，`--size WIDTHxHEIGHT` 指定输出尺寸。窗口 `--benchmark N` 走交换链并报告时间戳。专用森林录像程序以 1920×1080、固定模拟步长送 FFmpeg 编码，成片帧率描述播放时间。固定机位的阴影计时另见专题。[录制入口](../src/main.rs) · [多机位录制](../examples/forest_river_film.rs) · [森林阴影数据](forest-mega.md) |
