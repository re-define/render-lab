# 实验操作与历史测量

以下命令从包含 `Cargo.toml` 的引擎根目录运行。先在[实验目录](experiment-catalog.md)选择场景，再用控制台切换算法或固定观察条件。[框架启动](../README.md) · [验证规范](../CONTRIBUTING.md)

## 窗口与控制台

W/S 沿视线前进和后退，A/D 左右平移，在视口内按住鼠标右键改变朝向，F 根据场景包围盒重新取景。按 / 或 Enter 打开控制台，输入命令后按 Enter 执行，Esc 取消。Windows 下支持粘贴路径。工具栏可暂停模拟或隐藏 RDG。右侧图中的每个节点对应一次 pass，颜色标记队列，连线来自 RDG 编译结果，点击节点可在面板顶部查看屏障。

银狼 Motion Matching 中，按住 I/K 向角色前方/后方移动，J/L 向左/右横移。I+J 和 I+L 分别边走边左转和右转。松开后角色平滑停下。相机跟随角色平移，仍可用 WASD 和鼠标调整视角。地面蓝线显示未来轨迹，圆点标出 0.20、0.47 和 0.77 秒后的预测位置。

| 命令 | 效果 |
| --- | --- |
| `timeline frame.json` | 导出最近完成帧的 GPU 各队列区间、等待来源和依赖链，可用 Perfetto 打开。 |
| `help`、`stats` | 查看命令、实体数、pass 数和 GPU 区间耗时之和 |
| `fit`、`pause`、`rdg` | 重新取景、切换模拟暂停、切换依赖图 |
| `hud on`、`hud off` | 在视口上显示或隐藏 GPU 时间、呈现模式和抗锯齿状态 |
| `post taa`、`post off`、`post reset`、`post status` | 启用 TAA、关闭后处理、清除时域历史或查看后端状态 |
| `post dlss`、`post dlaa` | 在启用 NVIDIA NGX 的构建上选择 DLSS Quality 或原生分辨率 DLAA。[森林对照与运行环境](../docs/viewport-post.md) |
| `post view motion`、`post view reprojection`、`post view off` | 显示屏幕运动向量、检查重投影误差或返回原先的抗锯齿模式 |
| `import "路径"` | 把 glTF 的资产和实例加入当前场景 |
| `save "路径"`、`load "路径"` | 保存或打开自包含场景，路径可以包含空格 |
| `lod 0`、`lod 3` | 在 cluster 实验中查看全精度和三像素误差下的结果 |
| `lodview on`、`lodview off` | 按最终可见 meshlet 的实际 LOD 层级着色。窗口顶部显示颜色图例，RDG 面板列出各层绘制的三角数 |
| `vegetation auto`、`vegetation triangles`、`vegetation voxels` | 查看混合 DAG、全三角对照或放宽误差后的混合粗级 |
| `vegetation raster auto/compute/mesh/compare` | 自动选择、强制计算光栅、mesh 对照、逐像素重放同一份砖列表。详见[体素计算光栅](../docs/procedural-vegetation.md#体素计算光栅) |
| `vegetation view paths`、`vegetation view beauty`、`vegetation camera near` | 橙色标出三角植被，青色标出体素植被。摄像机也可选择 `overview`、`far` 或 `forest` |
| `vegetation pixels 1.45`、`vegetation wind 1.5`、`vegetation time 0` | 调整混合 DAG 的屏幕误差、风力和风场时间。参考[植被实验方案](../docs/procedural-vegetation.md) |
| `effect restart`、`effect time 1.9`、`pause` | 台阶流水或破碎与消融实验中重播、定位到指定秒数或切换暂停。左侧晶体炸裂，右侧晶体逐渐消融。 |
| `light list`、`light point 2 3 5 60 15 1 0.6 0.25`、`light sun 0.42 -0.73 -0.55 1.85` | 查看场景光源实体，或添加点光源和平行光。参考[场景光源实例](../docs/scene-lights.md) |
| `motion 3 0`、`motion 1.35 -1`、`motion 0 0 1.2` | 给银狼指定向前速度（米/秒）、转速（弧度/秒）和可选的向右速度（米/秒）。三个例子分别是直跑、左转行走和向右横移 |
| `motionsearch kd`、`motionsearch scan` | 在 PCA + KD-tree（默认）与同一归一化度量的穷举查询之间切换。窗口显示最近一次查询实际对多少个姿势计算了完整代价 |
| `record "captures/live.mp4"`、`stoprecord` | 开始和结束视口录制，扩展名可用 `.mp4` 或 `.gif`。窗口模式按实际经过的时间输出 30 fps |
| `toon outline image`、`toon outline hull`、`toon outline quad`、`toon outline off` | 切换图像空间描边、法线外扩、退化四边形描边或原图 |
| `toon width 2`、`toon threshold 0.16` | 分别设置描边宽度（0.25–8 像素）和二分光照的法线点积阈值（−1 至 1） |
| `toon light -80`、`toon light 80` | 设置世界空间主光的水平角度。初始头部朝向对应 0° 正面光 |
| `toon sdf on`、`toon sdf off` | 在面部 SDF 与面部普通二分光之间切换 |
| `toon view face`、`toon view full` | 切换头肩特写与全身取景 |
| `texture neural`、`texture original`、`texture stats` | 在 Modern Hall 中切换特征网格与原始贴图，或打印三张纹理的有效载荷字节数；切换保留当前摄像机 |
| `gi off`、`gi ssgi`、`gi probes`、`gi ray` | 切换纯直射、屏幕空间 GI、辐照度探针或单次漫反射世界空间追踪 |
| `gi view indirect`、`gi view combined`、`gi camera detail` | 单独查看间接光、恢复合成或观察遮挡隔板 |
| `shadow hard`、`shadow pcf`、`shadow pcss`、`shadow ray` | 比较硬阴影图、固定 3×3 PCF、PCSS 和 16 样本面积光追踪 |
| `shadow view visibility`、`shadow size 1.1`、`shadow bias 0.018` | 显示可见度，并调整面积光半宽和偏移（单位为米） |
| `shadow camera contact`、`shadow camera distance` | 检查细栅栏、接触阴影和远处接收面。`shadow camera overview` 恢复总览 |
| `rt view beauty`、`rt view normals`、`rt view distance`、`rt view reflections` | 查看硬件光追的合成结果、世界法线、命中距离和单次反射 |
| `restir uniform`、`restir ris`、`restir reuse`、`restir reference` | 切换单样本均匀采样、单帧 RIS、时空 ReSTIR DI 或分层高样本参考 |
| `restir denoise off`、`restir denoise spatial`、`restir denoise svgf` | 独立切换关闭降噪、空间 à-trous、时域累积加方差引导的 à-trous |
| `restir history-stats on/off` | 统计表面、历史接受和位移至少一个像素的受跟踪表面数 |
| `restir source auto`、`restir source scene`、`restir source bank` | 自动选择光源表、显式使用场景实例，或显式使用程序化灯库 |
| `restir lights 65536`、`restir candidates 16`、`restir samples 512`、`restir camera contact` | 调整程序灯库的灯数、每像素初始候选数、参考模式的样本数，或切换近景 |
| `ibl yaw 90`、`ibl ev -1`、`ibl sky on` | 旋转 HDR 环境、调整曝光 EV、显示真实环境背景 |
| `ibl view combined`、`ibl view diffuse`、`ibl view specular` | 查看完整 PBR、环境漫反射或环境镜面反射 |

## 实验场景

### 多光源 ReSTIR DI 与降噪

冷暖两组点光照亮柱廊、地面和球体。光强分布含少数强光和许多低能量灯，柱子与隔板造成遮挡。灯数默认 16,384，可调到 65,536。选择不同灯数时保持总功率标度相近。`restir uniform` 对每个可见像素均匀取一盏灯。`restir ris` 计算 16 个初始候选的未遮挡贡献并选一盏灯。`restir reuse` 进一步从上帧同一表面和本帧五个邻居重采样，最终仍只对所选灯做一次 GPU BVH 可见性查询。历史记录随分辨率、光源数、算法和重载变化重置。时域复用用共享表面运动查找上一帧像素，并校验标签、预期深度与法线。空间复用仍用本帧位置和法线。[银狼与混合光照对照](../docs/lighting-comparison.md)当前场景使用静态点灯，有限历史与空间复用会引入相关性和估计偏差。[ReSTIR DI 原文](https://research.nvidia.com/labs/rtr/publication/bitterli2020spatiotemporal/)

空间降噪执行两轮边缘保持的 à-trous。SVGF 风格降噪重投影直接光、一阶和二阶亮度矩，再执行三轮方差引导的 à-trous。耗时分别显示在 RDG。降噪作用于线性入射光，最后才乘材质基础色并色调映射。[SVGF 原文](https://research.nvidia.com/labs/rtr/publication/schied2017spatiotemporal/)

以下命令预热 23 帧并保存第 24 帧。同机位基准可追加 `--command "restir uniform" --command "restir denoise off"`。高样本参考可用 `--command "restir reference" --command "restir samples 512" --command "restir denoise off" --warmup 0`。如果灯数不超过样本数，参考模式会逐灯计算。

```powershell
cargo run --release --locked -- --experiment restir --warmup 23 --capture target/restir.png
```

RTX 5070 的 800×500 固定机位截图中，取画面 `x=100..699, y=150..479`，以 512 样本分层渲染作为参考。显示空间 RGB PSNR 在 16,384 盏灯下分别为 7.23 dB（均匀采样）、9.47 dB（RIS）、12.49 dB（ReSTIR）、21.29 dB（ReSTIR 加空间降噪）和 21.67 dB（ReSTIR 加 SVGF）。在达到指定预热帧后的截图上，ReSTIR 加 SVGF 的表面重建、复用、阴影射线、滤波及历史写入等 GPU pass 的时间之和，在 16,384 和 65,536 盏灯下分别约 1.46 与 1.49 ms。

### HDR 环境光 PBR

18 个球体展示三档金属度（0、0.5、1）和六档粗糙度（0.06、0.19、0.35、0.52、0.73、0.96）。启动时并行由线性 HDR 图生成漫反射辐照度、七个 GGX 预过滤层级和 128×128 BRDF 积分 LUT，同一进程内复用这些数据。可见性缓冲材质 pass 用 split-sum 进行镜面近似，按金属度与 Fresnel 分配漫反射和镜面能量。房间照明完全取自环境图，基础反射可用 `ibl yaw` 转动。粗糙度越高，高光越宽。金属度为 1 的一行在漫反射视图中为黑色。[Karis 的 split-sum 实现](https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf) · [HDR 环境图](https://polyhaven.com/a/studio_small_02)

### GI、阴影和硬件光追

**GI** 是开放前面的 Cornell 风格房间，带红绿反弹墙、矩形面积灯、白色方块和球体，以及遮挡后的蓝色隔间。相机、直射和材质统一，便于看色污染和遮挡差异。`gi ssgi` 在屏幕空间用可见表面做漫反射光线步进；`gi probes` 用 7×5×7 个世界空间探针保存六方向辐照度，采样时对邻近探针做可见性测试；`gi ray` 使用 GPU 上的软件 BVH 遍历计算一次漫反射。三者在半分辨率求间接光，再以可见表面的几何信息做双边上采样。当前使用固定空间采样。追踪模式仍可见少量采样噪声，探针模式会抹平部分细节。[辐照度探针研究](https://research.nvidia.com/index.php/publication/2019-05_dynamic-diffuse-global-illumination-ray-traced-irradiance-fields)

**阴影** 带细栅栏、接触地面的球、留出 0.42 米和 1.15 米间隙的悬空球、远端球及倾斜接收面。硬阴影、PCF、PCSS 从同一光源机位光栅化深度。PCSS 先搜索遮挡物，再根据深度差估计过滤半径。`shadow ray` 在同一几何上对面积光执行 16 次可见性采样，提供对照。`shadow view visibility` 用灰度直接显示光源可见度，`shadow size 0` 可把 PCSS 恢复为点光源比较。[PCSS 算法](https://developer.nvidia.com/gpugems/gpugems3/part-ii-light-and-shadows/chapter-8-summed-area-variance-shadow-maps)

**硬件光追** 用镜面、金属球、遮挡物和镜头外的绿色立柱测试实际的 GPU 遍历。RDG 分别记录构建三角形 BLAS、构建实例化 TLAS 和追踪光线，RHI 管理构建缓冲、同步和 shader binding table。光追程序通过 `vkCmdTraceRaysKHR` 调度 raygen、miss 和 closest-hit。raygen 依次发送主光线、可见性光线和一次镜面反射，便于逐阶段观察。静态 BLAS 按不可变几何版本跨帧槽共享，兼容拓扑的变形可使用 UPDATE。可用 `rays software` 和 `rays hardware` 比较固定点光源的遮挡结果，`rays normals` 查看射线几何法线，`rays off` 返回材质画面。运行此场景需要 `VK_KHR_acceleration_structure` 和 `VK_KHR_ray_tracing_pipeline`，GI 和阴影使用普通计算着色器和现有的可见性缓冲渲染。[Vulkan 光追指南](https://docs.vulkan.org/guide/latest/extensions/ray_tracing.html)

场景由引擎生成，几何、材质和实验设置可以用 `save`、`load` 保存和恢复。抓取固定机位时，可用 `--command` 设置比较模式。

### 2DGaussian 地砖压缩

剪枝版本保留 3,277 条高斯。控制台执行 `gaussian load assets/gaussian/FloorTiles-pruned.g2t`，再执行 `gaussian compact-tile` 或 `gaussian compact-bvh`。含索引的 GPU 地砖载荷分别为 68,620 和 61,640 字节，GPU 全纹理 RGB8 PSNR 为 30.60 dB。[数量、存储精度与结构实测](../docs/gaussian-pruning.md) · [画质曲线与纹理局部](../captures/gaussian-pruning.png)

Modern Hall 的地砖用 6,554 个二维高斯表示。控制台可用 `gaussian tile`、`gaussian bvh`、`gaussian scan` 和 `gaussian original` 在同一机位对照。RTX 5070 本地显存上，地砖着色 P50 为全扫描 2.66 ms、BVH 0.167 ms、默认 32 × 32 tile 0.115 ms。计入索引后，BVH 用 299,232 B，tile 用 326,188 B。`gaussian stats` 显示结构大小。用 `--experiment gaussian --scene FILE.g2t` 可独立预览从旧 PLY 打包的资产。[GPU 时间、显存分项和旧训练代码审查](../docs/gaussian-texture.md)

### Modern Hall 神经纹理压缩

从 `assets/scenes/modern-hall/` 读取三张不透明基础色贴图。默认沿场景自带摄像机的位姿观察楼梯，在引擎的 50° 垂直视场角下比较木纹、地砖和壁纸。原作者为 NewSee2l035，本实验从场景纹理训练了压缩资产。[场景说明](../assets/README.md) · [原作](https://blendswap.com/blend/6304) · [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/)

**训练。** PyTorch 同时优化每个 4 × 4 纹素块的 8 维特征和每张贴图独有的 MLP。PCA 初始化网格和解码器，随后在 RTX 5070 上用 Adam 做 1,600 步 RGB 均方误差拟合，每步随机抽取 2,048 个块。前向传播模拟 8 位特征量化，反向传播使用直通梯度。导出的 FGM1 文件包含 16 字节头、960 个 FP16 权重与偏置，以及每个块的 8 字节特征。894 × 894 的地砖按边缘复制补齐，采样和画质统计保留原始尺寸。随机访问特征网格的设计参考 NVIDIA 的研究，本文报告的是这个单层基础色原型的测量数据。[训练代码](../tools/train_texture_grid.py) · [Random-Access Neural Compression of Material Textures](https://research.nvidia.com/publication/2023-08_random-access-neural-compression-material-textures)

**着色。** 可见性缓冲提供表面和三角形，材质 resolve 重建 UV。神经材质只读取一个块的特征，计算 8 → 16 的隐藏层和 ReLU，再按块内纹素位置选取 3 个输出，恢复 sRGB 后与原图模式走相同的线性化、材质颜色乘法和 PBR 光照。训练时最后一层同时预测块内 48 个值。原图模式与神经模式均采用重复寻址、最近邻、单一分辨率。神经模式的 RDG 材质缓冲只上传三个压缩包与默认白纹素，CPU 场景保留原始图像供交互对照。这里的压缩比只统计纹理有效载荷。[着色器](../shaders/materials/texture_grid.glsl)

| 材质 | 分辨率 | 压缩包（字节） | 初始化 PSNR | PyTorch 训练后 PSNR |
| --- | ---: | ---: | ---: | ---: |
| 木纹 | 1600 × 1200 | 961,936 | 47.89 dB | 48.28 dB |
| 地砖 | 894 × 894 | 403,344 | 40.19 dB | 41.78 dB |
| 壁纸 | 512 × 512 | 133,008 | 33.16 dB | 34.88 dB |

PSNR 按导出文件重建的全分辨率 sRGB 和源图计算。三份文件合计 1,498,288 字节，含网格、权重及头部，为 4.02 bit/texel。相对于引擎 RGBA8 的 11,925,520 字节为 7.96 倍，相对于 RGB8 为 5.97 倍，相对于 glTF 中的 PNG 为 3.57 倍。800 × 500 的固定机位截图中，134,922 个像素使用这三张贴图，其着色后 RGB PSNR 为 43.98 dB。把 PyTorch 解码结果舍入到 RGB8 再用原图路径渲染，与 Vulkan 直接解码的画面逐通道最大相差 1 个色阶。[原始渲染](../captures/modern-hall-original.png) · [神经渲染](../captures/modern-hall-neural.png) · [原尺寸纹理局部与八倍误差图](../captures/modern-hall-texture-detail.png)

**GPU 实测。** RTX 5070，release，855 × 551，MAILBOX，每帧 162,537 个像素进入被测材质 pass。原图与压缩缓冲均放在设备本地堆，按原图、神经、神经、原图顺序对照，每轮预热 60 帧、采样 180 帧。原图 pass 的两轮 P50 为 0.140 和 0.138 ms，神经 pass 为 0.144 和 0.147 ms。整帧 GPU 跨度 P50 为原图 5.08 和 5.05 ms、神经 5.05 和 5.14 ms。全部基础色 atlas 的 Vulkan 实际分配从 11,925,536 B 降到 1,498,304 B，节省 87.44%，含网格、权重、偏置、默认白纹素和分配对齐。[两个实验的显存分项](../docs/gaussian-texture.md#两个压缩实验的显存)

```powershell
# 使用包含 PyTorch、NumPy 和 Pillow 的 Python 环境重新拟合
python tools/train_texture_grid.py assets/scenes/modern-hall/modern_hall_core.gltf assets/texture/modern-hall
cargo run --release --locked -- --experiment texture --command "texture stats"
cargo run --release --locked -- --experiment texture --command "texture original" --benchmark 480
cargo run --release --locked -- --experiment texture --command "texture neural" --benchmark 480
```

### 银狼卡通着色

人物使用 `assets/characters/silver-wolf/scene.gltf` 的六个图元、原有蒙皮和基础色贴图。启动时显示面部近景，窗口上方可切换三种描边、旋转光源或开关面部 SDF。对照时可以先选 Off，再在同一镜头下依次切换描边方式。

- 图像空间描边在材质着色后读取可见性 ID 和深度，在人物外轮廓及有相对深度跳变的遮挡边缘着墨。
- 法线外扩在 mesh shader 中沿蒙皮后顶点法线的投影方向按像素宽度扩张背面，和原人物共同做深度测试。开口、单面发片的轮廓可能不连续。
- 退化四边形描边在导入时构建边与邻面的关联。位置和骨骼影响相同的顶点跨 UV 接缝合并。GPU 对每条边生成两个初始零面积三角形，只有开边或邻面朝向相反的轮廓边向外展开。几何描边和人物表面共用可见性缓冲。
- 人物材质以 `N·L` 阈值选择亮色或暗色。面部材质 `material_5` 有单独的实验用 SDF，程序为脸部 UV 上的鼻侧和脸颊轮廓生成七张带符号距离场。着色时在 `Head_08` 的局部坐标系中计算光照角度，镜像采样左右光照并插值相邻角度。原 glTF 仅提供基础色贴图。纹理、拓扑及实验设置随场景存档。

人物资产为 X9_YT 的 *Star Rail - Silver Wolf*，采用 CC BY 4.0 许可。本实验添加了着色、描边和面部距离场。[模型原页](https://sketchfab.com/3d-models/star-rail-silver-wolf-7fc76dd8fe5444ccb559cc497d4bfb23) · [作者](https://sketchfab.com/X9_YT) · [许可](https://creativecommons.org/licenses/by/4.0/)

### Cluster LOD

**Cluster LOD** 默认从 `assets/scenes/dragon/dragon_core.gltf` 取出四个龙图元，构造一份常驻几何，由 36 条龙共享。橙色方块只有普通顶点和索引，绑定卡通材质。龙使用 PBR 材质。带普通网格标记的资产在重新打开或预处理后仍走普通路径。

meshoptimizer 生成的简化 DAG 描述 cluster group 之间的替换关系。预处理修正父子误差和投影包围球，再以 group 为叶子构建独立的八叉空间 BVH。每个叶子引用一个 group，每个 group 保存其全部 cluster 的连续区间。内部节点保存几何包围球、误差投影包围球及后代的最大父级误差。[Nanite SIGGRAPH 技术演讲](https://advances.realtimerendering.com/s2021/Karis_Nanite_SIGGRAPH_Advances_2021_final.pdf)

RDG 为整帧注册一份 BVH 节点工作列表和一份候选 cluster 列表。GPU 实例视锥剔除将可见实例的根放入节点列表。逐层间接派发时，每个 64 线程工作组可处理 8 个来自任意实例的候选节点，每个节点由 8 个线程并行检查子项。通过视锥和父级投影误差测试的内部子项写入下一层，叶子则立即将 group 的成员追加到候选 cluster 列表。独立的 cluster pass 检查每个成员的几何包围球和细化误差。节点缓冲的两个区间交替读写，各层的派发参数在 GPU 上产生。RDG 中的 `GPU BVH level N` 对应实际派发，`Peak BVH frontier` 记录帧内单层待处理节点数的峰值。

主阶段用上帧 Hi-Z 标记可能被遮挡的 cluster，mesh shader 将其余 cluster 写入可见性与深度缓冲。普通网格也用 mesh shader 光栅化。两种几何的深度一起生成本帧 Hi-Z，复测通过的 cluster 由同一 mesh 管线补画，最终深度留给下一帧。

**材质着色**。普通网格、蒙皮和 cluster 写入同一 `R32G32_UINT` 可见性缓冲，两个整数标识表面实例和三角形。GPU 按可见像素绑定的材质统计分桶、分配连续区间并填入像素索引。每种着色程序通过间接 dispatch 只处理自己的像素，重建透视正确的属性并访问纹理。PBR 和 Toon 有独立的 GLSL 实现。新增着色模型可以复用几何光栅和通用分桶，在独立的 GLSL 文件实现自己的材质函数，复用 `shaders/resolve.glsl` 编译 compute SPIR-V。用 `Arc::make_mut(&mut engine.materials).register("lab.ink", spirv)?` 注册内核，将返回的标识赋给 `material.shading`。稳定标识随场景保存，分桶序号在运行时分配。旧存档中实例级的着色设置在读取时迁移为材质。分桶设计参考 Epic 的 GPU-driven materials 演讲。[Epic 技术演讲与幻灯片](https://www.unrealengine.com/blog/take-a-deep-dive-into-nanite-gpu-driven-materials)

```powershell
cargo run --locked -- --experiment clusters --command "lodview on"
cargo run --locked -- --experiment clusters --command "lod 0.25" --command "lodview on" --capture target/dragons-lod.png
```

### 银狼动作库与轨迹

动作库使用 NVIDIA Kimodo-SOMA-RP-v1.2 在 RTX 5070 上生成的 39 段动作。输入是逐帧的水平根轨迹与朝向，覆盖静立、慢走、走、跑、左右转弯走、左右转弯跑、起步、减速停步、后退和左右横移。每类使用种子 41、42、43，每段 4 秒、30 Hz，总计 156 秒、4,680 帧。生成使用空文本、40 步去噪、CFG 权重 1.8，关闭生成后的脚部修正。[Kimodo 源码](https://github.com/nv-tlabs/kimodo) · [模型与权重](https://huggingface.co/nvidia/Kimodo-SOMA-RP-v1.2)

离线转换先对齐 SOMA 与银狼的静止骨轴，再沿银狼的骨架层次计算 21 个关节的局部四元数，并按腿长缩放轨迹。烘焙时将手臂改到适合各类步态的参考姿势并保留摆臂变化，对四元数和双脚支撑切换做平滑，按更新后的骨骼姿势重算脚部检索特征。动作库位于 `assets/motion/`，每段最后 23 帧提供前瞻，整库共有 3,783 个可检索起点。

移动控制和未来预测执行同一套积分。目标速度通过临界阻尼模型从当前速度和加速度连续变化，转身速度单独平滑。预测过程中局部位移随预期朝向旋转，按住转弯键时会形成弧线，反向输入时也保留已有的动量。轨迹、朝向和足部特征一起用于查找动作。加速和制动阶段允许过渡片段进入候选，稳定行走时筛选持续步态。跳到新帧时，对关节旋转和髋部平移应用衰减偏移，减少换段时的突跳。播放速度在输入与动画同向时小幅匹配实际移动速度。控制台的 `motion` 指令可原地预览指定轨迹。交互目前在平地预览，脚部位置来自烘焙数据和混合后的骨骼姿势。[Epic Game Animation Sample](https://dev.epicgames.com/documentation/unreal-engine/game-animation-sample-project-in-unreal-engine) · [Ubisoft La Forge](https://www.ubisoft.com/en-us/studio/laforge/news/6xXL85Q3bF2vEj76xmnmIu/introducing-learned-motion-matching) · [临界阻尼模型](https://theorangeduck.com/page/spring-roll-call)

原始 SOMA 输出保存在 `assets/motion/source/`。可以直接用第二条命令重新做银狼的骨架映射。要从头生成，在支持 CUDA 的 Python 环境安装官方 Kimodo（本次使用提交 `e1c219c99b4b94ba48be1fb6a1cbc64ecee4c954`）及模型权重，并给第一条命令指定空目录。生成脚本会跳过已有的片段。模型权重版本记在 `bank.json`。

```powershell
python tools/generate_kimodo.py assets/motion/source
python tools/bake_silver_wolf.py assets/motion/source assets/characters/silver-wolf/scene.gltf assets/motion
```

### Motion Matching 的查询与性能

每次查询构造 24 维角色空间特征，包含三个时刻的水平位移与朝向（12 维），以及左右脚踝的位置和速度（各 6 维）。只用可检索的 3,783 个起点拟合各维的均值与标准差。对库和查询使用完全相同的变换 `(x − mean) × gain`，其中 `gain = sqrt(weight) / max(std, floor)`。轨迹位置、朝向、脚位置、脚速度的权重依次为 5、1.6、0.8、0.018，尺度下限依次为 0.05 米、0.1、0.05 米和 0.15 米/秒，防止放大近乎常量的朝向分量。

在加权归一化空间拟合 PCA，取前六个正交主成分。按六维中跨度最大的轴做中位数切分，得到 511 个树节点、256 个叶子，每叶 14 或 15 个候选。均值、增益、投影矩阵、节点包围盒与叶子样本号一起写入 `assets/motion/search.json`。运行时用可用的顺播候选初始化上界，优先访问距离查询点较近的子树。投影点到子树包围盒的距离为原空间距离提供下界，叶子仍用完整的 24 维代价精排。片段内跳转的附加代价为 0.28，跨片段为 0.42。全扫描与 KD-tree 使用相同的候选筛选和同分规则。

以下性能数据来自轨迹与过渡改造前的版本。Ryzen 7 9700X，release 构建，单角色，动作库驻内存，交替执行两种检索。预热 240 次后对同一组连续移动输入测量 2,160 次。全部 2,400 次查询以及另外 512 次扰动或片段边界测试，两条路径均得到相同的样本号与代价。

| CPU 范围 | P50 | P95 |
| --- | ---: | ---: |
| 全扫描查询 | 16.60 µs | 17.00 µs |
| PCA + KD-tree 查询 | 0.90 µs | 2.20 µs |
| 一次银狼系统更新（含 KD 查询及 21 关节回写） | 5.60 µs | 7.10 µs |
| 一次银狼系统更新（顺播及关节回写） | 4.60 µs | 4.90 µs |

全扫描实际精算候选数 P50/P95 为 3,769/3,773，KD-tree 为 107/354。银狼完整窗口另用 RTX 5070、855 × 551、MAILBOX、16 个 worker，固定直走输入，关闭录制和验证层，各预热 60 帧后采样 600 帧。KD 模式的帧间隔 P50/P95 为 5.006/5.416 ms，GPU 跨度为 1.329/1.456 ms，每秒 200.0 次呈现请求。全扫描为 4.989/5.426 ms、1.343/1.479 ms、每秒 200.1 次。

```powershell
cargo test --release --locked --lib benchmark_silver_motion -- --ignored --nocapture
cargo run --release --locked -- --experiment motion --command "motion 1.35 0" --command "motionsearch kd" --benchmark 600
cargo run --release --locked -- --experiment motion --command "motion 1.35 0" --command "motionsearch scan" --benchmark 600
```

岩柱森林默认开启 ReSTIR DI、GI-1.2 两跳漫反射和 Vulkan 硬件光追。控制台运行 `restir rays toggle` 可在软硬件后端间往返，也可用 `restir rays software`、`restir rays inline` 或 `restir rays hardware` 指定后端。`restir rays status` 显示实际能力与所选遍历路径。这一设置同时控制太阳阴影、漫反射命中和命中点到光源的遮挡查询。`vegetation shadows adaptive` 使用近景精细、远景简化的投影几何，`vegetation shadows mega` 在硬件模式下追踪主视图选簇及屏幕外补充簇。[岩柱森林逐帧开销](../docs/procedural-vegetation.md#岩柱森林的多跳光追) · [Mega Geometry](../docs/forest-mega.md)

### 录制实验

录制使用 FFmpeg。可以通过 `RENDER_LAB_FFMPEG` 指定可执行文件，或将其放入 PATH。下面的命令以固定时间步长运行同一场景，同时生成循环 GIF 和 H.264 MP4。每个输出帧对应一次 Vulkan 渲染，即使机器渲染速度低于目标帧率，离线录制也会保留每一帧。

```powershell
cargo run --release --locked -- --experiment motion --fps 25 --duration 8 --command "motion 0 0" --at 1 "motion 1.35 0" --at 3 "motion 3 0" --at 5 "motion 1.35 -1" --at 7 "motion 0 0" --record captures/silver-wolf-motion.gif --record captures/silver-wolf-motion.mp4
```

两种查询模式分别录制了这组输入。8 秒内所选片段与样本的切换记录完全一致，两份 MP4 解码后的 200 帧图像哈希逐帧一致。[银狼动作匹配 GIF](../captures/silver-wolf-motion.gif) · [同一段画面的 MP4](../captures/silver-wolf-motion.mp4)

## Cluster LOD 的历史性能基线

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
