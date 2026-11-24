# Unreal Animation Framework 的执行流程与动画算法

本文沿 UE 5.8 分支的固定提交 `f97cae4acf019789ed431e0ac28d47e15422784c` 追踪 UAF 从模块调度到骨骼姿态输出。相同提交中仍有 `AnimNext` 类型名。UAF 在该版本是 Experimental 插件。下文伪代码压缩了分支和错误处理，数学式采用通常的向量记法，便于与固定行号的 C++ 对照。源码链接需要 Epic GitHub 仓库访问权限。 [插件与模块边界][uaf] · [姿态图 API][graph-api]

## 1. 模块驱动图，图驱动姿态

一个模块实例包含脚本变量、事件 tick 和扩展组件。初始化根据脚本提供的事件设置 tick group、前驱及是否可在 worker 执行。外部写入变量代理后标记 dirty，受控复制点将这些变量带进实例。首个用户事件安排 worker 绑定、初始化和模块开始回调，事件本身可以调用 RigVM。排队的游戏线程工作完成后才结束相应的 tick 依赖。 [模块初始化和事件依赖][module] · [变量代理合入][copy-vars] · [事件执行][tick]

模块中的动画运行节点得到图资产、目标参考姿态、LOD 与时间增量，复用或分配图实例。它把输入事件交给 `UpdateGraph`，再用 `EvaluateGraph` 求解当前输出，最后回传图产生的事件。这里有两种计算手段。RigVM 运行模块脚本及绑定，姿态 Evaluation VM 执行解压、混合等任务序列。 [运行节点][run] · [求值程序执行][program]

~~~mermaid
flowchart LR
    T["模块 tick · 游戏输入与脚本绑定"] --> U["Update · 活跃分支、时钟、切换"]
    U --> E["Evaluate 遍历 · 生成本次任务序列"]
    E --> V["Evaluation VM · 解压、混合、修正"]
    V --> P["目标骨架 LOD 的姿态、曲线、属性"]
    P --> H["Pose History"]
    H -.->|已求值的历史与轨迹| U
    U -->|Trait 事件| T
~~~

## 2. Trait 栈与实例布局

`FTrait` 是共享行为对象，支持 `IUpdate`、`IEvaluate`、`ITimeline`、`IHierarchy` 等接口。编译图的共享数据存放节点模板及节点描述。模板决定 trait 在栈中的次序，以及共享／实例数据的大小和对齐。读归档时节点的共享数据按对齐布局，入口句柄解析到节点与 trait 索引。运行时分配节点实例时，根据模板分配并构造每个 trait 的实例数据，例如播放时间、子图句柄、同步状态或过渡历史。一个节点被其栈中的另一 trait 引用时可以共用节点实例，用弱引用指向这条栈内引用。 [模板和归档][writer] · [加载与根入口][load] · [节点数据布局][reader] · [运行时分配][alloc] · [FTrait API][trait]

接口调用从栈上层向下找第一个提供者。提供者可以通过 `super` 查找继续调用下方的实现。两层都实现 `IUpdate` 时，先进入上层，上层可以在执行基础播放器前后加逻辑。若上层拦截且不委托，下层行为便不会沿同一接口继续执行。这样可以在片段播放器之上叠加同步或通知，也可以在混合栈之上叠加 Motion Matching，角色间仍只共享只读定义。 [接口查找与 super][stack] · [同步 trait][sync-trait] · [Motion Matching 接口][matching]

## 3. Update 选活动分支，Evaluate 发出计算任务

**Update。** `UpdateGraph` 用显式栈遍历当前图。进入节点时更新相关性、传播事件和绑定输入，调用 `PreUpdate`，把自己作为后处理节点压回栈，再排入活跃孩子。孩子的 `FTraitUpdateState` 可包含该分支的权重、时间增量和 newly-relevant 标志。所有孩子处理完，再执行该节点 `PostUpdate`。从无效到活跃的节点先调用 `OnBecomeRelevant`，让序列播放器有机会重置时间。以 two-way blend 为例，`w` 近 0 时只走 A，近 1 时只走 B，混合区才为两棵子树分别传 `1−w` 和 `w`。 [Update 的遍历和事件][update] · [两路混合的分支选择][blend-trait] · [序列相关性与推进][sequence-time]

**Evaluate。** 第二次遍历通过 `PreEvaluate`、`PostEvaluate` 追加任务，普通混合树的孩子先发采样任务，父节点再发合成任务。VM 按顺序读写命名值栈。以两路片段为例，下面三条任务就能产生一份最终 keyframe。保留 `RefPose` 与 LOD 可让初始化和缺失轨道有一致的起点。当前源码既有常规 keyframe 栈，也有按类型存属性的 value bundle 路径，具体选择受运行时配置约束。 [求值遍历与 VM 输出][evaluate] · [两路混合的两种路径][blend-task]

~~~text
PushSequence(A, 已更新的 tA)  # 得到 A 的局部姿态、曲线和属性
PushSequence(B, 已更新的 tB)  # 得到 B
BlendTwoKeyframes(w)           # 弹出 A/B，压入合成后的姿态
~~~

**取样。** 片段播放器用 `dt × play_rate` 推进时间，通过 `AdvanceTime` 处理循环或碰到边界。求值任务把时间、循环和时间区间交给解压入口，按骨架绑定列出需解压的轨道与输出骨骼。原始轨道和压缩轨道走对应路径，压缩分支调用动画资产的 codec。曲线及属性随姿态一起求值。根运动可作为时间区间上的 delta 单独写成属性，是否锁定根骨由解压上下文决定。这样，供蒙皮的根骨姿态和从片段抽取的移动量都有明确来源。 [时间线][sequence-time] · [取样任务][sample] · [骨骼映射、解压与根锁][decode]

## 4. 混合算法

### 4.1 权重混合与逐骨混合

常规 keyframe 路径做逐骨 TRS 合成。平移和缩放取 `(1−w)A+wB`。旋转先让两个四元数同半球，再做加权和并归一化，相当于 nlerp 的做法。曲线走带标识的插值，属性使用自己的类型混合规则。极端权重直接保留一份 keyframe。逐骨版把混合权重细化到骨骼，便于同一输出的上下半身用不同来源。Additive 任务在基姿态上累积一个差值姿态，区分局部和 mesh-space 旋转语义。 [两路 keyframe 合成][blend-task] · [SoA 四元数方向和加权][soa] · [逐骨混合][per-bone] · [添加性任务][additive]

### 4.2 同步组的时间重映射

同步组从本帧活跃的时间线选 leader。可当 leader 的成员按累计权重比较，强制角色走相应规则。leader 正常推进，follower 以 leader 的可匹配位置同步。能匹配标记相位时，可将 leader 位于两个脚步标记之间的分数映射到 follower 对应的两个标记时刻。归一化进度同步可写为 `t_f = (t_l / duration_l) × duration_f`，源代码还处理 leader 更换、循环与重新对齐。标记同步保留同相位落脚，比例同步使不同片长在同一相对进度推进。 [leader 选择][leader] · [标记相位逻辑][phase] · [比例回退][phase-fallback]

### 4.3 Dead Blending 怎样消除跳转时的突变

这一 trait 接收惯性过渡请求。在过渡开始时，用旧姿态的前后帧差估计速度，再与新姿态的当前值比较。平移速度用差分，角速度用最短弧四元数差转换为旋转向量再除以采样间隔，缩放在对数空间估速。三种速度分别限幅，逐轴半衰期根据朝向目标的差值与速度计算，并限制在设置的上下界内。 [请求与任务发射][dead-trait] · [速度和半衰期估计][dead-init]

令某轴半衰期为 `h`，`c = ln(2)/h`。以指数衰减速度 `v(t) = v₀ e⁻ᶜᵗ` 积分，旧姿态的平移外推为 `x_dead(t) = x₀ + (v₀/c)(1−e⁻ᶜᵗ)`。源码用衰减近似函数。旋转把同形式的角位移转成四元数增量，缩放把对数差取指数再相乘。最终以过渡曲线衰减旧分支权重，在当前新姿态与外推姿态之间合成。旋转合成用相对旋转向量并保持方向连续。过渡期间持续采样新片段，旧姿态的速度由历史外推提供。 [外推式][dead-extrap] · [合成与旋转连续性][dead-apply]

## 5. Pose Search 怎样决定播哪一帧

Pose Search 是独立的数据库和查询库。UAF 用 `FMotionMatchingTrait`、Pose History 接口和混合栈把它接进 Update/Evaluate。以下选择逻辑以 UAF trait 和 `PCAKDTree` 这条搜索路径为准。 [UAF Pose Search][pose-api] · [搜索模式][search-mode]

1. **离线采样。** 资产索引器按 schema 的采样率和通道，从每个可索引时刻提取关节位置、速度、朝向或轨迹等向量。速度可以由前后样本差分。在互相兼容的通道上估计偏差，利用尺度和权重建立加权特征。令 `s` 为数据库的有效平方根权重向量，`x` 为特征，则加权样本为 `y = s ⊙ x`。用加权样本均值 `μ` 中心化后保留主轴 `Pₖ`，写入 `z = (y−μ)Pₖ` 及 KD-tree。运行时投影复用该库的权重、中心和主轴。 [资产索引与差分][indexer] · [偏差与权重][preprocess] · [查询投影][project]
2. **构造候选。** `PreUpdate` 读取当前时间线、已采集的姿态和轨迹，在触发搜索时为相关 schema 构建查询。可继续播放的当前片段在相应时刻得到续播候选，按中断配置判断是否保留。切换搜索把查询映射到 PCA 空间，查 K 个近邻。条件允许时在树搜索中过滤，否则候选出来后过滤。多个原始 pose 共用压缩投影条目时再展开对应 pose。 [UAF 搜索节流与结果][matching] · [续播候选][continuing] · [近邻及过滤][knn] · [展开候选][expand]
3. **复评和胜出。** 候选的原特征比较可写为 `Σᵢ((xᵢ−qᵢ)s′ᵢ)² + C_metadata + C_continue + C_interaction + C_context`，其中 `s′` 是该次比较可用的动态权重。不同筛选条件决定候选是否参与。PCA 阶段使用库的投影，复评使用完整特征维度，并在配置要求时重建被裁剪存储的特征。K 近邻加截断 PCA 时，召回与全扫描可能不同，动态权重和额外代价也会改变最终排序。 [代价实现][cost] · [近邻模式与扫描校验][search-mode]
4. **播放。** 续播胜出就继续推进当前播放器。跳转可生成以选定片段、采样时刻、镜像和播放率为输入的子图请求，交给 `IBlendStack`。新片段开始产生姿态，尚有权重的旧图继续参与混合。姿态历史 trait 把采集任务附到求值程序，用实际输出的姿态构造后续查询。Motion Matching 还可在求值路径处理根运动属性。 [子图创建和入栈][activate] · [混合栈][blend-stack] · [采集历史][history] · [根运动求值任务][root-motion]

Motion Matching 图的更新顺序可以概括为「上次 Pose History → 续播／跳转选择 → 更新活跃播放器 → 取样和过渡 → 当前姿态入历史」。数据库的某一行只是片段时刻和特征，画面中的姿态还要经过混合栈、同步组及图后处理。

[uaf]: https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAF?application_version=5.8
[graph-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph?application_version=5.8
[trait]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/UAFAnimGraph/FTrait?application_version=5.8
[pose-api]: https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAFPoseSearch?application_version=5.8
[search-mode]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/PoseSearch/EPoseSearchMode?application_version=5.8
[module]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAF/Source/UAF/Private/Module/AnimNextModuleInstance.cpp#L121-L217
[copy-vars]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAF/Source/UAF/Private/Module/AnimNextModuleInstance.cpp#L412-L433
[tick]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAF/Source/UAF/Private/Module/ModuleTickFunction.cpp#L125-L241
[run]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/RigUnit_AnimNextRunAnimationGraph_v2.cpp#L25-L180
[program]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/EvaluationProgram.cpp#L19-L30
[writer]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitCore/TraitWriter.cpp#L25-L181
[load]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/AnimNextAnimationGraph.cpp#L124-L231
[reader]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitCore/TraitReader.cpp#L56-L115
[alloc]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitCore/ExecutionContext.cpp#L125-L260
[stack]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitCore/TraitStackBinding.cpp#L210-L285
[sync-trait]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/SynchronizeUsingGroups.cpp#L27-L165
[update]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitInterfaces/IUpdate.cpp#L503-L704
[blend-trait]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/BlendTwoWay.cpp#L29-L128
[sequence-time]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/SequencePlayer.cpp#L141-L193
[evaluate]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/TraitInterfaces/IEvaluate.cpp#L116-L306
[blend-task]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/BlendKeyframes.cpp#L34-L147
[sample]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/PushAnimSequenceKeyframe.cpp#L24-L144
[decode]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/DecompressionTools.cpp#L100-L304
[soa]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAF/Source/UAF/Private/TransformArrayOperations_SoA.cpp#L315-L379
[per-bone]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/BlendKeyframesPerBone.cpp#L17-L133
[additive]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/ApplyAdditiveKeyframe.cpp#L24-L149
[leader]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/SyncGroup_GraphInstanceComponent.cpp#L299-L358
[phase]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/SyncGroup_GraphInstanceComponent.cpp#L1548-L1616
[phase-fallback]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Graph/SyncGroup_GraphInstanceComponent.cpp#L1990-L2042
[dead-trait]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/InertializationDeadBlend.cpp#L32-L173
[dead-init]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/DeadBlending.cpp#L243-L301
[dead-extrap]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/DeadBlending.cpp#L93-L185
[dead-apply]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/EvaluationVM/Tasks/DeadBlending.cpp#L373-L451
[indexer]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchAssetIndexer.cpp#L581-L651
[preprocess]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchDerivedData.cpp#L654-L826
[project]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchIndex.cpp#L722-L742
[matching]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/MotionMatchingTrait.cpp#L90-L281
[continuing]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchLibrary.cpp#L1315-L1409
[knn]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchDatabase.cpp#L1976-L2036
[expand]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchDatabase.cpp#L2138-L2201
[cost]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/PoseSearch/Source/Runtime/Private/PoseSearchDatabase.cpp#L153-L193
[activate]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/MotionMatchingTrait.cpp#L276-L415
[blend-stack]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFAnimGraph/Source/UAFAnimGraph/Private/Traits/BlendStackTrait.cpp#L97-L181
[history]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/HistoryCollectorTrait.cpp#L209-L306
[root-motion]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/MotionMatchingTrait.cpp#L554-L675
