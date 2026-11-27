# Render Lab 手册

第一次运行请看[构建与启动](../README.md)。已知场景名、想找命令或资产时，直接打开[实验目录](experiment-catalog.md)。

| 部分 | 适合带着什么问题来读 | 从哪里开始 |
| --- | --- | --- |
| 架构 | 一个物体由谁更新，哪些工作能并行，GPU 资源和历史在哪一帧退役？ | [一帧的数据流与模块图](architecture/README.md) |
| 算法 | 给定同一份输入，怎样选择簇、计算光照、驱动骨骼或重建纹理？ | [按研究问题查算法](algorithms/README.md) |
| 与虚幻引擎的差异及改进 | 两边解决了问题的哪一部分，哪些机制值得移植，怎样用实验判断取舍？ | [系统对照与验证顺序](unreal/README.md) |

**追一帧森林。** 从[实体变化与场景提取](architecture/extraction.md)进入[植被的构建和光栅化](algorithms/cluster-voxels.md)，再看[直接光和间接光](algorithms/visibility-lighting.md)。GPU 时间线和窗口完成间隔的解释见[性能测量](architecture/performance.md)。

**追一次银狼触地。** [角色子步协议](architecture/character-motion.md)说明谁能写根变换，[动作匹配](algorithms/animation-physics.md)说明怎样选姿，[物理骨链与末端约束](algorithms/physical-animation.md)说明怎样承重、落脚和扶栏。换角色时的组合能力见[UAF、Mover 与 Chaos 对照](unreal/animation-physics.md)。

修改引擎时使用[开发与验证规范](../CONTRIBUTING.md)。使用仓库附带的模型、动作和环境贴图时查阅[资产来源与许可](../assets/README.md)。
