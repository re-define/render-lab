# Unreal Animation Framework 实现调研

本文依据 Epic 的 UE 5.8 分支提交 `f97cae4acf019789ed431e0ac28d47e15422784c`。该版本的 UAF 是 Experimental 插件，源码中仍有 `AnimNext` 命名。固定版本的源码链接需要 Epic GitHub 仓库访问权限。Render Lab 的接入草案列在后两节。 [UAF 插件][uaf-api] · [UAF Anim Graph API][graph-api]

## 1. 资产、实例与接口

| 层 | 共享定义 | 运行时状态 |
| --- | --- | --- |
| 模块与脚本 | 模块定义事件、变量和绑定。脚本组件可用 RigVM 执行。 | `FAnimNextModuleInstance` 初始化变量、实例组件和事件 tick，设置 tick group 及内外部前驱。 |
| 动画图 | `UUAFAnimGraph` 保存入口、只读图数据及变量定义。入口指向根 trait。 | `FAnimNextGraphInstance` 分配入口对应的根实例和变量，可由模块或父图托管。 |
| Trait | `FTrait` 提供无实例可变状态的行为。图的共享参数描述某节点使用的片段、阈值或连接。 | `FTraitInstanceData` 保存该节点实例的播放时间、混合权重、活跃状态和子图引用。 |
| 姿态求值 | 采样、混合、解压、添加性姿态、骨骼变换和属性处理各有任务实现。 | 遍历输出 `FEvaluationProgram`。带参考姿态、骨骼 LOD 和绑定的 `FEvaluationVM` 运行这些任务。 |

一个模块可以驱动多个动画图。模块的 RigVM 执行上下文和姿态求值 VM 有各自的输入及生命周期。节点的 base trait 提供基础能力，叠加 trait 可以覆写所实现的接口。调用时从当前栈上方向下找，叠加实现可以显式委托给更低一层。图实例共享编译数据，多个角色各有独立的播放和过渡状态。 [模块事件][module] · [图加载和实例分配][graph] · [根实例][instance] · [FTrait API][trait-api] · [Trait 栈接口查找][stack]

Cook 后的资产从共享归档恢复运行时图数据，编辑器路径可以随代码和内容更新而重新编译。`FTraitWriter` 将节点模板和每个节点的模板索引分别写入归档，再序列化节点共享数据。实例分配只构造所需的运行期状态，避免每个角色重建图定义。 [图的序列化][graph] · [Trait 归档][writer]

~~~mermaid
flowchart LR
    I["角色输入、移动状态与脚本变量"] --> M["模块事件 · 绑定变量与安排顺序"]
    M --> U["Update · 活动分支与时间线"]
    U --> E["Evaluate · 遍历并生成任务"]
    E --> V["Evaluation VM · 采样、混合与变换"]
    V --> P["目标骨架 LOD 的姿态、曲线和属性"]
    V --> H["Pose History"]
    H -.->|前一轮已求值的姿态| U
    U -->|Trait 事件| M
~~~

## 2. 一次动画更新怎样执行

1. **调度模块事件。** 模块实例按脚本组件提供的事件定义建立 tick。相邻事件建立前驱，也可挂外部组件 tick 依赖。允许 worker 的事件用相应线程选项，外部变量代理用 dirty flags 标记待同步字段。第一个用户事件执行 worker 绑定及初始化，后续执行事件和前后回调。必须在游戏线程调用的结果处理可以排队到该线程，并纳入完成依赖。 [模块实例][module] · [模块事件执行][ticks] · [变量代理][variables]
2. **推进图状态。** `FRigUnit_AnimNextRunAnimationGraph_v2` 找到或分配要运行的图实例，传入时间步和输入事件。`UpdateGraph` 从根用显式栈遍历。节点从不活跃变为活跃时可收到 `OnBecomeRelevant`，进入和离开子树时触发 `PreUpdate`、`PostUpdate`。`IUpdateTraversal` 可以筛选孩子和为孩子指定更新状态。Sequence Player 维护时间线，混合栈保留仍在淡出的图实例。该步产生活动分支、目标片段时间和事件。 [图运行入口][run] · [Update 遍历][update] · [序列播放器][sequence] · [混合栈][blend]
3. **生成并执行姿态任务。** `EvaluateGraph` 通过前后序钩子追加求值任务。子节点先产生片段 keyframe，依赖它们的混合及后处理随后读取。`FEvaluationProgram` 按顺序调用任务，`FEvaluationVM` 用命名栈管理中间值。输出依据参考姿态、骨骼映射与图 LOD 构造，可带曲线和额外属性。没有有效求值结果时可返回参考姿态。 [Evaluate 遍历与输出][evaluate] · [程序执行][program]
4. **交付结果和事件。** 图运行入口将输出 trait 事件交给模块。播放通知沿时间线推进，输出动作按线程能力交付。任务图是模块事件的调度手段，姿态求值程序内部按有序任务执行。要扩大吞吐，可以调度彼此独立的实例，实例的状态和临时姿态仍由各自的执行拥有。 [图结果与事件回传][run] · [通知调度][notifies] · [事件上下文 API][task-api]

## 3. Motion Matching 如何作为可替换层接入

UE 的 Game Animation Sample 展示 Animation Blueprint 路线，使用 Chooser 缩小待搜数据库，并用 Pose History 提供过去的骨骼和预测轨迹。UAF Pose Search 插件另有 trait 路线。`FMotionMatchingTrait` 在 Update 查询混合栈、姿态历史和时间线接口，用历史构造搜索上下文并更新续播或切换决策，混合栈承接过渡。`FHistoryCollectorTrait` 在 Evaluate 附加采集任务，将实际求值出的姿态用于下一轮。根运动还可由求值程序的 Motion Matching 任务修改。 [Game Animation Sample][sample] · [UAF Pose Search 插件][pose-api] · [Motion Matching][matching] · [历史采集][history] · [根运动任务][root-task]

**索引如何构建和使用。** Schema 声明轨迹采样点、姿态通道和权重。烘焙时从片段采样，按配置对兼容特征估计偏差，用平方根权重实现加权距离，再对加权样本中心化、求主轴并构建 KD-tree。查询应用索引的同一权重、均值和投影。KD-tree 取有限近邻，过滤后在原特征域复评候选，续播等路径也可参加比较。截断 PCA 加固定 K 近邻时，应把全维最优召回率作为实验指标。`BruteForce` 可用于这项对照，API 也列出 VP-tree 与事件模式。 [权重与 PCA][preprocess] · [投影][project] · [KD-tree 候选和复评][rerank] · [搜索模式 API][modes]

Render Lab 的银狼已经建立了一套不同的 KD-tree 搜索。离线和运行时共享同一归一化，搜索用正交投影下节点包围盒的距离作全维代价下界。叶内用完整向量和非负切换代价评分，只剪掉下界已高于当前最优的节点。现有回归将树查询与全扫描对照。未来可用同一组输入比较两种候选策略的查询 P95、召回率及动作切换。 [本引擎检索][lab-search] · [银狼回归][lab-silver]

## 4. Render Lab 接入草案

当前 `SilverMatcher` 负责带转向和加速度的运动预测、数据库检索、采样、惯性过渡和 ECS 写回。提取阶段从关节层级构建调色板，GPU 蒙皮生成姿态对应的几何，沿用可见性、运动向量和光追的数据路径。可以把动画逻辑拆成以下独立边界，让旧实验和新图发出相同类型的输出。 [银狼播放][lab-silver] · [轨迹与过渡][lab-travel] · [骨骼调色板][lab-palette] · [GPU 几何][lab-geometry]

| 拟议边界 | 数据及职责 |
| --- | --- |
| 骨架与动画资产 | 存父索引、参考局部姿态、映射与片段轨道。动画库、特征 schema、归一化和索引一起版本化。复用既有 glTF 及 Kimodo 烘焙入口。 |
| 编译图 | 将有类型的配置编译成节点、输入输出槽、初始状态和临时姿态容量。验证类型、空间、骨架版本及环。历史反馈通过跨 tick 槽表达。 |
| 图实例 | 保存每个角色的时间线、混合状态、Pose History 与输入。图和索引共享，实例换绑、切镜涉及的渲染历史与姿态历史各有重置规则。 |
| 节点 | 第一版提供片段采样、混合、惯性过渡和 Motion Matching，可扩展 warping、IK、事件及可选子图。用可测试的 Rust 枚举和连续数组表达，按活跃分支执行。 |
| 发布 | 输出局部 TRS、有效关节、根运动、曲线和通知。适配器按角色绑定提交 ECS，事件带实例、片段时间区间和步编号供去重。 |

**和物理如何对接。** 拟将动态角色的根运动当作目标传给控制器或求解器，由物理结果决定实际位移。完成求解后，再以实际地面命中及刚体姿态进行脚部 IK 或 ragdoll 混合。用前后两个系统表达这条链，根节点与每组骨骼只指定一个当步写回者。单独使用动画时可用现有直接写回路径对照。 [与物理共用的子步计划](chaos-physics.md#8-render-lab-接入草案)

**如何并行和提交。** `Schedule` 继续按资源键、组件键和实体 ID 判定依赖。独立角色或角色批可进同一个 Rayon 池，每 worker 复用临时姿态，避免为每个节点都派发小任务。系统以声明的实体集合写回，姿态和运行时状态在批次校验后提交。渲染只取已提交的状态。 [模拟调度][lab-schedule] · [访问限制与失败回滚][lab-runtime]

## 5. 迁移验收

| 阶段 | 验证内容 |
| --- | --- |
| 片段播放器与发布 | 用同一输入回放对照银狼的根路径、关键关节、通知次数和蒙皮像素，验证保存、加载和换绑。 |
| 可组合图 | 用原检索器和新节点组合跑已有转弯、停止、倒退及横移。核对片段选择、切换时间、脚滑与姿态速度连续性。 |
| 并行和规模 | 对照单角色、共享图的多角色和多骨架角色。分别量 Update、Evaluate、提交和提取的 P50/P95、临时姿态峰值与 worker 分布。 |
| 动画与物理交接 | 走台阶、贴地、跌倒和起身过程中检查根运动所有权、骨骼跳变、事件重复和运动向量。测试变帧率与不同 worker 数下的稳定回放。 |

[uaf-api]: https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAF?application_version=5.8
[graph-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph?application_version=5.8
[trait-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph/FTrait?application_version=5.8
[task-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAF/FModuleTaskContext?application_version=5.8
[sample]: https://dev.epicgames.com/documentation/en-us/unreal-engine/game-animation-sample-project-in-unreal-engine?application_version=5.8
[pose-api]: https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAFPoseSearch?application_version=5.8
[modes]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/PoseSearch/EPoseSearchMode?application_version=5.8
[module]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAF/Source/UAF/Private/Module/AnimNextModuleInstance.cpp#L78-L217
[graph]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/AnimNextAnimationGraph.cpp#L48-L178
[writer]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitCore/TraitWriter.cpp#L25-L181
[instance]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/AnimNextGraphInstance.cpp#L212-L285
[stack]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitCore/TraitStackBinding.cpp#L210-L285
[ticks]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAF/Source/UAF/Private/Module/ModuleTickFunction.cpp#L125-L241
[variables]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAF/Source/UAF/Private/Variables/UAFInstanceVariableDataProxy.cpp#L11-L80
[run]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/RigUnit_AnimNextRunAnimationGraph_v2.cpp#L25-L180
[update]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitInterfaces/IUpdate.cpp#L503-L704
[evaluate]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitInterfaces/IEvaluate.cpp#L116-L300
[program]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/EvaluationProgram.cpp#L19-L30
[sequence]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/SequencePlayer.cpp#L42-L217
[blend]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/BlendStackTrait.cpp#L97-L181
[notifies]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/NotifyDispatcher.cpp#L32-L80
[matching]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/MotionMatchingTrait.cpp#L75-L450
[history]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/HistoryCollectorTrait.cpp#L209-L306
[root-task]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/MotionMatchingTrait.cpp#L554-L675
[preprocess]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchDerivedData.cpp#L654-L826
[project]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchIndex.cpp#L722-L742
[rerank]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchDatabase.cpp#L1963-L2196
[lab-search]: ../src/experiments/motion/silver/search.rs#L116
[lab-silver]: ../src/experiments/motion/silver.rs#L292
[lab-travel]: ../src/experiments/motion/silver/locomotion.rs
[lab-palette]: ../src/render/scene.rs#L404
[lab-geometry]: ../src/render/geometry.rs
[lab-schedule]: ../src/engine/schedule.rs
[lab-runtime]: runtime-and-scene.md#模拟阶段如何读写
