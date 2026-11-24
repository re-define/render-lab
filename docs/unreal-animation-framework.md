# Unreal Animation Framework 的执行流程与动画算法

本文沿 UE 5.8 分支的固定提交 `f97cae4acf019789ed431e0ac28d47e15422784c` 追踪 UAF 从模块调度到骨骼姿态输出。相同提交中仍有 `AnimNext` 类型名。UAF 在该版本是 Experimental 插件。下文伪代码压缩了分支和错误处理，数学式采用通常的向量记法，便于与固定行号的 C++ 对照。源码链接需要 Epic GitHub 仓库访问权限。 [插件与模块边界][uaf] · [姿态图 API][graph-api]

[接触对齐与抓取](#6-如何精确碰到踩到或抓到目标) · [骨骼重定向](#7-同一动作怎样适配不同骨骼) · [物理反馈与骨骼混合](#8-收到物理反馈后怎样调整骨骼)

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

## 6. 如何精确碰到、踩到或抓到目标

让角色走到交互物旁、让手腕在接触帧对准握点、让物体在抓住后移动，是连续的三件事。UE 可组合 Motion Warping、IK 和游戏侧的交互绑定。Motion Warping 修正根运动，先让目标进入身体的可达范围。IK 在最后的姿态上修正手或脚。是否抓住物体由交互逻辑根据本步解出的姿态和物理状态确认。UE 的 Motion Warping、Animation Blueprint 骨骼节点与 UAF 扩展有各自的入口。UAF 可以用 Control Rig 任务读取上一个姿态并写回修正结果。 [Motion Warping 文档][motion-warping] · [Two Bone IK 文档][two-bone-api] · [UAF Control Rig 插件][uaf-control-rig] · [Control Rig 求值任务][control-task]

### 6.1 将交互点反解为根目标

资产需要标出要对齐的接触时刻及坐标系。例如，抬手动画用掌心握点和掌心方向，物体上用握点 socket。两端都有完整的平移和旋转。用 `T(A←B)` 表示把 B 空间变到 A 空间，采用列向量的数学约定。如果片段接触帧的根到接触标记为 `T(root←contact)`，世界里的目标接触位姿为 `T(world←contact_goal)`，则

~~~text
T(world ← root_goal) = T(world ← contact_goal) × inverse(T(root ← contact))
~~~

若设成把根直接送往物体握点，掌心到根的偏移会多算一次。`URootMotionModifier_Warp` 的 Static 和 Bone provider 会在窗口结束时计算动画根相对静态标记或命名骨骼的偏移，再与游戏侧的 warp target 合成。游戏侧目标还可从组件或它的 socket 取得并随组件更新。 [窗口与对象目标][target] · [计算根相对接触点][warp-offset]

### 6.2 Skew Warp 如何在窗口结束时靠近目标

假设动作在 `t_hit` 抓到物体。把 warp 窗口设为动作开始接近以后、接触时刻之前的一段区间。Motion Warping 读到当前动画落在该窗口时激活 modifier。每步抽取从上次播放时刻到窗口末端的总剩余根运动 `R`，以及本步在窗口内的增量 `dr`。目标转换到移动者当前对应的局部空间，得到尚需到达的位移 `D`。 [从通知激活窗口并修改根运动][windows] · [剩余运动与窗口截取][skew-range]

1. 以 `R` 的方向为前轴建同步坐标系，把 `D` 与 `dr` 转进去。沿前轴的比例可写为 `s = dot(D, normalize(R)) / ‖R‖`。
2. 按目标相对前轴的偏航与俯仰构造两个 shear。实现用这些角度的正切填入矩阵，把纵向根运动的一部分映射到侧向和高度。按源码次序组合缩放与 shear，再作用到**本步增量**，最后换回移动者坐标。目标位于前轴投影退化方向时有单独的方向和长度处理。
3. 原窗口几乎没有平移时，Skew Warp 可以按窗口进度和曲线补一段从起始位置到目标的移动。若本步跨过窗口末端，末端之后的原片段根运动单独保留。旋转按剩余时间修正，可匹配目标旋转、转向目标，并按选择的旋转模式限制速率。 [同步坐标、缩放与 shear][skew-math] · [原地窗口、速度限幅和跨窗][skew-step] · [旋转修正][warp-rotation]

Character adapter 在根运动转换到世界空间前接入 modifier。角色移动仍要通过 sweep 等碰撞步骤，所以墙面阻挡、速度上限或忽略 Z 都可能留下终点误差。交互应使用提交后的角色位姿求末端 IK，并检查真实接触误差。Pose Search 的多人交互也有一条 UAF 对齐入口。`FEvaluationNotify_PoseSearchInteractionAlignment` 取得所选动作及角色对应的对齐变换，在剩余对齐时间内推进，并从当前角色上下文反算根运动 delta。它适合先让多角色会合，局部握点仍可随后通过 IK 校正。 [Character adapter][character-warp] · [移动组件应用根运动与 sweep][movement-root] · [扫掠移动][movement-sweep] · [UAF 交互对齐][interaction]

### 6.3 从物体握点得到手腕目标

手腕原点通常在掌根，物体握点可能在掌心。分别标定物体局部握点 `T(object←grip)` 和角色手局部握点 `T(hand←grip)`。两者相合时

~~~text
T(world ← grip_goal) = T(world ← object_actual) × T(object ← grip)
T(world ← hand_goal) = T(world ← grip_goal) × inverse(T(hand ← grip))
T(mesh  ← hand_goal) = inverse(T(world ← mesh_actual)) × T(world ← hand_goal)
~~~

这是由两侧 socket 重合约束推导的组合方式。每个更新用同一份角色和物体位姿快照算目标，移动物体在接触前持续重新采样。手指开合可以由抓取片段、曲线或 Control Rig 处理。只指定手腕位置时，还应单独提供手掌朝向与肘部弯曲平面。 [对象目标来源][target] · [IK 的输入空间和末端旋转][two-bone-node]

**两骨 IK 的解析解。** 令肩或髋为 `A`，两段长度为 `L₁,L₂`，`u` 是从 `A` 到目标的单位向量，`d` 是该距离。把 pole 向量在垂直于 `u` 的平面上投影并归一化得到弯曲方向 `v`。可达且非退化时，中间关节的位置为

~~~text
x = (L₁² + d² − L₂²) / (2d)
y = sqrt(max(0, L₁² − x²))
elbow_or_knee = A + x u + y v
~~~

解出中间点后，分别旋转上下两段，让原方向对准新的两段方向。UE 代码用余弦与投影计算这个三角形，含目标几乎重合、pole 与目标方向共线、伸展阈值和最远距离处理。两段长度不同，最小理论可达距离为 `|L₁−L₂|`，限位还可能进一步缩小可达域。源代码中的三角公式在正常可达区成立，过近目标要按项目的可达性处理。末端朝向可由节点选项或单独的腕部约束控制。 [Two Bone IK 三角求解][two-bone-math] · [由新方向构造骨旋转][two-bone-orient] · [末端旋转控制][two-bone-node]

**多个接触用全身 IK。** 同时要求手抓扶手、另一只手扶墙、双脚着地时，目标共享胸腔或骨盆，适合由 PBIK 一起求。UE 每帧从输入姿态更新骨与求解体，按权重设置 effector pin，先做可选的 root pre-pull、偏好角和链预拉，然后迭代 pin、骨连接和角度限制。根可钉在输入姿态。pin 先用目标差旋转对应求解体，再按修正量推动位置。收尾可沿根到叶消除拉伸、满足角限制，再从求解体回写骨骼。迭代数、骨骼刚度、活动链和目标的相容性一起决定残差。 [全身 IK 输入及根策略][pbik-update] · [子链与主迭代][pbik-iteration] · [pin 求解][pbik-pin] · [关节限制][pbik-limit]

**抓取的完成条件。** 下面是一种把 UE 各部件组合起来的流程。接触通知标记允许交互的时段。应用将求解后的握点距离和朝向误差与自己的容差比较，再提交绑定。位置误差可取两握点的欧氏距离，旋转误差可取 `2 arccos(clamp(|q_hand · q_object|, 0, 1))`。动画时标已到且手还离目标很远时，可以延长或中止交互。持有状态应用同一握点偏移。随手走的物体使用附着，仍参与物理的物体可建立约束并在下一物理步生效。释放时结束附着或约束，并淡出手部 IK。 [动画通知与求值顺序](#3-update-选活动分支evaluate-发出计算任务) · [物理约束连接组件与骨骼][grab-constraint]

~~~text
选取能到达目标的动作和接触时刻
每步调整根运动，读取通过碰撞后的实际角色根
从物体实际位姿与握点偏移得到手腕目标
解 IK 和手指，读取最终手部握点，检查距离与旋转误差
接触窗口内且误差满足容差时，建立附着或物理约束
~~~

## 7. 同一动作怎样适配不同骨骼

UE 的 IK Retargeter 运行有顺序的 operation stack。典型流程是先用 retarget pose 校正两副骨架的参考关系，再由骨盆／根运动、FK Chains、IK Goals 和目标 IK Rig 各自处理一部分。不同拓扑靠语义骨链建立关系，例如将左右腿映射到各自的髋到踝，链内的骨骼数量允许不同。目标仍使用自己的骨长、父链和蒙皮权重。每个目标骨架可有自己的 retarget asset 和握点定义。运行时的 Retarget Pose From Mesh 节点允许从已先更新的源网格或图内姿态读取源动作，离线也可导出成各目标的片段。 [操作栈][retarget-stack-api] · [逐项执行][retarget-run] · [运行期输入][retarget-runtime-api] · [离线导出][retarget-export-api]

### 7.1 从参考姿态映射整条链

1. **对齐参考。** 源和目标各设 retarget pose，例如把源 T-pose 与目标 A-pose 调到可比的臂和掌心朝向。关键是明确骨链起止及链的局部朝向。骨盆平移可按源目标参考骨盆高度比例缩放，源码还可与绝对位移混合，并单独控制水平与竖直量。 [骨盆尺度][pelvis-scale] · [骨盆运动计算][pelvis-motion]
2. **按归一化弧长对应骨骼。** 在 Interpolated 旋转模式，令目标骨骼参数 `uᵢ = 到第 i 骨的参考累计长度 / 本链参考总长`，在源链上用同一个 `uᵢ` 插值参考姿态及当前姿态。目标脊柱比源脊柱多一个关节时，可落在两个源骨之间。还可选一一映射和从末端匹配。 [骨链参数][chain-param] · [按参数插值 TRS][chain-sample] · [映射模式][fk-rotation]
3. **应用旋转差。** 源链先按当前目标父链方向对齐。以 Interpolated 路径为例，计算 `Δq = q_source_current × inverse(q_source_reference)`，然后 `q_target = Δq × q_target_reference`。选择保留目标偏移的平移模式时，目标骨的位置由已更新父变换乘自己的局部参考偏移得到。这样迁移的是旋转行为，目标链仍保留自己的骨长。 [父链对齐][fk-decode] · [旋转差与平移选项][fk-delta]

### 7.2 用目标骨长和末端约束补偿

FK 迁移后，同样的肘膝角度在高矮角色上会落到不同位置。默认的 IK chain 位置计算用**参考起点到末端的直线距离**作比例基准。若该距离在源、目标分别为 `d_src_ref`、`d_dst_ref`，源当前的末端减起点向量为 `v_src`，可把基础目标写成 `p_goal = p_dst_start + v_src × (d_dst_ref / d_src_ref)`。源码随后按设置加入骨盆影响、与源目标位置混合、局部偏移和延伸。旋转也按源末端的参考到当前增量迁移。最终把目标交给 Run IK Rig，在目标骨长和限位上求解。 [直线长度预计算][ik-length] · [末端目标算法][ik-goal] · [运行目标 IK Rig][ik-run]

**接触要在目标角色上复验。** 重定向后的自然抓取姿态可以由链比例计算。若两名角色都必须握同一个场景把手，则在采样与重定向以后，按各自腕到掌心偏移重建**同一世界目标**。矮角色可允许骨盆或肩先靠近，或选择更合适的原动作。脚锁也可以在重定向阶段使用 speed planting。UE 用轨道提供的脚速与阈值判定低速支撑，保存或释放相应 IK 目标，并按配置用弹簧过渡。若地形与原动作不同，仍需运行时地面反馈。 [速度阈值和目标保持][speed-plant] · [不同体型的 IK 配置][retarget-ik-api]

## 8. 收到物理反馈后怎样调整骨骼

三类反馈的输入不同。场景查询返回地面或表面的命中位置和法线，可变成 IK 目标。图内刚体节点求解局部辅助骨骼。世界刚体则让身体或物体真正参与物理，再把已求解的刚体姿态合入骨架。下述世界刚体和 RigidBody Animation Node 是 UE 已有的组件／AnimGraph 实现路径。UAF 可通过求值任务或 Control Rig 消费目标与姿态，具体接线由模块配置决定。 [UAF 的 Control Rig 任务][control-task] · [RigidBody 节点 API][rigid-node-api] · [世界物理骨骼混合][phys-blend]

### 8.1 碰撞查询转换成脚部目标

Foot Placement 的基本输入包括动画的 FK 脚、IK 脚与骨盆、脚的速度或曲线、种植锁定状态，以及本步的移动和地面信息。算法用碰撞查询取得候选落脚面；判断是否进入、维持或解除支撑；对齐时沿接近方向 `d` 与落脚平面求交。若平面经过 `p₀`、法线为 `n`、未对齐位置为 `f`，交点是 `f+d[n·(p₀−f)/(n·d)]`。代码从交点再减掉沿同一方向的输入动画参考高度偏移，用参考脚底平面到地面法线的最短弧旋转调整脚朝向，并保留或去除部分扭转。按每条腿的伸展范围约束并平滑骨盆偏移，再限制脚目标。输出的骨盆和 IK 脚目标须交给后续腿部 IK，才能让大腿和小腿真正弯到目标。命中不可靠或转入摆动期时，按曲线或阈值逐步释放锁定。 [地面 sweep][foot-query] · [脚底与法线对齐][foot-plane] · [落脚与解除条件][foot-lock] · [骨盆范围][foot-pelvis] · [生成骨盆和脚目标][foot-output]

移动平台上的接触可表达为 `T(world←platform_actual) × T(platform←contact_locked)`，每步用已经发布的平台变换计算当前目标。这是组合接触锁定与物理快照的空间约定。用于图内的位置目标应转到当步 mesh 空间，避免角色移动和平台移动被重复补偿。

### 8.2 图内辅助骨骼模拟

以尾巴、挂件或头发骨链为例，`FAnimNode_RigidBody` 根据 Physics Asset 分配模拟与运动学体，可选组件、世界或基骨空间。输入动画将非模拟体推到运动学目标，或者在开始模拟时用前后姿态估计并传入初速度。节点推进自己的 Immediate Physics 模拟，取得动态体结果后转换回角色组件空间，以 `FBoneTransform` 写回对应骨骼。切换模拟空间、瞬移或重置时需要重新初始化预测和速度。这样挂件可从动画挂点运动，同时对局部撞击、重力和角色惯性做出响应。 [图内刚体初始化][rban-init] · [初速度与重置][rban-start] · [写入运动学目标][rban-target] · [步进][rban-step] · [把刚体输出换回骨骼][rban-out]

### 8.3 世界物理、物理动画驱动与 ragdoll

世界物理的典型时间线是「动画求值与运动学目标 → Chaos 子步 → 读取刚体结果 → 骨架合成」。Skeletal Mesh 组件可以把动画骨骼作为运动学目标交给物理，其 EndPhysics tick 等待世界物理完成，然后混合骨骼。动态手臂或 ragdoll 的最终合成必须读取本次已完成的刚体结果。对于计划让动画继续施力的关节，`UPhysicalAnimationComponent` 在 PrePhysics 调整运动学目标体和 drive。世界模式从动画骨骼的组件空间姿态算目标，局部模式积累动画骨局部变换直到找到最近物理祖先，再挂到其当前刚体世界位姿。世界模式可配线性与角向 drive，局部模式代码把线性 drive 参数置零。驱动通过刚度、阻尼和力限制控制追随力度，所以受阻时肢体仍可偏离动画目标。 [动画推送运动学骨骼][kinematic-push] · [EndPhysics 前驱][end-physics] · [驱动时钟][phys-drive-tick] · [目标空间][phys-target] · [设置 drive 参数][phys-drive]

**物理结果转回局部骨骼。** `PerformBlendPhysicsBones` 从 Physics Asset 的骨骼映射取世界刚体变换。UE 为父骨维护一个世界缓存，可直接用有效的物理父变换，或递归用当前局部姿态构造。设此参考父为 `T(world←parent_ref)`，则要混合的局部物理变换可近似写成

~~~text
T(parent_ref ← bone_phys) = inverse(T(world ← parent_ref)) × T(world ← bone_phys)
L_out = BlendTRS(L_anim, L_phys, physics_weight)
~~~

源码还处理根和 mesh 的缩放，保留动画骨骼的 scale，再由新局部姿态重建组件空间。无物理体或禁用模拟的骨骼按对应分支保留动画或随父传播。`physics_weight` 是最终显示的混合权重。drive 则作用于求解器，其驱动强度和显示权重控制的是不同阶段。 [父骨世界缓存][phys-parent] · [缓存物理骨姿态][phys-cache] · [局部变换及权重合成][phys-blend]

接入交互时应明确最终姿态写入顺序。例如碰撞后全身倒地阶段，让 pelvis 与已交给 ragdoll 的肢体服从刚体结果；恢复阶段才逐步提高动画驱动、减小物理显示混合。普通走路落脚由地面查询驱动 IK。抓住的物体若已由物理约束连接到手，下一子步以实际物体位姿更新可用目标，避免本帧再以独立刚性手部 IK 覆盖同一受约束关节。物理子步中的碰撞、位置和速度求解见 [Chaos 刚体算法](chaos-physics.md)。 [物理回写调度][end-physics] · [局部混合][phys-blend]

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

[motion-warping]: https://dev.epicgames.com/documentation/en-us/unreal-engine/motion-warping-in-unreal-engine?application_version=5.8
[two-bone-api]: https://dev.epicgames.com/documentation/en-us/unreal-engine/animation-blueprint-two-bone-ik-in-unreal-engine?application_version=5.8
[uaf-control-rig]: https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/UAFControlRig?application_version=5.8
[retarget-stack-api]: https://dev.epicgames.com/documentation/en-us/unreal-engine/retargeting-operation-stack-in-unreal-engine-5-8
[retarget-ik-api]: https://dev.epicgames.com/documentation/en-us/unreal-engine/animating-ik-retarget-settings-in-unreal-engine?application_version=5.8
[rigid-node-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/AnimGraphRuntime/FAnimNode_RigidBody?application_version=5.8
[control-task]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFControlRig/Source/UAFControlRig/Private/ControlRigTask.cpp#L85-L205
[target]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/RootMotionModifier.cpp#L65-L135
[warp-offset]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/RootMotionModifier.cpp#L393-L466
[windows]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/MotionWarpingComponent.cpp#L426-L565
[skew-range]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/RootMotionModifier_SkewWarp.cpp#L136-L194
[skew-math]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/RootMotionModifier_SkewWarp.cpp#L31-L134
[skew-step]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/RootMotionModifier_SkewWarp.cpp#L189-L253
[warp-rotation]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/RootMotionModifier.cpp#L528-L628
[character-warp]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/MotionWarping/Source/MotionWarping/Private/MotionWarpingCharacterAdapter.cpp#L18-L56
[movement-root]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/Components/CharacterMovementComponent.cpp#L2089-L2176
[movement-sweep]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/Components/CharacterMovementComponent.cpp#L4436-L4475
[interaction]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/UAF/UAFPoseSearch/Source/UAFPoseSearch/Private/PoseSearchInteractionAlignment.cpp#L20-L82
[two-bone-node]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimGraphRuntime/Private/BoneControllers/AnimNode_TwoBoneIK.cpp#L109-L197
[two-bone-math]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimationCore/Private/TwoBoneIK.cpp#L70-L184
[two-bone-orient]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimationCore/Private/TwoBoneIK.cpp#L15-L59
[pbik-update]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/FullBodyIK/Source/PBIK/Private/Core/PBIKSolver.cpp#L221-L297
[pbik-iteration]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/FullBodyIK/Source/PBIK/Private/Core/PBIKSolver.cpp#L531-L605
[pbik-pin]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/FullBodyIK/Source/PBIK/Private/Core/PBIKConstraint.cpp#L299-L355
[grab-constraint]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/PhysicsEngine/PhysicsConstraintComponent.cpp#L404-L450
[retarget-run]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/IKRetargetProcessor.cpp#L1286-L1429
[pelvis-scale]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/PelvisMotionOp.cpp#L180-L228
[pelvis-motion]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/PelvisMotionOp.cpp#L290-L389
[chain-param]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/IKRetargetProcessor.cpp#L903-L929
[chain-sample]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/IKRetargetProcessor.cpp#L675-L714
[fk-rotation]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/FKChainsOp.cpp#L241-L341
[fk-decode]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/FKChainsOp.cpp#L143-L223
[fk-delta]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/FKChainsOp.cpp#L342-L414
[ik-length]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/IKChainsOp.cpp#L22-L65
[ik-goal]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/IKChainsOp.cpp#L79-L167
[ik-run]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/RunIKRigOp.cpp#L278-L325
[speed-plant]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/IKRig/Source/IKRig/Private/Retargeter/RetargetOps/SpeedPlantingOp.cpp#L53-L110
[foot-query]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/AnimationWarping/Source/Runtime/Private/BoneControllers/AnimNode_FootPlacement.cpp#L130-L188
[foot-plane]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/AnimationWarping/Source/Runtime/Private/BoneControllers/AnimNode_FootPlacement.cpp#L414-L471
[foot-lock]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/AnimationWarping/Source/Runtime/Private/BoneControllers/AnimNode_FootPlacement.cpp#L601-L729
[foot-pelvis]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/AnimationWarping/Source/Runtime/Private/BoneControllers/AnimNode_FootPlacement.cpp#L1911-L1978
[foot-output]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Animation/AnimationWarping/Source/Runtime/Private/BoneControllers/AnimNode_FootPlacement.cpp#L1230-L1286
[rban-init]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimGraphRuntime/Private/BoneControllers/AnimNode_RigidBody.cpp#L1170-L1233
[rban-start]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimGraphRuntime/Private/BoneControllers/AnimNode_RigidBody.cpp#L594-L708
[rban-target]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimGraphRuntime/Private/BoneControllers/AnimNode_RigidBody.cpp#L775-L832
[rban-step]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimGraphRuntime/Private/BoneControllers/AnimNode_RigidBody.cpp#L470-L506
[rban-out]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/AnimGraphRuntime/Private/BoneControllers/AnimNode_RigidBody.cpp#L889-L980
[kinematic-push]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/Components/SkeletalMeshComponent.cpp#L3321-L3351
[end-physics]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/Components/SkeletalMeshComponent.cpp#L774-L796
[phys-drive-tick]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/PhysicsEngine/PhysicalAnimationComponent.cpp#L48-L72
[phys-target]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/PhysicsEngine/PhysicalAnimationComponent.cpp#L213-L272
[phys-drive]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/PhysicsEngine/PhysicalAnimationComponent.cpp#L350-L388
[phys-parent]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/PhysicsEngine/PhysAnim.cpp#L110-L141
[phys-cache]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/PhysicsEngine/PhysAnim.cpp#L253-L289
[phys-blend]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Engine/Private/PhysicsEngine/PhysAnim.cpp#L291-L380
[pbik-limit]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Plugins/Experimental/FullBodyIK/Source/PBIK/Private/Core/PBIKConstraint.cpp#L111-L195
[retarget-runtime-api]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/IKRig/FAnimNode_RetargetPoseFromMesh?application_version=5.8
[retarget-export-api]: https://dev.epicgames.com/documentation/en-us/unreal-engine/auto-retargeting-in-unreal-engine?application_version=5.8
