# Chaos 物理与破碎实现调研

本文依据 Epic 的 UE 5.8 分支提交 `f97cae4acf019789ed431e0ac28d47e15422784c`，关注世界刚体、碰撞、约束、破碎和动画接入。源码链接需要 Epic GitHub 仓库访问权限。Chaos 还覆盖布料等独立求解器，这里的刚体流程以 `FPBDRigidsEvolutionGBF` 的实现为准。 [Chaos 类型索引][chaos-api] · [刚体求解器 API][solver-api]

## 1. 外部对象和内部求解状态

| 部件 | 职责 |
| --- | --- |
| Physics Proxy | 隔离游戏线程对象和物理线程粒子。刚体、Geometry Collection、关节等有自己的代理。外部用句柄更新属性，内部以求解器粒子及关联的形状运行。 |
| `FPBDRigidsSolver` | 管理粒子、约束容器、材质、代理、时间和回调，持有 `FPBDRigidsEvolutionGBF`。负责消费输入和缓冲结果。 |
| 粒子数据 | `FPBDRigidsSOAs` 组织粒子数据和筛选视图。可动体存质心与惯量、速度、角速度、当前和预测变换。静态或运动学对象为动态体的约束提供边界条件。 |
| `FChaosMarshallingManager` | 在外部输入与内部求解间用时间戳、物理步编号和前后队列交接。支持收集脏代理、命令及子步数据、回收缓冲和可选历史。 |
| 外部结果 | 求解器按类型写入脏刚体、Geometry Collection、簇及部分约束的输出。单体代理消费带时间戳的结果，在异步路径有前后记录时可插值。 |

游戏线程与物理求解的异步交接，以及一次求解中工作任务的并行，是两个独立维度。可在保留固定子步和独立约束岛的前提下选择同步提交。 [求解器与任务类型][solver-header] · [输入发布][push] · [输入消费][process] · [带时间戳的数据队列][marshal] · [脏输出打包][buffer] · [单体代理回写与插值][pull]

~~~mermaid
flowchart TD
    G["游戏线程 · 输入及脏代理"] -->|时间戳和步编号| P["物理线程 · 应用输入"]
    P --> I["速度和姿态预测 · 运动学目标"]
    I --> B["更新空间索引 · 宽相候选"]
    B --> C["中相、精确接触和 CCD"]
    C --> D["接触与关节构成约束图"]
    D --> S["分组求解岛 · Gather / Solve / Scatter"]
    S --> F["睡眠、破碎、姿态与事件"]
    F -->|脏结果缓冲| O["游戏线程 · 消费结果和显示插值"]
~~~

## 2. 时钟和线程交接

输入以外部帧和物理子步分别编号。`FChaosMarshallingManager::Step_External` 可以用一次外部提交生成多个子步条目，设置区间索引，并为后续子步复制必要输入。求解任务消费一份带时间戳的推送，应用属性、回调和命令。持续力在覆盖的子步持续生效，一次性冲量应按输入约定只应用一次，运动学目标可随子步分段。子步越小通常越有利于保持堆叠和关节稳定，但会增加碰撞和求解次数。UE 项目设置分别提供最大子步长和数目、异步 tick 及其固定步长。 [输入队列][marshal] · [子步与力的处理][substep] · [Chaos 项目设置][settings]

后端将求解完成的变更存到 pull 记录，标记已消费的外部时间戳及模拟的起止时间。异步游戏线程读到的是已发布的物理结果。插值以该结果的模拟时刻为基准，事件也随物理子步排序。精确查询正在求解的接触或形状，需要使用求解时的物理上下文或从已发布的查询场景查询。 [Pull 数据的时间][marshal] · [消费与时间插值][pull]

## 3. 预测、宽相、中相、窄相与 CCD

`AdvanceOneTimeStepImpl` 先积分动态体，处理运动学目标，并维护内部空间加速结构。位置和旋转预测使用当前速度、角速度和施加的力。速度或碰撞厚度可扩张包围盒，运动大的 CCD 物体可用扫掠包围盒。预测让后续阶段在待求解的位姿上查找候选。 [一子步的积分与空间结构][advance-start] · [预测、包围和 CCD 标记][integrate]

| 阶段 | 输入与输出 |
| --- | --- |
| Broad phase | 按 AABB 查询空间加速结构，收集可能重叠的粒子对。用过滤逻辑和每线程上下文压缩候选，交给对应的粒子对处理中相。 |
| Mid phase | 管理同一粒子对内的形状对与缓存。遇到复合形状或层级几何时深入其子形状或 BVH，避免组合枚举，也可沿用仍有效的接触对象。 |
| Narrow phase | 按类型做精确接触，形成接触点、局部锚点、法线和分离距离等流形数据。按配置检查接触流形是否可恢复，否则更新；清理过期的碰撞记录。 |
| CCD | 筛选需要连续检测的对象及形状对，扫掠查询接触时刻。处理顺序允许在约束图构建前回退碰撞涉及的预测状态，更新接触；求解后还有校正路径。 |

宽相候选是可能接触，流形才为求解器提供实际约束。CCD 的碰撞回退针对当前子步的运动。网络重模拟的回退则从历史步重新运行。 [宽相收集][broad] · [中相与流形恢复][mid] · [形状扫掠][sweep] · [CCD 与约束图的执行顺序][advance-ccd]

## 4. 约束岛、并行和迭代求解

1. **建图。** 将当前接触和关节连接到粒子节点。能互相影响的动态体构成岛。一个静态地面或运动学边界可以为多个岛提供约束输入，岛边界以动态可写状态是否相交为准。已有关系会维护和更新，删去约束可能让岛拆分。 [岛管理][island]
2. **分配工作。** `FPBDIslandGroupManager::BuildGroups` 估计各岛的约束量，按配置可从重到轻分配，将多个小岛合成工作量较均衡的组。独立组能并行；大岛仍可能成为关键路径。源码保留多种执行组织，一条任务式路径先 Gather Bodies，再分批 Gather Constraints，完成并求解后 Scatter Constraints，最后 Scatter Bodies。 [负载分组][groups] · [执行依赖][gather]
3. **逐阶段求解。** 组从粒子收集求解体与约束数据。`IslandGroupManager` 运行位置、速度以及配置启用的投影阶段。接触位置阶段修正互穿及部分摩擦约束，速度阶段处理相对速度、反弹和摩擦。法向约束单侧生效，接触可推开物体，切向摩擦受法向响应与材质限制。关节约束可限制线性和角向自由度。碰撞容器的投影接口在这条实现中为空。Evolution 可选择 Gauss–Seidel、SIMD Gauss–Seidel 或 Partial Jacobi 等接触求解策略，具体路径依设置。顺序 Gauss–Seidel 处理后续约束时可看到本轮已修正的姿态，有共享刚体的接触不能直接无序并行写同一状态。 [组内阶段][apply] · [碰撞约束求解][collision-solve] · [求解器选择][solver-mode] · [关节和求解变体][chaos-api]
4. **提交和休眠。** 迭代结束后写回修正结果，仍活跃的接触数据可被下一步沿用。Evolution 在这里处理睡眠和停用，推进簇破碎，按配置捕获回退数据，再发布脏粒子变换。休眠减少参与积分和求解的对象，外力或新接触可唤醒。 [子步收尾][advance-end] · [唤醒和岛更新][island]

复现稳定性时应同时记录子步长、迭代数、形状、接触偏移和质量比例。只测一个大堆叠会偏向大岛串行瓶颈，多个独立堆叠才能分辨岛并行及任务分组的成本。

## 5. Chaos Destruction

**离线资产。** Geometry Collection 将预切碎块、每块局部变换、碰撞形状、层级和物性集中起来。编辑器构建 Voronoi 等碎块及邻接关系。运行时由 `FGeometryCollectionPhysicsProxy` 建立粒子和簇。一个完整簇可作为一个活动刚体求解，其质心、惯量和碰撞形状来自成员。层级控制释放粒度，邻接图控制成员断开后哪些片段仍连在一起。 [Geometry Collection 工作流][destruction] · [组件数据][gc-api] · [代理初始化][gc-init] · [质量与簇创建][cluster-create]

**运行期。** 子步求解后，`FRigidClustering::AdvanceClustering` 使用碰撞约束累计冲量等数据处理破碎。`ReleaseClusterParticlesImpl` 这条路径以子体内部阈值与碰撞／外部应变比较，按配置传播破损和冲击，释放子体并可为剩余连接部分创建新簇。Physics Fields 也能施加力、锚定或改变可破坏状态。求解器记录每次破碎和实体状态变化，外部消费结果以驱动渲染和效果。每步处理数量可设上限，剩余候选按预算续做。这里的“应变”是可配置的破损度量，具体算法和数据转换由损伤模式决定。 [释放条件与传播][release] · [每步破碎预算][break-budget] · [官方描述][destruction]

**先看现有实验。** Render Lab 已生成 Voronoi 碎片，按播放时间计算弹道、角速度和地板接触下限，再按实体 ID 写局部矩阵。碎片没有互撞求解，溶解属于渲染覆盖。现有几何可以转化为破碎资产的输入，试验中再比较按时间触发和撞击触发的行为。 [现有碎片与弹道][lab-fracture] · [效果播放][lab-playback]

## 6. 历史与调试

`FRewindData` 按物理帧和阶段保存用于回退的刚体与关节数据，检查目标是否落在有效历史范围内。重模拟还依赖 marshalling 保留的输入和命令。网络校正时，恢复旧步、应用权威状态并向前重播；显示层可对结果做校正插值。这个机制需要历史内存和重播预算，固定步和排序能减少变化来源，跨机或不同后端的逐位一致仍需单独验证。 [历史捕获入口][rewind-capture] · [回退检查及执行][rewind] · [网络物理文档][network]

Chaos Visual Debugger 能录制、回放并检查物理帧及子阶段。对照画面时应能定位对应物理步的输入、活动粒子、碰撞对、接触、岛和破碎事件，调试器可据此区分碰撞漏检、穿透、质量异常、睡眠误判和渲染插值不连续。 [Chaos Visual Debugger][cvd] · [Evolution 的阶段标记][advance-ccd]

## 7. 建议的独立 Rust 刚体模块

| 边界 | 第一版的实现重点 |
| --- | --- |
| 资产与句柄 | 用稳定实体 ID 连接带代际编号的刚体句柄。把碰撞形状、局部形状变换、过滤组、质量、质心及惯量放进物理数据。刚体使用米、秒、千克，单测导入缩放。三角层级与光追 AS 由渲染侧按自己的用途管理。 |
| 空间查询 | 先接静态地形和运动学胶囊、球、盒及凸体。查询独立发布一份带步号的快照，提供 ray、shape cast、overlap。窄相先用解析几何及凸体配对，最后加入复杂复合几何与 CCD。 |
| 求解 | 固定步半隐式积分，先做接触与关节。以稳定的体对与形状对 ID 保留流形，记录法向、摩擦、恢复和迭代误差。动态岛并行，岛内可先串行，观测大岛再决定 SIMD、分色或 Jacobi。 |
| 破碎 | 后续复用现有 Voronoi 资产，加入质量与惯量聚合、局部碎块变换、连接图和分层激活。用代表形状模拟未破坏的簇，破碎时给子体继承父体的线速度和角速度，按已释放质心计算对应速度。 |
| 验收与观测 | 堆叠比较最大穿透与静止速度；摆锤比较关节误差；高速体比较 CCD 命中；晶体检查破碎顺序、事件、质量和动量误差。量宽／窄相、岛、求解和提交 P95，同时记录活跃体数、候选数、接触数与每岛约束数。 |

## 8. Render Lab 接入草案

拟用现有 `Schedule::fixed_stage` 作为首先验证的时钟。一个子步先提交角色意图与运动学目标，再让刚体求解拥有该步唯一的物理状态，随后用结果推进动画后处理和姿态发布。不同固定阶段有各自的累计时间，因此互相依赖的步骤放进同一个固定阶段。 [现有固定步及执行][lab-schedule] · [动画输入、姿态与根运动](unreal-animation-framework.md#4-render-lab-接入草案)

| 子步 | 输入与提交 |
| --- | --- |
| 动画前置／运动控制 | 获取已接受的输入、上一物理结果和动画历史，生成胶囊移动或根运动请求。动画试算状态随本子步走，供后置步骤复用。 |
| 求解 | 消费本子步的命令，推进物理世界，产生带步编号的碰撞查询快照、脏刚体姿态与事件。先实现同步栅栏内的并行岛。需要异步时再引入有界命令队列与已完成区间的结果发布。 |
| 姿态合成和提交 | 用当前物理结果做 IK 或 ragdoll 混合。物理提交为动态根或碎块提供变换，动画提交负责角色骨骼，每个变换有唯一的发布者。ECS 中若有父节点，按父变换将物理世界姿态换回局部坐标。 |
| 渲染 | 从已完成的状态生成调色板和实例快照。渲染帧可在两个固定步样本之间插值，瞬移、改绑或重播会使相应历史失效。现有 GPU 蒙皮与运动向量读取实际发布的相邻渲染状态。 |

现有 ECS 回滚只还原已登记的可撤销编辑。拟将求解输出和事件先暂存，在子步成功后发布；有状态求解器的私有粒子、缓存和时间必须有可回滚的提交边界。碰撞条件依赖物理资源顺序，姿态写回再按实际实体范围声明。同一子步失败后重试时，冲量、触发和破碎事件需要按步号去重。 [模拟调度与回滚][lab-runtime]

建议先用碰撞、台阶和堆叠场景核对固定步与查询，再接现有晶体破碎，然后接银狼根运动、脚部放置和 ragdoll。等同步执行的行为可回放确认后，再决定异步物理线程、重模拟、车辆和布料的实现边界。

[chaos-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/Chaos?application_version=5.8
[solver-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/Chaos/FPBDRigidsSolver_1?application_version=5.8
[settings]: https://dev.epicgames.com/documentation/en-us/unreal-engine/physics-settings-in-the-unreal-engine-project-settings?application_version=5.8
[substep]: https://dev.epicgames.com/documentation/en-us/unreal-engine/physics-sub-stepping-in-unreal-engine?application_version=5.8
[destruction]: https://dev.epicgames.com/documentation/en-us/unreal-engine/destruction-overview?application_version=5.8
[gc-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/GeometryCollectionEngine/UGeometryCollectionComponent?application_version=5.8
[cvd]: https://dev.epicgames.com/documentation/en-us/unreal-engine/chaos-visual-debugger-in-unreal-engine?application_version=5.8
[network]: https://dev.epicgames.com/documentation/en-us/unreal-engine/networked-physics-overview?application_version=5.8
[solver-header]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Framework/PhysicsSolverBase.h#L108-L250
[push]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PBDRigidsSolver.cpp#L1500-L1620
[process]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PBDRigidsSolver.cpp#L1974-L2180
[solver-mode]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L503-L527
[marshal]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/ChaosMarshallingManager.cpp#L42-L136
[buffer]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PBDRigidsSolver.cpp#L2996-L3126
[pull]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PhysicsProxy/SingleParticlePhysicsProxy.cpp#L272-L408
[advance-start]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L529-L661
[integrate]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L868-L1048
[advance-ccd]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L659-L760
[advance-end]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L765-L829
[broad]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/SpatialAccelerationBroadPhase.cpp#L17-L152
[mid]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/ParticlePairMidPhase.cpp#L471-L567
[sweep]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/ParticlePairMidPhase.cpp#L572-L682
[island]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandManager.cpp#L1637-L1735
[groups]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandGroupManager.cpp#L120-L218
[gather]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandGroupManager.cpp#L493-L617
[apply]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandGroupManager.cpp#L259-L290
[collision-solve]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/PBDCollisionContainerSolver.cpp#L734-L932
[gc-init]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PhysicsProxy/GeometryCollectionPhysicsProxy.cpp#L1300-L1450
[cluster-create]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L233-L332
[release]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L1109-L1325
[break-budget]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L1656-L1750
[rewind-capture]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PBDRigidsSolver.cpp#L1272-L1313
[rewind]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/RewindData.cpp#L901-L1072
[lab-fracture]: ../src/experiments/effects/fracture.rs#L164
[lab-playback]: ../src/experiments/effects.rs#L137
[lab-schedule]: ../src/engine/schedule.rs#L294
[lab-runtime]: runtime-and-scene.md#模拟阶段如何读写
