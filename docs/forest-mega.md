# 森林的 Mega Geometry 阴影

在支持 VK_NV_cluster_acceleration_structure 的设备上运行森林实验。

~~~powershell
cargo run --release --locked -- --experiment gi12-forest --command "vegetation shadows mega" --command "restir gi direct"
~~~

`vegetation shadows mega` 选择 NV 簇几何，`vegetation shadows adaptive` 选择近景精细、远景简化的代理几何。`restir rays hardware` 和 `restir rays software` 同时控制 ReSTIR 与 GI 的求交。标准 KHR 与软件模式可遍历同一批代理三角形。选择 Mega 后切到软件模式时使用自适应代理，切回硬件恢复 CLAS。旧命令 `restir proxy-rays` 仍可使用。显式选择的硬件能力缺失时，RHI 会报告错误。[岩柱森林同几何对照](procedural-vegetation.md#岩柱森林的多跳光追)

## 几何和遍历

GPU 在全局 DAG 上用主相机的屏幕误差选择簇。对于直接光，沿方向光的方向和角半径保守扩展候选集合。视锥内、被 Hi-Z 挡住和屏幕外仍有投影贡献的簇可进入阴影构建。光栅分支再执行相机可见性及 Hi-Z 测试。间接光的接收点可能在相机之外，因此 GI 探针模式保留全场景的投影候选。

三角簇的 mesh shader、体素计算光栅和加速结构准备共享风骨骼与物理图元编号。每个选中的三角簇或体素外表面页产生一个世界坐标三角形 CLAS，汇成 Cluster BLAS 后与普通网格一同进入 TLAS。any-hit 将命中点投影回主相机，复用光栅的过渡覆盖区间和实例散列。GI 的最近命中和到光源的遮挡都使用此 TLAS。最近命中从簇 ID 找回本帧实例槽、簇页和材质，用图元和重心坐标重建风动后的世界法线与 UV。开启间接光时保留各方向的屏幕外簇，直接光模式可按灯光方向收窄投影集合。[GPU 选簇与装配](../src/experiments/clusters/gpu/mega.rs) · [命中属性](../shaders/gi12_hit.rchit) · [NVIDIA 动画簇样例](https://github.com/nvpro-samples/vk_animated_clusters)

体素网格烘焙在资产局部坐标。构建器比较三个正交投影下相对细叶的覆盖误差，候选排序同时考虑砖的投影重叠。邻接且属于同一骨骼的体素省略共有面。主相机 DDA 和阴影 CLAS 共用外表面的页和图元映射。孤立体素和 LOD 双层过渡仍会产生重复覆盖。GPU 光栅诊断可分别查看投影覆盖、逐像素深度拒绝和真正进入 DDA 的次数。[体素候选与表面提取](../src/experiments/clusters/bvh/voxels.rs) · [共享顶点变形](../shaders/cluster_pose.glsl) · [体素光栅同列表实测](procedural-vegetation.md#体素光栅同列表实测)

## 跨帧缓存与失效

每个原型在全场景分配一段连续的簇编号。GPU 按原型段起点、该原型内实例序号和资产 meshlet 编号确定稳定 ID，0 留给空集哨兵。ID 决定显式 CLAS 目标地址，和并行剔除的输出顺序无关。每个帧槽拥有独立的 CLAS 存储、版本表和构建 scratch，RHI 在帧槽退役后才能再次使用它。窗口最多两帧在途，两个槽分别预热。[原生分配与版本](../src/rhi/clusters.rs)

GPU 为每个选中 ID 生成当前覆盖区间及 BLAS 引用地址。已有几何的复用条件包括原型和布局代数、实例的 64 位变换版本、风变形开关、该实例实际生效的风权重与非零权重时的模拟时间。风权重与光栅骨骼使用同一段屏幕尺寸衰减函数。相机距离跨过衰减区间也会改变姿态。改变选择、覆盖比例或候选顺序时，有效的已有簇保留原生地址。资产源、骨骼或实例布局变化会保守失效该帧槽整批 CLAS。重新创建场景提取器会产生新的布局标识，即使变换版本数值再次相同也会进入冷构建。[版本分类与构建记录](../shaders/mega_clusters.comp) · [实例布局](../src/render/scene/routes.rs)

GPU 只为未命中项展开世界空间几何，把需要重建的记录和显式目标地址压紧。每帧仍构建一个退化三角形哨兵，保证空集合可提交。Cluster BLAS 和顶层 TLAS 按本帧选中列表重建，已经不可见或不能投影的旧条目留在固定池中，不参与本帧遍历。每个真实簇恰好落入“首次／代数失效、变换失效、姿态失效、命中”四类之一。控制台 `vegetation cache off` 可以在同一显式地址实现上强制重建全部选中的簇，`vegetation cache on` 恢复默认复用。

### 配对基准

~~~powershell
cargo run --release --locked --example forest_cluster_cache -- captures/forest-mega/clas-cache.csv 32 2
cargo test --release --locked --lib mega_tests -- --ignored --nocapture --test-threads=1
~~~

GeForce RTX 5070，800 × 500，森林 ReSTIR 直接光，离屏单帧槽。每个条件独立预热 12 帧，记录 32 帧，第二轮倒转开关顺序。每个条件共 64 帧，数据表报告两轮合并的 nearest-rank P95，簇数是每帧平均值。固定和相机运动关闭风，相机运动沿水平轨道旋转 0.18 弧度。风动每帧前进 1/60 秒。切镜依次切换四组距离与偏航角，末段回到原机位。两组逐帧检验选中簇数相等。[512 帧原始计数、计时与内存字节数](../captures/forest-mega/clas-cache.csv) · [测试程序](../examples/forest_cluster_cache.rs)

| 工况 | 选中簇/帧 | 复用簇/帧 | 新建或失效簇/帧 | CLAS 构建 P95 关 → 开 (ms) | 遮挡射线 P95 关 → 开 (ms) | 整帧 P95 关 → 开 (ms) |
|---|---:|---:|---:|---:|---:|---:|
| 固定 | 171,525 | 171,525 | 0 | 11.289 → 0.030 | 0.184 → 0.184 | 18.979 → 6.109 |
| 相机运动 | 171,801 | 171,638 | 163 | 11.204 → 0.059 | 0.195 → 0.309 | 19.108 → 5.993 |
| 风动 | 171,525 | 0 | 171,525 姿态失效 | 10.954 → 11.016 | 0.183 → 0.201 | 19.215 → 19.001 |
| 切镜 | 172,458 | 170,495 | 1,962 | 11.763 → 1.039 | 0.186 → 0.184 | 19.004 → 7.516 |

固定机位下，驱动分配的设备本地内存总峰值为 1,875.1 MiB，其中加速结构类峰值 1,293.8 MiB、构建 scratch 峰值 61.4 MiB。不同工况的本地峰值最高达 1,899.2 MiB。固定池按全部候选簇数量和设备对最坏单簇的对齐尺寸预留，因此此次关闭复用与启用复用的内存预留相同。连续风动时，当前簇的世界空间姿态全部换版，CLAS 构建和整帧耗时与强制重建相近。切镜下几何准备 P95 由 2.959 ms 降至 1.506 ms，Cluster BLAS 聚合仍约 0.595 ms。各 pass 时间与跨队列整帧跨度分别测量，不能直接相加得到整帧时间。

### 正确性检查

320 × 200 硬光测试中，一棵完全在视锥外的树向可见地面投影。相同风姿态下的缓存帧、强制重建帧和再次强制重建帧的 64,000 个像素逐字节一致；改变风姿态后 4,334 个地面像素发生变化，删除这棵树后 11,848 个像素变化。另一个测试让硬件射线匹配光栅给出的实例、meshlet、三角形和重建位置。本次冷／热相同姿态的内部样本各为 25,070/25,070，另外两组风和 LOD 为 25,149/25,149 和 23,640/23,640。每组另行报告光栅亚像素边缘。

完整森林对照保留重叠枝叶，用同一机位做全灯参考着色并关闭降噪。本次冷／热帧的 64,000 像素中有 8 个不同，RGB 三通道的绝对 8 位差值总和为 126；风动缓存／强制重建为 3 个像素、45 阶；相同风姿态下强制重建／再次强制重建的自身差异为 1 个像素、373 阶。这些是一次实测，GPU 并行顺序可改变少量重叠表面的采样结果。回归分别检查确定性地面接收器的零差异、全森林的差异上界，以及变换、风姿态、新提取器布局的失效计数。[硬件回归](../src/experiments/restir/studies/mega_tests.rs)

## 早期逐帧重建基线

~~~powershell
cargo run --release --locked --example forest_shadows -- captures/forest-mega realtime 64 2 mega
cargo run --release --locked --example forest_shadows -- captures/forest-mega reference 64 2 mega
cargo run --release --locked --example forest_river_film -- captures/forest-mega/forest-river-1080p.mp4
~~~

阴影测量使用与软件基线相同的 800 × 500 固定机位。每套设置预热 48 个绘制帧，每轮取 64 帧，两轮反序运行。实时模式在采样像素上追踪一条候选光源的阴影射线并做时空降噪。参考模式遍历所有灯，具有角半径的太阳取 16 个圆盘样本，关闭降噪。整帧使用跨队列 GPU 时间跨度。几何准备和构建分别计时，阴影查询包含执行射线的 pass 开销。

### RTX 5070，驱动 621.14，单位 ms

| 采样 | 遮挡几何 | 太阳 | 几何准备 P50 | 构建 P50 | 阴影查询 P50 | 整帧 P50 | 整帧 P95 |
|---|---|---|---:|---:|---:|---:|---:|
| 实时 | 软件 BVH | 硬 | 0.287 | 0.104 | 2.193 | 8.155 | 8.710 |
| 实时 | 软件 BVH | 软 | 0.285 | 0.098 | 2.992 | 9.008 | 9.532 |
| 实时 | Mega Geometry | 硬 | 1.583 | 11.140 | 0.175 | 17.720 | 18.318 |
| 实时 | Mega Geometry | 软 | 1.565 | 11.216 | 0.176 | 17.839 | 18.470 |
| 参考 | 软件 BVH | 硬 | 0.269 | 0.097 | 3.992 | 9.184 | 10.038 |
| 参考 | 软件 BVH | 软 | 0.246 | 0.090 | 57.075 | 65.421 | 85.862 |
| 参考 | Mega Geometry | 硬 | 1.578 | 11.183 | 0.247 | 17.038 | 17.550 |
| 参考 | Mega Geometry | 软 | 1.571 | 11.232 | 1.628 | 18.563 | 19.273 |

以下历史测量使用隐式地址并逐帧重建全部选中的 CLAS。固定机位共 171,525 个实际簇，含 24,072 个屏外簇、2,976 个体素簇和 6,257,427 个三角形，不含空集合哨兵。构建以 CLAS 本身为主，聚合 Cluster BLAS 约 0.42 ms，顶层 TLAS 约 0.02 ms。几何准备、构建和射线 pass 可能与其他队列重叠。软件侧每帧对风动后的投影几何更新包围盒和 refit 分层 BVH。

三组风场与 LOD 条件下，GPU 对照扫描共 73,859 个严格位于三角形内部的样本，全部匹配光栅的实例、簇、三角形和位置。边缘另计，光栅的亚像素定点覆盖规则可能覆盖解析射线未实际穿过的边界。独立屏外树测试中，树本身的可见像素为零，改变风姿态后有 4,334 个接收面像素变化。

[实时四图](../captures/forest-mega/realtime-comparison.png) · [参考四图](../captures/forest-mega/reference-comparison.png) · [原像素局部放大](../captures/forest-mega/detail.png) · [实时逐帧 CSV](../captures/forest-mega/realtime.csv) · [参考逐帧 CSV](../captures/forest-mega/reference.csv)

## 多视角森林流水

录像用固定 1/30 秒模拟步长，五个各十二秒的移动镜头，原生 1920 × 1080，TAA。第一段为完整 600 米河谷，含 22,750 棵树和 256,081 丛草，使用原森林光照。后四段保留同一生成种子、完整河道与地形，近岸保留原始密度，在远岸按位置取样树木，并逐镜释放 GPU 资源，以容纳逐帧重建的硬件簇。河面由 GPU 更新，水面反射取自当前画面。

[一分钟 MP4](../captures/forest-mega/forest-river-1080p.mp4) · [五个机位](../captures/forest-mega/film-views.jpg)
