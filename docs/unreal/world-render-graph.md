# 世界数据、渲染图与队列

## ECS 与 MassEntity

Render Lab 的世界保存稳定实体 ID、层级、资产引用和可序列化组件。类型化运行状态与变更记录让提取器只访问受影响的实例，姿态通过专用批量接口提交。调度器能按具体实体的变换读写范围判定冲突。在 500 个外观相同的角色上，共享动作资产减少了重复资源，但不同角色的脚锁、位姿和撤销记录仍产生独立写入。[世界与时钟](../architecture/world-runtime.md) · [姿态提交和增量提取](../architecture/extraction.md)

UE 的 MassEntity 按 Fragment 组合形成 archetype，将同一组合的实体分块存放，Processor 的查询按批取得符合条件的数据。这提供了另一种优化粒度：一段循环连续处理同布局数据。这里比较的是 MassEntity 子系统，UE 的 Actor、组件和其他游戏系统另有各自的生命周期。[Epic MassEntity](https://dev.epicgames.com/documentation/unreal-engine/overview-of-mass-entity-in-unreal-engine?application_version=5.8)

值得先实验的是按骨架布局打包高频姿态写入。实体的持久 ID 与外部引用仍归世界持有，用索引把记录映射回实体。必须同时测试一名角色换骨架、500 人一起更新、森林只移动相机，以及中途一个系统失败后的回滚。记录调度、提交、撤销准备、读写实体数和 P95，才能判断按块处理节省的访问是否抵消了索引维护。[调度与回滚](../../src/engine/schedule.rs) · [人群基准](../../src/experiments/motion/silver/contact/crowd_profile.rs)

## RDG、临时资源与队列

两边都延迟执行 pass，通过已声明的资源关系安排工作。Render Lab 已有活跃性裁剪、缓冲区字节范围、跨帧槽静态加速结构共享、暂存缓冲复用和跨队列时间线信号量。当前图像依赖按整个资源追踪。逻辑队列最终映射到设备实际提供的队列；把一个 pass 标为 compute 只决定可用执行路径，能否重叠要看资源依赖和设备时间戳。[编译与提交](../architecture/render-graph.md) · [缓冲范围](../../src/rdg/ranges.rs) · [暂存复用条件](../../src/rhi/transient.rs)

UE RDG 将 setup 和 execute 分开，能从 pass 参数提取资源依赖，并提供 transient allocator、异步计算、调试验证和 RDG Insights。值得借鉴的是将资源的存活区间、等待与实际重叠放在同一视图，使编写 pass 的人能发现过宽的声明。Render Lab 已能导出每条物理队列的时间线，可以从导出中定位生产者，再回查具体声明。[Epic RDG 与 RDG Insights](https://dev.epicgames.com/documentation/unreal-engine/render-dependency-graph-in-unreal-engine?application_version=5.8) · [本引擎时间线导出](../../src/rhi/timeline.rs)

历史森林回放中，在同一台 RTX 5070 上，420 帧轨迹去掉开头 48 帧后，单队列和强制三队列的 GPU P95 分别为 23.65 与 25.73 ms。优先挑一个实际造成等待的图像，细化范围，同时保留布局转换和别名边界。复测需配对相同相机阶段，检查像素一致、GPU P95、等待来源及峰值内存。这个对照只适用于该次负载。[森林回放数据与复现命令](../architecture/performance.md#岩柱森林的物理队列)
