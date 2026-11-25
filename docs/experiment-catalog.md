# 实验与操作

从仓库根运行 `cargo run --release --locked -- --experiment 名称`。省略预设时打开默认演示。窗口控制台和 `--command` 共用命令分派。保存和加载使用 `save PATH`、`load PATH`。不同的主渲染技术由装配器校验兼容性。[完整预设](../src/presets.rs) · [调试命令](../src/console.rs)

| 预设 | 输入、执行内容和观察方法 |
| --- | --- |
| `clusters` | 多个 Dragon 和普通网格共存，GPU 遍历簇并按屏幕误差选层。`lodview on` 给选层着色，`lod 0.25` 改阈值。 |
| `vegetation`、`vegetation-scale` | 程序化树、草、风动与混合表示 DAG；近景检查轮廓，大规模布局检查剔除。比较 `vegetation triangles`、`vegetation voxels` 和 `vegetation auto`。 |
| `river-forest`、`forest-karst` | 密林与河流，岩柱版本增加岩石布景和光照研究配置。适合固定机位比较投影几何、运动向量、间接光与后处理。 |
| `confluence` | 同场组合森林、流水、银狼、IBL 与 ReSTIR。用 `feature water off` 等命令独立切换贡献层。 |
| `water` | 计算着色器推进台阶上的水体状态，渲染随水面变化的表面。用 `effect restart` 重置。 |
| `destruction` | 物体破碎和消融，观察碎片局部变换、可见性及材料随时间变化。 |
| `motion` | 轨迹预测驱动银狼的 Motion Matching；IJKL 控制移动，`motionsearch kd` 与 `motionsearch scan` 可对照检索。 |
| `silver-contact` | 走、跳、撑栏、钻行与终点触摸。可调重力、起跳冲量和 IK，支持保存并继续中途动作。 |
| `toon` | 二分光照、面部 SDF 与轮廓。图像、法线外扩及边拓扑四边形路径共用同一人物资产。 |
| `texture` | Modern Hall 的基础色用特征网格和小型 MLP 在着色时恢复。`texture original`、`texture neural` 用于同视图对照。 |
| `gaussian` | 2D Gaussian 表示贴图；支持扫描、tile、BVH 与紧凑参数。用 `gaussian stats` 检查表示成本，比较时计入索引。 |
| `gi` | 独立间接光研究路径，可切换 `gi off`、`gi ssgi`、`gi probes` 和 `gi ray`。 |
| `shadow` | 阴影贴图的 hard、PCF、PCSS 和射线阴影，可调尺寸、偏移和调试视图。 |
| `rt` | 独立主射线渲染，观察硬件管线、加速结构与命中输出，适合分离光追前端与光栅材质的行为。 |
| `restir` | 大量光源的均匀采样、RIS、时空复用和参考路径；支持降噪、候选数与历史统计。 |
| `gi12-forest`、`gi12-silver` | 森林和角色用 ReSTIR 直接光及探针 / 缓存间接光，比较材质命中、直接光、单跳和多跳。 |
| `ibl`、`mixed` | 前者在 HDR 环境下评估 PBR；后者把 IBL 与多光源采样接入同一实验。 |

几何实验入口位于 [clusters](../src/experiments/clusters.rs) 和 [vegetation](../src/experiments/vegetation.rs)。流水、破碎与消融位于 [effects](../src/experiments/effects.rs)。基础光照实验在 [lighting](../src/experiments/lighting.rs)，储层与间接光研究在 [restir](../src/experiments/restir.rs)。

## 资产处理

神经纹理可用 `python tools/train_texture_grid.py assets/scenes/modern-hall/modern_hall_core.gltf assets/texture/modern-hall` 在安装 PyTorch、NumPy 和 Pillow 的环境重新拟合。Gaussian 工具负责打包、量化、剪枝与指标计算，CUDA 对照工具校验外部训练实现的梯度。评价应同时给出拟合误差、渲染端解码时间，以及含加速结构的存储费用。[神经纹理训练](../tools/train_texture_grid.py) · [Gaussian 工具](../tools/gaussian_texture.py) · [梯度检查](../tools/check_2dgaussian_gradients.cu)

场景运行直接使用仓库内的成品资产，各资源的原始署名和使用条件见[资产表](../assets/README.md)。
