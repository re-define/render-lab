# Render Lab 文档

第一次运行可从仓库首页的 `movement` 实验开始。要跟踪一帧从输入直到呈现，先看架构图，再按关心的模块阅读。[构建与运行](../README.md) · [架构图与一帧的数据流](architecture.md)

| 想解决的问题 | 阅读入口 |
| --- | --- |
| 同一个实体谁能移动、何时更新，模块之间如何传递结果 | [世界、Move 与模块时钟](world-runtime.md) |
| 银狼怎样选动作、根据支撑反馈调整姿态并用 IK 触达目标 | [动画与物理](animation-physics.md) |
| 哪些 ECS 变化需要重新提取，哪些 GPU 数据可以跨帧复用 | [场景提取与资产发布](extraction.md) |
| 三角簇、体素、材质、光追和时域后处理怎样进入 RDG 与 RHI | [渲染实现](rendering.md) |
| 如何选择实验、资产和调试命令 | [实验目录](experiment-catalog.md) |
| 如何测模拟、GPU 与窗口耗时，以及已有对照能说明什么 | [性能与复现](performance.md) |

代码与文档修改走同一套验证流程。第三方资产的原作、许可与使用范围单独记录。[开发与验证](../CONTRIBUTING.md) · [资产署名](../assets/README.md)
