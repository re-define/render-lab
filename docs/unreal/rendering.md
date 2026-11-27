# Nanite Foliage、Lumen 与森林实验

## 体素何时比三角形更合适

**共同问题。** 细叶和枝条会在简化时失去覆盖，远处保留很多互相遮挡的小三角面也可能浪费光栅工作。Epic 的 Nanite Foliage 文档将 Assemblies、体素和蒙皮分别用于资产复用、聚合几何简化和运动。构建时比较表示的简化误差，体素适合更能保持整体外观的层级。[Epic Nanite Foliage，UE 5.8](https://dev.epicgames.com/documentation/unreal-engine/nanite-foliage?application_version=5.8)

**Render Lab 当前的选择。** 三角与体素子簇可在同一层级中聚合。纯三角组有三角简化候选，混合组的体素子级会参与生成新的体素父级。局部砖保存占用和表面属性；显示时先用屏幕误差选簇、分两次检查遮挡，再用最终三角深度裁剪体素像素。仓库可以逐项打开着色器，追到每块砖的 DDA、深度竞争和结果解析。Epic 公开页描述的是功能、资产工作流及简化行为，不能据此认定两边使用同一属性布局、误差公式或 dispatch 顺序。[本引擎的构建与光栅算法](../algorithms/cluster-voxels.md) · [属性采样](../../src/experiments/clusters/bvh/voxels/sampling.rs)

**下一步实验。** 选同一棵近景树的高细节三角画面作参考。固定机位路径，分别在静风与定相位风下记录树冠像素覆盖、LOD 交界处的帧间轮廓变化和枝叶透光空隙，再比较不同构建误差权重。最后用整片森林检查 DDA 次数、实际深度胜出与 GPU P95。这样能找出保住视觉覆盖所要付的光栅成本。[体素计数与两条光栅路径](../../src/experiments/clusters/gpu/bricks.rs) · [森林回放](../../examples/forest_shadows/zoom.rs)

## GI 缓存里存的是什么

**本引擎。** ReSTIR DI 为直接光重用候选光源；间接光实验独立从可见表面选探针，追踪射线，在实际命中 UV 处按材质求辐亮度。缓存路径保存世界空间单元和探针历史，双跳模式延迟发布第二次命中的贡献。软件 BVH、硬件内联查询或 RT 管线是求交后端选择，直接光、间接光和降噪各有独立开关。[照明阶段和实验命令](../algorithms/visibility-lighting.md) · [GI 射线](../../shaders/gi12_trace.glsl)

**Lumen。** Lumen 先用屏幕追踪利用当前视图的细节，再以软件距离场或硬件光追补足命中。Surface Cache 用离线布置的 Cards 从多个方向捕获表面材质，并在运行时更新供照明查询的表面信息。Render Lab 的世界空间辐亮度单元与这些表面参数化数据有不同的键、内容和失效条件，不能把单元数换算成 Lumen 的 Cards 数。[Epic Lumen Technical Details，UE 5.8](https://dev.epicgames.com/documentation/unreal-engine/lumen-technical-details-in-unreal-engine?application_version=5.8)

**下一步实验。** 先显示缓存覆盖、命中年龄和动态表面的失效原因，再给探针更新设定每帧预算。森林用于检查镜头推近导致的新显露区域，运动银狼用于检查旧辐亮度附着在错误表面的拖影。固定直接光、降噪器和每帧射线预算，记录恢复到参考亮度容差所需的帧数与 GPU P95。有了这些观测，再决定屏幕追踪预查询带来的收益。[探针与缓存构图](../../src/experiments/light_stack/diffuse_gi.rs) · [森林和银狼预设](../../src/experiments/restir/studies.rs)
