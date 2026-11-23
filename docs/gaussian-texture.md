# 2DGaussian 地砖解码实验

后续剪枝实验把这张地砖从 6,554 条降到 3,277 条，改用 16 B 记录和紧凑索引。GPU 全纹理 PSNR 降幅为 0.043 dB，含索引的地砖载荷降至 tile 68,620 B 或 BVH 61,640 B。[剪枝、精度与结构的逐项对照](gaussian-pruning.md)

Modern Hall 的地砖使用 894 × 894 的不透明基础色。示例用 PyTorch Adam、新的两层空间初始化和 1,800 步训练拟合了 6,554 个各向异性高斯，保存为原项目的八字段物理尺度 PLY。其他材质与摄像机沿用前一个纹理实验。这里的拟合与测量都针对这张地砖；原项目的历史 HDR 光照贴图权重未随仓库提供。[地砖 PLY](../assets/gaussian/FloorTiles.ply) · [场景来源与授权](../assets/README.md)

## 离线索引与运行时采样

旧项目的 CUDA 前向已经有 16 × 16 分箱。它每次重建一整张图，接口再按坐标读取稠密张量。本实验把分箱移到打包阶段，只在可见性缓冲的 PBR 材质 resolve 中评价实际请求的纹素。压缩模式下 GPU 纹理缓冲包含高斯和索引，场景内另外两张基础色图仍按原方式采样。[原项目接口](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/Interface.py) · [CUDA 前向](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/submodules/picturegs/cuda_rasterizer/forward.cu) · [Vulkan 着色器](../shaders/materials/gaussian.glsl)

打包器接受 PLY 顶点属性 `x, y, r, g, b, scale_0, scale_1, rot_0`。中心和尺度单位为纹素，角度为弧度。旧格式 RGB 已乘训练时的 opacity。解码采用单位 opacity 与单位 multiplier。每个记录保留预计算的逆协方差、RGB、中心和原 CUDA 的 16 像素 tile 覆盖范围。每个候选以 `min(0.99, exp(-0.5 dᵀQd))` 加权累加颜色，低于 `1/255` 的权重跳过。RGB 可以有负分量，最终累加后再做非负截断。

候选范围取权重截止椭圆的保守轴对齐包围盒与原始覆盖范围的交集。每个 tile 保存连续的候选 ID 区间，ID 按 PLY 行序排列，使不同索引保持相同的浮点累加顺序。32 像素 tile 还会在采样时检查原 16 像素覆盖边界。高斯数不超过 65,536 时使用 16 位 ID，更大模型使用 32 位。外部 G2T 加载时校验长度、目录单调性、ID 边界、参数有限性与 conic 正定性。[打包和训练](../tools/gaussian_texture.py)

## BVH 与 tile 在本地显存上的对测

`gaussian bvh` 可直接用于示例地砖或导入的 G2T。切换模式或加载保存的 BVH 场景时，CPU 从原始高斯记录构建一棵二维包围盒树；随后各帧复用同一份材质缓冲。节点包围裁剪至纹理边界的 CUDA 覆盖矩形。构建器沿中心 x、中心 y 和矩形面积尝试划分，以两侧「覆盖面积 × 记录数」之和选择切分点。叶子最多包含 8 个高斯，叶子记录在 GPU 缓冲中连续存放。GPU 在 PBR pass 重建 UV，以纹素坐标查询树；命中内部节点就访问第一个子节点，未命中就沿跳转偏移越过整个子树。通过叶子覆盖检查的记录使用与全扫描相同的高斯权重。每个查询只保存当前节点索引。[BVH 构建器](../src/experiments/gaussian/bvh.rs) · [GPU 遍历与解码](../shaders/materials/gaussian.glsl)

RTX 5070，release，855 × 551，MAILBOX，16 个工作线程，Modern Hall 固定机位，每帧 103,095 个地砖像素。对每种模式预热 60 帧、测 180 帧，再颠倒顺序重复。数据是 Vulkan 时间戳的两轮中位数。四种模式使用同一设备本地堆。

| 地砖模式 | 地砖 GPU 载荷（B） | 着色 pass P50（ms） | 整帧 GPU 跨度 P50（ms） |
| --- | ---: | ---: | ---: |
| 原始 RGBA8 | 3,196,944 | 0.083 / 0.086 | 3.04 / 3.19 |
| Gaussian 全扫描 | 262,192 | 2.658 / 2.664 | 6.94 / 7.03 |
| Gaussian BVH | 299,232 | 0.167 / 0.167 | 3.60 / 3.65 |
| Gaussian 32 × 32 tile | 326,188 | 0.115 / 0.115 | 3.52 / 3.55 |

BVH 解码本场景地砖的速度是扫描的 15.9 倍。tile 以单次目录查找定位候选，pass 耗时比 BVH 低约 31%，保留为此场景的默认模式。两者均在着色时直接解码。BVH 按叶序累加，浮点舍入顺序随之变化；800 × 500 的 GPU 对照图中，85,766 个地砖像素里只有 1 个像素与扫描相差 1 个 8 位色阶。tile 与扫描逐像素相同。乱序且覆盖范围重叠的合成数据另有测试，对所有纹素核对命中记录集合。

## 两个压缩实验的显存

下表取每轮基准最后一帧的 Vulkan 分配记录。纹理 atlas 包含全部基础色和一个 4 B 的默认白纹素。RHI 为 atlas 单独调用 `vkAllocateMemory`，统计使用申请成功时的 `allocationSize` 和实际 memory type、heap index。这台 RTX 5070 上，四种配置的 atlas 都位于类型 4、堆 0（设备本地、CPU 可映射）。CPU 保留用于对照的源图和 G2T。切换到 BVH 后，GPU 上只上传 BVH 版本的地砖。

在同一台机器上用 320 × 200 的原图帧单独回放旧分配策略，本地堆原为 983,040 B，非本地堆为 14,251,216 B。只改变 atlas 放置后，本地堆增至 12,908,576 B，非本地堆降至 2,325,680 B，两边的差都恰好是整个 atlas 的 11,925,536 B。旧版压缩节省的是 GPU 可访问的共享内存。本表中的独显显存节省量使用新的原图分配作为基线。

| Modern Hall 配置 | 全部基础色 atlas 的实际分配（B） | 较全原图节省 | RHI 显式分配的本地堆合计（B） |
| --- | ---: | ---: | ---: |
| 三张 RGBA8 原图 | 11,925,536 | 基线 | 18,520,096 |
| 三张特征网格 + MLP | 1,498,304 | 10,427,232 B（87.44%） | 8,092,864 |
| 只压地砖，Gaussian tile | 9,054,768 | 2,870,768 B（24.07%） | 15,649,328 |
| 只压地砖，Gaussian BVH | 9,027,824 | 2,897,712 B（24.30%） | 15,622,384 |

本地堆合计覆盖 RDG 图像、缓冲和描述符堆的显式分配。原图以外的显存资源也在这个口径中，因此整项降幅分别为 MLP **56.30%**、tile **15.50%**、BVH **15.65%**。交换链图像由 WSI 管理，驱动内部资源与桌面进程的总显存另计。这里测的是稳态分配，切换模式瞬间的峰值及训练用的 PyTorch 张量另计。[实际分配与回退](../src/rhi/device.rs) · [RHI 帧统计](../src/rhi/mod.rs)

| 有效载荷的组成 | 参数或特征（B） | GPU 查询结构（B） | 格式头（B） | 合计（B） |
| --- | ---: | ---: | ---: | ---: |
| 三张特征网格 + MLP | 1,492,480 网格 + 5,760 FP16 权重和偏置 | 0，按块坐标直接寻址 | 48 | 1,498,288 |
| 地砖 Gaussian 全扫描 | 262,160 高斯记录 | 0 | 32 | 262,192 |
| 地砖 Gaussian tile | 262,160 高斯记录 | 3,140 目录 + 60,856 候选 ID（含对齐） | 32 | 326,188 |
| 地砖 Gaussian BVH | 262,160 高斯记录 | 37,040 节点（2,315 × 16 B） | 32 | 299,232 |

单独比较同一张 894 × 894 地砖，RGBA8 为 3,196,944 B，MLP 为 403,344 B（省 87.38%，41.78 dB），Gaussian tile 为 326,188 B（省 89.80%），Gaussian BVH 为 299,232 B（省 90.64%）。两种 Gaussian 索引使用同一份 30.65 dB 的拟合。BVH 比 tile 少 26,956 B，比地砖 MLP 少 104,112 B。Gaussian 实验中的木纹和壁纸走 RGBA8。所有基线均为单层纹理，引擎使用最近邻采样。[三张贴图的训练与分项画质](../README.md#modern-hall-神经纹理压缩)

可在 `render_lab` 目录复测。`BENCHMARK` JSON 的 `memory.atlas_buffer_bytes` 是逻辑长度，`memory.atlas.allocated_bytes` 是计入驱动对齐的分配，`memory.device_local_bytes` 和 `nonlocal_bytes` 分别是 RHI 显式分配到两类堆的总量。

```powershell
cargo run --release --locked -- --experiment gaussian --command "gaussian bvh" --benchmark 180
cargo run --release --locked -- --experiment texture --command "texture neural" --benchmark 180
```

## 早期 tile 尺寸对照（原内存放置策略）

RTX 5070，release，855 × 551，MAILBOX，16 个工作线程，固定摄像机。每帧有 103,095 个可见像素属于地砖。各模式先预热 60 帧，采集 180 帧，再以相反顺序重复一次。下表为两轮各自的 GPU 时间戳中位数，计时范围是整个地砖材质着色 pass。

| 解码方式 | 每个纹理 tile 的平均候选数 | 地砖载荷，含索引 | GPU pass P50，两轮 |
| --- | ---: | ---: | ---: |
| 原始 RGBA8 | — | 3,196,944 B | 0.139 / 0.140 ms |
| 全扫描，同一高斯集 | 每查询遍历 6,554 条记录 | 同一 G2T，忽略目录 | 8.697 / 8.715 ms |
| 8 × 8 | 20.86 | 835,752 B | 0.406 / 0.404 ms |
| 16 × 16 | 24.51 | 428,444 B | 0.196 / 0.199 ms |
| **32 × 32** | **38.81** | **326,188 B** | **0.150 / 0.152 ms** |

32 × 32 的候选数 P95 为 43，最大为 47。默认文件含 262,160 字节高斯记录，其余 64,028 字节为头和索引，总计 3.26 bit/texel。相对于 RGB8 和 RGBA8，分别压缩 7.35 倍和 9.80 倍。32 × 32 访问的平均候选较多，但索引更小，此机位的运行时间更短。当时的整帧 GPU 跨度 P50 从全扫描的 16.06–16.12 ms 下降到 3.50–3.67 ms。上面的 BVH 对测已按新的显存放置策略重新测量扫描、tile 和原图。

同机位的 800 × 500 截图中，三种索引均逐像素等于全扫描。有损误差来自高斯拟合：全贴图 RGB PSNR 为 30.65 dB，85,766 个受影响的地砖像素经过 PBR 着色后为 31.80 dB。石材细噪声和白色接缝边缘可见平滑。演示使用单层最近邻基础色；文件预览按引擎的基础色 PBR 管线显示，不作 HDR 光照贴图质量评价。[原图渲染](../captures/gaussian-floor-original.png) · [tile 渲染](../captures/gaussian-floor-tile.png) · [纹理局部与四倍误差](../captures/gaussian-floor-detail.png)

## 运行与接入旧 PLY

在 `render_lab` 目录执行。打包需要 Python、NumPy 和 plyfile，只有重新拟合还需要 PyTorch 和 Pillow。独立预览直接读取 G2T，保存场景时高斯载荷一同序列化。

```powershell
cargo run --release --locked -- --experiment gaussian
python tools/gaussian_texture.py pack old.ply old.g2t --width 2048 --height 1024 --tile 32
cargo run --release --locked -- --experiment gaussian --scene old.g2t
```

控制台的 `gaussian tile`、`gaussian bvh`、`gaussian scan` 分别选择 tile、BVH 和全扫描。`gaussian stats` 显示候选规模、BVH 节点数和载荷字节。Modern Hall 还可用 `gaussian original` 查看原图，或用 `gaussian load "path.g2t"` 替换同尺寸地砖。旧 PLY 不包含分辨率，打包时应使用训练时的宽和高。

```powershell
python tools/gaussian_texture.py fit assets/scenes/modern-hall/modern_hall_core.gltf target/floor.ply
python tools/gaussian_texture.py pack target/floor.ply target/floor.g2t --width 894 --height 894
python tools/test_gaussian_texture.py
```

## 旧训练代码审查

审查版本为 `20a39e6`。下列 CUDA 数值由与上游前向及反向实现直接链接的独立程序测得。输入为 32 × 16 图、一个旋转各向异性高斯与一个非零的像素上游梯度。中心差分步长为 0.001，样本远离裁剪阈值。[可复现用例](../tools/check_2dgaussian_gradients.cu)

| 参数 | 上游反向 | 前向中心差分 | 修复后反向 |
| --- | ---: | ---: | ---: |
| 中心 x | −1.716831 | −0.107363 | −0.107302 |
| 中心 y | −1.193216 | −0.149131 | −0.149152 |
| 尺度 x | 0.012915 | 0.007957 | 0.007973 |
| 尺度 y | 0.071907 | 0.093818 | 0.093821 |
| 旋转 | −0.031999 | −0.050783 | −0.050755 |
| opacity | 0.684275 | 0.684261 | 0.684275 |
| multiplier（代码字段 `negative`） | 0 | 0.478998 | 0.478992 |

1. **位置梯度单位错误。** 前向已经以纹素为单位读取中心。反向对 x、y 分别多乘了 `W/2` 和 `H/2`，所以位置步长会受训练分辨率影响。[backward.cu](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/submodules/picturegs/cuda_rasterizer/backward.cu#L89-L92)
2. **协方差交叉项与 alpha 的反向不匹配。** 前向指数中的交叉项是 `−B·dx·dy`，反向对 B 多乘了 0.5，导致各向异性尺度和角度梯度错误。前向还有 0.99 上限和 multiplier，旧反向忽略 multiplier 且未屏蔽上限饱和区的梯度。multiplier 初始为 1，若计划保持常量，可直接取消对它的优化。附带补丁仅修复 CUDA 反向，在上述七个参数上与中心差分的最大绝对差小于 0.00007。[上游反向](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/submodules/picturegs/cuda_rasterizer/backward.cu#L130-L175) · [反向修复补丁](../tools/2dgaussian-backward.patch)
3. **保存和两条加载路径使用了不同参数空间。** 实际调用上游 `Model.save/load_ply`，物理尺度 2.0 在普通 renderer 的 getter 中变成 `exp(2)=7.389`。独立 renderer 直接读取物理尺度，但也直接使用加载时的 `inverse_sigmoid(1)=+∞`，会把高斯覆盖区域内本该衰减的权重推到 0.99。应区分加载后的物理参数与可训练参数，颜色已经乘 opacity 的旧文件只能按明确的旧格式约定解码。若要求精确还原保存前的训练前向，未来格式需要分别保留训练时的 RGB、opacity 和使用到的 multiplier；0.99 上限和 1/255 阈值使折叠 opacity 后的结果无法处处等价。[保存与加载](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/scene/gaussian_models.py#L83-L135) · [专用解码入口](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/renderer_render/__init__.py#L21-L27)
4. **普通 Adam 分支没有参数更新。** `training_setup` 会为 `default` 构造 Adam，但训练循环只在 `sghmc` 分支执行 `step` 与 `zero_grad`。仓库随附配置选择 SGHMC，所以这一项影响切换到普通 Adam 的实验。补上 Adam 分支时也要处理增点代码无条件读取 SGHMC 专有 `momentum` 状态的问题。[训练循环](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/train.py#L123-L138) · [优化器与增点状态](https://github.com/fadeded-jpj/2DGaussian/blob/20a39e6aed59fed6404e6b8cb7d1c14b2547a642/scene/gaussian_models.py#L210-L247)

在原仓库的本地副本可用 `git apply --check <补丁路径>` 检查补丁。数值用例链接该仓库 `submodules/picturegs/cuda_rasterizer/forward.cu` 和 `backward.cu`，包含路径为该目录及 `submodules/picturegs/third_party/glm`。本次用 CUDA 12.8、MSVC 与 RTX 5070（`sm_120`）验证。上游的完整历史数据集训练没有重跑。
