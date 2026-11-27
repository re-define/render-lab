# 动画图、角色运动与物理反馈

以银狼接触场景为例，本引擎提供通用的实体调度器和姿态发布接口，角色动画程序由 Rust 系统实现。Epic 在 UE 5.8 的公开文档中将 UAF 标为实验性功能。它把动画逻辑和数据做成可实例化的资产，并提供图执行、扩展接口及与其他系统的集成。下表区分世界调度、角色内部求值和渲染变形这三层。[引擎调度器](../../src/engine/schedule.rs) · [UAF 插件与扩展](https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAF?application_version=5.8)

| 问题 | 本引擎 | UE 5.8 UAF |
| --- | --- | --- |
| 谁定义一套角色行为 | 实验装配注册选姿、根动力学和反馈系统。角色配置资产指定片段、骨架通道、动作窗口及目标偏移。角色引用的姿态图资产决定片段遮罩、骨链、足部和手部 IK 的组合，世界阶段由 Rust 装配。 | 作者将逻辑与数据保存为动画图资产。运行时可从一份图分配多个实例，按角色提供变量和绑定。[图资产与实例分配](https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph/UUAFAnimGraph?application_version=5.8) |
| 姿态怎样算出来 | 固定子步内计算预测轨迹、归一化检索、采样和惯性化。接触路径用实际根运动修正骨架，最后发布一份局部姿态。显示阶段按已编译的图指令组合旋转、物理反馈和 IK。 | Anim Graph 提供可组合的 Trait 行为。姿态求值程序由栈式 Evaluation VM 执行，带有参考姿态、骨架绑定、LOD 和曲线过滤等上下文。Pose Search、Warping、Control Rig 均有独立集成。[Trait](https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph/FTrait?application_version=5.8) · [姿态求值 VM](https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph/FEvaluationVM) |
| 两个角色共享什么 | 共享动作库、PCA/KD 索引、编译后的姿态程序和网格。基础动作游标、叠加层时钟、移动速度、脚锁和接触历史由各角色的 ECS 组件持有。 | 同类 Trait 的行为实现可被多个 worker 并发调用。只读参数由实例共享，混合权重等可变数据按 Trait 实例存放。[共享数据与实例数据](https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph/FTrait?application_version=5.8) |
| 动画怎样接入 Move 和物理 | 系统声明读写集、目标实体和阶段先后。动画通过同一子步的 ProposedMotion 与 ResolvedMotion 和根运动求解器交换输入输出。足部、掌部与共享支撑约束有明确的角色语义。 | UAF 定义模块事件、外部系统依赖和线程安全约定，并提供 Mover UAF 等插件。角色怎样采用物理反馈仍取决于接入的模块与图。[模块 API](https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAF?application_version=5.8) · [UAF 集成列表](https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAF?application_version=5.8) |

银狼可以通过 JSON 图继续用 Motion Matching 驱动基础动作，同时在给定时间窗为上半身混合另一段动作。图绑定只读片段、逐通道权重和求值顺序。角色保留自己的图时钟和物理状态，最后一次性发布姿态。UAF 则公开了可扩展的 Trait、求值 VM 和分层插件。Render Lab 当前支持一条显示姿态链和只读片段分支。采样基础动作与惯性化仍封装在角色的准备阶段，尚不能由图作者替换；扩充节点种类也需要改 Rust。下一步可用第二种基础姿态源检验公共的节点和骨架绑定接口。[本引擎姿态图](../algorithms/pose-graphs.md) · [UE 分层插件](https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAFLayering?application_version=5.8) · [子步交换类型](../../src/movement/character.rs)

Epic 将动画求值表示成 Evaluation Program，由持有骨架绑定、LOD、求值标志和栈的 Evaluation VM 执行。本引擎借鉴了提前编排操作、按绑定缩小实际工作量的思路。具体执行使用单条可变显示姿态、成组的局部混合、绑定期计算的受影响关节和作业内的片段采样缓存。角色时钟与 IK 状态独立，多个角色可以共享同一个程序。[Epic Evaluation VM](https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph/FEvaluationVM?application_version=5.8) · [本引擎求值算法](../algorithms/pose-graphs.md)

## Mover 与 Chaos 分别提供什么

Mover 将运动模式及其状态封装为可组合的 Actor 移动模块，并可通过 Network Prediction 或 Chaos 网络物理执行回滚重演。Render Lab 的 Move 已按实体分派输入、声明运动写入者，银狼也能在独立移动和物理接触间交接速度。现有调度回滚负责撤销失败子步的本地修改。要做预测重演，还需要按模拟 tick 保存输入、校正状态和重演所需的外部支撑数据。[Epic Mover](https://dev.epicgames.com/documentation/unreal-engine/mover-in-unreal-engine?application_version=5.8) · [实体运动策略](../../src/scene/motion_policy.rs) · [本引擎子步交换](../architecture/character-motion.md)

Chaos 提供通用刚体、关节、物理动画、布娃娃和破碎等机制。它的破碎资产可以包含分层碎片、连接关系和损伤阈值。Render Lab 角色的实际根运动由胶囊、盒碰撞与支撑求解，局部肢体用带柔度的骨链约束调整姿态；碎块实验用预生成的 Voronoi 单元和按时间计算的位姿。这两类本地输出可以分别检验根与四肢反馈、几何破碎与着色，现阶段没有共享的一套全身多刚体约束求解器。[Epic Chaos Physics](https://dev.epicgames.com/documentation/unreal-engine/physics-in-unreal-engine?application_version=5.8) · [角色骨链](../algorithms/physical-animation.md) · [碎块算法](../algorithms/effects-textures.md)

可先把身体的碰撞形状、关节局部框架和角度限制做成可绑定资产，挑一条触达中的手臂验证。固定动作与根运动，检查手掌误差、关节超限、支撑冲量和重载后连续性，再决定是否扩到全身。另一项独立实验是录制同一段移动平台输入，回退到早期子步重新模拟，逐步比较根姿态、支撑 ID 和触地序号。这样可以分别评估角色约束和历史数据的完备程度。[接触回归](../../src/experiments/motion/silver/contact/exchange_tests.rs) · [碰撞历史](../../src/experiments/motion/silver/contact/collision/history.rs)
