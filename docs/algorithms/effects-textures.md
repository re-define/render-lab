# 水面、碎块与纹理解码

## 台阶流水与森林河流

在 `water` 里，GPU 为 49 条横向流线和每条 241 个年龄采样点各生成一个水团。给定播放时刻、横向位置和年龄，可以确定它从入口何时发射。积分着色器以至多 1/90 秒的步长推进前向位置与竖直速度，加入重力，遇到阶面时校正高度、更新冲击与泡沫信号。相邻流线与相邻年龄的差分产生两条切线，其叉积给出水面法线。按上一显示时刻再求位置，能为同一水面提供运动向量。这个模型适合复现已知渠道的流动，水团之间没有压力投影或体积守恒求解。[台阶参数与 GPU 构图](../../src/experiments/effects/water.rs) · [积分](../../shaders/water_integrate.comp) · [水面重建](../../shaders/water_surface.comp)

`river-forest` 采用另一套河道网格。中心线、宽度和水位函数供河床造型、植被选址及水面生成共同使用，所以改一段河道会同时改变岸线和植被分布。复现台阶重力参数请选 `water`；观察河岸、倒影与植被交界请选 `river-forest`。[河道函数与绘制](../../src/experiments/effects/river.rs) · [场景目录与命令](../experiment-catalog.md)

## 破碎与消融

`destruction` 用种子点生成 Voronoi 碎块。构建每个单元时按种子对的平分面裁剪封闭几何，并给新露出的截面封面。播放时按碎块的初速度、角速度、重力和落地高度计算姿态。消融使用空间标量场与随时间变化的阈值，在像素可见性阶段丢弃表面。前者改变各块的位置和轮廓，后者改变保留下来的覆盖。用 `effect time` 固定时间可复现同一帧。[几何裁剪与位姿](../../src/experiments/effects/fracture.rs) · [消融场](../../shaders/dissolve_field.glsl) · [片元测试](../../shaders/dissolve.frag)

## 用特征网格或 2D Gaussian 还原贴图

| 路径 | 离线表示 | GPU 上给一个像素着色 |
| --- | --- | --- |
| `texture` | 每个 4×4 RGB 图块学习 8 维特征。PyTorch 联合拟合网格与 8→16→48 的 MLP，并在训练时模拟特征量化。打包时每块 8 字节，解码器参数单独保存。 | 用 UV 找图块，解码 8 个特征并算一次 ReLU 隐层，再按块内坐标只求对应 RGB 的三个输出。训练时的 48 个输出是整块 16 个像素的颜色。[训练与离线解码](../../tools/train_texture_grid.py) · [材质解码](../../shaders/materials/texture_grid.glsl) |
| `gaussian` | 保存椭圆高斯的中心、二次型和颜色，另建 tile 索引或 BVH。紧凑格式降低记录和索引的字节数。 | 按 UV 找像素，用全表、tile 的候选列表或无栈 BVH 找可能覆盖它的高斯，计算指数权重并累加颜色。BVH 中未命中的内部节点可用跳转索引越过整棵子树。[打包](../../tools/gaussian_texture.py) · [着色器](../../shaders/materials/gaussian.glsl) |

比较算法时固定源贴图、UV 和输出尺寸，先对照解码颜色，再看每像素候选与 GPU 时间。显存分别统计特征或高斯记录、解码器、tile 目录及引用或 BVH 节点，并记录图集的实际分配。小图上固定解码器也可能占主要部分。`texture stats` 与 `gaussian stats` 可查看当前实验的数据。[神经压缩实验](../../src/experiments/texture.rs) · [Gaussian 大小与逐像素回归](../../src/experiments/gaussian.rs)
