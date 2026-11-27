# 植被簇的构建、选择与光栅化

森林资产由多个实例共享一套原型。三角簇保存可绘制的网格片段，体素簇保存稀疏砖；BVH 节点负责空间遍历。实例变换与风动姿态在运行时施加。体素网格位于原型的局部空间，因此两棵尺寸或朝向不同的树可以引用同一份占用数据。[原型打包](../../src/experiments/clusters/bvh.rs) · [GPU 表](../../src/experiments/clusters/gpu/tables.rs)

## 构建一个父层

1. 从叶网格的 meshlet 开始，将当前层的成员分组。一个组的子级可以包含三角簇和已有体素簇。纯三角组可以锁住跨组的接缝位置，做带属性的简化，再把结果拆回网格簇。含有体素子级的组继续用整个子组评估体素父级；现有实现只为纯三角组生成三角简化候选。[层级构建与候选条件](../../src/experiments/clusters/bvh/voxels/levels.rs) · [混合子级输入](../../src/experiments/clusters/bvh/voxels/children.rs)
2. 对可以体素化的区域，在有限长度的射线上采样表面。覆盖、命中面的 UV、法线方向信息和骨骼归属参与生成体素属性。取自代表表面的 UV 能保住贴图岛边界；方向覆盖还用于筛选稀疏表皮。每个 4×4×4 砖的 64 位掩码只编码占用，着色与变形另读属性。[表面采样与 UV 回归](../../src/experiments/clusters/bvh/voxels/sampling.rs) · [覆盖筛选](../../src/experiments/clusters/bvh/voxels/coverage.rs) · [砖布局](../../src/experiments/clusters/bvh/voxels/bricks.rs)
3. 用子级误差作为下限，结合三角简化误差、投影覆盖差异和体素尺寸评估替代，并限制父级砖数量。纯三角区域还会先检查是否适合用体积表示。构建的结果是带误差的替代关系，运行时针对每个实例和视图作选择。[误差估计与预算](../../src/experiments/clusters/bvh/voxels.rs)

## 一帧怎样得到可见表面

GPU 先剔除视锥外的实例，随后从全局工作列表展开 BVH 与候选簇。投影到屏幕上的误差决定采用父层还是继续细化。上一帧的层次深度先筛遮挡，本帧三角绘制产生的新深度用于复查候选。普通网格也可以写入视图的可见性与深度。[遍历与两阶段剔除](../../src/experiments/clusters/gpu.rs) · [普通网格接口](../../src/render/geometry.rs)

compute 体素路径在三角深度就绪后统一处理砖。每个工作组投影至多八个砖，计算屏幕矩形，将候选像素分配给线程。通过深度等测试后，像素中心的射线进入砖的局部坐标，用 DDA 逐格检查占用掩码。命中深度和表面标识一起参加 64 位原子竞争，最后解析胜者，供后续材质读取。这个阶段直接利用三角深度，耗时取决于投影覆盖、实际进入 DDA 的次数和深度竞争。[pass 装配](../../src/experiments/clusters/gpu/bricks.rs) · [像素分配](../../shaders/brick_raster.comp) · [砖内求交](../../shaders/brick_trace.glsl)

判断 overdraw 是否下降，固定原型、风相位、分辨率和相机路径，分别记录候选像素、早期深度拒绝、DDA 次数、命中和原子胜出次数，再核对轮廓、深度差和整帧 GPU P95。树冠变稀也会降低候选数，所以要同时看可见覆盖。[测量计数定义](../../src/experiments/clusters/gpu/bricks.rs) · [森林回放与测量口径](../architecture/performance.md) · [与 Nanite Foliage 的差异](../unreal/rendering.md)
