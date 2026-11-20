# Render Lab

一个独立的 Rust 渲染实验框架。默认场景并排展示 PBR 与带描边的卡通物体。研究场景包括 cluster LOD、Motion Matching 和基于银狼 glTF 的卡通人物渲染。

## 运行

已验证环境为 Windows 11、Rust 1.92、Vulkan SDK 和 RTX 5070。构建脚本通过 PATH 查找 `glslc`，也可用 `GLSLC` 指定可执行文件。运行时需要 Vulkan 1.4、`VK_EXT_descriptor_heap`、`VK_EXT_mesh_shader`、timeline semaphore，以及支持时间戳的图形与计算队列族。Windows 交互视口使用 `VK_KHR_win32_surface` 和 `VK_KHR_swapchain`。

在本目录运行：

```powershell
cargo run --locked
cargo run --locked -- --experiment clusters
cargo run --locked -- --experiment motion
cargo run --release --locked -- --experiment toon
cargo run --release --locked -- --experiment texture
cargo run --release --locked -- --experiment gaussian
cargo run --locked -- --scene assets/scenes/cornell-box/cornell_box_core.gltf
```

`--scene` 接受 glTF、GLB 和本框架保存的 JSON。给 cluster 实验同时传入 `--scene` 会为该场景构建层次。任一启动方式都可附加 `--capture target/frame.png`，通过相同的三线程管线预热一帧、捕获第二帧后退出。动作截图先执行固定时长的模拟，便于回归比较。

## 窗口与控制台

W/S 沿视线前进和后退，A/D 左右平移，在视口内按住鼠标右键改变朝向，F 根据场景包围盒重新取景。按 / 或 Enter 打开控制台，输入命令后按 Enter 执行，Esc 取消。Windows 下支持粘贴路径。右侧图中的每个节点对应一次实际执行的 pass，连线来自 RDG 编译结果，点击节点可查看资源屏障。

| 命令 | 效果 |
| --- | --- |
| `help`、`stats` | 查看命令、实体数、pass 数和 GPU 区间耗时之和 |
| `fit`、`pause`、`rdg` | 重新取景、切换模拟暂停、切换依赖图 |
| `import "路径"` | 把 glTF 的资产和实例加入当前场景 |
| `save "路径"`、`load "路径"` | 保存或打开自包含场景，路径可以包含空格 |
| `lod 0`、`lod 3` | 在 cluster 实验中查看全精度和三像素误差下的结果 |
| `lodview on`、`lodview off` | 按最终可见 meshlet 的实际 LOD 层级着色。窗口顶部显示颜色图例，RDG 面板列出各层绘制的三角数 |
| `motion 3 0`、`motion 1.35 -1`、`motion 0 0 1.2` | 给银狼指定向前速度（米/秒）、转速（弧度/秒）和可选的向右速度（米/秒）。三个例子分别是直跑、左转行走和向右横移 |
| `motionsearch kd`、`motionsearch scan` | 在 PCA + KD-tree（默认）与同一归一化度量的穷举查询之间切换。窗口显示最近一次查询实际对多少个姿势计算了完整代价 |
| `record "captures/live.mp4"`、`stoprecord` | 开始和结束视口录制，扩展名可用 `.mp4` 或 `.gif`。窗口模式按实际经过的时间输出 30 fps |
| `toon outline image`、`toon outline hull`、`toon outline quad`、`toon outline off` | 切换图像空间描边、法线外扩、退化四边形描边或原图 |
| `toon width 2`、`toon threshold 0.16` | 分别设置描边宽度（0.25–8 像素）和二分光照的法线点积阈值（−1 至 1） |
| `toon light -80`、`toon light 80` | 设置世界空间主光的水平角度。初始头部朝向对应 0° 正面光 |
| `toon sdf on`、`toon sdf off` | 在面部 SDF 与面部普通二分光之间切换 |
| `toon view face`、`toon view full` | 切换头肩特写与全身取景 |
| `texture neural`、`texture original`、`texture stats` | 在 Modern Hall 中切换特征网格与原始贴图，或打印三张纹理的有效载荷字节数；切换保留当前摄像机 |

## 帧流水线

| 执行位置 | 所有权与职责 |
| --- | --- |
| GPUI 主线程 | 持有可写 World、相机和模拟 Schedule，收集输入，按阶段并行计算系统更新 |
| 渲染线程 | 接收只读帧快照，由 Technique 并行提取几何与材质、声明资源和 pass、编译 RDG |
| RHI 线程 | 持有 Vulkan 队列与帧资源，通过共享 Rayon 任务池并行录制每个 pass，再按 RDG 顺序提交 |

两个容量为一的通道限制积压。主线程在通道满时继续处理输入和模拟。交互视口是嵌入 GPUI 的 Win32 子窗口。GPU 将着色结果复制到取得的 swapchain image 后执行 `vkQueuePresentKHR`。默认选 MAILBOX，surface 不支持时选择 FIFO，标题栏区域显示实际模式。窗口尺寸变化时重建交换链，每张交换链图像独立持有呈现完成信号量。普通交互只读取时间戳和少量调试计数。截图、录制和像素测试会读回最终的视口画面。

每个可并行录制的 pass 独占 command pool、primary command buffer 和 timestamp query pool。RDG 将光栅、计算、复制映射到同一通用队列族的最多三条独立 VkQueue，硬件队列不足时合并角色。同队列连续 pass 合成一次提交。跨队列依赖使用各队列自己的 timeline semaphore，同一生产队列的多条依赖合成一次最大值等待，后续提交跳过已覆盖的等待。窗口 acquire/present 使用 WSI 二值信号量。当前帧资源在 RHI 线程等待本帧 timeline 完成后复用。性能面板中的时间仅覆盖 GPU 上的 pass。

RHI 直接使用 vulkanalia 绑定。buffer 描述符通过 `vkWriteResourceDescriptorsEXT` 写入资源堆，shader binding 按 push data 中的槽号选择描述符。示例把纹理像素存入只读 storage buffer，采用最近邻采样。堆的对齐、步长与实现保留空间由设备查询结果决定。[Khronos 扩展规范](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_descriptor_heap.html)

## 替换实验模块

- **模拟**。实现 `engine::System`，调用 `Schedule::stage` 注册。同阶段的系统读取同一个世界快照，返回编辑操作，按注册顺序提交。需要消费上一阶段结果的物理、约束或动画系统放入后续阶段。系统内部也可以使用 Rayon 并行处理实体。
- **渲染**。实现 `render::Technique`，从 `Snapshot` 构建 `rdg::Graph`。仅替换几何选择时可实现 `GeometrySelection` 并复用 `VisibilityRenderer`。新增 compute/graphics pass 可提供自己的 SPIR-V 与资源绑定。示例中的 GPU 压紧与间接绘制经过普通 RDG 接口。
- **同步**。按资源生产、消费的顺序声明 pass，可以额外声明 `after` 边。编译器检查未初始化读取、非法用途和环，追踪 RAW、WAR、WAW，以及颜色与深度图像布局。同一访问阶段的连续读取共享同步。后端把一个 pass 前的 buffer 依赖合成一个内存屏障，连同图像转换一次下发 `vkCmdPipelineBarrier2`。当前粒度是整项资源。
- **实体**。`World` 使用稳定 ID 关联资产、节点和实例。网格顶点流、meshlet、分层加速数据、边面拓扑、材质路径、描边、骨骼绑定和扩展组件可以组合。大块几何与图像通过 Arc 共享。新系统的命名空间组件可存入 `Entity::components` 并随场景往返。
- **导入与存档**。glTF 节点、共享图元、基础色纹理和材质、皮肤以及动画轨道转换为实体。三角形列表、条带和扇形统一成三角形列表。存档带版本号，加载时校验引用与层次，保存时原子替换文件。导入失败时保留原有世界。当前材质预览着重基础色、金属度和粗糙度，动画实验的动作库采用下面的固定采样格式。

## 实验场景

### 2DGaussian 地砖压缩

剪枝版本保留 3,277 条高斯。控制台执行 `gaussian load assets/gaussian/FloorTiles-pruned.g2t`，再执行 `gaussian compact-tile` 或 `gaussian compact-bvh`。含索引的 GPU 地砖载荷分别为 68,620 和 61,640 字节，GPU 全纹理 RGB8 PSNR 为 30.60 dB。[数量、存储精度与结构实测](docs/gaussian-pruning.md) · [画质曲线与纹理局部](captures/gaussian-pruning.png)

Modern Hall 的地砖用 6,554 个二维高斯表示。控制台可用 `gaussian tile`、`gaussian bvh`、`gaussian scan` 和 `gaussian original` 在同一机位对照。RTX 5070 本地显存上，地砖着色 P50 为全扫描 2.66 ms、BVH 0.167 ms、默认 32 × 32 tile 0.115 ms。计入索引后，BVH 用 299,232 B，tile 用 326,188 B。`gaussian stats` 显示结构大小。用 `--experiment gaussian --scene FILE.g2t` 可独立预览从旧 PLY 打包的资产。[GPU 时间、显存分项和旧训练代码审查](docs/gaussian-texture.md)

### Modern Hall 神经纹理压缩

从 `assets/scenes/modern-hall/` 读取三张不透明基础色贴图。默认沿场景自带摄像机的位姿观察楼梯，在引擎的 50° 垂直视场角下比较木纹、地砖和壁纸。原作者为 NewSee2l035，本实验从场景纹理训练了压缩资产。[场景说明](assets/README.md) · [原作](https://blendswap.com/blend/6304) · [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/)

**训练。** PyTorch 同时优化每个 4 × 4 纹素块的 8 维特征和每张贴图独有的 MLP。PCA 初始化网格和解码器，随后在 RTX 5070 上用 Adam 做 1,600 步 RGB 均方误差拟合，每步随机抽取 2,048 个块。前向传播模拟 8 位特征量化，反向传播使用直通梯度。导出的 FGM1 文件包含 16 字节头、960 个 FP16 权重与偏置，以及每个块的 8 字节特征。894 × 894 的地砖按边缘复制补齐，采样和画质统计保留原始尺寸。随机访问特征网格的设计参考 NVIDIA 的研究，本文报告的是这个单层基础色原型的测量数据。[训练代码](tools/train_texture_grid.py) · [Random-Access Neural Compression of Material Textures](https://research.nvidia.com/publication/2023-08_random-access-neural-compression-material-textures)

**着色。** 可见性缓冲提供表面和三角形，材质 resolve 重建 UV。神经材质只读取一个块的特征，计算 8 → 16 的隐藏层和 ReLU，再按块内纹素位置选取 3 个输出，恢复 sRGB 后与原图模式走相同的线性化、材质颜色乘法和 PBR 光照。训练时最后一层同时预测块内 48 个值。原图模式与神经模式均采用重复寻址、最近邻、单一分辨率。神经模式的 RDG 材质缓冲只上传三个压缩包与默认白纹素，CPU 场景保留原始图像供交互对照。这里的压缩比只统计纹理有效载荷。[着色器](shaders/materials/texture_grid.glsl)

| 材质 | 分辨率 | 压缩包（字节） | 初始化 PSNR | PyTorch 训练后 PSNR |
| --- | ---: | ---: | ---: | ---: |
| 木纹 | 1600 × 1200 | 961,936 | 47.89 dB | 48.28 dB |
| 地砖 | 894 × 894 | 403,344 | 40.19 dB | 41.78 dB |
| 壁纸 | 512 × 512 | 133,008 | 33.16 dB | 34.88 dB |

PSNR 按导出文件重建的全分辨率 sRGB 和源图计算。三份文件合计 1,498,288 字节，含网格、权重及头部，为 4.02 bit/texel。相对于引擎 RGBA8 的 11,925,520 字节为 7.96 倍，相对于 RGB8 为 5.97 倍，相对于 glTF 中的 PNG 为 3.57 倍。800 × 500 的固定机位截图中，134,922 个像素使用这三张贴图，其着色后 RGB PSNR 为 43.98 dB。把 PyTorch 解码结果舍入到 RGB8 再用原图路径渲染，与 Vulkan 直接解码的画面逐通道最大相差 1 个色阶。[原始渲染](captures/modern-hall-original.png) · [神经渲染](captures/modern-hall-neural.png) · [原尺寸纹理局部与八倍误差图](captures/modern-hall-texture-detail.png)

**GPU 实测。** RTX 5070，release，855 × 551，MAILBOX，每帧 162,537 个像素进入被测材质 pass。原图与压缩缓冲均放在设备本地堆，按原图、神经、神经、原图顺序对照，每轮预热 60 帧、采样 180 帧。原图 pass 的两轮 P50 为 0.140 和 0.138 ms，神经 pass 为 0.144 和 0.147 ms。整帧 GPU 跨度 P50 为原图 5.08 和 5.05 ms、神经 5.05 和 5.14 ms。全部基础色 atlas 的 Vulkan 实际分配从 11,925,536 B 降到 1,498,304 B，节省 87.44%，含网格、权重、偏置、默认白纹素和分配对齐。[两个实验的显存分项](docs/gaussian-texture.md#两个压缩实验的显存)

```powershell
# 使用包含 PyTorch、NumPy 和 Pillow 的 Python 环境重新拟合
python tools/train_texture_grid.py assets/scenes/modern-hall/modern_hall_core.gltf assets/texture/modern-hall
cargo run --release --locked -- --experiment texture --command "texture stats"
cargo run --release --locked -- --experiment texture --command "texture original" --benchmark 480
cargo run --release --locked -- --experiment texture --command "texture neural" --benchmark 480
```

**卡通人物**使用 `assets/characters/silver-wolf/scene.gltf` 的六个图元、原有蒙皮和基础色贴图。启动时显示面部近景，窗口上方可切换三种描边、旋转光源或开关面部 SDF。对照时可以先选 Off，再在同一镜头下依次切换描边方式。

- 图像空间描边在材质着色后读取可见性 ID 和深度，在人物外轮廓及有相对深度跳变的遮挡边缘着墨。
- 法线外扩在 mesh shader 中沿蒙皮后顶点法线的投影方向按像素宽度扩张背面，和原人物共同做深度测试。开口、单面发片的轮廓可能不连续。
- 退化四边形描边在导入时构建边与邻面的关联。位置和骨骼影响相同的顶点跨 UV 接缝合并。GPU 对每条边生成两个初始零面积三角形，只有开边或邻面朝向相反的轮廓边向外展开。几何描边和人物表面共用可见性缓冲。
- 人物材质以 `N·L` 阈值选择亮色或暗色。面部材质 `material_5` 有单独的实验用 SDF，程序为脸部 UV 上的鼻侧和脸颊轮廓生成七张带符号距离场。着色时在 `Head_08` 的局部坐标系中计算光照角度，镜像采样左右光照并插值相邻角度。原 glTF 仅提供基础色贴图。纹理、拓扑及实验设置随场景存档。

人物资产为 X9_YT 的 *Star Rail - Silver Wolf*，采用 CC BY 4.0 许可。本实验添加了着色、描边和面部距离场。[模型原页](https://sketchfab.com/3d-models/star-rail-silver-wolf-7fc76dd8fe5444ccb559cc497d4bfb23) · [作者](https://sketchfab.com/X9_YT) · [许可](https://creativecommons.org/licenses/by/4.0/)

**Cluster LOD** 默认从 `assets/scenes/dragon/dragon_core.gltf` 取出四个龙图元，构造一份常驻几何，由 36 条龙共享。橙色方块只有普通顶点和索引，绑定卡通材质。龙使用 PBR 材质。带普通网格标记的资产在重新打开或预处理后仍走普通路径。

meshoptimizer 生成的简化 DAG 描述 cluster group 之间的替换关系。预处理修正父子误差和投影包围球，再以 group 为叶子构建独立的八叉空间 BVH。每个叶子引用一个 group，每个 group 保存其全部 cluster 的连续区间。内部节点保存几何包围球、误差投影包围球及后代的最大父级误差。[Nanite SIGGRAPH 技术演讲](https://advances.realtimerendering.com/s2021/Karis_Nanite_SIGGRAPH_Advances_2021_final.pdf)

RDG 为整帧注册一份 BVH 节点工作列表和一份候选 cluster 列表。GPU 实例视锥剔除将可见实例的根放入节点列表。逐层间接派发时，每个 64 线程工作组可处理 8 个来自任意实例的候选节点，每个节点由 8 个线程并行检查子项。通过视锥和父级投影误差测试的内部子项写入下一层，叶子则立即将 group 的成员追加到候选 cluster 列表。独立的 cluster pass 检查每个成员的几何包围球和细化误差。节点缓冲的两个区间交替读写，各层的派发参数在 GPU 上产生。RDG 中的 `GPU BVH level N` 对应实际派发，`Peak BVH frontier` 记录帧内单层待处理节点数的峰值。

主阶段用上帧 Hi-Z 标记可能被遮挡的 cluster，mesh shader 将其余 cluster 写入可见性与深度缓冲。普通网格也用 mesh shader 光栅化。两种几何的深度一起生成本帧 Hi-Z，复测通过的 cluster 由同一 mesh 管线补画，最终深度留给下一帧。

**材质着色**。普通网格、蒙皮和 cluster 写入同一 `R32G32_UINT` 可见性缓冲，两个整数标识表面实例和三角形。GPU 按可见像素绑定的材质统计分桶、分配连续区间并填入像素索引。每种着色程序通过间接 dispatch 只处理自己的像素，重建透视正确的属性并访问纹理。PBR 和 Toon 有独立的 GLSL 实现。新增着色模型可以复用几何光栅和通用分桶，在独立的 GLSL 文件实现自己的材质函数，复用 `shaders/resolve.glsl` 编译 compute SPIR-V。用 `Arc::make_mut(&mut engine.materials).register("lab.ink", spirv)?` 注册内核，将返回的标识赋给 `material.shading`。稳定标识随场景保存，分桶序号在运行时分配。旧存档中实例级的着色设置在读取时迁移为材质。分桶设计参考 Epic 的 GPU-driven materials 演讲。[Epic 技术演讲与幻灯片](https://www.unrealengine.com/blog/take-a-deep-dive-into-nanite-gpu-driven-materials)

```powershell
cargo run --locked -- --experiment clusters --command "lodview on"
cargo run --locked -- --experiment clusters --command "lod 0.25" --command "lodview on" --capture target/dragons-lod.png
```

**银狼 Motion Matching** 使用 NVIDIA Kimodo-SOMA-RP-v1.2 在 RTX 5070 上生成的 39 段动作。输入是逐帧的水平根轨迹与朝向，覆盖静立、慢走、走、跑、左右转弯走、左右转弯跑、起步、减速停步、后退和左右横移。每类使用种子 41、42、43，每段 4 秒、30 Hz，总计 156 秒、4,680 帧。生成使用空文本、40 步去噪、CFG 权重 1.8，关闭生成后的脚部修正。[Kimodo 源码](https://github.com/nv-tlabs/kimodo) · [模型与权重](https://huggingface.co/nvidia/Kimodo-SOMA-RP-v1.2)

离线转换先对齐 SOMA 与银狼的静止骨轴，再沿银狼的骨架层次计算 21 个关节的局部四元数，并按腿长缩放轨迹。动作库位于 `assets/motion/`，运行示例只需银狼模型和已烘焙的数据。每段最后 23 帧用于提供前瞻，整库共有 3,783 个可检索的起点。代价包含未来第 6、14、23 帧的位移与朝向、双脚位置和速度，以及切换惩罚。在 30 Hz 播放时，每五个采样步重新检索一次，收到新指令时提前检索。窗口内显示实际选中的片段、样本和代价。预览固定水平根位置，保留转身和髋部起伏。当前实现未加入运行时锁脚或地形 IK。

原始 SOMA 输出保存在 `assets/motion/source/`。可以直接用第二条命令重新做银狼的骨架映射。要从头生成，在支持 CUDA 的 Python 环境安装官方 Kimodo（本次使用提交 `e1c219c99b4b94ba48be1fb6a1cbc64ecee4c954`）及模型权重，并给第一条命令指定空目录。生成脚本会跳过已有的片段。模型权重版本记在 `bank.json`。

```powershell
python tools/generate_kimodo.py assets/motion/source
python tools/bake_silver_wolf.py assets/motion/source assets/characters/silver-wolf/scene.gltf assets/motion
```

### Motion Matching 的查询与性能

每次查询构造 24 维角色空间特征，包含三个时刻的水平位移与朝向（12 维），以及左右脚踝的位置和速度（各 6 维）。只用可检索的 3,783 个起点拟合各维的均值与标准差。对库和查询使用完全相同的变换 `(x − mean) × gain`，其中 `gain = sqrt(weight) / max(std, floor)`。轨迹位置、朝向、脚位置、脚速度的权重依次为 5、1.6、0.8、0.018，尺度下限依次为 0.05 米、0.1、0.05 米和 0.15 米/秒，防止放大近乎常量的朝向分量。

在加权归一化空间拟合 PCA，前六个正交主成分保留 96.68% 的方差。按六维中跨度最大的轴做中位数切分，得到 511 个树节点、256 个叶子，每叶 14 或 15 个候选。均值、增益、投影矩阵、节点包围盒与叶子样本号一起写入 `assets/motion/search.json`。运行时用可用的顺播候选初始化上界，优先访问距离查询点较近的子树。投影点到子树包围盒的距离为原空间距离提供下界，叶子仍用完整的 24 维代价和 0.18 的跳转惩罚精排。全扫描与 KD-tree 的候选排除、连续播放和同分规则相同。

Ryzen 7 9700X，release 构建，单角色，动作库驻内存，交替执行两种检索。预热 240 次后对同一组连续移动输入测量 2,160 次。全部 2,400 次查询以及另外 512 次扰动或片段边界测试，两条路径均得到相同的样本号与代价。

| CPU 范围 | P50 | P95 |
| --- | ---: | ---: |
| 全扫描查询 | 16.60 µs | 17.00 µs |
| PCA + KD-tree 查询 | 0.90 µs | 2.20 µs |
| 一次银狼系统更新（含 KD 查询及 21 关节回写） | 5.60 µs | 7.10 µs |
| 一次银狼系统更新（顺播及关节回写） | 4.60 µs | 4.90 µs |

全扫描实际精算候选数 P50/P95 为 3,769/3,773，KD-tree 为 107/354。银狼完整窗口另用 RTX 5070、855 × 551、MAILBOX、16 个 worker，固定直走输入，关闭录制和验证层，各预热 60 帧后采样 600 帧。KD 模式的帧间隔 P50/P95 为 5.006/5.416 ms，GPU 跨度为 1.329/1.456 ms，每秒 200.0 次呈现请求。全扫描为 4.989/5.426 ms、1.343/1.479 ms、每秒 200.1 次。

```powershell
cargo test --release --locked --lib benchmark_silver_motion -- --ignored --nocapture
cargo run --release --locked -- --experiment motion --command "motionsearch kd" --benchmark 600
cargo run --release --locked -- --experiment motion --command "motionsearch scan" --benchmark 600
```

### 录制实验

录制使用 FFmpeg。可以通过 `RENDER_LAB_FFMPEG` 指定可执行文件，或将其放入 PATH。下面的命令以固定时间步长运行同一场景，同时生成循环 GIF 和 H.264 MP4。每个输出帧对应一次 Vulkan 渲染，即使机器渲染速度低于目标帧率，离线录制也会保留每一帧。

```powershell
cargo run --release --locked -- --experiment motion --fps 25 --duration 8 --command "motion 0 0" --at 1 "motion 1.35 0" --at 3 "motion 3 0" --at 5 "motion 1.35 -1" --at 7 "motion 0 0" --record captures/silver-wolf-motion.gif --record captures/silver-wolf-motion.mp4
```

两种查询模式分别录制了这组输入。8 秒内所选片段与样本的切换记录完全一致，两份 MP4 解码后的 200 帧图像哈希逐帧一致。[银狼动作匹配 GIF](captures/silver-wolf-motion.gif) · [同一段画面的 MP4](captures/silver-wolf-motion.mp4)

## 验证

```powershell
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
$env:VK_INSTANCE_LAYERS = "VK_LAYER_KHRONOS_validation"
$env:VK_LAYER_VALIDATE_SYNC = "1"
cargo test --locked --all-targets -- --include-ignored
```

最后一条命令执行真实 Vulkan 测试，包括 MAILBOX 优先选择、交换链缩放与直接呈现、跨队列同步、材质切换、相机越过遮挡物后的补绘、普通网格与多龙共存、LOD 切换及蒙皮输出。CPU 测试覆盖 RDG 依赖、相机操作、场景迁移和往返、glTF 导入、cluster cut 和动作检索。

### 实机测量

在原生窗口运行 `cargo run --release --locked -- --experiment clusters --benchmark 900`，固定相机，预热 60 帧后采样并退出。在 PowerShell 中通过 `$env:RENDER_LAB_WORKERS="1"` 或 `"16"` 对照同一场景的录制开销。性能测量关闭验证层，正确性测试单独启用验证层。

输出包含首次实际执行的帧、稳态 P50/P95、呈现请求速率、每个 pass 的 GPU 时间、依赖和实际屏障、时间线等待，以及参与录制的工作线程数与重叠录制数。支持 calibrated timestamps 的设备还会报告从最早开始到最晚结束的跨队列 GPU 时间跨度。帧间隔取 RHI 完成时间，MAILBOX 允许显示器丢弃已提交的旧画面。

以下历史基线来自改造前按实例分配工作组的版本。2026-11-20 在 Ryzen 7 9700X、RTX 5070、驱动 621.14 上测试固定视角。窗口视口为 855 × 551，显示模式为 MAILBOX，使用 Release 构建。每项先预热 60 帧，默认视图禁用 LOD 染色。

| 像素误差 | worker | 采样帧 | 呈现请求/秒 | 帧间隔 P50 / P95 | GPU 跨度 P50 / P95 | 录制 P50 |
| --- | ---: | ---: | ---: | --- | --- | ---: |
| 0.25 | 16 | 900 | 199.99 | 5.003 / 5.173 ms | 3.606 / 3.737 ms | 0.092 ms |
| 0.25 | 1 | 900 | 200.00 | 5.002 / 5.214 ms | 3.492 / 3.604 ms | 0.147 ms |
| 0（L0） | 16 | 300 | 73.78 | 13.546 / 14.725 ms | 13.216 / 14.297 ms | 0.105 ms |

默认帧绘制 309,614 个龙三角形，L0 绘制 9,755,587 个。默认配置中参与命令录制的 worker 中位数为 16，单帧内同时处于录制区间的峰值中位数为 14。图有 39 个 pass 和 50 条依赖边，15 条边跨队列。90 项资源屏障需求合并为 38 次 RDG 屏障调用，包含 37 个全局内存屏障和 9 个图像屏障。交换链额外有两次布局转换。实际 11 次队列提交，各 timeline 合计 11 次 signal、11 个跨队列 wait 项。该组数据对应上述视口、场景和渲染设置。

采用全局列表后，在相同视口、0.25 像素误差、16 个 worker 下对新旧版本各预热 60 帧并采样 300 帧。新路径有 4 轮 BVH 派发，最宽一层有 403 个待处理节点。进入 cluster 测试的条目由 14,479 降至 11,031。两条路径的主阶段均绘制 5,319 个 cluster，各 LOD 三角数一致。剔除相关 pass 的 GPU 耗时之和 P50 从 0.0090 ms 增至 0.0268 ms，整帧 GPU 跨度 P50 分别为 3.491 ms 和 2.972 ms。新图共有 43 个 pass，120 项资源屏障需求合并为 42 次屏障调用，时间线等待为 11 项。
