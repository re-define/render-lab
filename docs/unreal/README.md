# 与虚幻引擎的差异及改进

本部分以仓库代码和 Epic 的 **UE 5.8** 公开文档为基线。UAF 和 Mover 在该版仍标为实验性。下文的验证顺序是对 Render Lab 的工程建议；已测量的引擎数据见[性能与复现](../architecture/performance.md)。[Epic UAF](https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAF?application_version=5.8) · [Epic Mover](https://dev.epicgames.com/documentation/unreal-engine/mover-in-unreal-engine?application_version=5.8)

| 关心的系统 | 差距最容易在哪一步看到 | 对照 |
| --- | --- | --- |
| 世界数据与系统调度 | 500 个角色共享资产时，每步仍要提交大量角色局部姿态 | [ECS、MassEntity 与工作批次](world-render-graph.md) |
| 渲染图与 RHI | 图中有可并行的 pass，实际多物理队列仍可能更慢 | [RDG、内存与物理队列](world-render-graph.md) |
| 几何与照明 | 远景树冠的覆盖误差，以及新显露区域的间接光恢复速度 | [Nanite Foliage 与 Lumen](rendering.md) |
| 动画、移动与接触 | 给另一种角色组合既有的选姿、分层、IK 和物理反馈 | [UAF、Mover 与 Chaos](animation-physics.md) |
| 毛发 | 将一套 GPU 引导束求解绑定到不同人物，处理快速转头与发梢碰撞 | [Groom 与毛发实验](groom.md) |

## 建议先验证什么

| 顺序 | 最小改动 | 采用前要回答的问题 |
| --- | --- | --- |
| 1 | [银狼的遮罩和末端 IK 已支持图资产](../algorithms/pose-graphs.md)。下一步试验第二种基础姿态源。 | 基础姿态来源能否独立于骨架和求解器替换？沿用单人和 500 人基准，检查默认图回放、脚锁和求值开销。 |
| 2 | 针对人群中高频写入的数据试一个连续批次，例如同布局角色的姿态提交；保留实体 ID、回滚和变更记录。 | 对同一组输入，单人与稀疏编辑是否退化？全量与增量矩阵是否一致？用已有的人群及三种森林编辑负载复测提交字节、访问实体和 P95。 |
| 3 | 给一个确实共享图像的生产者与消费者声明 mip 或层级范围，导出优化前后两条队列时间线。 | 消失的等待能否定位到具体资源？相同森林回放的像素、显存峰值和 GPU P95 怎样变化？Hi-Z 已有缓冲区区间追踪，可充当既有对照。 |
| 4 | 在森林和毛发的固定输入回放中分别加入覆盖误差与发梢误差观测；给 GI 缓存的未命中、过期和更新预算增加诊断。 | 固定画质阈值后，还有多少 DDA 调用、探针射线或引导点求解可以省下？报告实际整帧耗时、历史恢复帧数及最差轮廓误差。 |

第一项解决角色行为的组合成本。第二、三项涉及写入布局与资源生命周期，需要分别合入和复测。第四项为三条独立实验线提供质量基线，适合按研究主题单独推进。[角色阶段](../../src/experiments/motion/silver/contact/stage.rs) · [提取基准](../architecture/extraction.md) · [物理队列时间线](../../src/rhi/timeline.rs)
